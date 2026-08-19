//! Unified agent runtime backed by the harness.
//!
//! [`UnifiedAgent`] is the single agent runtime combining harness durability
//! with provider streaming and tool execution.

use crate::compaction::{compact_messages_to_token_budget, should_auto_compact};
use crate::config::AgentConfig;
use crate::error::AgentError;
use crate::events::AgentEvent;
use crate::harness::{
    AgentHarness, HarnessEventHub, HookRegistry, JsonlStore, ProcedureError, ProvisionedEntry,
    QueueKind, Reducer, SessionStore,
};
use crate::provider::ProviderRouter;
use crate::tool_dispatcher::ToolDispatcher;
use crate::types::{
    AgentMessage, AgentToolDefinition, AgentToolResult, ImageAttachment, TokenUsage,
    ToolExecutionMode,
};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use threadlane_provider::openai::ToolCall;
use threadlane_provider::router::{PayloadFormat, ProviderClient};
use tokio::sync::{broadcast, Mutex};

pub use crate::types::TurnState;

/// The single, unified agent runtime.
pub struct UnifiedAgent {
    harness: AgentHarness<JsonlStore>,
    pub tool_dispatcher: ToolDispatcher,
    provider_client: ProviderClient,
    provider_router: ProviderRouter,
    /// In-memory turn state (model, messages, tools). The durable state is
    /// in the harness journal; this is a working copy for the current turn.
    pub turn: Arc<Mutex<TurnState>>,
    config: AgentConfig,
    pub api_key: String,
    pub account_id: Option<String>,
    pub session_id: String,
    pub work_dir: Option<PathBuf>,
    pub event_tx: broadcast::Sender<AgentEvent>,
    pub hook_registry: HookRegistry,
    steering_queue: Vec<AgentMessage>,
    follow_up_queue: Vec<AgentMessage>,
    stream_rules: Vec<(crate::rules::StreamRule, regex::Regex)>,
    prompt_cache_key: Option<String>,
    allowed_tool_names: Option<HashSet<String>>,
    provider_trace_recorder: Option<crate::provider::ProviderTraceRecorder>,
    message_recorder: Option<crate::provider::AssistantMessageRecorder>,
}

impl UnifiedAgent {
    pub fn new(
        api_key: impl Into<String>,
        account_id: Option<String>,
        model: impl Into<String>,
        session_file: &Path,
        config: AgentConfig,
    ) -> Result<Self, AgentError> {
        let api_key: String = api_key.into();
        let model = model.into();
        // Ensure the session file and its parent directory exist.
        if let Some(parent) = session_file.parent() {
            std::fs::create_dir_all(parent).ok();
        }
        if !session_file.exists() {
            std::fs::File::create(session_file)
                .map_err(|e| AgentError::Session(format!("create session file: {e}")))?;
        }
        let store = JsonlStore::open(session_file)
            .map_err(|e| AgentError::Session(format!("open session journal: {e}")))?;
        let (event_tx, _) = broadcast::channel(config.event_channel_capacity);
        let harness = AgentHarness::new(store);
        let hooks = harness.hooks().clone();
        let tool_dispatcher = ToolDispatcher::new(event_tx.clone(), hooks.clone());
        let provider_client = ProviderClient::new(api_key.clone(), account_id.clone());
        let turn = Arc::new(Mutex::new(TurnState {
            system_prompt: config.default_system_prompt.clone(),
            messages: Vec::new(),
            model,
            reasoning_effort: Default::default(),
            tools: Vec::new(),
        }));

        Ok(Self {
            harness,
            tool_dispatcher,
            provider_client,
            provider_router: ProviderRouter::new(),
            turn,
            config,
            api_key,
            account_id,
            session_id: String::new(),
            work_dir: None,
            event_tx,
            hook_registry: hooks,
            steering_queue: Vec::new(),
            follow_up_queue: Vec::new(),
            stream_rules: Vec::new(),
            prompt_cache_key: None,
            allowed_tool_names: None,
            provider_trace_recorder: None,
            message_recorder: None,
        })
    }

