//! Single, unified agent runtime.
//!
//! [`AgentRuntime`] is the sole agent execution engine. It owns provider
//! routing, tool dispatch, the harness durability layer, and the turn loop.
//! It replaces the previous split between [`UnifiedAgent`] and
//! [`ProviderRunExecutor`].

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
    ToolExecutionMode, TurnState,
};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use threadlane_provider::openai::ToolCall;
use threadlane_provider::router::{PayloadFormat, ProviderClient};
use tokio::sync::{broadcast, Mutex};

/// Unified source for model-visible context.
///
/// Durable callers should provide the canonical session projection. The
/// legacy closure aliases below remain available for compatibility, but new
/// integrations should use this single source instead of coordinating several
/// precedence-based callbacks.
pub trait ModelContextSource: Send + Sync {
    fn project(&self) -> Result<Vec<AgentMessage>, String>;
}

impl<F> ModelContextSource for F
where
    F: Fn() -> Result<Vec<AgentMessage>, String> + Send + Sync,
{
    fn project(&self) -> Result<Vec<AgentMessage>, String> {
        self()
    }
}

pub type ModelContextProjector = Arc<dyn Fn() -> Vec<AgentMessage> + Send + Sync>;

/// The single, unified agent runtime.
///
/// Owns the harness (durable session store), provider routing, tool dispatch,
/// and the turn loop. This replaces both `UnifiedAgent` and
/// `ProviderRunExecutor`.
pub struct AgentRuntime {
    /// Durable session journal.
    harness: AgentHarness<JsonlStore>,
    /// Tool dispatch with hook-based routing.
    pub tool_dispatcher: ToolDispatcher,
    /// Provider client for API calls.
    provider_client: ProviderClient,
    /// Provider format router (Chat Completions vs Codex Responses).
    provider_router: ProviderRouter,
    /// In-memory working copy of turn state. The harness is authoritative;
    /// this copy is refreshed from the canonical store before each turn.
    pub turn: Arc<Mutex<TurnState>>,
    /// Agent configuration (compaction, stream rules, model roles, etc.).
    config: AgentConfig,
    /// API key for the active provider.
    pub api_key: String,
    /// Optional account ID for the active provider.
    pub account_id: Option<String>,
    /// Session identifier.
    pub session_id: String,
    /// Working directory for tool execution.
    pub work_dir: Option<PathBuf>,
    /// Event broadcast channel.
    pub event_tx: broadcast::Sender<AgentEvent>,
    /// Hook registry for before/after tool hooks.
    pub hook_registry: HookRegistry,
    /// Steering queue — high-priority prompts injected mid-turn.
    steering_queue: Vec<AgentMessage>,
    /// Follow-up queue — appends to turn after completion.
    follow_up_queue: Vec<AgentMessage>,
    /// Compiled stream rules for runtime monitoring.
    stream_rules: Vec<(crate::rules::StreamRule, regex::Regex)>,
    /// Prompt cache key for provider-side caching.
    prompt_cache_key: Option<String>,
    /// Optional allowlist of tool names.
    allowed_tool_names: Option<HashSet<String>>,
    /// Provider trace recorder (for auditing).
    provider_trace_recorder: Option<crate::provider::ProviderTraceRecorder>,
    /// Assistant message recorder (for persistence).
    message_recorder: Option<crate::provider::AssistantMessageRecorder>,
    /// Optional external model context projector. When set, this is used
    /// instead of the internal harness projection for refreshing turn state.
    model_context_source: Option<Arc<dyn ModelContextSource>>,
    model_context_projector: Option<ModelContextProjector>,
    /// Optional external model context refresh hook.
    model_context_refresh: Option<crate::provider::ModelContextRefresh>,
    /// Harness event hub for wiring durability events.
    pub harness_event_hub: HarnessEventHub,
}

impl AgentRuntime {
    // ── Construction ──────────────────────────────────────────────────

