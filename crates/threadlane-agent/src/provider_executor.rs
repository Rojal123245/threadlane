//! Transient provider execution engine.
//!
//! [`ProviderRunExecutor`] owns provider routing, streaming state, tool
//! dispatch, and transient event emission. It does NOT own any durable storage,
//! harnesses, or prompt persistence.

use crate::compaction::{compact_messages_to_token_budget, should_auto_compact};
use crate::config::AgentConfig;
use crate::error::AgentError;
use crate::events::AgentEvent;
use crate::harness::{AcceptedRun, HarnessEventHub, HookRegistry};
use crate::provider::ProviderRouter;
use crate::tool_dispatcher::ToolDispatcher;
use crate::types::{
    AgentMessage, AgentToolDefinition, AgentToolResult, TokenUsage,
    ToolExecutionMode, TurnState,
};
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;

pub type ModelContextProjector = Arc<dyn Fn() -> Vec<AgentMessage> + Send + Sync>;
use threadlane_provider::openai::ToolCall;
use threadlane_provider::router::{PayloadFormat, ProviderClient};
use tokio::sync::{broadcast, Mutex};

/// Pure provider execution engine owning provider routing, streaming, tool
/// dispatch, and transient events without durable storage coupling.
pub struct ProviderRunExecutor {
    pub tool_dispatcher: ToolDispatcher,
    pub provider_client: ProviderClient,
    pub provider_router: ProviderRouter,
    /// Working copy of turn state for the active request.
    pub turn: Arc<Mutex<TurnState>>,
    pub config: AgentConfig,
    pub api_key: String,
    pub account_id: Option<String>,
    pub session_id: String,
    pub work_dir: Option<PathBuf>,
    pub event_tx: broadcast::Sender<AgentEvent>,
    pub harness_event_hub: HarnessEventHub,
    pub hook_registry: HookRegistry,
    pub steering_queue: Vec<AgentMessage>,
    pub follow_up_queue: Vec<AgentMessage>,
    pub stream_rules: Vec<(crate::rules::StreamRule, regex::Regex)>,
    pub prompt_cache_key: Option<String>,
    pub allowed_tool_names: Option<HashSet<String>>,
    pub provider_trace_recorder: Option<crate::provider::ProviderTraceRecorder>,
    pub message_recorder: Option<crate::provider::AssistantMessageRecorder>,
    pub model_context_projector: Option<ModelContextProjector>,
    pub model_context_refresh: Option<crate::provider::ModelContextRefresh>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[tokio::test]
    async fn projector_replaces_stale_transient_messages() {
        let mut executor = ProviderRunExecutor::new("", None, "test-model", AgentConfig::default());
        executor.turn.lock().await.messages = vec![AgentMessage::user("stale", Vec::new())];
        let calls = Arc::new(AtomicUsize::new(0));
        let calls_for_projector = calls.clone();
        executor.set_model_context_projector(Some(Arc::new(move || {
            calls_for_projector.fetch_add(1, Ordering::SeqCst);
            vec![AgentMessage::user("canonical", Vec::new())]
        })));

        executor.refresh_projected_messages().await;

        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(executor.turn.lock().await.messages,
            vec![AgentMessage::user("canonical", Vec::new())]);
    }
}

impl ProviderRunExecutor {
    pub fn new(
        api_key: impl Into<String>,
        account_id: Option<String>,
        model: impl Into<String>,
        config: AgentConfig,
    ) -> Self {
        let api_key: String = api_key.into();
        let model = model.into();
        let (event_tx, _) = broadcast::channel(config.event_channel_capacity);
        let harness_event_hub = HarnessEventHub::new(config.event_channel_capacity);
        let hooks = HookRegistry::default();
        let tool_dispatcher = ToolDispatcher::new(event_tx.clone(), hooks.clone());
        let provider_client = ProviderClient::new(api_key.clone(), account_id.clone());
        let turn = Arc::new(Mutex::new(TurnState {
            system_prompt: config.default_system_prompt.clone(),
            messages: Vec::new(),
            model,
            reasoning_effort: Default::default(),
            tools: Vec::new(),
        }));

        Self {
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
            model_context_projector: None,
            model_context_refresh: None,
        }
    }

    pub fn set_model_context_projector(&mut self, projector: Option<ModelContextProjector>) {
        self.model_context_projector = projector;
    }

    pub fn set_model_context_refresh(&mut self, refresh: Option<crate::provider::ModelContextRefresh>) {
        self.model_context_refresh = refresh;
    }

    async fn refresh_projected_messages(&self) {
        if let Some(refresh) = &self.model_context_refresh {
            let _ = refresh(self.turn.clone()).await;
            return;
        }
        let Some(projector) = &self.model_context_projector else {
            return;
        };
        let messages = projector();
        self.turn.lock().await.messages = messages;
    }

    pub fn subscribe(&self) -> broadcast::Receiver<AgentEvent> {
        self.event_tx.subscribe()
    }

    pub fn steer(&mut self, message: AgentMessage) {
        self.steering_queue.push(message);
    }

    pub fn follow_up(&mut self, message: AgentMessage) {
        self.follow_up_queue.push(message);
    }

    /// Execute a turn loop for a pre-accepted run token.
    pub async fn execute_accepted(&mut self, accepted: &AcceptedRun) {
        assert!(!accepted.run_id.is_empty(), "accepted run id must not be empty");
        assert_eq!(accepted.lane, "main", "provider executor serves main lane");
        assert!(accepted.accepted_through_seq > 0, "accepted run must name a committed prefix");
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

    fn synced_dispatcher(&self) -> ToolDispatcher {
        let mut td = self.tool_dispatcher.clone();
        td.tool_execution_mode = ToolExecutionMode::Parallel;
        td.work_dir = self.work_dir.clone();
        td.session_id = self.session_id.clone();
        td.allowed_tool_names = self.allowed_tool_names.clone();
        td
    }

    pub async fn run_turns(&mut self) {
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

    pub async fn execute_tools_for_replay(
        &self,
        calls: &[ToolCall],
    ) -> Vec<AgentToolResult> {
        let dispatcher = self.synced_dispatcher();
        dispatcher.execute_tools_for_replay(calls).await
    }

    /// Sets reasoning effort for provider calls.
    pub async fn set_reasoning_effort(&self, effort: crate::types::ReasoningEffort) {
        self.turn.lock().await.reasoning_effort = effort;
    }

    /// Resumes provider/tool execution without appending a duplicate user prompt.
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

    /// Builds provider payloads for testing from the current transient copy.
    /// Production request construction must refresh that copy from the
    /// canonical model surface before each step.
    pub async fn build_api_payloads(&self) -> (serde_json::Value, serde_json::Value) {
        let turn = self.turn.lock().await.clone();
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
}