    // ── Public API ────────────────────────────────────────────────────

    pub fn subscribe(&self) -> broadcast::Receiver<AgentEvent> {
        self.event_tx.subscribe()
    }

    pub fn steer(&mut self, message: AgentMessage) {
        self.steering_queue.push(message);
    }

    pub fn follow_up(&mut self, message: AgentMessage) {
        self.follow_up_queue.push(message);
    }

    pub async fn prompt(&mut self, text: &str) {
        self.prompt_message(AgentMessage::user(text, Vec::new()))
            .await;
    }

    pub async fn run_accepted(&mut self, run_id: &str, lane: &str, accepted_through_seq: u64) {
        assert!(!run_id.is_empty(), "accepted run id must not be empty");
        assert_eq!(lane, "main", "unified provider executor currently serves main lane");
        assert!(accepted_through_seq > 0, "accepted run must name a committed prefix");
        let _ = self.event_tx.send(AgentEvent::AgentStart);
        self.run_turns().await;
        let _ = self.event_tx.send(AgentEvent::AgentEnd {
            usage: TokenUsage::default(),
        });
    }

    pub async fn prompt_message(&mut self, message: AgentMessage) {
        // PromptProcedure records run intent, the user entry, and the first
        // step attempt before provider work. The provider context is then
        // projected from those canonical records.
        let run_id = format!("foreground-{}", self.harness.store().next_sequence());
        if let Err(error) = self.harness.accept_prompt(&run_id, message) {
            let _ = self.event_tx.send(AgentEvent::AgentError {
                error: format!("failed to accept prompt before provider work: {error}"),
            });
            return;
        }
        if let Err(error) = self.harness.drive_to_completion() {
            let _ = self.event_tx.send(AgentEvent::AgentError {
                error: format!("failed to commit prompt before provider work: {error}"),
            });
            return;
        }

        let accepted_through_seq = self.harness.store().next_sequence().saturating_sub(1);
        self.run_accepted(&run_id, "main", accepted_through_seq).await;
    }

    pub fn set_credentials(&mut self, api_key: String, account_id: Option<String>) {
        self.api_key = api_key;
        self.account_id = account_id;
        self.provider_client = ProviderClient::new(self.api_key.clone(), self.account_id.clone());
    }

    pub fn set_prompt_cache_key(&mut self, key: Option<String>) {
        self.prompt_cache_key = key;
    }

    pub fn prompt_cache_enabled(&self) -> bool {
        self.prompt_cache_key.is_some()
    }

    pub fn set_provider_trace_recorder(
        &mut self,
        recorder: Option<crate::provider::ProviderTraceRecorder>,
    ) {
        self.provider_trace_recorder = recorder;
    }

    pub fn set_message_recorder(
        &mut self,
        recorder: Option<crate::provider::AssistantMessageRecorder>,
    ) {
        self.message_recorder = recorder;
    }

    pub fn set_model_roles(&mut self, roles: crate::types::ModelRoles) {
        self.config.model_roles = roles;
    }

    pub fn model_roles(&self) -> &crate::types::ModelRoles {
        &self.config.model_roles
    }

    pub fn provider_client(&self) -> &ProviderClient {
        &self.provider_client
    }

    pub fn config(&self) -> &AgentConfig {
        &self.config
    }

    /// Returns a clone of the in-memory turn state (for subagent context).
    pub fn state_clone(&self) -> Arc<Mutex<TurnState>> {
        self.turn.clone()
    }

    /// Returns a snapshot of the current turn state.
    pub async fn get_state(&self) -> TurnState {
        self.turn.lock().await.clone()
    }