    /// Create a new runtime directly backed by an existing [`AgentHarness`].
    ///
    /// The runtime shares the harness's store, hooks, and event hub directly.
    pub fn from_harness(
        api_key: impl Into<String>,
        account_id: Option<String>,
        model: impl Into<String>,
        harness: AgentHarness<JsonlStore>,
        config: AgentConfig,
    ) -> Self {
        let api_key: String = api_key.into();
        let model = model.into();
        let (event_tx, _) = broadcast::channel(config.event_channel_capacity);
        let harness_event_hub = harness.events().clone();
        let hooks = harness.hooks().clone();
        let tool_dispatcher = ToolDispatcher::new(event_tx.clone(), hooks.clone());
        let provider_client = ProviderClient::new(api_key.clone(), account_id.clone());
        let turn = Arc::new(Mutex::new(TurnState {
            system_prompt: config.default_system_prompt.clone(),
            messages: Vec::new(),
            model,
            reasoning_effort: Default::default(),
        }));

        Self {
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
            harness_event_hub,
            hook_registry: hooks,
            steering_queue: Vec::new(),
            follow_up_queue: Vec::new(),
            stream_rules: Vec::new(),
            prompt_cache_key: None,
            allowed_tool_names: None,
            provider_trace_recorder: None,
            message_recorder: None,
            model_context_source: None,
            model_context_projector: None,
            model_context_refresh: None,
        }
    }

