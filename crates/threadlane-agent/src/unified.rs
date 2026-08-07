//! Unified agent runtime backed by the harness.
//!
//! [`UnifiedAgent`] is the single agent runtime combining harness durability
//! with provider streaming and tool execution.

use crate::compaction::{
    compact_messages_to_token_budget, is_context_overflow_error, should_auto_compact,
};
use crate::config::AgentConfig;
use crate::error::AgentError;
use crate::events::AgentEvent;
use crate::harness::{AgentHarness, HarnessEventHub, HookRegistry, JsonlStore, ProcedureError};
use crate::journal::AgentJournal;
use crate::loop_engine::{AbortOnDrop, ProviderStepAccumulator};
use crate::provider::ProviderRouter;
use crate::rules::StreamRuleMonitor;
use crate::tool_dispatcher::ToolDispatcher;
use crate::types::{
    AgentMessage, AgentToolDefinition, AgentToolResult, TokenUsage, ToolExecutionMode,
};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use threadlane_provider::openai::{StreamEvent, ToolCall};
use threadlane_provider::router::{PayloadFormat, PayloadSource, ProviderClient};
use tokio::sync::{broadcast, mpsc, Mutex};

/// Messages and metadata for the current turn, held in memory alongside the
/// durable harness journal.
#[derive(Debug, Clone)]
pub struct TurnState {
    pub system_prompt: String,
    pub messages: Vec<AgentMessage>,
    pub model: String,
    pub reasoning_effort: crate::types::ReasoningEffort,
    pub tools: Vec<serde_json::Value>,
}

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
    /// Journal for durability. Set by CodingAgent after construction.
    pub journal: Option<Arc<dyn AgentJournal>>,
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
            journal: None,
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

    pub async fn prompt_message(&mut self, message: AgentMessage) {
        // Push the user message.
        {
            let mut turn = self.turn.lock().await;
            turn.messages.push(message);
        }

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

    /// Returns a clone of the in-memory turn state (for subagent context).
    pub fn state_clone(&self) -> Arc<Mutex<TurnState>> {
        self.turn.clone()
    }

    /// Returns a snapshot of the current turn state.
    pub async fn get_state(&self) -> TurnState {
        self.turn.lock().await.clone()
    }

    /// Compacts the turn history, keeping recent messages.
    pub async fn compact_history(
        &self,
        _options: Option<crate::compaction::CompactionOptions>,
    ) -> bool {
        let mut turn = self.turn.lock().await;
        let compacted = compact_messages_to_token_budget(
            &turn.messages,
            self.config.auto_compaction_keep_recent_tokens,
        );
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
        let turn = self.turn.lock().await;
        let state = crate::types::AgentState {
            system_prompt: String::new(),
            model: turn.model.clone(),
            reasoning_effort: Default::default(),
            tools: turn.tools.clone(),
            messages: turn.messages.clone(),
            is_streaming: false,
            pending_tool_calls: Vec::new(),
            metadata: Default::default(),
        };
        let tools: Vec<_> = self.configured_tool_definitions();
        let chat = self.provider_router.build_payload(
            PayloadFormat::ChatCompletions,
            &state,
            &tools,
            self.prompt_cache_key.as_deref(),
        );
        let codex = self.provider_router.build_payload(
            PayloadFormat::Codex,
            &state,
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
        let mut turn_number = 0;
        let mut overflow_recovery_attempted = false;

        loop {
            turn_number += 1;

            // Drain steering queue into turn state.
            if !self.steering_queue.is_empty() {
                let items: Vec<_> = self.steering_queue.drain(..).collect();
                let mut turn = self.turn.lock().await;
                turn.messages.extend(items);
            }

            // Auto-compaction.
            {
                let mut turn = self.turn.lock().await;
                if should_auto_compact(&turn.messages, &self.config) {
                    turn.messages = compact_messages_to_token_budget(
                        &turn.messages,
                        self.config.auto_compaction_keep_recent_tokens,
                    );
                }
            }

            let _ = self.event_tx.send(AgentEvent::TurnStart { turn_number });

            // --- Provider streaming ---
            let model = {
                let turn = self.turn.lock().await;
                turn.model.clone()
            };
            let (stream_tx, mut stream_rx) = mpsc::channel(100);
            let client = self.provider_client.clone();
            let pc_key = self.prompt_cache_key.clone();
            let tool_executors: Vec<_> = self
                .tool_dispatcher
                .configured_tool_definitions()
                .into_iter()
                .map(|_| ())
                .collect();
            let _dummy = tool_executors; // suppress unused warning

            let payload_source = PayloadSource::lazy(model, {
                let turn_clone = self.turn.clone();
                let router = self.provider_router.clone();
                move |format| {
                    let turn = turn_clone.clone();
                    let router = router.clone();
                    Box::pin(async move {
                        let state = {
                            let turn = turn.lock().await;
                            crate::types::AgentState {
                                system_prompt: String::new(),
                                model: turn.model.clone(),
                                reasoning_effort: Default::default(),
                                tools: turn.tools.clone(),
                                messages: turn.messages.clone(),
                                is_streaming: false,
                                pending_tool_calls: Vec::new(),
                                metadata: Default::default(),
                            }
                        };
                        router.build_payload(format, &state, &[], None)
                    })
                }
            });

            let _stream_task = AbortOnDrop::new(tokio::spawn(async move {
                client
                    .stream_chat_completion(payload_source, pc_key, stream_tx)
                    .await;
            }));

            let _ = self.event_tx.send(AgentEvent::MessageStart {
                role: "assistant".into(),
            });

            let mut current_text = String::new();
            let mut current_reasoning = String::new();
            let mut captured_tool_calls: Vec<ToolCall> = Vec::new();
            let mut provider_step = ProviderStepAccumulator::default();
            let mut monitor = StreamRuleMonitor::new(self.stream_rules.clone(), &self.config);

            while let Some(evt) = stream_rx.recv().await {
                let _ = provider_step.push(&evt);
                match evt {
                    StreamEvent::ContentToken(token) => {
                        current_text.push_str(&token);
                        if monitor.push_chunk(&token).is_some() {
                            break;
                        }
                        let _ = self.event_tx.send(AgentEvent::MessageUpdate {
                            text_delta: Some(token),
                            reasoning_delta: None,
                            tool_call_name: None,
                        });
                    }
                    StreamEvent::ReasoningToken(token) => {
                        current_reasoning.push_str(&token);
                        let _ = self.event_tx.send(AgentEvent::MessageUpdate {
                            text_delta: None,
                            reasoning_delta: Some(token),
                            tool_call_name: None,
                        });
                    }
                    StreamEvent::ToolCallStart { name, .. } => {
                        let _ = self.event_tx.send(AgentEvent::MessageUpdate {
                            text_delta: None,
                            reasoning_delta: None,
                            tool_call_name: Some(name),
                        });
                    }
                    StreamEvent::ToolCallArgsDelta { .. } => {}
                    StreamEvent::Finished { tool_calls, .. } => {
                        captured_tool_calls = tool_calls;
                        break;
                    }
                    StreamEvent::Error(err) => {
                        if !overflow_recovery_attempted && is_context_overflow_error(&err) {
                            let mut turn = self.turn.lock().await;
                            turn.messages = compact_messages_to_token_budget(
                                &turn.messages,
                                self.config.auto_compaction_keep_recent_tokens,
                            );
                            overflow_recovery_attempted = true;
                            continue;
                        }
                        let _ = self.event_tx.send(AgentEvent::AgentError { error: err });
                        return;
                    }
                }
            }

            // Record assistant message in turn state.
            let assistant_msg = AgentMessage::Assistant {
                content: if current_text.is_empty() {
                    None
                } else {
                    Some(current_text)
                },
                tool_calls: if captured_tool_calls.is_empty() {
                    None
                } else {
                    Some(captured_tool_calls.clone())
                },
                stop_reason: None,
                deferred_handle: None,
            };

            if !current_reasoning.trim().is_empty() {
                let thinking = AgentMessage::Custom {
                    custom_type: "thinking".into(),
                    payload: serde_json::json!({ "text": current_reasoning }),
                };
                if let Some(journal) = &self.journal {
                    let _ = journal.record_assistant_message(thinking.clone()).await;
                }
                self.turn.lock().await.messages.push(thinking);
            }

            if let Some(journal) = &self.journal {
                let _ = journal
                    .record_assistant_message(assistant_msg.clone())
                    .await;
            }
            self.turn.lock().await.messages.push(assistant_msg.clone());

            let _ = self.event_tx.send(AgentEvent::MessageEnd {
                message: assistant_msg,
            });

            if captured_tool_calls.is_empty() {
                let _ = self.event_tx.send(AgentEvent::TurnEnd {
                    turn_number,
                    tool_results: Vec::new(),
                });
                if !self.follow_up_queue.is_empty() {
                    let items: Vec<_> = self.follow_up_queue.drain(..).collect();
                    self.turn.lock().await.messages.extend(items);
                    continue;
                }
                break;
            }

            // Execute tools.
            let tool_results = self
                .synced_dispatcher()
                .execute_tools(&captured_tool_calls)
                .await;

            // Append tool results to turn state.
            {
                let mut turn = self.turn.lock().await;
                for r in &tool_results {
                    let msg = AgentMessage::Tool {
                        tool_call_id: r.tool_call_id.clone(),
                        name: r.name.clone(),
                        content: r.content.clone(),
                        is_error: r.is_error,
                        terminate: r.terminate,
                    };
                    if let Some(journal) = &self.journal {
                        let _ = journal.record_tool_message(msg.clone()).await;
                    }
                    turn.messages.push(msg);
                }
            }

            let _ = self.event_tx.send(AgentEvent::TurnEnd {
                turn_number,
                tool_results: tool_results.clone(),
            });

            if tool_results.iter().any(|r| r.terminate) {
                break;
            }
        }
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
            .map_err(|e| ProcedureError::Effects(e))
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

    /// Resumes a pending turn (no-op in unified agent).
    pub async fn resume_pending_turn(&mut self) {}

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