    /// Replace the in-memory provider working copy from the canonical active
    /// event-log projection. The system prompt stays runtime-owned because it
    /// is captured separately in `RunContextCaptured`.
    pub async fn projected_messages(&self) -> Result<Vec<AgentMessage>, AgentError> {
        let context = self
            .harness
            .store()
            .model_context("main")
            .map_err(|error| AgentError::Session(error.to_string()))?;
        let system_prompt = self.turn.lock().await.system_prompt.clone();
        Ok(std::iter::once(AgentMessage::System {
            content: system_prompt,
        })
        .chain(context.messages())
        .collect())
    }

    pub async fn sync_turn_from_model_context(&self) -> Result<(), AgentError> {
        let messages = self.projected_messages().await?;
        let mut turn = self.turn.lock().await;
        turn.messages = messages;
        Ok(())
    }

    /// Read the durable transcript projection for UI reconciliation and audit
    /// views. It must not be used to build a provider request.
    pub fn transcript_projection(
        &self,
    ) -> crate::harness::TranscriptProjection {
        self.harness.store().transcript("main")
    }

    pub async fn compact_history(
        &self,
        options: Option<crate::compaction::CompactionOptions>,
    ) -> bool {
        let mut turn = self.turn.lock().await;
        let compacted = match options {
            Some(opts) => crate::compaction::compact_messages(&turn.messages, &opts),
            None => {
                let by_tokens = compact_messages_to_token_budget(
                    &turn.messages,
                    self.config.auto_compaction_keep_recent_tokens,
                );
                if by_tokens.len() == turn.messages.len() {
                    crate::compaction::compact_messages(
                        &turn.messages,
                        &crate::compaction::CompactionOptions::default(),
                    )
                } else {
                    by_tokens
                }
            }
        };
        let changed = compacted.len() != turn.messages.len();
        turn.messages = compacted;
        changed
    }

    /// Auto-compacts if the turn history exceeds the token threshold.
    pub async fn auto_compact_history(&self) -> bool {
        let mut turn = self.turn.lock().await;
        if !should_auto_compact(&turn.messages, &self.config) {
            return false;
        }
        let compacted = compact_messages_to_token_budget(
            &turn.messages,
            self.config.auto_compaction_keep_recent_tokens,
        );
        let changed = compacted.len() != turn.messages.len();
        turn.messages = compacted;
        changed
    }

    /// Builds provider payloads for testing (delegates to the router).
    pub async fn build_api_payloads(&self) -> (serde_json::Value, serde_json::Value) {
        let mut turn = self.turn.lock().await.clone();
        if let Ok(messages) = self.projected_messages().await {
            turn.messages = messages;
        }
        let tools: Vec<_> = self.configured_tool_definitions();
        let chat = self.provider_router.build_payload(
            PayloadFormat::ChatCompletions,
            &turn,
            &tools,
            self.prompt_cache_key.as_deref(),
        );
        let codex = self.provider_router.build_payload(
            PayloadFormat::Codex,
            &turn,
            &tools,
            self.prompt_cache_key.as_deref(),
        );
        (chat, codex)
    }

    pub fn register_tool_executor(
        &mut self,
        executor: Arc<dyn crate::tool_executor::ToolExecutor>,
    ) -> Result<(), AgentError> {
        self.tool_dispatcher.register_tool_executor(executor)
    }

    pub fn configured_tool_definitions(&self) -> Vec<AgentToolDefinition> {
        self.tool_dispatcher.configured_tool_definitions()
    }

    pub fn set_allowed_tool_names(&mut self, names: Option<HashSet<String>>) {
        self.allowed_tool_names = names.clone();
        self.tool_dispatcher.allowed_tool_names = names;
    }

    pub fn set_stream_rules(&mut self, rules: Vec<crate::rules::StreamRule>) {
        self.stream_rules = rules
            .into_iter()
            .filter_map(|r| regex::Regex::new(&r.pattern).ok().map(|re| (r, re)))
            .collect();
    }

    pub fn tool_executor_count(&self) -> usize {
        self.tool_dispatcher.tool_executor_count()
    }