    /// Create a new runtime backed by the given session journal path.
    ///
    /// If `session_file` is provided, opens (or creates) a JSONL journal.
    /// Otherwise, an in-memory store is used.
    pub fn new(
        api_key: impl Into<String>,
        account_id: Option<String>,
        model: impl Into<String>,
        session_file: Option<&Path>,
        config: AgentConfig,
    ) -> Result<Self, AgentError> {
        let store = if let Some(path) = session_file {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).ok();
            }
            if !path.exists() {
                std::fs::File::create(path)
                    .map_err(|e| AgentError::Session(format!("create session file: {e}")))?;
            }
            JsonlStore::open(path)
                .map_err(|e| AgentError::Session(format!("open session journal: {e}")))?
        } else {
            // Ephemeral store backed by a temp file.
            let tmp =
                std::env::temp_dir().join(format!("threadlane-ephemeral-{}", std::process::id()));
            let _ = std::fs::create_dir_all(tmp.parent().unwrap());
            JsonlStore::open(&tmp)
                .map_err(|e| AgentError::Session(format!("open ephemeral journal: {e}")))?
        };

        let harness_event_hub = HarnessEventHub::new(config.event_channel_capacity);
        let harness = AgentHarness::with_events(store, harness_event_hub);
        Ok(Self::from_harness(api_key, account_id, model, harness, config))
    }

    // ── Model context ─────────────────────────────────────────────────

    /// Sets an external model context projector. When set, this is used
    /// instead of the internal harness projection for refreshing turn state
    /// before each turn. Useful when the caller manages the session store
    /// externally (e.g. CodingSessionHarness).
    /// Sets the unified model-context source for durable integrations.
    pub fn set_model_context_source(&mut self, source: Option<Arc<dyn ModelContextSource>>) {
        self.model_context_source = source;
    }

    /// Compatibility setter for older integrations.
    pub fn set_model_context_projector(&mut self, projector: Option<ModelContextProjector>) {
        self.model_context_projector = projector;
    }

    /// Sets an external model context refresh hook.
    pub fn set_model_context_refresh(
        &mut self,
        refresh: Option<crate::provider::ModelContextRefresh>,
    ) {
        self.model_context_refresh = refresh;
    }

    /// Refreshes the in-memory turn messages from the authoritative source.
    /// Prefers the model_context_refresh hook, then the projector closure,
    /// then falls back to the internal harness projection.
    async fn refresh_projected_messages(&self) {
        if let Some(source) = &self.model_context_source {
            if let Ok(messages) = source.project() {
                self.turn.lock().await.messages = messages;
            }
            return;
        }
        // Compatibility path for older callers. New integrations should use
        // one ModelContextSource rather than this precedence chain.
        if let Some(refresh) = &self.model_context_refresh {
            let _ = refresh(self.turn.clone()).await;
            return;
        }
        if let Some(projector) = &self.model_context_projector {
            self.turn.lock().await.messages = projector();
            return;
        }
        if let Ok(context) = self.harness.store().model_context("main") {
            let system_prompt = self.turn.lock().await.system_prompt.clone();
            let messages: Vec<AgentMessage> = std::iter::once(AgentMessage::System {
                content: system_prompt,
            })
            .chain(context.messages())
            .collect();
            self.turn.lock().await.messages = messages;
        }
    }

    /// Returns the canonical messages from the harness projection.
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

    /// Syncs the in-memory turn state from the canonical harness projection.
    pub async fn sync_turn_from_model_context(&self) -> Result<(), AgentError> {
        let messages = self.projected_messages().await?;
        let mut turn = self.turn.lock().await;
        turn.messages = messages;
        Ok(())
    }

    /// Read the durable transcript projection for UI reconciliation and audit
    /// views. Must not be used to build a provider request.
    pub fn transcript_projection(&self) -> crate::harness::TranscriptProjection {
        self.harness.store().transcript("main")
    }

    pub fn subscribe(&self) -> broadcast::Receiver<AgentEvent> {
        self.event_tx.subscribe()
    }

    pub fn model(&self) -> String {
        self.turn
            .try_lock()
            .map(|t| t.model.clone())
            .unwrap_or_else(|_| {
                self.harness
                    .store()
                    .model()
                    .unwrap_or_else(|| "gpt-4o".to_string())
            })
    }

    pub fn steer(&mut self, message: AgentMessage) {
        let seq = self.harness.store().next_sequence();
        let _ = self.harness.enqueue_unbound(
            QueueKind::Steer,
            ProvisionedEntry {
                id: format!("queued-steer-{seq}"),
                parent_id: None,
                message: message.clone(),
                surface_op: crate::harness::SurfaceOperation::Append,
            },
        );
        let _ = self.harness.drive_to_completion();
        self.steering_queue.push(message);
    }

    pub fn follow_up(&mut self, message: AgentMessage) {
        let seq = self.harness.store().next_sequence();
        let _ = self.harness.enqueue_unbound(
            QueueKind::FollowUp,
            ProvisionedEntry {
                id: format!("queued-followup-{seq}"),
                parent_id: None,
                message: message.clone(),
                surface_op: crate::harness::SurfaceOperation::Append,
            },
        );
        let _ = self.harness.drive_to_completion();
        self.follow_up_queue.push(message);
    }

    /// Records a user prompt through the harness then runs the turn loop.
    pub async fn prompt_message(&mut self, message: AgentMessage) {
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
        self.run_accepted(&run_id, "main", accepted_through_seq)
            .await;
    }

    /// Prompt shorthand (user message with no images).
    pub async fn prompt(&mut self, text: &str) {
        self.prompt_message(AgentMessage::user(text, Vec::new()))
            .await;
    }

    /// Execute a turn loop for a pre-accepted run token.
    pub async fn run_accepted(&mut self, run_id: &str, lane: &str, accepted_through_seq: u64) {
        assert!(!run_id.is_empty(), "accepted run id must not be empty");
        assert_eq!(lane, "main", "agent runtime currently serves main lane");
        assert!(
            accepted_through_seq > 0,
            "accepted run must name a committed prefix"
        );
        let _ = self.event_tx.send(AgentEvent::AgentStart);
        self.run_turns().await;
        let _ = self.event_tx.send(AgentEvent::AgentEnd {
            usage: TokenUsage::default(),
        });
    }

    /// Resumes provider/tool execution without appending a duplicate prompt.
    pub async fn resume_pending_turn(&mut self) {
        let _ = self.event_tx.send(AgentEvent::AgentStart);
        self.run_turns().await;
        let _ = self.event_tx.send(AgentEvent::AgentEnd {
            usage: TokenUsage::default(),
        });
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

    pub fn state_clone(&self) -> Arc<Mutex<TurnState>> {
        self.turn.clone()
    }

    pub async fn get_state(&self) -> TurnState {
        self.turn.lock().await.clone()
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

    pub async fn set_reasoning_effort(&self, effort: crate::types::ReasoningEffort) {
        self.turn.lock().await.reasoning_effort = effort;
    }

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

    pub async fn execute_tools_for_replay(&self, calls: &[ToolCall]) -> Vec<AgentToolResult> {
        self.synced_dispatcher()
            .execute_tools_for_replay(calls)
            .await
    }

    pub async fn execute_tools(&self, calls: &[ToolCall]) -> Vec<AgentToolResult> {
        self.synced_dispatcher().execute_tools(calls).await
    }

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

    pub async fn run_steer(&mut self) {
        if !self.steering_queue.is_empty() {
            let items: Vec<_> = self.steering_queue.drain(..).collect();
            if let Some(recorder) = self.message_recorder.as_ref() {
                for item in &items {
                    let _ = recorder(item.clone()).await;
                }
            }
            {
                let mut turn = self.turn.lock().await;
                turn.messages.extend(items);
            }
            self.run_turns().await;
        }
    }

    pub async fn run_follow_up(&mut self) {
        if !self.follow_up_queue.is_empty() {
            let items: Vec<_> = self.follow_up_queue.drain(..).collect();
            if let Some(recorder) = self.message_recorder.as_ref() {
                for item in &items {
                    let _ = recorder(item.clone()).await;
                }
            }
            {
                let mut turn = self.turn.lock().await;
                turn.messages.extend(items);
            }
            self.run_turns().await;
        }
    }

    pub async fn fetch_deferred(
        &self,
        model: &str,
        handle_id: &str,
    ) -> Result<threadlane_provider::DeferredResponse, String> {
        self.provider_client.fetch_deferred(model, handle_id).await
    }

    pub async fn cancel_deferred(&self, model: &str, handle_id: &str) -> Result<(), String> {
        self.provider_client.cancel_deferred(model, handle_id).await
    }

    // ── Harness accessors ─────────────────────────────────────────────

    pub fn harness(&self) -> &AgentHarness<JsonlStore> {
        &self.harness
    }

    pub fn harness_mut(&mut self) -> &mut AgentHarness<JsonlStore> {
        &mut self.harness
    }

    pub fn drive_harness(&mut self) -> Result<(), ProcedureError> {
        self.harness
            .drive_to_completion()
            .map_err(ProcedureError::Effects)
    }

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

    pub fn cancel_harness_queue_entry(&mut self, entry_id: &str) -> Result<(), String> {
        self.harness
            .cancel_unbound(entry_id)
            .map_err(|error| error.to_string())?;
        self.harness
            .drive_to_completion()
            .map_err(|error| error.to_string())?;
        Ok(())
    }

    // ── Internal helpers ──────────────────────────────────────────────

    fn synced_dispatcher(&self) -> ToolDispatcher {
        let mut td = self.tool_dispatcher.clone();
        td.tool_execution_mode = ToolExecutionMode::Parallel;
        td.work_dir = self.work_dir.clone();
        td.session_id = self.session_id.clone();
        td.allowed_tool_names = self.allowed_tool_names.clone();
        td
    }

    /// Runs the main turn loop.
    async fn run_turns(&mut self) {
        self.refresh_projected_messages().await;
        let tool_dispatcher = self.synced_dispatcher();
        let mut driver = crate::turn_driver::TurnDriver {
            turn: self.turn.clone(),
            provider_client: self.provider_client.clone(),
            provider_router: self.provider_router.clone(),
            prompt_cache_key: self.prompt_cache_key.clone(),
            tool_dispatcher,
            config: self.config.clone(),
            event_tx: self.event_tx.clone(),
            harness_event_hub: self.harness_event_hub.clone(),
            provider_trace_recorder: self.provider_trace_recorder.clone(),
            message_recorder: self.message_recorder.clone(),
            model_context_refresh: self.model_context_refresh.clone(),
            stream_rules: self.stream_rules.clone(),
            steering_queue: &mut self.steering_queue,
            follow_up_queue: &mut self.follow_up_queue,
        };
        driver.run_turns().await;
    }
}
