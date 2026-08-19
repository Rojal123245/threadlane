//! CodingAgent composition root — agent construction, lifecycle, subagent dispatch,
//! harness journal extension, and event-loop orchestration.

use super::capabilities::{
    build_broker_dispatcher, create_after_tool_hook_handler, dispatch_hook_requests,
    extension_before_tool_hook_handler, render_agent_catalog, restored_tool_policy, McpCapability,
    PlanCapability, SkillCapability, SubagentCapability, WasiCapability,
};
use super::harness::{
    harness_cancellation_state, HarnessWatch, InterruptedSubagentRecoveryState,
    SubagentLaneIdentity, SubagentStartError,
};
use super::CodingSessionHarness as HarnessJournal;
use super::ManagedProcessRegistry;
use crate::agents::{discover_agents, AgentDefinition, AgentScope};
use crate::commands::{execute_slash_command, parse_slash_command, CommandAction};
use crate::context::ProjectContext;
use crate::extension_broker::CapabilityDispatcher;
use crate::plan::SessionPlanStore;
use crate::system_prompt::{build_system_prompt, SystemPromptBuildOptions, SystemPromptConfig};
#[cfg(test)]
use async_trait::async_trait;
use log::warn;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
#[cfg(test)]
use std::sync::atomic::AtomicBool;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use threadlane_agent::harness::{
    DeferredResolution, Entry as HarnessEntry, HookContext, HookKind, JsonlStore, OperationOutcome,
    PromptSnapshot, ProvisionedEntry, QueueKind, Record as HarnessRecord, Reducer, SessionStore,
    Snapshot, ToolRecovery, ToolSpec,
};
use threadlane_agent::{
    repair_interrupted_tool_turn, AgentEvent, AgentMessage, AgentToolResult, ImageAttachment,
    ReasoningEffort, SessionTree, SubagentRecoveryStatus, TokenUsage, TurnState, UnifiedAgent,
};
#[cfg(test)]
use threadlane_agent::{AgentToolDefinition, ToolExecutor};
use threadlane_mcp::McpManager;
use threadlane_provider::openai::fetch_available_models;
use threadlane_skills::{SkillManager, SkillRegistry};
use threadlane_wasi::packages::default_global_threadlane_dir;
use threadlane_wasi::{WasiExtensionManager, WasiLegacyEffect};
use tokio::sync::broadcast;
use tokio::time::{timeout, Duration};

pub(crate) const MAX_SUBAGENT_TASKS: usize = 8;
pub(crate) const MAX_SUBAGENT_TASK_CHARS: usize = 32_000;
const SUBAGENT_TIMEOUT: Duration = Duration::from_secs(10 * 60);
const SUBAGENT_RECOVERY_PROMPT: &str =
    "Continue from the recovered checkpoint and finish the assigned task.";
const MAX_PERSISTED_SYSTEM_PROMPT_BYTES: usize = 256 * 1024;

fn sha256_hex(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn durable_prompt_snapshot(content: &str) -> PromptSnapshot {
    let sha256 = threadlane_agent::harness::TraceString::new(sha256_hex(content.as_bytes()))
        .expect("sha256 digest is bounded");
    let explicitly_redacted = std::env::var("THREADLANE_REDACT_SYSTEM_PROMPTS")
        .ok()
        .is_some_and(|value| matches!(value.as_str(), "1" | "true" | "yes"));
    if explicitly_redacted || content.len() > MAX_PERSISTED_SYSTEM_PROMPT_BYTES {
        PromptSnapshot::Redacted {
            sha256,
            byte_len: content.len(),
            reason: threadlane_agent::harness::TraceString::new(if explicitly_redacted {
                "configured_redaction"
            } else {
                "size_limit"
            })
            .expect("redaction reason is bounded"),
        }
    } else {
        PromptSnapshot::Full {
            content: threadlane_agent::harness::BoundedPromptText::new(content)
                .expect("system prompt is within byte limit"),
            sha256,
        }
    }
}

pub(crate) fn is_retryable_generation_error(error: &str) -> bool {
    let error = error.to_ascii_lowercase();
    [
        "timeout",
        "timed out",
        "connection reset",
        "connection refused",
        "temporarily unavailable",
        "rate limit",
        "status 429",
        "status 502",
        "status 503",
        "status 504",
    ]
    .iter()
    .any(|marker| error.contains(marker))
}

pub(crate) fn generation_event_drain_error(
    error: broadcast::error::TryRecvError,
) -> Option<&'static str> {
    match error {
        broadcast::error::TryRecvError::Lagged(_) => None,
        broadcast::error::TryRecvError::Empty | broadcast::error::TryRecvError::Closed => {
            Some("generation ended without a durable AgentEnd event")
        }
    }
}
static NEXT_SUBAGENT_UI_RUN_ID: AtomicU64 = AtomicU64::new(1);

pub(crate) type AgentRunner = Arc<
    dyn Fn(
            Vec<AgentRunTask>,
            bool,
            Option<String>,
        ) -> Pin<Box<dyn Future<Output = Result<Value, String>> + Send>>
        + Send
        + Sync,
>;

#[cfg(test)]
pub(crate) type AgentWorkObserver = Arc<std::sync::Mutex<Vec<AgentWork>>>;
#[cfg(test)]
pub(crate) type SubagentObserverState = Arc<std::sync::Mutex<Option<AgentWorkObserver>>>;
#[cfg(test)]
pub(crate) type SubagentBoundaryObserver = Arc<dyn Fn() + Send + Sync>;

#[derive(Clone)]
pub(crate) struct SubagentRunContext {
    api_key: String,
    account_id: Option<String>,
    parent_model: String,
    parent_session_id: String,
    work_dir: PathBuf,
    extensions: Arc<WasiExtensionManager>,
    parent_event_tx: broadcast::Sender<AgentEvent>,
    parent_leaf_id: Option<String>,
    session_file: Option<PathBuf>,
    #[cfg(test)]
    scheduler_observer: Option<AgentWorkObserver>,
    #[cfg(test)]
    child_work_observer: Option<SubagentBoundaryObserver>,
    #[cfg(test)]
    child_tool_observer: Option<Arc<AtomicBool>>,
    semaphore: Arc<tokio::sync::Semaphore>,
}

use crate::policy::ToolPolicy;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum AgentWork {
    RequestTurn(String),
    SteerMessage {
        content: String,
        images: Vec<ImageAttachment>,
    },
    NextRunMessage {
        content: String,
        images: Vec<ImageAttachment>,
    },
    QueueMessage {
        content: String,
        images: Vec<ImageAttachment>,
    },
}

fn harness_next_seq(store: &JsonlStore) -> u64 {
    store
        .entries()
        .iter()
        .map(|entry| entry.seq)
        .chain(store.records().iter().map(HarnessRecord::seq))
        .max()
        .unwrap_or(0)
        + 1
}

fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

fn enqueue_harness_queue(
    session_file: &Path,
    queue: QueueKind,
    content: String,
    images: Vec<ImageAttachment>,
) -> Result<String, String> {
    let mut agent = UnifiedAgent::new(
        "dummy",
        None,
        "dummy",
        session_file,
        threadlane_agent::AgentConfig::default(),
    )
    .map_err(|e| e.to_string())?;
    agent.enqueue_harness_queue(queue, content, images)
}

pub(crate) fn enqueue_harness_follow_up(
    session_file: &Path,
    content: String,
    images: Vec<ImageAttachment>,
) -> Result<String, String> {
    enqueue_harness_queue(session_file, QueueKind::FollowUp, content, images)
}

fn consume_harness_queue(session_file: &Path, queue: QueueKind) -> Result<(), String> {
    let mut agent = UnifiedAgent::new(
        "dummy",
        None,
        "dummy",
        session_file,
        threadlane_agent::AgentConfig::default(),
    )
    .map_err(|e| e.to_string())?;
    agent.consume_harness_queue(queue)
}