    /// Syncs the agent state into the dispatcher for tool execution.
    fn synced_dispatcher(&self) -> ToolDispatcher {
        let mut td = self.tool_dispatcher.clone();
        td.tool_execution_mode = ToolExecutionMode::Parallel;
        td.work_dir = self.work_dir.clone();
        td.session_id = self.session_id.clone();
        td.allowed_tool_names = self.allowed_tool_names.clone();
        td
    }

    // ── Turn loop ─────────────────────────────────────────────────────

    async fn run_turns(&mut self) {
        // The durable event log is authoritative at every provider boundary.
        // In-memory state only holds the active turn's transient stream data.
        if let Err(error) = self.sync_turn_from_model_context().await {
            let _ = self.event_tx.send(AgentEvent::AgentError {
                error: format!("failed to construct canonical model context: {error}"),
            });
            return;
        }
        let tool_dispatcher = self.synced_dispatcher();
        let mut driver = crate::turn_driver::TurnDriver {
            turn: self.turn.clone(),
            provider_client: self.provider_client.clone(),
            provider_router: self.provider_router.clone(),
            prompt_cache_key: self.prompt_cache_key.clone(),
            tool_dispatcher,
            config: self.config.clone(),
            event_tx: self.event_tx.clone(),
            harness_event_hub: self.harness.events().clone(),
            provider_trace_recorder: self.provider_trace_recorder.clone(),
            message_recorder: self.message_recorder.clone(),
            model_context_refresh: Some(Arc::new({
                let harness = self.harness.store().clone();
                move |turn| {
                    let harness = harness.clone();
                    Box::pin(async move {
                        let projection = harness
                            .model_context("main")
                            .map_err(|error| error.to_string())?;
                        turn.lock().await.messages = projection.messages();
                        Ok(())
                    })
                }
            })),
            stream_rules: self.stream_rules.clone(),
            steering_queue: &mut self.steering_queue,
            follow_up_queue: &mut self.follow_up_queue,
        };
        driver.run_turns().await;
    }

    // ── Harness accessors ─────────────────────────────────────────────

    pub fn harness(&self) -> &AgentHarness<JsonlStore> {
        &self.harness
    }

    pub fn harness_mut(&mut self) -> &mut AgentHarness<JsonlStore> {
        &mut self.harness
    }

    pub fn harness_event_hub(&self, path: &Path) -> HarnessEventHub {
        use std::collections::HashMap;
        static HUBS: std::sync::OnceLock<std::sync::Mutex<HashMap<PathBuf, HarnessEventHub>>> =
            std::sync::OnceLock::new();
        let hubs = HUBS.get_or_init(|| std::sync::Mutex::new(HashMap::new()));
        let mut hubs = hubs.lock().unwrap_or_else(|e| e.into_inner());
        hubs.entry(path.to_path_buf())
            .or_insert_with(|| HarnessEventHub::new(256))
            .clone()
    }

    /// Drives pending harness effects to completion.
    pub fn drive_harness(&mut self) -> Result<(), ProcedureError> {
        self.harness
            .drive_to_completion()
            .map_err(ProcedureError::Effects)
    }

    /// Enqueues a message into the harness durability queue for the main lane.
    pub fn enqueue_harness_queue(
        &mut self,
        queue: QueueKind,
        content: String,
        images: Vec<ImageAttachment>,
    ) -> Result<String, String> {
        let state = Reducer::reduce(self.harness.store()).map_err(|error| error.to_string())?;
        let lane = state
            .lane("main")
            .ok_or_else(|| "main harness lane is unavailable".to_string())?;
        let parent_id = lane.leaf_id.clone();
        let seq = self.harness.store().entries().len() as u64 + 1;
        let entry_id = format!("queued-{seq}");
        self.harness
            .enqueue_unbound(
                queue,
                ProvisionedEntry {
                    id: entry_id.clone(),
                    parent_id,
                    message: AgentMessage::user(content, images),
                    surface_op: crate::harness::SurfaceOperation::Append,
                },
            )
            .map_err(|error| error.to_string())?;
        self.harness
            .drive_to_completion()
            .map_err(|error| error.to_string())?;
        Ok(entry_id)
    }

    /// Consumes queued entries of the specified kind from the harness main lane.
    pub fn consume_harness_queue(&mut self, queue: QueueKind) -> Result<(), String> {
        let state = Reducer::reduce(self.harness.store()).map_err(|error| error.to_string())?;
        let queued = state
            .lane("main")
            .map(|lane| {
                lane.queued
                    .iter()
                    .filter(|entry| entry.queue == queue)
                    .cloned()
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        for entry in queued {
            self.harness
                .consume_unbound(&entry.target.id)
                .map_err(|error| error.to_string())?;
        }
        self.harness
            .drive_to_completion()
            .map_err(|error| error.to_string())?;
        Ok(())
    }

    /// Cancels a queued entry in the harness by entry ID.
    pub fn cancel_harness_queue_entry(&mut self, entry_id: &str) -> Result<(), String> {
        self.harness
            .cancel_unbound(entry_id)
            .map_err(|error| error.to_string())?;
        self.harness
            .drive_to_completion()
            .map_err(|error| error.to_string())?;
        Ok(())
    }

    /// Replays already-intended safe tools.
    pub async fn execute_tools_for_replay(&self, tool_calls: &[ToolCall]) -> Vec<AgentToolResult> {
        self.synced_dispatcher()
            .execute_tools_for_replay(tool_calls)
            .await
    }

    /// Sets reasoning effort for provider calls.
    pub async fn set_reasoning_effort(&self, effort: crate::types::ReasoningEffort) {
        self.turn.lock().await.reasoning_effort = effort;
    }

    /// Resumes provider/tool execution from the current durable conversation
    /// without appending a duplicate user prompt.
    pub async fn resume_pending_turn(&mut self) {
        let _ = self.event_tx.send(AgentEvent::AgentStart);
        self.run_turns().await;
        let _ = self.event_tx.send(AgentEvent::AgentEnd {
            usage: TokenUsage::default(),
        });
    }

    /// Fetches a deferred response.
    pub async fn fetch_deferred(
        &self,
        model: &str,
        handle_id: &str,
    ) -> Result<threadlane_provider::DeferredResponse, String> {
        self.provider_client.fetch_deferred(model, handle_id).await
    }

    /// Cancels a deferred response.
    pub async fn cancel_deferred(&self, model: &str, handle_id: &str) -> Result<(), String> {
        self.provider_client.cancel_deferred(model, handle_id).await
    }

    /// Sets the system prompt for subsequent turns.
    pub async fn set_system_prompt(&self, prompt: String) {
        let mut turn = self.turn.lock().await;
        turn.system_prompt = prompt.clone();
        if let Some(AgentMessage::System { content }) = turn.messages.first_mut() {
            *content = prompt;
        } else {
            turn.messages
                .insert(0, AgentMessage::System { content: prompt });
        }
    }

    /// Runs any pending steer messages as a prompt.
    pub async fn run_steer(&mut self) {
        if !self.steering_queue.is_empty() {
            let items: Vec<_> = self.steering_queue.drain(..).collect();
            {
                let mut turn = self.turn.lock().await;
                turn.messages.extend(items);
            }
            self.run_turns().await;
        }
    }

    /// Runs any pending follow-up messages as a prompt.
    pub async fn run_follow_up(&mut self) {
        if !self.follow_up_queue.is_empty() {
            let items: Vec<_> = self.follow_up_queue.drain(..).collect();
            {
                let mut turn = self.turn.lock().await;
                turn.messages.extend(items);
            }
            self.run_turns().await;
        }
    }

    /// Executes tools (convenience pass-through to tool_dispatcher).
    pub async fn execute_tools(&self, calls: &[ToolCall]) -> Vec<AgentToolResult> {
        self.synced_dispatcher().execute_tools(calls).await
    }
}