fn consume_harness_follow_ups(session_file: &Path) -> Result<(), String> {
    consume_harness_queue(session_file, QueueKind::FollowUp)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentRunTask {
    pub(crate) agent: String,
    pub(crate) task: String,
    pub(crate) instructions: Option<String>,
    pub(crate) tools: Option<Vec<String>>,
    pub(crate) model: Option<String>,
}

#[derive(Clone, Debug)]
pub(crate) enum SubagentLaneStatus {
    Completed,
    Failed,
}

#[derive(Clone, Debug)]
pub(crate) struct CompletedSubagentLane {
    lane_name: String,
    run_id: String,
    parent_leaf_id: Option<String>,
    task: String,
    agent: String,
    status: SubagentLaneStatus,
    messages: Vec<AgentMessage>,
    error: Option<String>,
}

pub(crate) fn recover_v2_subagent_records(
    session_file: &Path,
) -> Result<Vec<HarnessRecord>, String> {
    let store = JsonlStore::open(session_file).map_err(|error| error.to_string())?;
    // Collect non-main-lane records directly — no conversion needed since
    // harness::Record is now the canonical type.
    let mut records: Vec<HarnessRecord> = store
        .records()
        .iter()
        .filter(|r| r.lane() != "main")
        .cloned()
        .collect();
    let open_runs: HashMap<_, _> = records
        .iter()
        .filter_map(|record| match record {
            HarnessRecord::OperationStarted { lane, id, .. } => Some((lane.clone(), id.clone())),
            _ => None,
        })
        .collect();
    let mut checkpoint_messages = HashMap::<String, Vec<AgentMessage>>::new();
    for entry in store.entries() {
        if entry.lane == "main" {
            continue;
        }
        if let Some(run_id) = open_runs.get(&entry.lane) {
            checkpoint_messages
                .entry(run_id.clone())
                .or_default()
                .push(entry.message.clone());
            records.push(HarnessRecord::WriteDeferred {
                id: entry.id.clone(),
                seq: entry.seq,
                lane: entry.lane.clone(),
                timestamp: entry.timestamp,
                run_id: run_id.clone(),
                target: ProvisionedEntry {
                    id: entry.id.clone(),
                    surface_op: threadlane_agent::harness::SurfaceOperation::Append,
                    parent_id: entry.parent_id.clone(),
                    message: entry.message.clone(),
                },
            });
        }
    }
    for (run_id, messages) in checkpoint_messages {
        let has_checkpoint = messages
            .iter()
            .any(|message| matches!(message, AgentMessage::Assistant { .. }))
            || messages
                .iter()
                .filter(|message| matches!(message, AgentMessage::User { .. }))
                .count()
                > 1;
        if has_checkpoint {
            let lane = records
                .iter()
                .find_map(|record| match record {
                    HarnessRecord::OperationStarted { id, lane, .. } if id == &run_id => {
                        Some(lane.clone())
                    }
                    _ => None,
                })
                .unwrap_or_default();
            let task = messages.iter().rev().find_map(|message| match message {
                AgentMessage::User { content } => Some(content.clone()),
                _ => None,
            });
            if let Some(_task) = task {
                records.push(HarnessRecord::StepAttempt {
                    id: format!("task-attempt-{run_id}-recovered"),
                    seq: records.iter().map(HarnessRecord::seq).max().unwrap_or(0) + 1,
                    lane,
                    timestamp: now_millis(),
                    run_id,
                    attempt: 1,
                    result_entry_id: String::new(),
                    compaction_reason: None,
                });
            }
        }
    }
    Ok(records)
}

#[derive(Clone, Debug, Default)]
pub struct SubagentCancellationGuard;

struct ActiveRun {
    id: u64,
    handle: tokio::task::AbortHandle,
}

#[derive(Default)]
struct ActiveRunState {
    next_id: u64,
    active: Option<ActiveRun>,
    cancellation_guard: Option<SubagentCancellationGuard>,
}

#[derive(Clone)]
pub struct CodingAgentCancellation {
    state: Arc<std::sync::Mutex<ActiveRunState>>,
    harness_session_file: Option<PathBuf>,
    event_tx: broadcast::Sender<AgentEvent>,
}

impl CodingAgentCancellation {
    pub fn track_active_run(&self, handle: tokio::task::AbortHandle) -> Result<u64, String> {
        let mut state = self.state.lock().map_err(|error| error.to_string())?;
        if state.active.is_some() {
            return Err("A generation is already running".into());
        }
        state.next_id = state.next_id.wrapping_add(1);
        let id = state.next_id;
        state.active = Some(ActiveRun { id, handle });
        Ok(id)
    }

    pub fn finish_active_run(&self, id: u64) {
        if let Ok(mut state) = self.state.lock() {
            if state.active.as_ref().is_some_and(|active| active.id == id) {
                state.active = None;
            }
        }
    }

    fn clear_cancellation_guard(&self) {
        if let Ok(mut state) = self.state.lock() {
            state.cancellation_guard = None;
        }
        if let Some(path) = self.harness_session_file.as_deref() {
            harness_cancellation_state(path).store(false, Ordering::SeqCst);
        }
    }

    pub fn cancel(&self) -> Result<(), String> {
        let durable_run_id = if let Some(path) = self.harness_session_file.as_deref() {
            let mut journal = HarnessJournal::open(path)?;
            journal.request_abort()?
        } else {
            None
        };
        let handle = {
            let mut state = self.state.lock().map_err(|error| error.to_string())?;
            state.active.take().map(|active| active.handle)
        };
        let acknowledged = handle.is_some();
        if let Some(handle) = handle {
            handle.abort();
        }
        if let (Some(path), Some(run_id)) = (
            self.harness_session_file.as_deref(),
            durable_run_id.as_deref(),
        ) {
            HarnessJournal::open(path)?.observe_abort_signal(run_id, acknowledged)?;
        }
        let _ = self.event_tx.send(AgentEvent::AgentError {
            error: "Generation cancelled".into(),
        });
        Ok(())
    }
}

pub(crate) fn abort_open_subagent_operations(
    session_file: &Path,
) -> Result<SubagentCancellationGuard, String> {
    cancel_open_subagent_operations(session_file)?;
    Ok(SubagentCancellationGuard)
}

pub fn cancel_open_subagent_operations(session_file: &Path) -> Result<(), String> {
    if session_file.exists() {
        let mut journal = HarnessJournal::open(session_file)?;
        journal.refresh()?;
        let open_runs = Reducer::reduce(&journal.store)
            .map_err(|error| error.to_string())?
            .lanes
            .into_iter()
            .filter(|lane| lane.name != "main")
            .filter_map(|lane| lane.open_operation)
            .collect::<Vec<_>>();
        for run_id in open_runs {
            journal
                .store
                .finish_operation(
                    &run_id,
                    OperationOutcome::Aborted,
                    Some("Generation cancelled".into()),
                )
                .map_err(|error| error.to_string())?;
            journal
                .store
                .drive_to_completion()
                .map_err(|error| error.to_string())?;
        }
    }
    Ok(())
}

#[derive(Clone, Default)]
pub(crate) struct AgentWorkScheduler {
    pending: Arc<std::sync::Mutex<Vec<AgentWork>>>,
    #[cfg(test)]
    test_observer: SubagentObserverState,
}

impl AgentWorkScheduler {
    pub(crate) fn schedule(&self, work: AgentWork) {
        if let Ok(mut pending) = self.pending.lock() {
            pending.push(work);
        }
    }

    fn drain(&self) -> Vec<AgentWork> {
        self.pending
            .lock()
            .map(|mut pending| std::mem::take(&mut *pending))
            .unwrap_or_default()
    }

    #[cfg(test)]
    pub(crate) fn set_test_observer(&self, observer: Arc<std::sync::Mutex<Vec<AgentWork>>>) {
        if let Ok(mut current) = self.test_observer.lock() {
            *current = Some(observer);
        }
    }

    async fn run_unified(&self, agent: &mut UnifiedAgent) -> bool {
        let pending = self.drain();
        if pending.is_empty() {
            return false;
        }
        #[cfg(test)]
        if let Ok(Some(observer)) = self.test_observer.lock().map(|observer| observer.clone()) {
            if let Ok(mut observed) = observer.lock() {
                observed.extend(pending);
            }
            return true;
        }
        for work in pending {
            match work {
                AgentWork::RequestTurn(prompt) => agent.prompt(&prompt).await,
                AgentWork::SteerMessage { content, images } => {
                    agent.steer(AgentMessage::user(content, images));
                    agent.run_steer().await;
                }
                AgentWork::NextRunMessage { content, images } => {
                    agent.follow_up(AgentMessage::user(content, images));
                    agent.run_follow_up().await;
                }
                AgentWork::QueueMessage { content, images } => {
                    agent.follow_up(AgentMessage::user(content, images));
                    agent.run_follow_up().await;
                }
            }
        }
        true
    }
}

#[cfg(test)]
pub(crate) struct DeterministicSubagentToolExecutor {
    observed: Arc<AtomicBool>,
}

#[cfg(test)]
#[async_trait]
impl ToolExecutor for DeterministicSubagentToolExecutor {
    fn executor_id(&self) -> &str {
        "threadlane.test.subagent_tool"
    }

    fn tool_definitions(&self) -> Vec<AgentToolDefinition> {
        vec![AgentToolDefinition {
            name: "test_child_tool".into(),
            description: None,
            parameters: serde_json::json!({"type": "object"}),
            strict: None,
        }]
    }

    async fn execute_tool(&self, name: &str, _args: &str) -> Option<Result<String, String>> {
        (name == "test_child_tool").then(|| {
            self.observed.store(true, Ordering::SeqCst);
            Ok("test child tool result".into())
        })
    }
}

#[derive(Clone)]
pub struct CodingAgentWorkHandle {
    scheduler: AgentWorkScheduler,
    session_file: Option<PathBuf>,
}

impl CodingAgentWorkHandle {
    pub fn queue_follow_up(&self, content: impl Into<String>) {
        self.queue_follow_up_with_images(content, Vec::new());
    }

    pub(crate) fn queue_follow_up_with_images(
        &self,
        content: impl Into<String>,
        images: Vec<ImageAttachment>,
    ) {
        let content = content.into();
        if let Some(path) = self.session_file.as_deref() {
            if let Err(error) = enqueue_harness_follow_up(path, content.clone(), images.clone()) {
                warn!("Failed to persist queued follow-up: {error}");
                return;
            }
        }
        self.scheduler
            .schedule(AgentWork::QueueMessage { content, images });
    }

    pub fn queue_steer_with_images(
        &self,
        content: impl Into<String>,
        images: Vec<ImageAttachment>,
    ) -> Result<(), String> {
        let content = content.into();
        if let Some(path) = self.session_file.as_deref() {
            enqueue_harness_queue(path, QueueKind::Steer, content.clone(), images.clone())?;
        }
        self.scheduler
            .schedule(AgentWork::SteerMessage { content, images });
        Ok(())
    }

    pub(crate) fn queue_next_run_with_images(
        &self,
        content: impl Into<String>,
        images: Vec<ImageAttachment>,
    ) -> Result<(), String> {
        let content = content.into();
        if let Some(path) = self.session_file.as_deref() {
            enqueue_harness_queue(path, QueueKind::NextRun, content.clone(), images.clone())?;
        }
        self.scheduler
            .schedule(AgentWork::NextRunMessage { content, images });
        Ok(())
    }

    pub fn try_queue_follow_up_with_images(
        &self,
        content: impl Into<String>,
        images: Vec<ImageAttachment>,
    ) -> Result<(), String> {
        let content = content.into();
        if let Some(path) = self.session_file.as_deref() {
            enqueue_harness_follow_up(path, content.clone(), images.clone())?;
        }
        self.scheduler
            .schedule(AgentWork::QueueMessage { content, images });
        Ok(())
    }

    pub fn cancel_queued_follow_up(&self, entry_id: &str) -> Result<(), String> {
        let Some(path) = self.session_file.as_deref() else {
            return Err("session persistence is unavailable".into());
        };
        let mut agent = UnifiedAgent::new(
            "dummy",
            None,
            "dummy",
            path,
            threadlane_agent::AgentConfig::default(),
        )
        .map_err(|e| e.to_string())?;
        agent.cancel_harness_queue_entry(entry_id)
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct HarnessCompositionSnapshot {
    pub active_lane: String,
    pub session_file: Option<String>,
    pub model: String,
    pub provider: String,
    pub skills: Vec<String>,
    pub extensions: Vec<String>,
    pub sandbox_policy: String,
}

impl HarnessCompositionSnapshot {
    pub fn from_options(options: &CodingAgentOptions) -> Self {
        let provider = if options.model.starts_with("antigravity/") {
            "antigravity"
        } else if options.model.starts_with("opencode-go/") {
            "opencode-go"
        } else if options.model.starts_with("acp/") {
            "acp"
        } else {
            "openai"
        };
        Self {
            active_lane: "main".into(),
            session_file: options
                .session_file
                .as_ref()
                .map(|path| path.display().to_string()),
            model: options.model.clone(),
            provider: provider.into(),
            skills: Vec::new(),
            extensions: Vec::new(),
            sandbox_policy: "workspace-scoped capabilities".into(),
        }
    }

    pub fn resolved(
        options: &CodingAgentOptions,
        skills: &SkillRegistry,
        extensions: &WasiExtensionManager,
    ) -> Self {
        let mut snapshot = Self::from_options(options);
        snapshot.skills = skills
            .list_skills()
            .into_iter()
            .filter(|skill| skill.enabled && skill.is_valid)
            .map(|skill| skill.id)
            .collect();
        snapshot.extensions = extensions
            .extension_manifests()
            .into_iter()
            .map(|extension| extension.name)
            .collect();
        snapshot
    }
}

#[derive(Clone)]
pub struct CodingAgentOptions {
    pub api_key: String,
    pub account_id: Option<String>,
    pub model: String,
    pub work_dir: PathBuf,
    pub session_file: Option<PathBuf>,
    pub system_prompt: SystemPromptConfig,
    /// Agent-level configuration (compaction, stream rules, etc.).
    pub agent_config: Option<threadlane_agent::AgentConfig>,
    /// Coding-agent-specific configuration (subagents, WASI, etc.).
    pub coding_config: Option<crate::config::CodingAgentConfig>,
}

pub struct CodingAgent {
    pub(crate) agent: UnifiedAgent,
    pub session_tree: SessionTree,
    pub wasi_extensions: Arc<WasiExtensionManager>,
    tool_policy: Arc<tokio::sync::Mutex<ToolPolicy>>,
    work_dir: PathBuf,
    skills: Arc<SkillRegistry>,
    agent_runner: AgentRunner,
    broker_dispatcher: Arc<CapabilityDispatcher>,
    managed_processes: ManagedProcessRegistry,
    permission_handle: crate::permission::PermissionHandle,
    pub(crate) agent_work: AgentWorkScheduler,
    mcp_manager: Arc<McpManager>,
    plan_store: SessionPlanStore,
    prompt_templates: Option<Vec<crate::prompt_templates::PromptTemplate>>,
    dispatch_parent_leaf: Arc<std::sync::Mutex<Option<String>>>,
    completed_subagent_lanes: Arc<std::sync::Mutex<Vec<CompletedSubagentLane>>>,
    harness: Option<crate::coding_agent::harness::CodingSessionHarness>,
    harness_journal_error: Option<String>,
    harness_run_id: Arc<std::sync::Mutex<Option<String>>>,
    cancellation: CodingAgentCancellation,
    pub(crate) interrupted_subagent_recovery: InterruptedSubagentRecoveryState,
    #[cfg(test)]
    subagent_work_observer: SubagentObserverState,
    #[cfg(test)]
    subagent_branch_observer: Option<SubagentBoundaryObserver>,
}

impl CodingAgent {
    pub fn permission_handle(&self) -> crate::permission::PermissionHandle {
        self.permission_handle.clone()
    }

    pub(crate) fn set_tool_intent_recorder(
        &mut self,
        recorder: Option<threadlane_agent::ToolIntentRecorder>,
    ) {
        self.agent.tool_dispatcher.tool_intent_recorder = recorder;
    }

    pub(crate) fn set_tool_completion_recorder(
        &mut self,
        recorder: Option<threadlane_agent::ToolCompletionRecorder>,
    ) {
        self.agent.tool_dispatcher.tool_completion_recorder = recorder;
    }

    fn install_run_trace_recorders(&mut self, path: PathBuf, run_id: String) {
        let provider_path = path.clone();
        let provider_run_id = run_id.clone();
        self.agent
            .set_provider_trace_recorder(Some(Arc::new(move |event| {
                let path = provider_path.clone();
                let run_id = provider_run_id.clone();
                Box::pin(async move {
                    HarnessJournal::record_provider_trace_to_path(&path, &run_id, event)
                })
            })));
        let message_path = path.clone();
        self.agent.set_message_recorder(Some(Arc::new(move |message| {
            let path = message_path.clone();
            Box::pin(async move {
                let mut journal = HarnessJournal::open(&path)?;
                journal.append_message(message).map(|_| ())
            })
        })));
        let tool_path = path.clone();
        let tool_run_id = run_id.clone();
        self.agent.tool_dispatcher.tool_execution_trace_recorder = Some(Arc::new(move |event| {
            let path = tool_path.clone();
            let run_id = tool_run_id.clone();
            Box::pin(async move {
                HarnessJournal::record_tool_execution_to_path(&path, &run_id, event).await
            })
        }));
        let completion_path = path.clone();
        let completion_run_id = run_id.clone();
        self.agent.tool_dispatcher.tool_completion_recorder = Some(Arc::new(move |result| {
            let path = completion_path.clone();
            let run_id = completion_run_id.clone();
            let result = result.clone();
            Box::pin(async move {
                HarnessJournal::record_tool_result_to_path(&path, &run_id, &result).await
            })
        }));
        self.permission_handle
            .set_trace_recorder(Some(Arc::new(move |event| {
                let path = path.clone();
                let run_id = run_id.clone();
                Box::pin(async move {
                    HarnessJournal::record_permission_trace_to_path(&path, Some(&run_id), event)
                })
            })));
    }

    pub(crate) async fn execute_accepted_run(
        &mut self,
        accepted: &super::harness::AcceptedRun,
    ) -> Result<(), String> {
        if accepted.lane != "main"
            || accepted.accepted_through_seq == 0
            || accepted.prompt_entry_id.is_empty()
            || accepted.assistant_entry_id.is_empty()
        {
            return Err("invalid accepted run proof".into());
        }
        let prompt_text = self
            .session_tree
            .nodes
            .get(&accepted.prompt_entry_id)
            .and_then(|node| match &node.message {
                AgentMessage::User { content } => Some(content.clone()),
                AgentMessage::UserWithImages { content, .. } => Some(content.clone()),
                _ => None,
            });
        if let Some(prompt) = prompt_text {
            if prompt.starts_with('/') {
                match Box::pin(self.handle_input_with_images(&prompt, Vec::new())).await {
                    Some(Ok(_)) => return Ok(()),
                    Some(Err(err)) => return Err(err),
                    None => return Ok(()),
                }
            }
        }
        self.harness_journal_error = None;
        self.agent
            .run_accepted(
                &accepted.run_id,
                &accepted.lane,
                accepted.accepted_through_seq,
            )
            .await;
        self.sync_session_tree_and_dispatch_assistant_hooks().await;
        if let Some(error) = self.harness_journal_error.take() {
            return Err(error);
        }
        Ok(())
    }

    pub(crate) async fn begin_harness_run(
        &mut self,
        prompt: AgentMessage,
    ) -> Result<Option<super::harness::AcceptedRun>, String> {
        if let Some(run_id) = self
            .harness_run_id
            .lock()
            .map_err(|_| "Harness run state is unavailable".to_string())?
            .clone()
        {
            return Err(format!("run {run_id} is already active; prompt acceptance cannot be repeated"));
        }
        let turn = self.agent.get_state().await;
        let model = self
            .agent
            .config()
            .model_roles
            .resolve_task(&turn.model)
            .to_string();
        let provider = self
            .agent
            .provider_client()
            .provider_kind(&model)
            .to_string();
        let tool_definitions = self.agent.configured_tool_definitions();
        let tool_schema = serde_json::to_vec(&tool_definitions)
            .map_err(|error| format!("failed to serialize resolved tool schema: {error}"))?;
        let enabled_tool_names = tool_definitions
            .iter()
            .map(|tool| tool.name.clone())
            .collect::<Vec<_>>();
        let mut capabilities = enabled_tool_names
            .iter()
            .map(|name| format!("tool:{name}"))
            .collect::<Vec<_>>();
        capabilities.extend(
            self.skills
                .list_skills()
                .into_iter()
                .filter(|skill| skill.enabled && skill.is_valid)
                .map(|skill| format!("skill:{}", skill.id)),
        );
        capabilities.extend(
            self.wasi_extensions
                .extension_manifests()
                .into_iter()
                .map(|extension| format!("extension:{}", extension.name)),
        );
        capabilities.push(format!("tool_policy:{:?}", *self.tool_policy.lock().await));
        capabilities.sort();
        capabilities.dedup();
        let capability_sha256 = sha256_hex(capabilities.join("\n").as_bytes());
        let prompt_template_ids = self
            .prompt_templates
            .as_ref()
            .map(|templates| {
                templates
                    .iter()
                    .map(|template| template.name.clone())
                    .collect()
            })
            .unwrap_or_default();
        let system_prompt = durable_prompt_snapshot(&turn.system_prompt);
        let work_dir = self.work_dir.to_string_lossy().into_owned();
        let Some(journal) = self.harness.as_mut() else {
            return Ok(None);
        };
        let run_id = journal.unique_run_id("foreground")?;
        let accepted = journal.begin_run(&run_id, prompt)?;
        journal.capture_run_context(
            &run_id,
            "main",
            model,
            provider,
            turn.reasoning_effort(),
            self.agent.prompt_cache_enabled(),
            work_dir,
            system_prompt,
            sha256_hex(&tool_schema),
            enabled_tool_names,
            capabilities,
            Some(capability_sha256),
            prompt_template_ids,
            None,
        )?;
        let context = HookContext {
            session_id: journal.store.session_id().to_owned(),
            lane: "main".into(),
            run_id: Some(run_id.clone()),
            resume_data: None,
            tool_call_id: None,
            tool_name: None,
            tool_arguments: None,
            tool_result_content: None,
            tool_result_is_error: None,
        };
        for failure in journal
            .store
            .hooks()
            .run(HookKind::BeforeRun, &context)
            .await
        {
            warn!("before-run hook {} failed: {}", failure.id, failure.message);
        }
        *self
            .harness_run_id
            .lock()
            .map_err(|_| "Harness run state is unavailable".to_string())? = Some(run_id.clone());
        if let Some(path) = self.session_tree.file_path.clone() {
            self.session_tree = SessionTree::load_from_file(&path)
                .map_err(|error| format!("failed to reload accepted prompt: {error}"))?;
            self.install_run_trace_recorders(path, run_id.clone());
        }
        Ok(Some(accepted))
    }

    pub(crate) fn adopt_harness_run(
        &mut self,
        accepted: &super::harness::AcceptedRun,
    ) -> Result<(), String> {
        let run_id = accepted.run_id.as_str();
        if accepted.lane != "main" || accepted.accepted_through_seq == 0 {
            return Err("invalid accepted run proof".into());
        }
        let Some(journal) = self.harness.as_mut() else {
            return Ok(());
        };
        journal.refresh()?;
        let state = Reducer::reduce(&journal.store).map_err(|error| error.to_string())?;
        let Some(open_run) = state
            .lane("main")
            .and_then(|lane| lane.open_operation.as_deref())
        else {
            return Err(format!("harness operation {run_id} is not open on main"));
        };
        if open_run != run_id {
            return Err(format!("harness operation {run_id} is not open on main"));
        }
        let has_context = journal.store.records().iter().any(|record| {
            matches!(
                record,
                HarnessRecord::RunContextCaptured {
                    run_id: captured_run_id,
                    ..
                } if captured_run_id == run_id
            )
        });
        if !has_context {
            let turn = self
                .agent
                .turn
                .try_lock()
                .map_err(|_| "adopted run context is currently locked".to_string())?;
            let model = self
                .agent
                .config()
                .model_roles
                .resolve_task(&turn.model)
                .to_string();
            let provider = self
                .agent
                .provider_client()
                .provider_kind(&model)
                .to_string();
            let tool_definitions = self.agent.configured_tool_definitions();
            let tool_schema = serde_json::to_vec(&tool_definitions)
                .map_err(|error| format!("failed to serialize resolved tool schema: {error}"))?;
            let enabled_tool_names = tool_definitions
                .iter()
                .map(|tool| tool.name.clone())
                .collect::<Vec<_>>();
            let mut capabilities = enabled_tool_names
                .iter()
                .map(|name| format!("tool:{name}"))
                .collect::<Vec<_>>();
            capabilities.extend(
                self.skills
                    .list_skills()
                    .into_iter()
                    .filter(|skill| skill.enabled && skill.is_valid)
                    .map(|skill| format!("skill:{}", skill.id)),
            );
            capabilities.extend(
                self.wasi_extensions
                    .extension_manifests()
                    .into_iter()
                    .map(|extension| format!("extension:{}", extension.name)),
            );
            if let Ok(policy) = self.tool_policy.try_lock() {
                capabilities.push(format!("tool_policy:{policy:?}"));
            }
            capabilities.sort();
            capabilities.dedup();
            let capability_sha256 = sha256_hex(capabilities.join("\n").as_bytes());
            let prompt_template_ids = self
                .prompt_templates
                .as_ref()
                .map(|templates| {
                    templates
                        .iter()
                        .map(|template| template.name.clone())
                        .collect()
                })
                .unwrap_or_default();
            journal.capture_run_context(
                run_id,
                "main",
                model,
                provider,
                turn.reasoning_effort(),
                self.agent.prompt_cache_enabled(),
                self.work_dir.to_string_lossy().into_owned(),
                durable_prompt_snapshot(&turn.system_prompt),
                sha256_hex(&tool_schema),
                enabled_tool_names,
                capabilities,
                Some(capability_sha256),
                prompt_template_ids,
                None,
            )?;
        }
        if let Some(path) = self.session_tree.file_path.clone() {
            self.session_tree = SessionTree::load_from_file(&path)
                .map_err(|error| format!("failed to refresh adopted session: {error}"))?;
            let prompt_entry_id = format!("entry-{run_id}-user");
            if self.session_tree.nodes.contains_key(&prompt_entry_id) {
                self.session_tree.switch_active_node(&prompt_entry_id);
            }
            self.install_run_trace_recorders(path, run_id.into());
        }
        *self
            .harness_run_id
            .lock()
            .map_err(|_| "Harness run state is unavailable".to_string())? = Some(run_id.into());
        Ok(())
    }

    pub(crate) async fn finish_harness_run(
        &mut self,
        run_id: Option<&str>,
        outcome: OperationOutcome,
        error: Option<String>,
    ) -> Result<(), String> {
        let (Some(journal), Some(run_id)) = (self.harness.as_mut(), run_id) else {
            return Ok(());
        };
        if matches!(
            outcome,
            OperationOutcome::Failed | OperationOutcome::Aborted
        ) {
            if let Some(message) = error
                .as_deref()
                .filter(|message| !message.trim().is_empty())
            {
                journal.append_message(AgentMessage::Custom {
                    custom_type: "agent_error".into(),
                    payload: serde_json::json!({ "error": message }),
                })?;
            }
        }
        let result = journal.finish_run(run_id, outcome, error);
        if result.is_ok() {
            let context = HookContext {
                session_id: journal.store.session_id().to_owned(),
                lane: "main".into(),
                run_id: Some(run_id.into()),
                resume_data: None,
                tool_call_id: None,
                tool_name: None,
                tool_arguments: None,
                tool_result_content: None,
                tool_result_is_error: None,
            };
            for failure in journal
                .store
                .hooks()
                .run(HookKind::AfterRun, &context)
                .await
            {
                warn!("after-run hook {} failed: {}", failure.id, failure.message);
            }
        }
        if let Ok(mut active) = self.harness_run_id.lock() {
            if active.as_deref() == Some(run_id) {
                *active = None;
            }
        }
        self.agent.set_provider_trace_recorder(None);
        self.agent.set_message_recorder(None);
        self.agent.tool_dispatcher.tool_execution_trace_recorder = None;
        self.permission_handle.set_trace_recorder(None);
        result
    }

    fn append_command_message(&mut self, message: AgentMessage) -> Result<(), String> {
        if let Some(journal) = self.harness.as_mut() {
            journal.append_message(message)?;
            if let Some(path) = self.session_tree.file_path.clone() {
                self.session_tree = SessionTree::load_from_file(&path)
                    .map_err(|error| format!("failed to reload command message: {error}"))?;
            }
        } else {
            self.session_tree.add_message(message);
        }
        Ok(())
    }

    fn prompt_parent_leaf(
        &mut self,
        message: AgentMessage,
        harness_persisted: bool,
    ) -> Option<String> {
        if !harness_persisted {
            return Some(self.session_tree.add_message(message));
        }
        let leaf = self.session_tree.active_node_id().map(str::to_owned);
        let projected_last = self
            .harness
            .as_mut()
            .and_then(|journal| {
                let _ = journal.refresh();
                journal
                    .store
                    .model_context("main")
                    .ok()
                    .and_then(|projection| projection.messages().pop())
            });
        if projected_last.as_ref() != Some(&message) {
            warn!("Persisted prompt is not the canonical model-context leaf; subagents will use the canonical active leaf");
        }
        leaf
    }

    async fn compact_history_with_harness(&mut self) -> Result<bool, String> {
        if !self.agent.compact_history(None).await {
            return Ok(false);
        }
        let state = self.agent.get_state().await;
        let summary = state
            .messages
            .iter()
            .rev()
            .find_map(threadlane_agent::compaction_summary_text)
            .ok_or_else(|| "compaction produced no durable summary".to_string())?;
        let retained_tail = compaction_retained_tail(&state.messages);
        self.persist_harness_compaction(summary, &retained_tail)?;
        if self.harness.is_some() {
            let path = self
                .session_tree
                .file_path
                .clone()
                .ok_or_else(|| "harness compaction has no session path".to_string())?;
            self.session_tree = SessionTree::load_from_file(&path)
                .map_err(|error| format!("failed to reload compacted session: {error}"))?;
        } else {
            self.session_tree.replace_active_branch(state.messages);
        }
        Ok(true)
    }

    fn persist_harness_compaction(
        &mut self,
        summary: &str,
        retained_tail: &[AgentMessage],
    ) -> Result<(), String> {
        if let Some(journal) = self.harness.as_mut() {
            journal.refresh()?;
            let run_id = journal.unique_run_id("foreground-compaction")?;
            journal
                .store
                .accept_compaction(&run_id, summary)
                .map_err(|error| error.to_string())?;
            journal
                .store
                .drive_to_completion()
                .map_err(|error| error.to_string())?;
            for message in retained_tail {
                journal.append_message(message.clone())?;
            }
        }
        Ok(())
    }

    async fn navigate_tree_branch(&mut self, node_id: &str) -> Result<String, String> {
        if !self.session_tree.nodes.contains_key(node_id) {
            return Err(format!("Node ID not found in session tree: {node_id}"));
        }
        let mut branch_ids = Vec::new();
        let mut current = Some(node_id.to_owned());
        while let Some(id) = current {
            let node = self
                .session_tree
                .nodes
                .get(&id)
                .ok_or_else(|| format!("Node ID not found in session tree: {id}"))?;
            branch_ids.push(id);
            current = node.parent_id.clone();
        }
        branch_ids.reverse();
        let mut harness_target_id = None;
        if let Some(journal) = self.harness.as_mut() {
            journal.refresh()?;
            let mut parent_id = None;
            for legacy_id in branch_ids {
                let node = self
                    .session_tree
                    .nodes
                    .get(&legacy_id)
                    .ok_or_else(|| format!("Node ID not found in session tree: {legacy_id}"))?;
                if matches!(node.message, AgentMessage::System { .. }) {
                    continue;
                }
                let entry_id = if journal
                    .store
                    .entries()
                    .iter()
                    .any(|entry| entry.id == legacy_id)
                {
                    legacy_id.clone()
                } else {
                    format!("v2-navigation-{legacy_id}")
                };
                if !journal
                    .store
                    .entries()
                    .iter()
                    .any(|entry| entry.id == entry_id)
                {
                    journal
                        .store
                        .append_entry_gated(HarnessEntry {
                            id: entry_id.clone(),
                            parent_id: parent_id.clone(),
                            lane: "main".into(),
                            seq: harness_next_seq(journal.store.store()),
                            timestamp: now_millis(),
                            message: node.message.clone(),
                            surface_op: threadlane_agent::harness::SurfaceOperation::Append,
                            terminate: matches!(
                                node.message,
                                AgentMessage::Tool {
                                    terminate: true,
                                    ..
                                }
                            ),
                        })
                        .map_err(|error| error.to_string())?;
                    journal
                        .store
                        .drive_to_completion()
                        .map_err(|error| error.to_string())?;
                }
                parent_id = Some(entry_id.clone());
                if legacy_id == node_id {
                    harness_target_id = Some(entry_id);
                }
            }
            let target_entry_id = harness_target_id.ok_or_else(|| {
                "navigation target was not materialized in the harness".to_string()
            })?;
            let run_id = journal.unique_run_id("foreground-navigation")?;
            journal
                .store
                .accept_navigation(&run_id, &target_entry_id, None)
                .map_err(|error| error.to_string())?;
            journal
                .store
                .drive_to_completion()
                .map_err(|error| error.to_string())?;
            if let Some(path) = self.session_tree.file_path.clone() {
                self.session_tree = SessionTree::load_from_file(&path)
                    .map_err(|error| format!("failed to reload navigated session: {error}"))?;
                self.session_tree.switch_active_node(&target_entry_id);
                self.agent
                    .sync_turn_from_model_context()
                    .await
                    .map_err(|error| format!("failed to project navigated context: {error}"))?;
                return Ok(format!("Switched session tree to node: {node_id}"));
            }
        }
        if self.session_tree.switch_active_node(node_id) {
            let branch_msgs = self.session_tree.get_active_branch_messages();
            let mut agent_state = self.agent.turn.lock().await;
            agent_state.messages = branch_msgs;
            Ok(format!("Switched session tree to node: {node_id}"))
        } else {
            Err(format!("Node ID not found in session tree: {node_id}"))
        }
    }

    pub(crate) fn set_credentials(&mut self, api_key: String, account_id: Option<String>) {
        self.agent.set_credentials(api_key, account_id);
    }

    pub fn set_model_roles(&mut self, roles: threadlane_agent::ModelRoles) {
        self.agent.set_model_roles(roles);
    }

    pub fn model_roles(&self) -> &threadlane_agent::ModelRoles {
        self.agent.model_roles()
    }

    pub(crate) async fn replay_safe_tools(
        &self,
        records: &[threadlane_agent::Record],
    ) -> Vec<AgentToolResult> {
        let calls = records
            .iter()
            .filter_map(|record| match record {
                threadlane_agent::Record::ToolStarted {
                    tool_call_id,
                    tool_name,
                    effective_args,
                    replay: threadlane_agent::ToolReplaySafety::Safe,
                    ..
                } => Some(threadlane_provider::openai::ToolCall {
                    id: tool_call_id.clone(),
                    r#type: "function".into(),
                    function: threadlane_provider::openai::ToolCallFunction {
                        name: tool_name.clone(),
                        arguments: effective_args.to_string(),
                    },
                    thought_signature: None,
                }),
                _ => None,
            })
            .collect::<Vec<_>>();
        if calls.is_empty() {
            return Vec::new();
        }
        self.agent.execute_tools_for_replay(&calls).await
    }

    pub(crate) async fn sync_session_history(&mut self) {
        if self.harness.is_some() {
            if let Err(error) = self.agent.sync_turn_from_model_context().await {
                warn!("Failed to project canonical model context: {error}");
            }
            return;
        }
        // Legacy in-memory sessions have no harness journal yet. Keep this
        // compatibility path isolated; durable sessions always project from
        // the canonical append-only event log above.
        let branch = self.session_tree.get_active_branch_messages();
        let mut state = self.agent.turn.lock().await;
        let system_prompt = state.system_prompt.clone();
        state.messages = std::iter::once(AgentMessage::System {
            content: system_prompt,
        })
        .chain(
            branch
                .into_iter()
                .filter(|message| !matches!(message, AgentMessage::System { .. })),
        )
        .collect();
    }

    pub async fn reload_extensions(&mut self) -> Result<usize, String> {
        let global_threadlane_dir = default_global_threadlane_dir();
        let loaded = self
            .wasi_extensions
            .reload_from_roots(global_threadlane_dir.as_deref(), Some(&self.work_dir))?;
        self.managed_processes.lock().await.clear();
        Ok(loaded)
    }

    /// Rediscover skills for this project, applying any persisted enable/disable
    /// overrides, and refresh the shared registry and the model-facing system prompt.
    pub fn refresh_skills(&mut self) {
        let mut skill_manager = SkillManager::new();
        skill_manager.discover_skills(Some(&self.work_dir));
        let skills = skill_manager.snapshot();
        self.skills = skills;
    }

    pub async fn refresh_mcp(&self) {
        self.mcp_manager.discover_and_connect().await;
    }

    pub fn new(options: CodingAgentOptions) -> Self {
        let coding_config = options.coding_config.unwrap_or_default();
        let agent_config = options.agent_config.unwrap_or_default();
        let project_context = ProjectContext::discover(&options.work_dir);
        let mut skill_manager = SkillManager::new();
        skill_manager.discover_skills(Some(&options.work_dir));
        let skills = skill_manager.snapshot();
        let skill_catalog = skills.render_model_catalog();

        // A missing session file represents an unsaved draft. GUI startup uses
        // this mode so merely opening the app neither creates nor selects a
        // conversation; the first send binds the draft to a new session.
        let mut session_tree = if let Some(session_path) = options.session_file.clone() {
            let session_id = || {
                session_path
                    .file_stem()
                    .map(|stem| stem.to_string_lossy().into_owned())
                    .unwrap_or_else(|| "session".into())
            };
            if session_path.exists() {
                SessionTree::load_from_file(&session_path).unwrap_or_else(|_| {
                    let mut session = SessionTree::new(session_id());
                    session.file_path = Some(session_path.clone());
                    session
                })
            } else {
                if let Some(parent) = session_path.parent() {
                    let _ = std::fs::create_dir_all(parent);
                }
                let mut session = SessionTree::new(session_id());
                session.file_path = Some(session_path);
                session
            }
        } else {
            SessionTree::new("draft")
        };
        let mut effective_model = session_tree
            .model
            .clone()
            .unwrap_or_else(|| options.model.clone());
        let (mut harness, harness_journal_error) = match session_tree.file_path.as_deref() {
            Some(path) => match crate::coding_agent::harness::CodingSessionHarness::open(path) {
                Ok(h) => (Some(h), None),
                Err(error) => (None, Some(error)),
            },
            None => (None, None),
        };
        if let Some(h) = harness.as_ref() {
            if let Some(model) = h.store.facts().get("model") {
                effective_model = model.clone();
                session_tree.model = Some(model.clone());
            }
            if let Some(name) = h.store.facts().get("name") {
                session_tree.name = Some(name.clone());
            }
            // V2 owns the durable active leaf. Legacy metadata may still
            // point at the previous turn, which makes a reopened session hide
            // prompts that were already committed to the harness journal.
            let has_v2_main_records = h
                .store
                .records()
                .iter()
                .any(|record| record.lane() == "main");
            if has_v2_main_records {
                if let Ok(state) = Reducer::reduce(&h.store) {
                    if let Some(leaf_id) =
                        state.lane("main").and_then(|lane| lane.leaf_id.as_deref())
                    {
                        if session_tree.nodes.contains_key(leaf_id) {
                            session_tree.switch_active_node(leaf_id);
                        }
                    }
                }
            }
        }
        session_tree
            .model
            .get_or_insert_with(|| effective_model.clone());
        let has_interrupted_subagents = match harness.as_mut() {
            Some(h) => h
                .snapshot()
                .map(|snapshot| snapshot.has_open_subagent_lanes())
                .unwrap_or(false),
            None => session_tree.file_path.is_some(),
        };
        let interrupted_subagent_recovery = if has_interrupted_subagents {
            InterruptedSubagentRecoveryState::Pending
        } else {
            InterruptedSubagentRecoveryState::Complete
        };
        let plan_store =
            SessionPlanStore::new(session_tree.plan().clone(), session_tree.file_path.clone());
        // Try to create a unified agent backed by the session file. If the
        // file is unavailable (e.g., directory, permission error), fall back
        // to an in-memory harness via a temp file.
        let session_path = session_tree.file_path.clone().unwrap_or_else(|| {
            std::env::temp_dir().join(format!(
                "threadlane-draft-{}.jsonl",
                session_tree.session_id
            ))
        });
        let (mut agent, _creation_error) = match UnifiedAgent::new(
            &options.api_key,
            options.account_id.clone(),
            &effective_model,
            &session_path,
            agent_config.clone(),
        ) {
            Ok(a) => (a, None),
            Err(error) => {
                let fallback = std::env::temp_dir().join(format!(
                    "threadlane-fallback-{}.jsonl",
                    session_tree.session_id
                ));
                let a = UnifiedAgent::new(
                    &options.api_key,
                    options.account_id.clone(),
                    &effective_model,
                    &fallback,
                    agent_config,
                )
                .map_err(|e| format!("{e}"))
                .expect("Failed to create fallback unified agent");
                (a, Some(error.to_string()))
            }
        };
        agent.session_id = session_tree.session_id.clone();
        if let Some(h) = harness.as_ref() {
            agent.hook_registry = h.store.hooks().clone();
        }
        let harness_run_id: Arc<std::sync::Mutex<Option<String>>> =
            Arc::new(std::sync::Mutex::new(None));
        let cancellation = CodingAgentCancellation {
            state: Arc::default(),
            harness_session_file: session_tree.file_path.clone(),
            event_tx: agent.event_tx.clone(),
        };

        agent.set_prompt_cache_key(Some(session_tree.session_id.clone()));

        let wasi_extensions = WasiExtensionManager::for_project_session(
            &options.work_dir,
            session_tree.session_id.clone(),
        );
        let global_threadlane_dir = default_global_threadlane_dir();
        let loaded_ext_count = wasi_extensions
            .reload_from_roots(global_threadlane_dir.as_deref(), Some(&options.work_dir))
            .unwrap_or_default();
        let agent_catalog = render_agent_catalog(&options.work_dir);
        let initial_tool_policy = restored_tool_policy(&wasi_extensions);
        let tool_policy = Arc::new(tokio::sync::Mutex::new(initial_tool_policy));
        let wasi_extensions = Arc::new(wasi_extensions);
        let agent_work = AgentWorkScheduler::default();
        #[cfg(test)]
        let subagent_work_observer = Arc::new(std::sync::Mutex::new(None));
        #[cfg(test)]
        let runner_observer: Option<SubagentObserverState> = Some(subagent_work_observer.clone());
        let runner_api_key = agent.api_key.clone();
        let runner_account_id = agent.account_id.clone();
        let runner_state = agent.turn.clone();
        let runner_work_dir = options.work_dir.clone();
        let runner_extensions = wasi_extensions.clone();
        let runner_event_tx = agent.event_tx.clone();
        let runner_session_file = session_tree.file_path.clone();
        let runner_semaphore = Arc::new(tokio::sync::Semaphore::new(
            coding_config.subagent_concurrency_limit,
        ));
        let dispatch_parent_leaf = Arc::new(std::sync::Mutex::new(None));
        let completed_subagent_lanes = Arc::new(std::sync::Mutex::new(Vec::new()));
        let runner_parent_leaf = dispatch_parent_leaf.clone();
        let runner_completed_lanes = completed_subagent_lanes.clone();
        let parent_session_id = session_tree.session_id.clone();
        let agent_runner: AgentRunner = Arc::new(move |tasks, parallel, tool_call_id| {
            #[cfg(test)]
            let observer = runner_observer.clone();
            let api_key = runner_api_key.clone();
            let account_id = runner_account_id.clone();
            let state = runner_state.clone();
            let work_dir = runner_work_dir.clone();
            let extensions = runner_extensions.clone();
            let event_tx = runner_event_tx.clone();
            let session_file = runner_session_file.clone();
            let semaphore = runner_semaphore.clone();
            let parent_leaf_id = runner_parent_leaf.lock().ok().and_then(|leaf| leaf.clone());
            let completed_lanes = runner_completed_lanes.clone();
            let parent_session_id = parent_session_id.clone();
            Box::pin(async move {
                let model = state.lock().await.model.clone();
                #[cfg(test)]
                let observer = observer
                    .and_then(|observer| observer.lock().ok().and_then(|value| value.clone()));
                let (output, thinking, lanes) = run_subagents_with_context(
                    tasks,
                    parallel,
                    tool_call_id,
                    SubagentRunContext {
                        api_key,
                        account_id,
                        parent_model: model,
                        parent_session_id: parent_session_id.clone(),
                        work_dir,
                        extensions,
                        parent_event_tx: event_tx,
                        parent_leaf_id,
                        session_file,
                        #[cfg(test)]
                        scheduler_observer: observer,
                        #[cfg(test)]
                        child_work_observer: None,
                        #[cfg(test)]
                        child_tool_observer: None,
                        semaphore,
                    },
                )
                .await?;
                accept_completed_subagent_lanes(&completed_lanes, lanes)?;
                Ok(serde_json::json!({
                    "message": output,
                    "output": output,
                    "thinking": thinking
                }))
            })
        });
        let (broker_dispatcher, managed_processes, permission_handle) = build_broker_dispatcher(
            tool_policy.clone(),
            wasi_extensions.clone(),
            true,
            options.work_dir.clone(),
            agent.event_tx.clone(),
            agent_work.clone(),
            Some(agent_runner.clone()),
            options.session_file.clone(),
        );
        // ── Capability registry: register tools + hooks declaratively ──
        let mcp_manager = Arc::new(McpManager::new(
            default_global_threadlane_dir(),
            Some(options.work_dir.clone()),
        ));
        let mut registry = threadlane_agent::CapabilityRegistry::new();
        registry.register(Box::new(SkillCapability {
            skills: skills.clone(),
        }));
        registry.register(Box::new(SubagentCapability {
            agent_runner: agent_runner.clone(),
        }));
        registry.register(Box::new(PlanCapability {
            plan_store: plan_store.clone(),
            event_tx: agent.event_tx.clone(),
            provider_client: agent.provider_client().clone(),
            turn: agent.turn.clone(),
            config: agent.config().clone(),
        }));

        registry.register(Box::new(WasiCapability {
            extensions: wasi_extensions.clone(),
            broker_dispatcher: broker_dispatcher.clone(),
            tool_policy: tool_policy.clone(),
        }));
        registry.register(Box::new(McpCapability {
            mcp_manager: mcp_manager.clone(),
        }));
        let (_wired, errors) = registry.wire_all(&mut agent);
        for error in &errors {
            eprintln!("{error}");
        }

        let manager_clone = mcp_manager.clone();
        threadlane_agent::get_runtime().spawn(async move {
            manager_clone.discover_and_connect().await;
        });
        agent.work_dir = Some(options.work_dir.clone());

        let mut system_prompt_config = options.system_prompt.clone();
        if initial_tool_policy == ToolPolicy::ReadOnly {
            system_prompt_config.guidelines.push(
                "The current workspace tool policy is read-only; do not request file mutations or host commands."
                    .to_string(),
            );
        }
        let prompt_tools = agent.configured_tool_definitions();
        let base_system_prompt = build_system_prompt(SystemPromptBuildOptions {
            config: &system_prompt_config,
            work_dir: &options.work_dir,
            tools: &prompt_tools,
            project_context: &project_context,
            skill_catalog: Some(&skill_catalog),
            agent_catalog: Some(&agent_catalog),
            loaded_extension_count: loaded_ext_count,
        });

        {
            let mut turn = agent.turn.try_lock().expect("Failed to lock initial state");
            turn.system_prompt = base_system_prompt.clone();
            turn.messages.push(AgentMessage::System {
                content: base_system_prompt.clone(),
            });
            turn.messages.extend(
                session_tree
                    .get_active_branch_messages()
                    .into_iter()
                    .filter(|message| !matches!(message, AgentMessage::System { .. })),
            );
        }

        Self {
            agent,
            session_tree,
            wasi_extensions,
            tool_policy,
            work_dir: options.work_dir,
            skills,
            agent_runner,
            broker_dispatcher,
            managed_processes,
            permission_handle,
            agent_work,
            mcp_manager,
            plan_store,
            prompt_templates: None,
            dispatch_parent_leaf,
            completed_subagent_lanes,
            harness,
            harness_journal_error,
            harness_run_id,
            cancellation,
            interrupted_subagent_recovery,
            #[cfg(test)]
            subagent_work_observer,
            #[cfg(test)]
            subagent_branch_observer: None,
        }
    }

    pub(crate) async fn run_scheduled_agent_work(&mut self) {
        while self.agent_work.run_unified(&mut self.agent).await {
            self.sync_session_tree_and_dispatch_assistant_hooks().await;
            if let Some(path) = self.session_tree.file_path.as_deref() {
                if let Err(error) = consume_harness_follow_ups(path) {
                    warn!("Failed to consume queued follow-up: {error}");
                }
                if let Err(error) = consume_harness_queue(path, QueueKind::Steer) {
                    warn!("Failed to consume queued steer: {error}");
                }
                if let Err(error) = consume_harness_queue(path, QueueKind::NextRun) {
                    warn!("Failed to consume queued next-run input: {error}");
                }
            }
        }
    }

    pub fn work_handle(&self) -> CodingAgentWorkHandle {
        CodingAgentWorkHandle {
            scheduler: self.agent_work.clone(),
            session_file: self.session_tree.file_path.clone(),
        }
    }

    pub fn subscribe(&self) -> broadcast::Receiver<AgentEvent> {
        self.agent.subscribe()
    }

    pub fn harness_snapshot(&mut self) -> Result<Option<Snapshot>, String> {
        let Some(journal) = self.harness.as_mut() else {
            return Ok(None);
        };
        journal.refresh()?;
        journal
            .store
            .snapshot()
            .map(Some)
            .map_err(|error| error.to_string())
    }

    pub fn harness_error(&self) -> Option<&str> {
        self.harness_journal_error.as_deref()
    }

    /// Returns the fully built system prompt used by this runtime when the
    /// agent state is not currently locked by an active turn.
    pub fn system_prompt_snapshot(&self) -> Option<String> {
        self.agent
            .turn
            .try_lock()
            .ok()
            .map(|state| state.system_prompt.clone())
    }

    pub(crate) fn watch_harness(&mut self) -> Result<Option<HarnessWatch>, String> {
        let Some(journal) = self.harness.as_mut() else {
            return Ok(None);
        };
        journal.refresh()?;
        let subscription = journal
            .store
            .watch_session()
            .map_err(|error| error.to_string())?;
        Ok(Some(HarnessWatch {
            hub: journal.store.events().clone(),
            subscription,
        }))
    }

    pub fn cancellation_handle(&self) -> CodingAgentCancellation {
        self.cancellation.clone()
    }

    pub(crate) fn cancel(&self) -> Result<(), String> {
        self.cancellation.cancel()
    }

    pub(crate) fn current_plan(&self) -> threadlane_agent::SessionPlan {
        self.plan_store.current()
    }
    pub fn has_interrupted_work(&self) -> bool {
        matches!(
            self.interrupted_subagent_recovery,
            InterruptedSubagentRecoveryState::Pending
        )
    }

    pub async fn resume_interrupted_turn(&mut self) -> Result<usize, String> {
        let count = self.recover_interrupted_subagent_lanes().await?;
        self.repair_interrupted_history().await;
        Ok(count)
    }

    async fn dispatch_assistant_hook(&self, message: &AgentMessage) {
        let AgentMessage::Assistant {
            content,
            tool_calls,
            ..
        } = message
        else {
            return;
        };
        let arguments = serde_json::json!({
            "content": content,
            "tool_calls": tool_calls,
        });
        for response in self
            .wasi_extensions
            .execute_hook_with_effects("assistant_message", &arguments.to_string())
            .into_iter()
            .flatten()
        {
            let _ = dispatch_hook_requests(
                &self.broker_dispatcher,
                &self.wasi_extensions,
                response.host_broker_requests,
            )
            .await;
        }
        let _ = dispatch_hook_requests(
            &self.broker_dispatcher,
            &self.wasi_extensions,
            self.wasi_extensions.take_pending_broker_requests(),
        )
        .await;
    }

    async fn sync_session_tree_and_dispatch_assistant_hooks(&mut self) {
        let state = self.agent.get_state().await;
        let harness_persists_messages = self.harness.is_some();

        // The loop engine keeps the complete provider conversation in memory,
        // including assistant tool-call messages and the tool results that
        // follow them. Persist the portion that is not in the session yet so
        // reloading a session produces the same provider history (and keeps
        // the tool-call/result ordering intact).
        let state_messages: Vec<AgentMessage> = state
            .messages
            .into_iter()
            .filter(|message| !matches!(message, AgentMessage::System { .. }))
            .collect();

        if harness_persists_messages {
            let durable_messages = self
                .harness
                .as_mut()
                .and_then(|harness| {
                    let _ = harness.refresh();
                    harness.store.model_context("main").ok()
                })
                .map(|projection| projection.messages())
                .unwrap_or_default();
            if requires_harness_compaction_reset(&durable_messages, &state_messages) {
                let summary = state_messages
                    .iter()
                    .find_map(threadlane_agent::compaction_summary_text)
                    .expect("compaction reset requires a summary")
                    .to_owned();
                let retained_tail = compaction_retained_tail(&state_messages);
                if let Err(error) = self.persist_harness_compaction(&summary, &retained_tail) {
                    self.harness_journal_error = Some(error);
                    return;
                }
                if let Some(path) = self.session_tree.file_path.clone() {
                    match SessionTree::load_from_file(&path) {
                        Ok(tree) => self.session_tree = tree,
                        Err(error) => warn!("Failed to reload compacted session: {error}"),
                    }
                }
                if let Some(last_assistant) = state_messages
                    .iter()
                    .rev()
                    .find(|message| matches!(message, AgentMessage::Assistant { .. }))
                {
                    self.dispatch_assistant_hook(last_assistant).await;
                }
                return;
            }

            // Provider messages are already persisted step-by-step by the
            // turn driver. Refresh the harness and reload the session tree.
            if let Some(harness) = self.harness.as_mut() {
                if let Err(error) = harness.refresh() {
                    self.harness_journal_error = Some(error);
                    return;
                }
            }
            if let Some(path) = self.session_tree.file_path.clone() {
                match SessionTree::load_from_file(&path) {
                    Ok(tree) => self.session_tree = tree,
                    Err(error) => warn!("Failed to reload V2 session tree: {error}"),
                }
            }
            if let Some(last_assistant) = state_messages
                .iter()
                .rev()
                .find(|message| matches!(message, AgentMessage::Assistant { .. }))
            {
                self.dispatch_assistant_hook(last_assistant).await;
            }
            return;
        }

        let persisted_messages = self.session_tree.get_active_branch_messages();

        let common_prefix = state_messages
            .iter()
            .zip(persisted_messages.iter())
            .take_while(|(state_message, persisted_message)| {
                serde_json::to_value(state_message).ok()
                    == serde_json::to_value(persisted_message).ok()
            })
            .count();

        let start_index = if common_prefix == persisted_messages.len() {
            // Agent::prompt records the same user message that CodingAgent
            // already stored for normal prompts. Avoid storing that duplicate.
            if matches!(
                (state_messages.get(common_prefix), persisted_messages.last()),
                (Some(state_message), Some(persisted_message))
                    if state_message.same_user_message(persisted_message)
            ) {
                common_prefix + 1
            } else {
                common_prefix
            }
        } else if persisted_messages.len() == common_prefix + 1
            && state_messages
                .get(common_prefix)
                .is_some_and(AgentMessage::is_user)
        {
            // Skills and extensions store the visible command, then prompt
            // the model with a different, generated user message. Keep that
            // generated message so the restored provider history is exact.
            common_prefix
        } else if state_messages
            .iter()
            .any(|message| threadlane_agent::compaction_summary_text(message).is_some())
        {
            // Auto-compaction creates a new active root branch. Persist that
            // branch in-place instead of treating it as a new session.
            let current_turn_start = state_messages
                .iter()
                .rposition(AgentMessage::is_user)
                .unwrap_or(state_messages.len());
            for message in state_messages.iter().skip(current_turn_start + 1) {
                self.dispatch_assistant_hook(message).await;
            }
            if harness_persists_messages {
                let Some(path) = self.session_tree.file_path.clone() else {
                    warn!("Failed to reload compacted session: no session path");
                    return;
                };
                match SessionTree::load_from_file(&path) {
                    Ok(tree) => self.session_tree = tree,
                    Err(error) => warn!("Failed to reload compacted session: {error}"),
                }
            } else {
                self.session_tree.replace_active_branch(state_messages);
            }
            return;
        } else {
            // A non-prefix means the session was changed independently. Do
            // not append a second, potentially duplicated conversation.
            return;
        };

        for message in state_messages.into_iter().skip(start_index) {
            self.dispatch_assistant_hook(&message).await;
            self.session_tree.add_message(message.clone());
        }
    }

    #[cfg(test)]
    pub(crate) fn set_subagent_work_observer(
        &self,
        observer: Arc<std::sync::Mutex<Vec<AgentWork>>>,
    ) {
        if let Ok(mut current) = self.subagent_work_observer.lock() {
            *current = Some(observer);
        }
    }

    #[cfg(test)]
    fn set_subagent_branch_observer(&mut self, observer: SubagentBoundaryObserver) {
        self.subagent_branch_observer = Some(observer);
    }

    fn commit_completed_subagent_lanes(&mut self) -> Result<(), String> {
        let lanes = {
            let mut completed = self
                .completed_subagent_lanes
                .lock()
                .map_err(|_| "Completed subagent lane sink is unavailable".to_string())?;
            std::mem::take(&mut *completed)
        };
        for (index, lane) in lanes.iter().enumerate() {
            let status = match lane.status {
                SubagentLaneStatus::Completed => "completed",
                SubagentLaneStatus::Failed => "failed",
            };
            let mut messages = Vec::with_capacity(lane.messages.len() + 1);
            messages.push(AgentMessage::Custom {
                custom_type: "subagent_lane".into(),
                payload: serde_json::json!({
                    "lane": lane.lane_name,
                    "run_id": lane.run_id,
                    "agent": lane.agent,
                    "task": lane.task,
                    "status": status,
                    "error": lane.error,
                }),
            });
            messages.extend(lane.messages.clone());
            if let Err(error) = self
                .session_tree
                .append_passive_branch(lane.parent_leaf_id.as_deref(), messages)
            {
                self.completed_subagent_lanes
                    .lock()
                    .map_err(|_| "Completed subagent lane sink is unavailable".to_string())?
                    .extend_from_slice(&lanes[index..]);
                return Err(error);
            }
            #[cfg(test)]
            if let Some(observer) = self.subagent_branch_observer.as_ref() {
                observer();
            }
        }
        for (index, lane) in lanes.iter().enumerate() {
            if let Some(path) = self.session_tree.file_path.as_deref() {
                let outcome = match lane.status {
                    SubagentLaneStatus::Completed => OperationOutcome::Completed,
                    SubagentLaneStatus::Failed => OperationOutcome::Failed,
                };
                let mut journal = HarnessJournal::open(path)?;
                if let Err(error) = journal.finish_subagent_lane(
                    &lane.lane_name,
                    &lane.run_id,
                    outcome,
                    lane.error.clone(),
                ) {
                    self.completed_subagent_lanes
                        .lock()
                        .map_err(|_| "Completed subagent lane sink is unavailable".to_string())?
                        .extend_from_slice(&lanes[index..]);
                    self.interrupted_subagent_recovery = InterruptedSubagentRecoveryState::Pending;
                    return Err(error);
                }
            }
        }
        Ok(())
    }

    pub(crate) async fn recover_interrupted_subagent_lanes(&mut self) -> Result<usize, String> {
        match &self.interrupted_subagent_recovery {
            InterruptedSubagentRecoveryState::Complete => return Ok(0),
            InterruptedSubagentRecoveryState::Pending => {}
        }
        if let Some(error) = self.harness_journal_error.as_ref() {
            return Err(format!("Harness Error: {error}"));
        }

        let result: Result<usize, String> = async {
            let path = self
                .session_tree
                .file_path
                .clone()
                .ok_or_else(|| "Interrupted subagent journal is unavailable".to_string())?;
            let records = recover_v2_subagent_records(&path).unwrap_or_default();
            let markers = self
                .session_tree
                .nodes
                .values()
                .filter_map(|node| match &node.message {
                    AgentMessage::Custom {
                        custom_type,
                        payload,
                    } if custom_type == "subagent_lane" => payload
                        .get("run_id")
                        .and_then(Value::as_str)
                        .and_then(|run_id| {
                            payload.get("lane").and_then(Value::as_str).map(|lane| {
                                (
                                    (lane.to_owned(), run_id.to_owned()),
                                    (
                                        node.id.clone(),
                                        payload
                                            .get("status")
                                            .and_then(Value::as_str)
                                            .unwrap_or("completed")
                                            .to_owned(),
                                        payload
                                            .get("error")
                                            .and_then(Value::as_str)
                                            .map(str::to_owned),
                                    ),
                                )
                            })
                        }),
                    _ => None,
                })
                .collect::<HashMap<_, _>>();
            let mut recovered = 0;

            for lane in threadlane_agent::interrupted_subagent_lanes(&records) {
                let retrying = |error: String| {
                    let _ = self.agent.event_tx.send(AgentEvent::SubagentRecovery {
                        run_id: lane.run_id.clone(),
                        status: SubagentRecoveryStatus::Retrying,
                        detail: Some("Recovery needs retry".into()),
                    });
                    error
                };
                let mut journal = HarnessJournal::open(&path).map_err(&retrying)?;
                let _ = self.agent.event_tx.send(AgentEvent::SubagentRecovery {
                    run_id: lane.run_id.clone(),
                    status: SubagentRecoveryStatus::Started,
                    detail: Some("Recovering interrupted task".into()),
                });
                if !lane.task_attempted {
                    let error = "Interrupted subagent had no persisted task attempt".to_string();
                    let messages = vec![AgentMessage::Custom {
                        custom_type: "subagent_lane".into(),
                        payload: serde_json::json!({
                            "lane": lane.lane,
                            "run_id": lane.run_id,
                            "agent": "recovered",
                            "task": lane.task,
                            "status": "aborted",
                            "error": error,
                        }),
                    }];
                    self.session_tree
                        .append_passive_branch_in_memory(lane.source_leaf_id.as_deref(), messages)
                        .map_err(&retrying)?;
                    journal
                        .finish_subagent_lane(
                            &lane.lane,
                            &lane.run_id,
                            OperationOutcome::Aborted,
                            Some(error),
                        )
                        .map_err(&retrying)?;
                    let _ = self.agent.event_tx.send(AgentEvent::SubagentRecovery {
                        run_id: lane.run_id.clone(),
                        status: SubagentRecoveryStatus::Aborted,
                        detail: Some("Interrupted task was not replayable".into()),
                    });
                    recovered += 1;
                    continue;
                }
                if let Some((marker_id, status, error)) =
                    markers.get(&(lane.lane.clone(), lane.run_id.clone()))
                {
                    let recorded = records
                        .iter()
                        .filter_map(|record| match record {
                            HarnessRecord::WriteDeferred {
                                lane: recorded_lane,
                                run_id,
                                target,
                                ..
                            } if *recorded_lane == lane.lane && *run_id == lane.run_id => {
                                serde_json::to_value(target).ok()
                            }
                            _ => None,
                        })
                        .collect::<HashSet<_>>();
                    let persisted = self
                        .session_tree
                        .nodes
                        .values()
                        .filter(|node| {
                            let mut parent = node.parent_id.as_deref();
                            while let Some(parent_id) = parent {
                                if parent_id == marker_id {
                                    return true;
                                }
                                parent = self
                                    .session_tree
                                    .nodes
                                    .get(parent_id)
                                    .and_then(|parent| parent.parent_id.as_deref());
                            }
                            false
                        })
                        .filter_map(|node| {
                            (!matches!(node.message, AgentMessage::Custom { .. }))
                                .then_some(node.message.clone())
                        })
                        .filter(|message| {
                            serde_json::to_value(message)
                                .ok()
                                .is_some_and(|message| !recorded.contains(&message))
                        })
                        .collect::<Vec<_>>();
                    journal
                        .checkpoint(&lane.lane, &lane.run_id, &persisted)
                        .map_err(&retrying)?;
                    let outcome = match status.as_str() {
                        "aborted" => OperationOutcome::Aborted,
                        "failed" => OperationOutcome::Failed,
                        _ => OperationOutcome::Completed,
                    };
                    journal
                        .finish_subagent_lane(&lane.lane, &lane.run_id, outcome, error.clone())
                        .map_err(&retrying)?;
                    let (status, detail) = match status.as_str() {
                        "aborted" => (SubagentRecoveryStatus::Aborted, "Recovery was aborted"),
                        "failed" => (SubagentRecoveryStatus::Retrying, "Recovery needs retry"),
                        _ => (SubagentRecoveryStatus::Recovered, "Recovered prior work"),
                    };
                    let _ = self.agent.event_tx.send(AgentEvent::SubagentRecovery {
                        run_id: lane.run_id.clone(),
                        status,
                        detail: Some(detail.into()),
                    });
                    recovered += 1;
                    continue;
                }

                if lane.safe_tools.is_empty()
                    && lane.unsafe_tools.is_empty()
                    && lane
                        .messages
                        .iter()
                        .any(|message| matches!(message, AgentMessage::Tool { .. }))
                {
                    journal
                        .finish_subagent_lane(
                            &lane.lane,
                            &lane.run_id,
                            OperationOutcome::Completed,
                            None,
                        )
                        .map_err(&retrying)?;
                    recovered += 1;
                    continue;
                }

                let claimed_safe_tools = journal
                    .claim_safe_replays(&lane.safe_tools)
                    .map_err(&retrying)?;
                let safe_results = self.replay_safe_tools(&claimed_safe_tools).await;
                let safe_messages = safe_results
                    .iter()
                    .cloned()
                    .map(|result| {
                        let terminate = result.terminates();
                        AgentMessage::Tool {
                            tool_call_id: result.tool_call_id,
                            name: result.name,
                            content: result.content,
                            is_error: result.is_error,
                            terminate,
                        }
                    })
                    .collect::<Vec<_>>();
                let unsafe_tool_ids = lane
                    .unsafe_tools
                    .iter()
                    .filter_map(|record| match record {
                        HarnessRecord::ToolStarted { tool_call_id, .. } => {
                            Some(tool_call_id.clone())
                        }
                        _ => None,
                    })
                    .collect::<HashSet<_>>();
                let unsafe_messages = lane
                    .messages
                    .iter()
                    .filter(|message| {
                        matches!(
                            message,
                            AgentMessage::Tool { tool_call_id, .. }
                                if unsafe_tool_ids.contains(tool_call_id)
                        )
                    })
                    .cloned()
                    .collect::<Vec<_>>();
                let mut tool_messages = unsafe_messages;
                tool_messages.extend(safe_messages.clone());
                journal
                    .checkpoint(&lane.lane, &lane.run_id, &tool_messages)
                    .map_err(&retrying)?;
                if unsafe_tool_ids.is_empty() && !safe_results.is_empty() {
                    journal.refresh().map_err(&retrying)?;
                    let results = safe_results
                        .iter()
                        .map(|result| threadlane_agent::harness::ToolResult {
                            call_id: result.tool_call_id.clone(),
                            name: result.name.clone(),
                            content: result.content.clone(),
                            is_error: result.is_error,
                            terminate: result.terminates(),
                        })
                        .collect::<Vec<_>>();
                    journal
                        .store
                        .finish_existing_tool_batch(&lane.run_id, &results, TokenUsage::default())
                        .map_err(|error| error.to_string())
                        .map_err(&retrying)?;
                    journal
                        .store
                        .drive_to_completion()
                        .map_err(|error| error.to_string())
                        .map_err(&retrying)?;
                }

                if !unsafe_tool_ids.is_empty() {
                    let error =
                        Some("Interrupted unsafe tool execution was not replayed".to_string());
                    let mut messages =
                        Vec::with_capacity(1 + lane.messages.len() + safe_messages.len());
                    messages.push(AgentMessage::Custom {
                        custom_type: "subagent_lane".into(),
                        payload: serde_json::json!({
                            "lane": lane.lane,
                            "run_id": lane.run_id,
                            "agent": "recovered",
                            "task": lane.task,
                            "status": "aborted",
                            "error": error,
                        }),
                    });
                    messages.extend(lane.messages.clone());
                    messages.extend(safe_messages);
                    self.session_tree
                        .append_passive_branch_in_memory(lane.source_leaf_id.as_deref(), messages)
                        .map_err(&retrying)?;
                    // Refresh the journal store so it sees entries written by checkpoint()
                    // before reconcile_abort_run validates the ToolFinished invariants.
                    journal.refresh().map_err(&retrying)?;
                    journal
                        .finish_subagent_lane(
                            &lane.lane,
                            &lane.run_id,
                            OperationOutcome::Aborted,
                            error,
                        )
                        .map_err(&retrying)?;
                    let _ = self.agent.event_tx.send(AgentEvent::SubagentRecovery {
                        run_id: lane.run_id.clone(),
                        status: SubagentRecoveryStatus::Aborted,
                        detail: Some("Unsafe tool was not replayed".into()),
                    });
                    recovered += 1;
                    continue;
                }

                let mut resume_messages = lane.messages.clone();
                resume_messages.extend(safe_messages);
                let _ = self.agent.event_tx.send(AgentEvent::SubagentRecovery {
                    run_id: lane.run_id.clone(),
                    status: SubagentRecoveryStatus::Retrying,
                    detail: Some("Resuming interrupted task".into()),
                });
                let model = self.agent.turn.lock().await.model.clone();
                #[cfg(test)]
                let scheduler_observer = self
                    .subagent_work_observer
                    .lock()
                    .ok()
                    .and_then(|observer| observer.clone());
                let result = run_subagent_task(
                    AgentDefinition {
                        name: "recovered".into(),
                        description: "Recovered interrupted subagent".into(),
                        tools: None,
                        model: None,
                        system_prompt:
                            "Resume the interrupted child task from its durable checkpoint.".into(),
                        source: crate::agents::AgentSource::Project,
                        file_path: self.work_dir.clone(),
                    },
                    lane.task.clone(),
                    SubagentRunContext {
                        api_key: self.agent.api_key.clone(),
                        account_id: self.agent.account_id.clone(),
                        parent_model: model,
                        parent_session_id: self.session_tree.session_id.clone(),
                        work_dir: self.work_dir.clone(),
                        extensions: self.wasi_extensions.clone(),
                        parent_event_tx: self.agent.event_tx.clone(),
                        parent_leaf_id: lane.source_leaf_id.clone(),
                        session_file: self.session_tree.file_path.clone(),
                        #[cfg(test)]
                        scheduler_observer,
                        #[cfg(test)]
                        child_work_observer: None,
                        #[cfg(test)]
                        child_tool_observer: None,
                        semaphore: Arc::new(tokio::sync::Semaphore::new(1)),
                    },
                    NEXT_SUBAGENT_UI_RUN_ID.fetch_add(1, Ordering::Relaxed),
                    0,
                    SubagentLaneIdentity {
                        lane_name: lane.lane.clone(),
                        run_id: lane.run_id.clone(),
                        source_leaf_id: lane.source_leaf_id.clone(),
                        started_seq: lane.started_seq,
                    },
                    resume_messages.clone(),
                )
                .await;
                let (status, outcome, error, resumed_messages) = match result {
                    Ok(result) if result.error.is_none() => (
                        "completed",
                        OperationOutcome::Completed,
                        None,
                        result.messages,
                    ),
                    Ok(result) => (
                        "failed",
                        OperationOutcome::Failed,
                        result.error,
                        result.messages,
                    ),
                    Err(error) => (
                        "failed",
                        OperationOutcome::Failed,
                        Some(error),
                        resume_messages,
                    ),
                };
                let mut messages = Vec::with_capacity(1 + resumed_messages.len());
                messages.push(AgentMessage::Custom {
                    custom_type: "subagent_lane".into(),
                    payload: serde_json::json!({
                        "lane": lane.lane,
                        "run_id": lane.run_id,
                        "agent": "recovered",
                        "task": lane.task,
                        "status": status,
                        "error": error,
                    }),
                });
                messages.extend(resumed_messages);
                self.session_tree
                    .append_passive_branch_in_memory(lane.source_leaf_id.as_deref(), messages)
                    .map_err(&retrying)?;
                journal
                    .finish_subagent_lane(&lane.lane, &lane.run_id, outcome, error)
                    .map_err(&retrying)?;
                let (status, detail) = if status == "completed" {
                    (SubagentRecoveryStatus::Recovered, "Recovery complete")
                } else {
                    (SubagentRecoveryStatus::Retrying, "Recovery needs retry")
                };
                let _ = self.agent.event_tx.send(AgentEvent::SubagentRecovery {
                    run_id: lane.run_id.clone(),
                    status,
                    detail: Some(detail.into()),
                });
                recovered += 1;
            }
            Ok(recovered)
        }
        .await;

        if result.is_ok() {
            self.interrupted_subagent_recovery = InterruptedSubagentRecoveryState::Complete;
        }
        result
    }

    async fn repair_interrupted_history(&mut self) -> bool {
        if let Some(path) = self.session_tree.file_path.clone() {
            if self.harness.is_some() {
                // Recovery starts from the same selected model-context branch
                // used for provider requests, never from the UI session tree.
                let Ok(tree) = SessionTree::load_from_file(&path) else {
                    return false;
                };
                let Some(journal) = self.harness.as_mut() else {
                    return false;
                };
                if journal.refresh().is_err() {
                    return false;
                }
                let Ok(projection) = journal.store.model_context("main") else {
                    return false;
                };
                let mut state = self.agent.turn.lock().await;
                let mut messages = Vec::with_capacity(projection.entries.len() + 1);
                messages.push(AgentMessage::System {
                    content: state.system_prompt.clone(),
                });
                messages.extend(projection.messages());
                let repaired = repair_interrupted_tool_turn(&mut messages);
                let changed = state.messages != messages;
                self.session_tree = tree;
                if repaired {
                    // The harness owns durable persistence. This in-memory tree
                    // update only keeps legacy UI reconciliation coherent.
                    self.session_tree.replace_active_branch_in_memory(
                        messages
                            .iter()
                            .filter(|message| !matches!(message, AgentMessage::System { .. }))
                            .cloned()
                            .collect(),
                    );
                }
                state.messages = messages;
                return changed;
            }
        }
        let repaired_branch = {
            let mut state = self.agent.turn.lock().await;
            if !repair_interrupted_tool_turn(&mut state.messages) {
                return false;
            }
            state
                .messages
                .iter()
                .filter(|message| !matches!(message, AgentMessage::System { .. }))
                .cloned()
                .collect::<Vec<_>>()
        };

        let persisted_branch = self.session_tree.get_active_branch_messages();
        if serde_json::to_value(&persisted_branch).ok()
            != serde_json::to_value(&repaired_branch).ok()
        {
            if self.harness.is_some() {
                self.session_tree
                    .replace_active_branch_in_memory(repaired_branch);
            } else {
                self.session_tree.replace_active_branch(repaired_branch);
            }
        }
        true
    }

    pub async fn set_reasoning_effort(&mut self, effort: ReasoningEffort) {
        self.agent.set_reasoning_effort(effort).await;
    }

    pub(crate) async fn set_model(&mut self, model: String) -> Result<(), String> {
        let model = model.trim();
        if model.is_empty() {
            return Err("model cannot be empty".into());
        }
        if self.harness.is_none() {
            self.session_tree
                .set_model(model.to_string())
                .map_err(|error| format!("Could not persist model switch: {error}"))?;
        }
        if let Some(journal) = self.harness.as_mut() {
            journal.refresh()?;
            journal
                .store
                .set_fact("main", "model", model.to_string(), None)
                .map_err(|error| error.to_string())?;
            journal
                .store
                .drive_to_completion()
                .map_err(|error| error.to_string())?;
            self.session_tree
                .set_model_in_memory(model.to_string())
                .map_err(|error| format!("Could not update model switch: {error}"))?;
        }
        self.agent.turn.lock().await.model = model.to_string();
        Ok(())
    }

    fn set_name(&mut self, name: String) -> Result<(), String> {
        if self.harness.is_some() {
            let journal = self
                .harness
                .as_mut()
                .ok_or_else(|| "harness journal disappeared during name update".to_string())?;
            journal.refresh().map_err(|error| error.to_string())?;
            journal
                .store
                .set_fact("main", "name", name.clone(), None)
                .map_err(|error| error.to_string())?;
            journal
                .store
                .drive_to_completion()
                .map_err(|error| error.to_string())?;
            self.session_tree
                .set_name_in_memory(name)
                .map_err(|error| format!("Could not update session name: {error}"))?;
            Ok(())
        } else {
            self.session_tree
                .set_name(name)
                .map_err(|error| format!("Could not persist session name: {error}"))
        }
    }

    pub fn set_fact(&mut self, key: &str, value: &str) -> Result<(), String> {
        if self.harness.is_some() {
            let journal = self
                .harness
                .as_mut()
                .ok_or_else(|| "harness journal disappeared during fact update".to_string())?;
            journal.refresh().map_err(|error| error.to_string())?;
            journal
                .store
                .set_fact("main", key, value.to_string(), None)
                .map_err(|error| error.to_string())?;
            journal
                .store
                .drive_to_completion()
                .map_err(|error| error.to_string())?;
            self.session_tree.set_fact_in_memory(key, value);
            Ok(())
        } else {
            self.session_tree
                .set_fact(key.to_owned(), value.to_owned())
                .map_err(|error| format!("Could not persist session fact: {error}"))
        }
    }

    pub async fn available_models(&self) -> Vec<String> {
        let api_key = self.agent.api_key.clone();
        let account_id = self.agent.account_id.clone();
        fetch_available_models(&api_key, account_id.as_deref()).await
    }

    pub(crate) async fn recover_harness_tool_batch(
        &mut self,
        run_id: &str,
    ) -> Result<bool, String> {
        let (assistant_entry_id, specs) = {
            let journal = self
                .harness
                .as_mut()
                .ok_or_else(|| "harness journal is unavailable".to_string())?;
            journal.refresh()?;
            let state =
                Reducer::reduce(journal.store.store()).map_err(|error| error.to_string())?;
            let lane = state
                .lane("main")
                .ok_or_else(|| "main harness lane is unavailable".to_string())?;
            let unfinished = lane
                .tools
                .iter()
                .filter(|tool| tool.run_id == run_id && !tool.completed)
                .cloned()
                .collect::<Vec<_>>();
            let Some(first) = unfinished.first() else {
                return Ok(false);
            };
            let assistant_entry_id = first.assistant_entry_id.clone();
            let assistant = journal
                .store
                .entries()
                .iter()
                .find(|entry| entry.id == assistant_entry_id)
                .ok_or_else(|| "unfinished tool batch assistant entry is missing".to_string())?;
            let calls = match &assistant.message {
                AgentMessage::Assistant {
                    tool_calls: Some(calls),
                    ..
                } => calls,
                _ => return Err("unfinished tool batch has no assistant tool calls".into()),
            };
            let mut specs = Vec::with_capacity(unfinished.len());
            for tool in &unfinished {
                let call = calls.get(tool.tool_index).ok_or_else(|| {
                    "unfinished tool ordinal is outside assistant declaration".to_string()
                })?;
                let effective_args = journal
                    .store
                    .records()
                    .iter()
                    .find_map(|record| match record {
                        HarnessRecord::ToolStarted {
                            run_id: record_run_id,
                            tool_call_id,
                            effective_args,
                            ..
                        } if record_run_id == run_id && tool_call_id == &tool.tool_call_id => {
                            Some(effective_args.clone())
                        }
                        _ => None,
                    })
                    .ok_or_else(|| "unfinished tool intent arguments are missing".to_string())?;
                specs.push(ToolSpec {
                    index: tool.tool_index,
                    call_id: call.id.clone(),
                    name: call.function.name.clone(),
                    effective_args,
                    result_entry_id: tool.result_entry_id.clone(),
                    replay: tool.replay.clone(),
                });
            }
            (assistant_entry_id, specs)
        };

        let recoveries = {
            let journal = self
                .harness
                .as_mut()
                .ok_or_else(|| "harness journal is unavailable".to_string())?;
            let recoveries = journal
                .store
                .resume_tool_batch(run_id, &assistant_entry_id, &specs)
                .map_err(|error| error.to_string())?;
            journal
                .store
                .drive_to_completion()
                .map_err(|error| error.to_string())?;
            recoveries
        };

        let replay_specs = recoveries
            .iter()
            .filter_map(|recovery| match recovery {
                ToolRecovery::Replay(spec) => Some(spec.clone()),
                ToolRecovery::Synthesized(_) => None,
            })
            .collect::<Vec<_>>();
        let replay_calls = replay_specs
            .iter()
            .map(|spec| threadlane_provider::openai::ToolCall {
                id: spec.call_id.clone(),
                r#type: "function".into(),
                function: threadlane_provider::openai::ToolCallFunction {
                    name: spec.name.clone(),
                    arguments: spec.effective_args.to_string(),
                },
                thought_signature: None,
            })
            .collect::<Vec<_>>();
        let replay_results = if replay_calls.is_empty() {
            Vec::new()
        } else {
            self.agent.execute_tools_for_replay(&replay_calls).await
        };

        let mut messages = Vec::with_capacity(recoveries.len());
        for recovery in recoveries {
            let (spec, result) = match recovery {
                ToolRecovery::Replay(spec) => {
                    let result = replay_results
                        .iter()
                        .find(|result| result.tool_call_id == spec.call_id)
                        .cloned()
                        .ok_or_else(|| {
                            format!("safe replay produced no result for {}", spec.call_id)
                        })?;
                    (spec, result)
                }
                ToolRecovery::Synthesized(result) => {
                    let spec = specs
                        .iter()
                        .find(|spec| spec.call_id == result.call_id)
                        .cloned()
                        .ok_or_else(|| {
                            format!("synthesized result has no spec for {}", result.call_id)
                        })?;
                    (
                        spec,
                        AgentToolResult::external(
                            result.call_id.clone(),
                            result.name.clone(),
                            result.content.clone(),
                            result.is_error,
                        ),
                    )
                }
            };
            if replay_specs
                .iter()
                .any(|replay| replay.call_id == spec.call_id)
            {
                let journal = self
                    .harness
                    .as_mut()
                    .ok_or_else(|| "harness journal is unavailable".to_string())?;
                journal.append_replayed_tool_entry(run_id, &assistant_entry_id, &spec, &result)?;
                journal.finish_replayed_tool(run_id, &result)?;
            }
            let terminate = result.terminates();
            messages.push(AgentMessage::Tool {
                tool_call_id: result.tool_call_id,
                name: result.name,
                content: result.content,
                is_error: result.is_error,
                terminate,
            });
        }

        {
            let mut state = self.agent.turn.lock().await;
            for message in messages {
                if !state.messages.iter().any(|current| current == &message) {
                    state.messages.push(message);
                }
            }
        }
        if let Some(path) = self.session_tree.file_path.clone() {
            self.session_tree = SessionTree::load_from_file(&path)
                .map_err(|error| format!("failed to refresh recovered tool history: {error}"))?;
        }
        Ok(true)
    }

    pub(crate) async fn handle_input(&mut self, input: &str) -> Option<Result<String, String>> {
        self.handle_input_with_images(input, Vec::new()).await
    }

    pub(crate) async fn resume_suspended_harness(&mut self) -> Result<bool, String> {
        if let Some(error) = self.harness_journal_error.as_ref() {
            return Err(format!("Harness Error: {error}"));
        }
        let Some(journal) = self.harness.as_mut() else {
            return Ok(false);
        };
        journal.refresh()?;
        journal
            .store
            .restore_hooks_for_lane("main")
            .map_err(|error| error.to_string())?;
        let state = Reducer::reduce(&journal.store).map_err(|error| error.to_string())?;
        let Some(lane) = state.lane("main") else {
            return Ok(false);
        };
        let Some(run_id) = lane.open_operation.clone() else {
            return Ok(false);
        };
        let context = HookContext {
            session_id: journal.store.session_id().to_owned(),
            lane: "main".into(),
            run_id: Some(run_id.clone()),
            resume_data: None,
            tool_call_id: None,
            tool_name: None,
            tool_arguments: None,
            tool_result_content: None,
            tool_result_is_error: None,
        };
        for failure in journal.store.hooks().run_before_resume(&context).await {
            warn!(
                "before-resume hook {} failed: {}",
                failure.id, failure.message
            );
        }
        if lane.abort_requested {
            journal.recover_abort()?;
            return Ok(true);
        }
        if lane.retry.is_some() {
            journal.begin_retry(&run_id)?;
            journal.refresh()?;
        }
        let start_seq = journal
            .store
            .records()
            .iter()
            .find_map(|record| match record {
                HarnessRecord::OperationStarted { id, seq, .. } if id == &run_id => Some(*seq),
                _ => None,
            })
            .ok_or_else(|| format!("missing harness operation {run_id}"))?;
        if journal.store.entries().iter().any(|entry| {
            entry.seq > start_seq
                && matches!(
                    &entry.message,
                    AgentMessage::Assistant {
                        deferred_handle: Some(_),
                        ..
                    }
                )
        }) {
            return self.redeem_suspended_deferred_from_provider(&run_id).await;
        }
        self.recover_harness_tool_batch(&run_id).await?;
        let journal = self
            .harness
            .as_mut()
            .ok_or_else(|| "harness journal disappeared during tool recovery".to_string())?;
        journal.refresh()?;
        let has_attempt = journal.store.records().iter().any(|record| {
            matches!(record, HarnessRecord::StepAttempt { run_id: record_run_id, .. } if record_run_id == &run_id)
        });
        let has_terminal_assistant = journal.store.entries().iter().any(|entry| {
            entry.seq > start_seq
                && matches!(
                    &entry.message,
                    AgentMessage::Assistant {
                        tool_calls: None,
                        ..
                    }
                )
        });
        if has_terminal_assistant && !has_attempt {
            journal.record_assistant_attempt(&run_id, TokenUsage::default())?;
            journal.finish_run(&run_id, OperationOutcome::Completed, None)?;
            return Ok(true);
        }
        *self
            .harness_run_id
            .lock()
            .map_err(|_| "Harness run state is unavailable".to_string())? = Some(run_id.clone());
        let mut events = self.subscribe();
        self.agent.resume_pending_turn().await;
        self.sync_session_tree_and_dispatch_assistant_hooks().await;
        self.run_scheduled_agent_work().await;
        let mut usage = TokenUsage::default();
        let mut failure = None;
        let mut tool_termination = HashMap::new();
        while let Ok(event) = events.try_recv() {
            match event {
                AgentEvent::AgentEnd { usage: value } => usage = value,
                AgentEvent::AgentError { error } => failure = Some(error),
                AgentEvent::ToolExecutionEnd {
                    tool_call_id,
                    result,
                    ..
                } => {
                    tool_termination.insert(tool_call_id, result.terminates());
                }
                _ => {}
            }
        }
        let journal = self
            .harness
            .as_mut()
            .ok_or_else(|| "harness journal disappeared during resume".to_string())?;
        if let Some(error) = failure {
            if is_retryable_generation_error(&error)
                && journal.schedule_retry(&run_id, &error).is_ok()
            {
                return Err(error);
            }
            journal.finish_run(&run_id, OperationOutcome::Failed, Some(error.clone()))?;
            if let Ok(mut active) = self.harness_run_id.lock() {
                if active.as_deref() == Some(run_id.as_str()) {
                    *active = None;
                }
            }
            return Err(error);
        }
        journal.record_completed_tools_with_termination(&run_id, &tool_termination)?;
        journal.record_assistant_attempt(&run_id, usage)?;
        journal.finish_run(&run_id, OperationOutcome::Completed, None)?;
        if let Ok(mut active) = self.harness_run_id.lock() {
            if active.as_deref() == Some(run_id.as_str()) {
                *active = None;
            }
        }
        Ok(true)
    }

    fn redeem_suspended_deferred(
        &mut self,
        run_id: &str,
        resolution: DeferredResolution,
    ) -> Result<bool, String> {
        let journal = self
            .harness
            .as_mut()
            .ok_or_else(|| "harness journal is unavailable".to_string())?;
        journal.redeem_deferred(run_id, resolution)
    }

    async fn redeem_suspended_deferred_from_provider(
        &mut self,
        run_id: &str,
    ) -> Result<bool, String> {
        let handle = {
            let journal = self
                .harness
                .as_mut()
                .ok_or_else(|| "harness journal is unavailable".to_string())?;
            journal.refresh()?;
            journal
                .store
                .entries()
                .iter()
                .rev()
                .find_map(|entry| match &entry.message {
                    AgentMessage::Assistant {
                        deferred_handle: Some(handle),
                        ..
                    } => Some(handle.clone()),
                    _ => None,
                })
                .ok_or_else(|| format!("deferred handle for {run_id} is missing"))?
        };
        let resolution = match self
            .agent
            .fetch_deferred(&handle.model, &handle.handle_id)
            .await?
        {
            threadlane_provider::DeferredResponse::Pending => DeferredResolution::Pending(handle),
            threadlane_provider::DeferredResponse::Ready { content } => {
                DeferredResolution::Ready(AgentMessage::Assistant {
                    content: Some(content),
                    tool_calls: None,
                    stop_reason: Some("deferred_ready".into()),
                    deferred_handle: None,
                })
            }
            threadlane_provider::DeferredResponse::Error { message } => {
                DeferredResolution::Error(message)
            }
        };
        self.redeem_suspended_deferred(run_id, resolution)
    }

    pub async fn cancel_suspended_deferred(&mut self, run_id: &str) -> Result<(), String> {
        let handle = {
            let journal = self
                .harness
                .as_mut()
                .ok_or_else(|| "harness journal is unavailable".to_string())?;
            journal.refresh()?;
            let open_run = Reducer::reduce(&journal.store)
                .map_err(|error| error.to_string())?
                .lane("main")
                .and_then(|lane| lane.open_operation.clone());
            if open_run.as_deref() != Some(run_id) {
                return Err(format!("deferred operation {run_id} is not open"));
            }
            journal
                .request_abort()?
                .ok_or_else(|| format!("deferred operation {run_id} is not open"))?;
            journal
                .store
                .entries()
                .iter()
                .rev()
                .find_map(|entry| match &entry.message {
                    AgentMessage::Assistant {
                        deferred_handle: Some(handle),
                        ..
                    } => Some(handle.clone()),
                    _ => None,
                })
                .ok_or_else(|| format!("deferred handle for {run_id} is missing"))?
        };
        self.agent
            .cancel_deferred(&handle.model, &handle.handle_id)
            .await
            .or_else(|error| {
                warn!("Deferred cancellation failed after durable abort: {error}");
                Ok(())
            })
    }

    pub async fn handle_input_with_images(
        &mut self,
        input: &str,
        images: Vec<ImageAttachment>,
    ) -> Option<Result<String, String>> {
        self.cancellation.clear_cancellation_guard();
        if let Err(error) = self.recover_interrupted_subagent_lanes().await {
            return Some(Err(error));
        }
        if let Some(error) = self.harness_journal_error.as_ref() {
            let error = format!("Harness Error: {error}");
            let _ = self.agent.event_tx.send(AgentEvent::AgentError {
                error: error.clone(),
            });
            return Some(Err(error));
        }
        let adopted_harness_run = self
            .harness_run_id
            .lock()
            .ok()
            .is_some_and(|run_id| run_id.is_some());
        if !adopted_harness_run {
            if let Some(journal) = self.harness.as_mut() {
                match journal.recover_abort() {
                    Ok(_) => {}
                    Err(error) => return Some(Err(format!("Harness Error: {error}"))),
                }
            }
        }
        self.repair_interrupted_history().await;
        *self.dispatch_parent_leaf.lock().unwrap() =
            self.session_tree.active_node_id().map(str::to_owned);
        let trimmed = input.trim();

        // 1. Expand prompt templates (e.g. /review, /component Button) if match
        if self.prompt_templates.is_none() {
            let global_dir = std::env::var_os("HOME")
                .map(PathBuf::from)
                .map(|h| h.join(".threadlane"))
                .unwrap_or_else(|| self.work_dir.join(".threadlane"));
            self.prompt_templates = Some(crate::prompt_templates::load_prompt_templates(
                &self.work_dir,
                &global_dir,
            ));
        }
        let templates = self.prompt_templates.as_ref().unwrap();
        let expanded_input = crate::prompt_templates::expand_prompt_template(trimmed, templates);
        let effective_input = expanded_input.trim();

        if let Some(command_input) = effective_input.strip_prefix('/') {
            let mut parts = command_input.split_whitespace();
            let cmd_name = parts.next().unwrap_or("");
            let cmd_args = parts.collect::<Vec<&str>>().join(" ");

            if cmd_name.starts_with("skill:") || cmd_name == "skill" {
                let skill_name = if let Some(skill_name) = cmd_name.strip_prefix("skill:") {
                    skill_name
                } else {
                    cmd_args.trim()
                };

                match self.skills.get_skill_instructions(skill_name) {
                    Ok(instructions) => {
                        let prompt = format!(
                            "Use the following Skill instructions for '{}':\n\n{}",
                            skill_name, instructions
                        );
                        let visible_prompt = AgentMessage::user(input, images.clone());
                        let harness_run_id = match self.begin_harness_run(visible_prompt).await {
                            Ok(run_id) => run_id,
                            Err(error) => return Some(Err(format!("Harness Error: {error}"))),
                        };
                        let parent_leaf = self.prompt_parent_leaf(
                            AgentMessage::user(input, images.clone()),
                            harness_run_id.is_some(),
                        );
                        *self.dispatch_parent_leaf.lock().unwrap() = parent_leaf;
                        self.agent
                            .prompt_message(AgentMessage::user(prompt, images.clone()))
                            .await;
                        self.sync_session_tree_and_dispatch_assistant_hooks().await;
                        self.run_scheduled_agent_work().await;
                        if let Err(error) = self.commit_completed_subagent_lanes() {
                            *self.dispatch_parent_leaf.lock().unwrap() = None;
                            let _ = self
                                .finish_harness_run(
                                    harness_run_id.as_ref().map(|run| run.run_id.as_str()),
                                    OperationOutcome::Failed,
                                    Some(error.clone()),
                                )
                                .await;
                            return Some(Err(error));
                        }
                        *self.dispatch_parent_leaf.lock().unwrap() = None;
                        if let Err(error) = self
                            .finish_harness_run(
                                harness_run_id.as_ref().map(|run| run.run_id.as_str()),
                                OperationOutcome::Completed,
                                None,
                            )
                            .await
                        {
                            return Some(Err(format!("Harness Error: {error}")));
                        }
                        return Some(Ok(format!("Loaded skill '{}'", skill_name)));
                    }
                    Err(err) => return Some(Err(format!("Skill Error: {}", err))),
                }
            }

            if cmd_name == "subagent" {
                let task_prompt = cmd_args.trim();
                if task_prompt.is_empty() {
                    let err = "Usage: /subagent <task description>".to_string();
                    let run_id = self.harness_run_id.lock().ok().and_then(|r| r.clone());
                    let _ = self
                        .finish_harness_run(
                            run_id.as_deref(),
                            OperationOutcome::Failed,
                            Some(err.clone()),
                        )
                        .await;
                    return Some(Err(err));
                }
                let task = AgentRunTask {
                    agent: "worker".to_string(),
                    task: task_prompt.to_string(),
                    instructions: None,
                    tools: None,
                    model: None,
                };
                let visible_prompt = AgentMessage::user(input, images.clone());
                let harness_run_id = match self.begin_harness_run(visible_prompt).await {
                    Ok(run_id) => run_id,
                    Err(error) => return Some(Err(format!("Harness Error: {error}"))),
                };
                if let Some(run_id) = harness_run_id.as_ref().map(|run| run.run_id.as_str()) {
                    if let Some(journal) = self.harness.as_mut() {
                        if let Err(error) = journal.prepare_assistant_attempt(run_id) {
                            let _ = self
                                .finish_harness_run(
                                    Some(run_id),
                                    OperationOutcome::Failed,
                                    Some(error.clone()),
                                )
                                .await;
                            return Some(Err(format!("Harness Error: {error}")));
                        }
                    }
                }
                let parent_leaf = self.prompt_parent_leaf(
                    AgentMessage::user(input, images.clone()),
                    harness_run_id.is_some(),
                );
                *self.dispatch_parent_leaf.lock().unwrap() = parent_leaf;
                let result = match (self.agent_runner)(vec![task], false, None).await {
                    Ok(result) => result,
                    Err(err) => {
                        *self.dispatch_parent_leaf.lock().unwrap() = None;
                        let _ = self
                            .finish_harness_run(
                                harness_run_id.as_ref().map(|run| run.run_id.as_str()),
                                OperationOutcome::Failed,
                                Some(err.clone()),
                            )
                            .await;
                        return Some(Err(format!("Subagent Error: {err}")));
                    }
                };
                let output = result["output"].as_str().unwrap_or_default().to_string();
                if let Err(error) = self.commit_completed_subagent_lanes() {
                    *self.dispatch_parent_leaf.lock().unwrap() = None;
                    let _ = self
                        .finish_harness_run(
                            harness_run_id.as_ref().map(|run| run.run_id.as_str()),
                            OperationOutcome::Failed,
                            Some(error.clone()),
                        )
                        .await;
                    return Some(Err(error));
                }
                *self.dispatch_parent_leaf.lock().unwrap() = None;
                let assistant = AgentMessage::Assistant {
                    content: Some(output.clone()),
                    tool_calls: None,
                    stop_reason: Some("subagent".into()),
                    deferred_handle: None,
                };
                if let Some(run_id) = harness_run_id.as_ref().map(|run| run.run_id.as_str()) {
                    if let Some(journal) = self.harness.as_mut() {
                        if let Err(error) =
                            journal.append_message(assistant.clone()).and_then(|_| {
                                journal.record_assistant_attempt(run_id, TokenUsage::default())
                            })
                        {
                            let _ = self
                                .finish_harness_run(
                                    Some(run_id),
                                    OperationOutcome::Failed,
                                    Some(error.clone()),
                                )
                                .await;
                            return Some(Err(format!("Harness Error: {error}")));
                        }
                    }
                }
                if harness_run_id.is_some() {
                    if let Some(path) = self.session_tree.file_path.clone() {
                        match SessionTree::load_from_file(&path) {
                            Ok(tree) => self.session_tree = tree,
                            Err(error) => {
                                return Some(Err(format!(
                                    "Harness Error: failed to reload subagent response: {error}"
                                )))
                            }
                        }
                    }
                } else {
                    self.session_tree.add_message(assistant);
                }
                self.run_scheduled_agent_work().await;
                if let Err(error) = self
                    .finish_harness_run(
                        harness_run_id.as_ref().map(|run| run.run_id.as_str()),
                        OperationOutcome::Completed,
                        None,
                    )
                    .await
                {
                    return Some(Err(format!("Harness Error: {error}")));
                }
                return Some(Ok(output));
            }

            if let Some(res) = self
                .wasi_extensions
                .execute_command_with_effects(cmd_name, &cmd_args)
            {
                let visible_prompt = AgentMessage::user(input, images.clone());
                let harness_run_id = match self.begin_harness_run(visible_prompt).await {
                    Ok(run_id) => run_id,
                    Err(error) => return Some(Err(format!("Harness Error: {error}"))),
                };
                let parent_leaf = self.prompt_parent_leaf(
                    AgentMessage::user(input, images.clone()),
                    harness_run_id.is_some(),
                );
                *self.dispatch_parent_leaf.lock().unwrap() = parent_leaf;
                return match res {
                    Ok(result) => {
                        let message = if result.message.is_empty() {
                            None
                        } else {
                            Some(result.message)
                        };
                        let dispatch = match self
                            .broker_dispatcher
                            .dispatch_envelopes(result.host_broker_requests)
                            .await
                        {
                            Ok(dispatch) => dispatch,
                            Err(error) => {
                                let _ = self
                                    .finish_harness_run(
                                        harness_run_id.as_ref().map(|run| run.run_id.as_str()),
                                        OperationOutcome::Failed,
                                        Some(error.message.clone()),
                                    )
                                    .await;
                                return Some(Err(format!("WASI Broker Error: {}", error.message)));
                            }
                        };
                        let agent_run_output =
                            dispatch.operation_results.iter().find_map(|result| {
                                if result.request.capability != "agent"
                                    || result.request.operation != "run"
                                {
                                    return None;
                                }
                                if let Some(error) = &result.error {
                                    return Some(Err(format!(
                                        "WASI Broker Error: {}",
                                        error.message
                                    )));
                                }
                                let output = result.value["output"].as_str().ok_or_else(|| {
                                    "agent.run returned no formatted output".to_string()
                                });
                                let thinking = serde_json::from_value::<Vec<AgentMessage>>(
                                    result.value["thinking"].clone(),
                                )
                                .map_err(|error| {
                                    format!("agent.run returned invalid thinking: {error}")
                                });
                                match (output, thinking) {
                                    (Ok(output), Ok(thinking)) => {
                                        for message in thinking {
                                            if let Err(error) = self.append_command_message(message)
                                            {
                                                return Some(Err(error));
                                            }
                                        }
                                        if let Err(error) =
                                            self.append_command_message(AgentMessage::Assistant {
                                                content: Some(output.to_string()),
                                                tool_calls: None,
                                                stop_reason: None,
                                                deferred_handle: None,
                                            })
                                        {
                                            return Some(Err(error));
                                        }
                                        Some(Ok(output.to_string()))
                                    }
                                    (Err(error), _) | (_, Err(error)) => Some(Err(error)),
                                }
                            });
                        self.wasi_extensions
                            .enqueue_broker_results(dispatch.operation_results);
                        self.run_scheduled_agent_work().await;
                        if result.api_version == 1 {
                            for effect in result.effects {
                                match effect {
                                    WasiLegacyEffect::SetToolPolicy { policy } => {
                                        let mut pol = self.tool_policy.lock().await;
                                        match policy.as_str() {
                                            "read_only" => *pol = ToolPolicy::ReadOnly,
                                            "full" => *pol = ToolPolicy::FullAccess,
                                            _ => continue,
                                        }
                                    }
                                    WasiLegacyEffect::RequestModelTurn { prompt } => {
                                        self.agent.prompt(&prompt).await;
                                        self.sync_session_tree_and_dispatch_assistant_hooks().await;
                                    }
                                }
                            }
                        }
                        if let Err(error) = self.commit_completed_subagent_lanes() {
                            *self.dispatch_parent_leaf.lock().unwrap() = None;
                            let _ = self
                                .finish_harness_run(
                                    harness_run_id.as_ref().map(|run| run.run_id.as_str()),
                                    OperationOutcome::Failed,
                                    Some(error.clone()),
                                )
                                .await;
                            return Some(Err(error));
                        }
                        *self.dispatch_parent_leaf.lock().unwrap() = None;
                        if let Some(agent_run_output) = agent_run_output {
                            let result = agent_run_output;
                            let outcome = if result.is_ok() {
                                OperationOutcome::Completed
                            } else {
                                OperationOutcome::Failed
                            };
                            if let Err(error) = self
                                .finish_harness_run(
                                    harness_run_id.as_ref().map(|run| run.run_id.as_str()),
                                    outcome,
                                    result.as_ref().err().cloned(),
                                )
                                .await
                            {
                                return Some(Err(format!("Harness Error: {error}")));
                            }
                            return Some(result);
                        }
                        let result = message.map(Ok);
                        let outcome = if result.is_some() {
                            OperationOutcome::Completed
                        } else {
                            OperationOutcome::Failed
                        };
                        if let Err(error) = self
                            .finish_harness_run(harness_run_id.as_ref().map(|run| run.run_id.as_str()), outcome, None)
                            .await
                        {
                            return Some(Err(format!("Harness Error: {error}")));
                        }
                        result
                    }
                    Err(err) => {
                        let message = format!("WASI Extension Error: {err}");
                        let _ = self
                            .finish_harness_run(
                                harness_run_id.as_ref().map(|run| run.run_id.as_str()),
                                OperationOutcome::Failed,
                                Some(message.clone()),
                            )
                            .await;
                        Some(Err(message))
                    }
                };
            }

            if let Some(cmd_action) = parse_slash_command(effective_input) {
                if cmd_action == CommandAction::Quit {
                    return Some(Ok("quitting".to_string()));
                }
                if cmd_action == CommandAction::Compact {
                    return Some(match self.compact_history_with_harness().await {
                        Ok(true) => Ok("Context compacted in the current session.".into()),
                        Ok(false) => Ok("Nothing to compact yet.".into()),
                        Err(error) => Err(format!("Harness Error: {error}")),
                    });
                }
                if let CommandAction::SwitchTreeBranch(node_id) = &cmd_action {
                    return Some(self.navigate_tree_branch(node_id).await);
                }
                if let CommandAction::SwitchModel(model) = &cmd_action {
                    if !model.is_empty() {
                        return Some(
                            self.set_model(model.clone())
                                .await
                                .map(|_| format!("Switched model to: {model}")),
                        );
                    }
                }
                if let CommandAction::SetName(name) = &cmd_action {
                    return Some(
                        self.set_name(name.clone())
                            .map(|_| format!("Session name set to: {name}")),
                    );
                }
                if let CommandAction::Plan(objective) = &cmd_action {
                    let task_prompt = objective.trim();
                    if task_prompt.is_empty() {
                        return Some(Ok("Usage: /plan <task objective> - generate an implementation plan with the Plan model.".into()));
                    }
                    let client = threadlane_provider::router::ProviderClient::new(
                        self.agent.api_key.clone(),
                        self.agent.account_id.clone(),
                    );
                    let active_model = self
                        .session_tree
                        .model
                        .clone()
                        .unwrap_or_else(|| "gpt-4o".into());
                    let plan_model = self
                        .agent
                        .model_roles()
                        .resolve_plan(&active_model)
                        .to_string();
                    match crate::plan::generate_plan_with_model(&client, &plan_model, task_prompt)
                        .await
                    {
                        Ok(plan) => {
                            if let Err(error) = self.plan_store.replace(plan.clone()) {
                                return Some(Err(format!("Failed to save plan: {error}")));
                            }
                            let _ = self
                                .agent
                                .event_tx
                                .send(AgentEvent::PlanUpdated { plan: plan.clone() });
                            let mut msg = format!(
                                "Generated implementation plan with model `{}`:\n",
                                plan_model
                            );
                            if let Some(exp) = &plan.explanation {
                                msg.push_str(&format!("\n> {}\n\n", exp));
                            }
                            for (i, item) in plan.items.iter().enumerate() {
                                let status_icon = match item.status {
                                    threadlane_agent::PlanItemStatus::Completed => "[x]",
                                    threadlane_agent::PlanItemStatus::InProgress => "[>]",
                                    threadlane_agent::PlanItemStatus::Pending => "[ ]",
                                };
                                msg.push_str(&format!(
                                    "{}. {} {}\n",
                                    i + 1,
                                    status_icon,
                                    item.step
                                ));
                            }
                            return Some(Ok(msg));
                        }
                        Err(error) => return Some(Err(format!("Plan generation failed: {error}"))),
                    }
                }
                if matches!(
                    cmd_action,
                    CommandAction::Advisor(_) | CommandAction::Roles(_)
                ) {
                    let output =
                        execute_slash_command(cmd_action, &mut self.agent, &mut self.session_tree)
                            .await;
                    let roles = self.agent.model_roles().clone();
                    let _ = self
                        .agent
                        .event_tx
                        .send(AgentEvent::ModelRolesUpdated { roles });
                    return Some(Ok(output));
                }
                let output =
                    execute_slash_command(cmd_action, &mut self.agent, &mut self.session_tree)
                        .await;
                return Some(Ok(output));
            }
        }

        if self.agent.auto_compact_history().await {
            let state = self.agent.get_state().await;
            if let Some(summary) = state
                .messages
                .iter()
                .rev()
                .find_map(threadlane_agent::compaction_summary_text)
            {
                let retained_tail = compaction_retained_tail(&state.messages);
                if let Err(error) = self.persist_harness_compaction(summary, &retained_tail) {
                    let _ = self.agent.event_tx.send(AgentEvent::AgentError { error });
                    return None;
                }
            }
            if self.harness.is_some() {
                let path = self
                    .session_tree
                    .file_path
                    .clone()
                    .ok_or_else(|| "harness compaction has no session path".to_string());
                match path.and_then(|path| {
                    SessionTree::load_from_file(&path)
                        .map_err(|error| format!("failed to reload compacted session: {error}"))
                }) {
                    Ok(tree) => self.session_tree = tree,
                    Err(error) => {
                        let _ = self.agent.event_tx.send(AgentEvent::AgentError { error });
                        return None;
                    }
                }
            } else {
                self.session_tree.replace_active_branch(state.messages);
            }
        }

        let msg = AgentMessage::user(effective_input, images);
        let harness_run_id = match self.begin_harness_run(msg.clone()).await {
            Ok(run_id) => run_id,
            Err(error) => {
                let message = format!("Harness Error: {error}");
                let _ = self.agent.event_tx.send(AgentEvent::AgentError {
                    error: message.clone(),
                });
                return Some(Err(message));
            }
        };
        let parent_leaf = self.prompt_parent_leaf(msg.clone(), harness_run_id.is_some());
        *self.dispatch_parent_leaf.lock().unwrap() = parent_leaf;
        if let (Some(run_id), Some(harness)) = (harness_run_id.as_ref().map(|run| run.run_id.as_str()), self.harness.as_mut()) {
            if let Err(error) = harness.prepare_assistant_attempt(run_id) {
                let _ = self
                    .finish_harness_run(Some(run_id), OperationOutcome::Failed, Some(error.clone()))
                    .await;
                return Some(Err(format!("Harness Error: {error}")));
            }
        }
        let mut harness_events = self.subscribe();
        if let Some(accepted) = harness_run_id.as_ref() {
            if let Err(error) = self.execute_accepted_run(accepted).await {
                self.harness_journal_error = Some(error);
            }
        } else {
            self.agent.prompt_message(msg).await;
            self.sync_session_tree_and_dispatch_assistant_hooks().await;
        }
        if let Some(error) = self.harness_journal_error.clone() {
            *self.dispatch_parent_leaf.lock().unwrap() = None;
            let _ = self
                .finish_harness_run(
                    harness_run_id.as_ref().map(|run| run.run_id.as_str()),
                    OperationOutcome::Failed,
                    Some(error.clone()),
                )
                .await;
            return Some(Err(format!("Harness Error: {error}")));
        }
        self.run_scheduled_agent_work().await;
        if let Some(error) = self.harness_journal_error.clone() {
            *self.dispatch_parent_leaf.lock().unwrap() = None;
            let _ = self
                .finish_harness_run(
                    harness_run_id.as_ref().map(|run| run.run_id.as_str()),
                    OperationOutcome::Failed,
                    Some(error.clone()),
                )
                .await;
            return Some(Err(format!("Harness Error: {error}")));
        }
        if let Err(error) = self.commit_completed_subagent_lanes() {
            *self.dispatch_parent_leaf.lock().unwrap() = None;
            let _ = self
                .finish_harness_run(
                    harness_run_id.as_ref().map(|run| run.run_id.as_str()),
                    OperationOutcome::Failed,
                    Some(error.clone()),
                )
                .await;
            let _ = self.agent.event_tx.send(AgentEvent::AgentError {
                error: error.clone(),
            });
            return Some(Err(error));
        }
        *self.dispatch_parent_leaf.lock().unwrap() = None;
        let mut tool_termination = HashMap::new();
        let (usage, failure) = loop {
            match harness_events.try_recv() {
                Ok(AgentEvent::ToolExecutionEnd {
                    tool_call_id,
                    result,
                    ..
                }) => {
                    tool_termination.insert(tool_call_id, result.terminates());
                }
                Ok(AgentEvent::AgentEnd { usage }) => break (usage, None),
                Ok(AgentEvent::AgentError { error }) => break (TokenUsage::default(), Some(error)),
                Ok(_) => continue,
                Err(error) => {
                    if let Some(message) = generation_event_drain_error(error) {
                        break (TokenUsage::default(), Some(message.into()));
                    }
                }
            }
        };
        if let Some(error) = failure {
            if let Some(run_id) = harness_run_id.as_ref().map(|run| run.run_id.as_str()) {
                let completion = self.harness.as_mut().map(|journal| {
                    journal.record_completed_tools_with_termination(run_id, &tool_termination)
                });
                if let Some(Err(completion_error)) = completion {
                    let _ = self
                        .finish_harness_run(
                            Some(run_id),
                            OperationOutcome::Failed,
                            Some(completion_error.clone()),
                        )
                        .await;
                    return Some(Err(format!("Harness Error: {completion_error}")));
                }
                if is_retryable_generation_error(&error) {
                    let scheduled = self
                        .harness
                        .as_mut()
                        .map(|journal| journal.schedule_retry(run_id, &error));
                    if matches!(scheduled, Some(Ok(_))) {
                        return Some(Err(error));
                    }
                }
                let _ = self
                    .finish_harness_run(Some(run_id), OperationOutcome::Failed, Some(error.clone()))
                    .await;
            }
            return Some(Err(error));
        }
        if let Some(run_id) = harness_run_id.as_ref().map(|run| run.run_id.as_str()) {
            let attempt_result = self.harness.as_mut().map(|journal| {
                journal
                    .record_completed_tools_with_termination(run_id, &tool_termination)
                    .and_then(|_| journal.record_assistant_attempt(run_id, usage))
            });
            if let Some(Err(error)) = attempt_result {
                let _ = self
                    .finish_harness_run(Some(run_id), OperationOutcome::Failed, Some(error.clone()))
                    .await;
                return Some(Err(format!("Harness Error: {error}")));
            }
        }
        if let Err(error) = self
            .finish_harness_run(harness_run_id.as_ref().map(|run| run.run_id.as_str()), OperationOutcome::Completed, None)
            .await
        {
            return Some(Err(format!("Harness Error: {error}")));
        }

        None
    }
}

fn requires_harness_compaction_reset(
    durable_messages: &[AgentMessage],
    state_messages: &[AgentMessage],
) -> bool {
    state_messages
        .iter()
        .any(|message| threadlane_agent::compaction_summary_text(message).is_some())
        && !state_messages.starts_with(durable_messages)
}

#[cfg(test)]
mod compaction_sync_tests {
    use super::{
        durable_prompt_snapshot, requires_harness_compaction_reset, CodingAgent,
        CodingAgentOptions, CompletedSubagentLane, SubagentLaneStatus,
        MAX_PERSISTED_SYSTEM_PROMPT_BYTES,
    };
    use crate::system_prompt::SystemPromptConfig;
    use threadlane_agent::{harness::JsonlStore, AgentMessage};

    fn summary() -> AgentMessage {
        AgentMessage::Custom {
            custom_type: "compaction_summary".into(),
            payload: serde_json::json!({"summary": "older context"}),
        }
    }

    #[test]
    fn oversized_system_prompt_is_redacted_with_a_digest() {
        let content = "x".repeat(MAX_PERSISTED_SYSTEM_PROMPT_BYTES + 1);
        assert!(matches!(
            durable_prompt_snapshot(&content),
            threadlane_agent::harness::PromptSnapshot::Redacted {
                sha256,
                byte_len,
                ..
            } if sha256.as_str().len() == 64 && byte_len == content.len()
        ));
    }

    #[test]
    fn in_loop_compaction_requires_a_durable_branch_reset() {
        let durable = vec![
            AgentMessage::user("old prompt", vec![]),
            AgentMessage::Assistant {
                content: Some("old response".into()),
                tool_calls: None,
                stop_reason: None,
                deferred_handle: None,
            },
        ];
        let state = vec![summary(), AgentMessage::user("current prompt", vec![])];

        assert!(requires_harness_compaction_reset(&durable, &state));
    }

    #[test]
    fn already_persisted_compaction_uses_normal_incremental_sync() {
        let durable = vec![summary(), AgentMessage::user("current prompt", vec![])];
        let mut state = durable.clone();
        state.push(AgentMessage::Assistant {
            content: Some("new response".into()),
            tool_calls: None,
            stop_reason: None,
            deferred_handle: None,
        });

        assert!(!requires_harness_compaction_reset(&durable, &state));
    }

    #[tokio::test]
    async fn invalid_compatibility_source_does_not_break_delayed_passive_commit() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.jsonl");
        let mut agent = CodingAgent::new(CodingAgentOptions {
            api_key: "test-key".into(),
            account_id: None,
            model: "test-model".into(),
            work_dir: dir.path().to_path_buf(),
            session_file: Some(path.clone()),
            system_prompt: SystemPromptConfig::default(),
            agent_config: None,
            coding_config: None,
        });
        agent
            .begin_harness_run(AgentMessage::user("prompt", vec![]))
            .await
            .unwrap();

        let identity = agent
            .harness
            .as_mut()
            .unwrap()
            .start_subagent_lane("worker", "inspect", Some("node_69"))
            .unwrap();
        assert!(identity.source_leaf_id.is_none());
        agent
            .completed_subagent_lanes
            .lock()
            .unwrap()
            .push(CompletedSubagentLane {
                lane_name: identity.lane_name,
                run_id: identity.run_id,
                parent_leaf_id: identity.source_leaf_id,
                task: "inspect".into(),
                agent: "worker".into(),
                status: SubagentLaneStatus::Completed,
                messages: vec![AgentMessage::Assistant {
                    content: Some("done".into()),
                    tool_calls: None,
                    stop_reason: Some("end_turn".into()),
                    deferred_handle: None,
                }],
                error: None,
            });

        agent.commit_completed_subagent_lanes().unwrap();

        let store = JsonlStore::open(&path).unwrap();
        assert!(store.entries().iter().any(|entry| matches!(
            &entry.message,
            AgentMessage::Custom { custom_type, .. } if custom_type == "subagent_lane"
        )));
        assert!(store
            .entries()
            .iter()
            .all(|entry| entry.parent_id.as_deref() != Some("node_69")));
    }
}

fn compaction_retained_tail(messages: &[AgentMessage]) -> Vec<AgentMessage> {
    let Some(summary_index) = messages
        .iter()
        .rposition(|message| threadlane_agent::compaction_summary_text(message).is_some())
    else {
        return Vec::new();
    };
    messages
        .iter()
        .skip(summary_index + 1)
        .filter(|message| !matches!(message, AgentMessage::System { .. }))
        .cloned()
        .collect()
}

async fn run_subagents_with_context(
    tasks: Vec<AgentRunTask>,
    parallel: bool,
    tool_call_id: Option<String>,
    context: SubagentRunContext,
) -> Result<(String, Vec<AgentMessage>, Vec<CompletedSubagentLane>), String> {
    let run_id = NEXT_SUBAGENT_UI_RUN_ID.fetch_add(1, Ordering::Relaxed);
    log::info!(
        "subagent batch run_id={}: {} task(s), parallel={parallel}",
        run_id,
        tasks.len()
    );
    for (task_index, task) in tasks.iter().enumerate() {
        log::debug!(
            "subagent queued run_id={run_id} task_index={task_index} agent={} task={}",
            task.agent,
            task.task
        );
        let _ = context.parent_event_tx.send(AgentEvent::SubagentQueued {
            run_id,
            task_index,
            agent: task.agent.clone(),
            task: task.task.clone(),
        });
    }
    let candidates = discover_agents(&context.work_dir, AgentScope::Both).agents;
    let lane_key = tool_call_id
        .map(|id| format!("tool-{id}"))
        .unwrap_or_else(|| "explicit".into());
    let run_one = |task_index: usize, task: AgentRunTask| {
        let candidate = candidates
            .iter()
            .find(|candidate| candidate.name == task.agent)
            .cloned();

        let mut config = match candidate {
            Some(static_config) => static_config,
            None => {
                let sys_prompt = task.instructions.clone().unwrap_or_else(|| {
                    format!(
                        "You are a specialized subagent acting as '{}'. Complete only the assigned task and report results clearly to the parent agent.",
                        task.agent
                    )
                });
                AgentDefinition {
                    name: task.agent.clone(),
                    description: format!("Dynamic subagent for {}", task.agent),
                    tools: task.tools.clone(),
                    model: task.model.clone(),
                    system_prompt: sys_prompt,
                    source: crate::agents::AgentSource::Project,
                    file_path: context.work_dir.clone(),
                }
            }
        };
        if let Some(inst) = &task.instructions {
            config.system_prompt = inst.clone();
        }
        if let Some(t) = &task.tools {
            config.tools = Some(t.clone());
        }
        if let Some(m) = &task.model {
            config.model = Some(m.clone());
        }

        let context = context.clone();
        let event_tx = context.parent_event_tx.clone();
        let lane_task = task.task.clone();
        let lane_agent = task.agent.clone();
        let lane_key = lane_key.clone();
        async move {
            let parent_leaf_id = context.parent_leaf_id.clone();
            let lane_hint = format!(
                "subagent-{}-{}:{task_index}",
                context.parent_session_id, lane_key
            );
            let _permit = context
                .semaphore
                .clone()
                .acquire_owned()
                .await
                .map_err(|_| "Subagent concurrency limiter closed".to_string())?;
            let start = match context.session_file.as_deref() {
                Some(path) => {
                    let mut journal = HarnessJournal::open(path)?;
                    let result = journal.start_subagent_lane(
                        &lane_hint,
                        &task.task,
                        parent_leaf_id.as_deref(),
                    );
                    match &result {
                        Ok(identity) => log::info!(
                            "subagent lane started: run_id={} lane={}",
                            identity.run_id,
                            identity.lane_name
                        ),
                        Err(e) => log::warn!(
                            "subagent lane start failed: hint={lane_hint} error={}",
                            e.error
                        ),
                    }
                    result
                }
                None => {
                    log::warn!(
                        "subagent lane={lane_hint}: no session_file, running without harness"
                    );
                    Ok(SubagentLaneIdentity {
                        lane_name: lane_hint.clone(),
                        run_id: lane_hint.clone(),
                        source_leaf_id: parent_leaf_id.clone(),
                        started_seq: 0,
                    })
                }
            };
            let result = match start {
                Ok(identity) => {
                    let _ = event_tx.send(AgentEvent::SubagentStarted {
                        run_id,
                        task_index,
                        journal_run_id: identity.run_id.clone(),
                    });
                    #[cfg(test)]
                    if let Some(observer) = context.child_work_observer.as_ref() {
                        observer();
                    }
                    timeout(
                        SUBAGENT_TIMEOUT,
                        run_subagent_task(
                            config,
                            task.task,
                            context,
                            run_id,
                            task_index,
                            identity.clone(),
                            Vec::new(),
                        ),
                    )
                    .await
                    .map_err(|_| "Subagent timed out".to_string())
                    .map(|result| (result, identity))?
                }
                Err(SubagentStartError { identity, error }) => (
                    Err(error),
                    identity.unwrap_or_else(|| SubagentLaneIdentity {
                        lane_name: lane_hint.clone(),
                        run_id: lane_hint,
                        source_leaf_id: parent_leaf_id.clone(),
                        started_seq: 0,
                    }),
                ),
            };
            let (result, identity) = result;
            let (succeeded, error) = match &result {
                Ok(result) if result.error.is_none() => (true, None),
                Ok(result) => (false, result.error.clone()),
                Err(error) => (false, Some(error.clone())),
            };
            log::info!(
                "subagent finished run_id={} journal_run_id={} succeeded={succeeded}",
                run_id,
                identity.run_id
            );
            let _ = event_tx.send(AgentEvent::SubagentFinished {
                run_id,
                task_index,
                journal_run_id: identity.run_id.clone(),
                succeeded,
                error,
            });
            let lane = CompletedSubagentLane {
                lane_name: identity.lane_name,
                run_id: identity.run_id,
                parent_leaf_id: identity.source_leaf_id,
                task: lane_task,
                agent: lane_agent,
                status: if succeeded {
                    SubagentLaneStatus::Completed
                } else {
                    SubagentLaneStatus::Failed
                },
                messages: result
                    .as_ref()
                    .map(|result| result.messages.clone())
                    .unwrap_or_default(),
                error: result
                    .as_ref()
                    .ok()
                    .and_then(|result| result.error.clone())
                    .or_else(|| result.as_ref().err().cloned()),
            };
            Ok((result, lane))
        }
    };
    let results = if parallel {
        futures::future::join_all(
            tasks
                .iter()
                .cloned()
                .enumerate()
                .map(|(task_index, task)| run_one(task_index, task)),
        )
        .await
    } else {
        let mut previous = String::new();
        let mut results = Vec::with_capacity(tasks.len());
        for (task_index, task) in tasks.iter().cloned().enumerate() {
            let task = AgentRunTask {
                agent: task.agent,
                task: task.task.replace("{previous}", &previous),
                instructions: task.instructions,
                tools: task.tools,
                model: task.model,
            };
            let result = run_one(task_index, task).await?;
            if let Ok(output) = &result.0 {
                previous = output.output.clone();
            }
            results.push(Ok(result));
        }
        results
    };
    let results = results.into_iter().collect::<Result<Vec<_>, String>>()?;
    let (tool_results, lanes): (Vec<_>, Vec<_>) = results.into_iter().unzip();
    let thinking = tool_results
        .iter()
        .filter_map(|result| result.as_ref().ok())
        .flat_map(|result| result.thinking.clone())
        .collect();
    Ok((
        format_subagent_results(tasks, tool_results, &lanes),
        thinking,
        lanes,
    ))
}

async fn checkpoint_new_subagent_messages(
    session_file: Option<&Path>,
    lane_name: &str,
    run_id: &str,
    state: &Arc<tokio::sync::Mutex<TurnState>>,
    checkpoint_cursor: &mut usize,
) -> Result<(), String> {
    let messages = state.lock().await.messages.clone();
    if let Some(path) = session_file {
        let mut journal = HarnessJournal::open(path)?;
        journal.checkpoint(lane_name, run_id, &messages[*checkpoint_cursor..])?;
    }
    *checkpoint_cursor = messages.len();
    Ok(())
}

async fn consume_subagent_turn_checkpoints(
    mut events: broadcast::Receiver<AgentEvent>,
    session_file: Option<PathBuf>,
    lane_name: String,
    run_id: String,
    state: Arc<tokio::sync::Mutex<TurnState>>,
    initial_checkpoint_cursor: usize,
) -> Result<usize, String> {
    let mut checkpoint_cursor = initial_checkpoint_cursor;
    while let Ok(event) = events.recv().await {
        if matches!(&event, AgentEvent::TurnEnd { .. }) {
            checkpoint_new_subagent_messages(
                session_file.as_deref(),
                &lane_name,
                &run_id,
                &state,
                &mut checkpoint_cursor,
            )
            .await?;
        }
        if matches!(&event, AgentEvent::AgentEnd { .. }) {
            break;
        }
    }
    Ok(checkpoint_cursor)
}

async fn checkpoint_subagent_final_snapshot(
    session_file: Option<&Path>,
    lane_name: &str,
    run_id: &str,
    state: &Arc<tokio::sync::Mutex<TurnState>>,
    checkpoint_cursor: &mut usize,
) -> Result<(), String> {
    checkpoint_new_subagent_messages(session_file, lane_name, run_id, state, checkpoint_cursor)
        .await
}

fn accept_completed_subagent_lanes(
    completed_lanes: &Arc<std::sync::Mutex<Vec<CompletedSubagentLane>>>,
    lanes: Vec<CompletedSubagentLane>,
) -> Result<(), String> {
    completed_lanes
        .lock()
        .map_err(|_| "Completed subagent lane sink is unavailable".to_string())?
        .extend(lanes);
    Ok(())
}

async fn run_subagent_task(
    config: AgentDefinition,
    task: String,
    context: SubagentRunContext,
    run_id: u64,
    task_index: usize,
    identity: SubagentLaneIdentity,
    resume_messages: Vec<AgentMessage>,
) -> Result<SubagentResult, String> {
    let model = config
        .model
        .clone()
        .unwrap_or_else(|| context.parent_model.clone());
    let lane_name = identity.lane_name.clone();
    let journal_run_id = identity.run_id.clone();
    // Use UnifiedAgent with a lane-aware journal adapter instead of
    // Agent + individual recorder callbacks.
    let subagent_session = context
        .session_file
        .clone()
        .unwrap_or_else(|| std::env::temp_dir().join(format!("subagent-{lane_name}.jsonl")));
    let mut agent = UnifiedAgent::new(
        context.api_key.clone(),
        context.account_id.clone(),
        &model,
        &subagent_session,
        threadlane_agent::AgentConfig::default(),
    )
    .map_err(|e| format!("Failed to create subagent: {e}"))?;

    if let Some(tools) = config.tools.clone() {
        agent.set_allowed_tool_names(Some(tools.into_iter().collect()));
    }
    let system_prompt = format!(
        "{}\n\nYou are an isolated subagent working in {}. Complete only the assigned task and return a concise final report to your parent agent.",
        config.system_prompt,
        context.work_dir.display(),
    );
    agent.set_system_prompt(system_prompt).await;
    let is_recovery = !resume_messages.is_empty();
    if is_recovery {
        agent.turn.lock().await.messages.extend(
            resume_messages
                .iter()
                .filter(|message| !matches!(message, AgentMessage::System { .. }))
                .cloned(),
        );
    }
    agent.work_dir = Some(context.work_dir.clone());

    // Session file used by checkpoint persistence below.
    let session_file_for_checkpoint = context.session_file.clone();

    let policy = Arc::new(tokio::sync::Mutex::new(
        if config.tools.as_ref().is_some_and(|tools| {
            !tools
                .iter()
                .any(|tool| matches!(tool.as_str(), "write_file" | "edit_file" | "write" | "edit"))
        }) {
            ToolPolicy::ReadOnly
        } else {
            ToolPolicy::FullAccess
        },
    ));
    let agent_work = AgentWorkScheduler::default();
    let (broker_dispatcher, _, _) = build_broker_dispatcher(
        policy.clone(),
        context.extensions.clone(),
        false,
        context.work_dir.clone(),
        agent.event_tx.clone(),
        agent_work.clone(),
        None,
        Some(subagent_session.clone()),
    );
    agent
        .hook_registry
        .register(
            HookKind::BeforeTool,
            "extension-before-tool",
            extension_before_tool_hook_handler(
                policy,
                context.extensions.clone(),
                broker_dispatcher.clone(),
            ),
        )
        .expect("extension before-tool hook must register");
    agent
        .hook_registry
        .register(
            HookKind::AfterTool,
            "extension-after-tool",
            create_after_tool_hook_handler(context.extensions.clone(), broker_dispatcher),
        )
        .expect("extension after-tool hook must register");

    #[cfg(test)]
    if let Some(observer) = context.scheduler_observer.as_ref() {
        if is_recovery
            && resume_messages
                .iter()
                .any(|message| matches!(message, AgentMessage::Tool { .. }))
        {
            return Ok(SubagentResult {
                output: "test subagent result".into(),
                thinking: Vec::new(),
                inner_tools: Vec::new(),
                error: None,
                messages: resume_messages,
            });
        }
        if let Some(tool_observer) = context.child_tool_observer.as_ref() {
            agent
                .register_tool_executor(Arc::new(DeterministicSubagentToolExecutor {
                    observed: tool_observer.clone(),
                }))
                .map_err(|e| e.to_string())?;
            let tool_results = agent
                .execute_tools(&[threadlane_provider::openai::ToolCall {
                    id: "test-child-tool".into(),
                    r#type: "function".into(),
                    function: threadlane_provider::openai::ToolCallFunction {
                        name: "test_child_tool".into(),
                        arguments: "{}".into(),
                    },
                    thought_signature: None,
                }])
                .await;
            if tool_results[0].is_error {
                return Err(tool_results[0].content.clone());
            }
        }
        let scheduler = AgentWorkScheduler::default();
        scheduler.set_test_observer(observer.clone());
        scheduler.schedule(if is_recovery {
            AgentWork::RequestTurn(SUBAGENT_RECOVERY_PROMPT.into())
        } else {
            AgentWork::QueueMessage {
                content: "test subagent follow-up".into(),
                images: Vec::new(),
            }
        });
        let observed_model = model.clone();
        let _ = scheduler.run_unified(&mut agent).await;
        let mut messages = if is_recovery {
            resume_messages.clone()
        } else {
            vec![AgentMessage::User {
                content: task.clone(),
            }]
        };
        messages.push(AgentMessage::Assistant {
            content: Some("test subagent result".into()),
            tool_calls: None,
            stop_reason: None,
            deferred_handle: None,
        });
        return Ok(SubagentResult {
            output: format!("test subagent result ({observed_model})"),
            thinking: Vec::new(),
            inner_tools: Vec::new(),
            error: None,
            messages,
        });
    }

    // The GUI subscribes only to the parent agent. Relay child lifecycle,
    // reasoning, and tool events so users can see subagent progress live.
    // Assistant text stays local and is returned below as one labelled result.
    let mut ui_events = agent.subscribe();
    let ui_event_prefix = format!("subagent-{run_id}:{task_index}:",);
    let event_tx_clone = context.parent_event_tx.clone();
    tokio::spawn(async move {
        while let Ok(event) = ui_events.recv().await {
            if let Some(event) = subagent_ui_event(event, &ui_event_prefix) {
                let _ = event_tx_clone.send(event);
            }
        }
    });

    // Persist only completed child turns; partial stream deltas stay in memory.
    let checkpoint_events = agent.subscribe();
    let checkpoint_state = agent.turn.clone();
    let checkpoint_session_file = session_file_for_checkpoint.clone();
    let checkpoint_lane_name = lane_name.clone();
    let checkpoint_run_id = journal_run_id.clone();
    let initial_checkpoint_cursor = agent.turn.lock().await.messages.len();
    let checkpoint_task = tokio::spawn(consume_subagent_turn_checkpoints(
        checkpoint_events,
        checkpoint_session_file,
        checkpoint_lane_name,
        checkpoint_run_id,
        checkpoint_state,
        initial_checkpoint_cursor,
    ));

    // Preserve provider and tool-loop errors in the command result as well.
    let mut events = agent.subscribe();
    agent
        .prompt(if is_recovery {
            SUBAGENT_RECOVERY_PROMPT
        } else {
            &task
        })
        .await;
    while agent_work.run_unified(&mut agent).await {}

    let mut checkpoint_cursor = checkpoint_task
        .await
        .map_err(|error| format!("Child turn checkpoint task failed: {error}"))??;
    checkpoint_subagent_final_snapshot(
        session_file_for_checkpoint.as_deref(),
        &lane_name,
        &journal_run_id,
        &agent.turn,
        &mut checkpoint_cursor,
    )
    .await?;

    let mut error = None;
    while let Ok(event) = events.try_recv() {
        if let AgentEvent::AgentError { error: message } = event {
            error = Some(message);
        }
    }
    if error.is_some() && config.model.is_some() && model != context.parent_model {
        let mut fallback_config = config.clone();
        fallback_config.model = None;
        return Box::pin(run_subagent_task(
            fallback_config,
            task,
            context,
            run_id,
            task_index,
            identity,
            resume_messages,
        ))
        .await;
    }

    let state = agent.get_state().await;
    let output = state
        .messages
        .iter()
        .rev()
        .find_map(|message| match message {
            AgentMessage::Assistant {
                content: Some(content),
                ..
            } => Some(content.clone()),
            _ => None,
        })
        .unwrap_or_default();
    let thinking: Vec<AgentMessage> = state
        .messages
        .iter()
        .filter(|message| matches!(message, AgentMessage::Custom { custom_type, .. } if custom_type == "thinking"))
        .cloned()
        .collect();
    let mut inner_tools = Vec::new();
    for message in &state.messages {
        match message {
            AgentMessage::Assistant {
                tool_calls: Some(calls),
                ..
            } => {
                for call in calls {
                    inner_tools.push(SubagentInnerTool {
                        id: call.id.clone(),
                        name: call.function.name.clone(),
                        arguments: call.function.arguments.clone(),
                        output: String::new(),
                        is_error: false,
                    });
                }
            }
            AgentMessage::Tool {
                tool_call_id,
                content,
                is_error,
                ..
            } => {
                if let Some(tool) = inner_tools.iter_mut().find(|t| &t.id == tool_call_id) {
                    tool.output = content.clone();
                    tool.is_error = *is_error;
                }
            }
            _ => {}
        }
    }
    let completion_error = error
        .map(|error| format!("Subagent '{}' failed: {error}", config.name))
        .or_else(|| {
            output.is_empty().then(|| {
                format!(
                    "Subagent '{}' completed without a final text response.",
                    config.name
                )
            })
        });
    Ok(SubagentResult {
        output: completion_error.clone().unwrap_or(output),
        thinking,
        inner_tools,
        error: completion_error,
        messages: state
            .messages
            .into_iter()
            .filter(|message| !matches!(message, AgentMessage::System { .. }))
            .collect(),
    })
}

pub(crate) fn subagent_ui_event(event: AgentEvent, tool_call_prefix: &str) -> Option<AgentEvent> {
    match event {
        // Parent lifecycle and the outer subagent tool own GUI status. Relaying a
        // child's lifecycle would mark a parallel delegation ready or failed
        // while sibling tasks and the parent turn are still running.
        AgentEvent::AgentStart
        | AgentEvent::AgentEnd { .. }
        | AgentEvent::AgentError { .. }
        | AgentEvent::TurnStart { .. }
        | AgentEvent::TurnEnd { .. }
        | AgentEvent::SubagentQueued { .. }
        | AgentEvent::SubagentStarted { .. }
        | AgentEvent::SubagentFinished { .. }
        | AgentEvent::SubagentRecovery { .. } => None,
        // Keep child prose and reasoning inside the child session. The final
        // labelled result renders it under the matching task after completion.
        AgentEvent::MessageUpdate { .. } => None,
        AgentEvent::ToolExecutionStart {
            tool_call_id,
            name,
            arguments,
        } => Some(AgentEvent::ToolExecutionStart {
            tool_call_id: format!("{tool_call_prefix}{tool_call_id}"),
            name,
            arguments,
        }),
        AgentEvent::ToolExecutionUpdate {
            tool_call_id,
            partial_result,
        } => Some(AgentEvent::ToolExecutionUpdate {
            tool_call_id: format!("{tool_call_prefix}{tool_call_id}"),
            partial_result,
        }),
        AgentEvent::ToolExecutionEnd {
            tool_call_id,
            name,
            result,
        } => Some(AgentEvent::ToolExecutionEnd {
            tool_call_id: format!("{tool_call_prefix}{tool_call_id}"),
            name,
            result,
        }),
        event => Some(event),
    }
}

#[derive(Clone, Debug)]
pub struct SubagentInnerTool {
    id: String,
    name: String,
    arguments: String,
    output: String,
    is_error: bool,
}

#[derive(Clone, Debug)]
pub struct SubagentResult {
    output: String,
    thinking: Vec<AgentMessage>,
    inner_tools: Vec<SubagentInnerTool>,
    error: Option<String>,
    messages: Vec<AgentMessage>,
}

fn tool_target_preview(name: &str, arguments: &str) -> String {
    let parsed: Option<serde_json::Value> = serde_json::from_str(arguments).ok();
    let get_str = |key: &str| {
        parsed
            .as_ref()
            .and_then(|v| v.get(key))
            .and_then(serde_json::Value::as_str)
    };
    let target = match name {
        "read_file" | "write_file" | "edit_file" | "edit_file_hashline" => get_str("path")
            .or_else(|| get_str("file_path"))
            .unwrap_or(arguments),
        "list_dir" => get_str("path").unwrap_or(arguments),
        "run_command" => get_str("command").unwrap_or(arguments),
        _ => arguments,
    };
    if target.chars().count() > 60 {
        target.chars().take(60).collect::<String>()
    } else {
        target.to_string()
    }
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct SubagentSessionData {
    run_id: String,
    task: String,
    agent: String,
    status: String,
    thinking: String,
    inner_tools: Vec<SubagentInnerToolData>,
    output: String,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct SubagentInnerToolData {
    name: String,
    target_preview: String,
    is_error: bool,
}

fn format_subagent_results(
    tasks: Vec<AgentRunTask>,
    results: Vec<Result<SubagentResult, String>>,
    lanes: &[CompletedSubagentLane],
) -> String {
    let sessions: Vec<SubagentSessionData> = tasks
        .into_iter()
        .zip(results)
        .zip(lanes)
        .map(|((task, result), lane)| match result {
            Ok(res) => {
                let mut thinking = String::new();
                for think_msg in &res.thinking {
                    if let AgentMessage::Custom { payload, .. } = think_msg {
                        if let Some(text) = payload.get("text").and_then(serde_json::Value::as_str)
                        {
                            thinking.push_str(text);
                            thinking.push_str("\n\n");
                        }
                    }
                }

                let inner_tools = res
                    .inner_tools
                    .into_iter()
                    .map(|tool| SubagentInnerToolData {
                        name: tool.name.clone(),
                        target_preview: tool_target_preview(&tool.name, &tool.arguments),
                        is_error: tool.is_error,
                    })
                    .collect();

                SubagentSessionData {
                    run_id: lane.run_id.clone(),
                    task: task.task,
                    agent: task.agent,
                    status: if res.error.is_some() {
                        "Failed".to_string()
                    } else {
                        "Done".to_string()
                    },
                    thinking: thinking.trim().to_string(),
                    inner_tools,
                    output: res.output,
                }
            }
            Err(error) => SubagentSessionData {
                run_id: lane.run_id.clone(),
                task: task.task,
                agent: task.agent,
                status: "Failed".to_string(),
                thinking: String::new(),
                inner_tools: Vec::new(),
                output: error,
            },
        })
        .collect();

    serde_json::to_string(&sessions)
        .unwrap_or_else(|e| format!("Failed to serialize subagent results: {}", e))
}
