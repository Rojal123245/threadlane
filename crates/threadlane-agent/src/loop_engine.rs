use crate::compaction::{
    compact_messages_to_token_budget, compaction_summary_text, is_context_overflow_error,
    should_auto_compact,
};
use crate::config::AgentConfig;
use crate::error::AgentError;
use crate::events::AgentEvent;
use crate::harness::HookRegistry;
use crate::journal::AgentJournal;
use crate::provider::ProviderRouter;
use crate::queue::PendingMessageQueue;
use crate::tool_executor::ToolExecutor;
use crate::types::{
    AgentMessage, AgentState, AgentToolDefinition, AgentToolResult, QueueMode, TokenUsage,
    ToolExecutionMode,
};
use serde_json::Value;
use std::collections::HashSet;
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;
use threadlane_provider::openai::{clamp_prompt_cache_key, ProviderUsage, StreamEvent, ToolCall};
use threadlane_provider::router::{PayloadFormat, PayloadSource, ProviderClient};
use threadlane_tools::{get_available_tools, get_codex_tools};
use tokio::sync::{broadcast, mpsc, Mutex};

pub(crate) struct AbortOnDrop<T> {
    handle: Option<tokio::task::JoinHandle<T>>,
}

impl<T> AbortOnDrop<T> {
    pub(crate) fn new(handle: tokio::task::JoinHandle<T>) -> Self {
        Self {
            handle: Some(handle),
        }
    }

    pub(crate) async fn join(mut self) -> Result<T, tokio::task::JoinError> {
        let result = self.handle.as_mut().expect("task handle missing").await;
        self.handle = None;
        result
    }
}

impl<T> Drop for AbortOnDrop<T> {
    fn drop(&mut self) {
        if let Some(handle) = &self.handle {
            handle.abort();
        }
    }
}

fn normalized_tool_call_id(id: &str, empty_index: usize) -> String {
    if id.is_empty() {
        format!("call_{empty_index}")
    } else {
        id.to_string()
    }
}

/// Removes an assistant tool-call turn that was interrupted before every call
/// received a tool result. Provider APIs reject replaying such incomplete turns.
pub fn repair_interrupted_tool_turn(messages: &mut Vec<AgentMessage>) -> bool {
    let mut index = 0;
    while index < messages.len() {
        let AgentMessage::Assistant {
            tool_calls: Some(tool_calls),
            ..
        } = &messages[index]
        else {
            index += 1;
            continue;
        };
        if tool_calls.is_empty() {
            index += 1;
            continue;
        }

        let expected_ids: HashSet<String> = tool_calls
            .iter()
            .enumerate()
            .map(|(idx, call)| normalized_tool_call_id(&call.id, idx))
            .collect();
        let mut completed_ids = HashSet::new();
        let mut next = index + 1;
        let mut tool_index = 0;
        while let Some(AgentMessage::Tool { tool_call_id, .. }) = messages.get(next) {
            let id = normalized_tool_call_id(tool_call_id, tool_index);
            tool_index += 1;
            completed_ids.insert(id);
            next += 1;
        }

        if expected_ids.is_subset(&completed_ids) {
            index = next;
            continue;
        }

        let truncate_at = index.checked_sub(1).filter(|previous| {
            matches!(
                &messages[*previous],
                AgentMessage::Custom { custom_type, .. } if custom_type == "thinking"
            )
        });
        messages.truncate(truncate_at.unwrap_or(index));
        return true;
    }
    false
}

fn token_usage_from_provider(usage: ProviderUsage) -> TokenUsage {
    TokenUsage {
        input_tokens: usage.input_tokens,
        output_tokens: usage.output_tokens,
        cache_read_tokens: usage.cache_read_tokens,
        cache_write_tokens: usage.cache_write_tokens,
        total_tokens: usage.total_tokens,
    }
}

#[derive(Debug, Clone)]
pub struct ProviderStepResult {
    pub text: String,
    pub reasoning: String,
    pub tool_calls: Vec<ToolCall>,
    pub usage: TokenUsage,
}

#[derive(Default)]
pub struct ProviderStepAccumulator {
    text: String,
    reasoning: String,
    result: Option<ProviderStepResult>,
}

impl ProviderStepAccumulator {
    pub fn push(&mut self, event: &StreamEvent) -> Result<Option<ProviderStepResult>, String> {
        match event {
            StreamEvent::ContentToken(token) => self.text.push_str(token),
            StreamEvent::ReasoningToken(token) => self.reasoning.push_str(token),
            StreamEvent::ToolCallStart { .. } | StreamEvent::ToolCallArgsDelta { .. } => {}
            StreamEvent::Finished { tool_calls, usage } => {
                let result = ProviderStepResult {
                    text: self.text.clone(),
                    reasoning: self.reasoning.clone(),
                    tool_calls: tool_calls.clone(),
                    usage: token_usage_from_provider(*usage),
                };
                self.result = Some(result.clone());
                return Ok(Some(result));
            }
            StreamEvent::Error(error) => return Err(error.clone()),
        }
        Ok(None)
    }

    pub fn finish(&self) -> Result<ProviderStepResult, String> {
        self.result
            .clone()
            .ok_or_else(|| "provider stream ended without a final response".into())
    }
}

pub fn convert_to_llm(messages: &[AgentMessage]) -> Vec<Value> {
    let messages = normalize_tool_call_ids(messages);
    messages
        .iter()
        .filter_map(|msg| match msg {
            AgentMessage::System { content } => Some(serde_json::json!({
                "role": "system",
                "content": content
            })),
            AgentMessage::User { content } => Some(serde_json::json!({
                "role": "user",
                "content": content
            })),
            AgentMessage::UserWithImages { content, images } => {
                let mut parts = Vec::new();
                if !content.trim().is_empty() {
                    parts.push(serde_json::json!({
                        "type": "text",
                        "text": content
                    }));
                }
                parts.extend(images.iter().map(|image| {
                    serde_json::json!({
                        "type": "image_url",
                        "image_url": {
                            "url": image.data_url,
                            "detail": "auto"
                        }
                    })
                }));
                Some(serde_json::json!({
                    "role": "user",
                    "content": parts
                }))
            }
            AgentMessage::Assistant {
                content,
                tool_calls,
                ..
            } => {
                let mut map = serde_json::Map::new();
                map.insert("role".into(), "assistant".into());
                if let Some(c) = content {
                    map.insert("content".into(), c.clone().into());
                }
                if let Some(t) = tool_calls {
                    map.insert(
                        "tool_calls".into(),
                        serde_json::to_value(t).unwrap_or_default(),
                    );
                }
                Some(Value::Object(map))
            }
            AgentMessage::Tool {
                tool_call_id,
                name,
                content,
                ..
            } => {
                let id_str = if tool_call_id.is_empty() {
                    "call_0"
                } else {
                    tool_call_id
                };
                Some(serde_json::json!({
                    "role": "tool",
                    "tool_call_id": id_str,
                    "name": name,
                    "content": content
                }))
            }
            AgentMessage::Custom { .. } => compaction_summary_text(msg).map(|summary| {
                serde_json::json!({
                    "role": "user",
                    "content": format!("<context-checkpoint>\n{summary}\n</context-checkpoint>")
                })
            }),
        })
        .collect()
}

pub fn convert_to_codex_llm(messages: &[AgentMessage]) -> (String, Vec<Value>) {
    let messages = normalize_tool_call_ids(messages);
    let mut instructions = String::new();
    let mut items = Vec::new();

    for msg in &messages {
        match msg {
            AgentMessage::System { content } => {
                if !instructions.is_empty() {
                    instructions.push_str("\n\n");
                }
                instructions.push_str(content);
            }
            AgentMessage::User { content } => {
                items.push(serde_json::json!({
                    "type": "message",
                    "role": "user",
                    "content": [{ "type": "input_text", "text": content }]
                }));
            }
            AgentMessage::UserWithImages { content, images } => {
                let mut parts = Vec::new();
                if !content.trim().is_empty() {
                    parts.push(serde_json::json!({
                        "type": "input_text",
                        "text": content
                    }));
                }
                parts.extend(images.iter().map(|image| {
                    serde_json::json!({
                        "type": "input_image",
                        "image_url": image.data_url,
                        "detail": "auto"
                    })
                }));
                items.push(serde_json::json!({
                    "type": "message",
                    "role": "user",
                    "content": parts
                }));
            }
            AgentMessage::Assistant {
                content,
                tool_calls,
                ..
            } => {
                if let Some(c) = content {
                    if !c.trim().is_empty() {
                        items.push(serde_json::json!({
                            "type": "message",
                            "role": "assistant",
                            "content": [{ "type": "output_text", "text": c }]
                        }));
                    }
                }
                if let Some(t_calls) = tool_calls {
                    for tc in t_calls {
                        items.push(serde_json::json!({
                            "type": "function_call",
                            "call_id": tc.id,
                            "name": tc.function.name,
                            "arguments": tc.function.arguments
                        }));
                    }
                }
            }
            AgentMessage::Tool {
                tool_call_id,
                content,
                ..
            } => {
                items.push(serde_json::json!({
                    "type": "function_call_output",
                    "call_id": tool_call_id,
                    "output": content
                }));
            }
            AgentMessage::Custom { .. } => {
                if let Some(summary) = compaction_summary_text(msg) {
                    items.push(serde_json::json!({
                        "type": "message",
                        "role": "user",
                        "content": [{
                            "type": "input_text",
                            "text": format!("<context-checkpoint>\n{summary}\n</context-checkpoint>")
                        }]
                    }));
                }
            }
        }
    }

    (instructions, items)
}

fn normalize_tool_call_ids(messages: &[AgentMessage]) -> Vec<AgentMessage> {
    let mut tool_index = 0;
    messages
        .iter()
        .map(|message| match message {
            AgentMessage::Assistant {
                content,
                tool_calls: Some(tool_calls),
                stop_reason,
                deferred_handle,
            } => {
                tool_index = 0;
                AgentMessage::Assistant {
                    content: content.clone(),
                    tool_calls: Some(
                        tool_calls
                            .iter()
                            .enumerate()
                            .map(|(idx, call)| {
                                let mut call = call.clone();
                                call.id = normalized_tool_call_id(&call.id, idx);
                                call
                            })
                            .collect(),
                    ),
                    stop_reason: stop_reason.clone(),
                    deferred_handle: deferred_handle.clone(),
                }
            }
            AgentMessage::Tool {
                tool_call_id,
                name,
                content,
                is_error,
                terminate,
            } => {
                let normalized = normalized_tool_call_id(tool_call_id, tool_index);
                tool_index += 1;
                AgentMessage::Tool {
                    tool_call_id: normalized,
                    name: name.clone(),
                    content: content.clone(),
                    is_error: *is_error,
                    terminate: *terminate,
                }
            }
            other => {
                tool_index = 0;
                other.clone()
            }
        })
        .collect()
}

pub type ToolIntentRecorder = Arc<
    dyn Fn(&str, &str, &str) -> Pin<Box<dyn Future<Output = Result<(), String>> + Send>>
        + Send
        + Sync,
>;

pub type ToolCompletionRecorder = Arc<
    dyn Fn(&str, bool) -> Pin<Box<dyn Future<Output = Result<(), String>> + Send>> + Send + Sync,
>;

pub type ProviderUsageRecorder = Arc<
    dyn Fn(TokenUsage) -> Pin<Box<dyn Future<Output = Result<(), String>> + Send>> + Send + Sync,
>;

pub type ProviderDiscardedUsageRecorder = ProviderUsageRecorder;

pub type StreamingStateRecorder = Arc<
    dyn Fn(
            crate::harness::StreamingState,
        ) -> Pin<Box<dyn Future<Output = Result<(), String>> + Send>>
        + Send
        + Sync,
>;

pub type ProviderHookRecorder = Arc<
    dyn Fn(
            crate::harness::HookKind,
        ) -> Pin<Box<dyn Future<Output = Result<Vec<String>, String>> + Send>>
        + Send
        + Sync,
>;

pub type AssistantMessageRecorder = Arc<
    dyn Fn(AgentMessage) -> Pin<Box<dyn Future<Output = Result<(), String>> + Send>> + Send + Sync,
>;

pub(crate) async fn run_provider_hook(
    recorder: Option<&ProviderHookRecorder>,
    kind: crate::harness::HookKind,
) -> Result<(), String> {
    let Some(recorder) = recorder else {
        return Ok(());
    };
    for failure in recorder(kind).await? {
        eprintln!("provider {:?} hook failed: {failure}", kind);
    }
    Ok(())
}

#[derive(Clone)]
pub struct AgentLoop {
    pub state: Arc<Mutex<AgentState>>,
    pub api_key: String,
    pub account_id: Option<String>,
    provider_client: ProviderClient,
    prompt_cache_key: Option<String>,
    pub tool_execution_mode: ToolExecutionMode,
    allowed_tool_names: Option<HashSet<String>>,
    steering_queue: PendingMessageQueue,
    follow_up_queue: PendingMessageQueue,
    pub hook_registry: HookRegistry,
    pub tool_intent_recorder: Option<ToolIntentRecorder>,
    pub tool_completion_recorder: Option<ToolCompletionRecorder>,
    pub provider_usage_recorder: Option<ProviderUsageRecorder>,
    pub provider_discarded_usage_recorder: Option<ProviderDiscardedUsageRecorder>,
    pub streaming_state_recorder: Option<StreamingStateRecorder>,
    pub provider_hook_recorder: Option<ProviderHookRecorder>,
    pub assistant_message_recorder: Option<AssistantMessageRecorder>,
    pub tool_message_recorder: Option<AssistantMessageRecorder>,
    pub session_id: String,
    pub event_tx: broadcast::Sender<AgentEvent>,
    tool_executors: Vec<Arc<dyn ToolExecutor>>,
    /// Compatibility slot for existing callers. New code should use
    /// `register_tool_executor` so ordering and schema conflicts are validated.
    extension_manager: Option<Arc<dyn ToolExecutor>>,
    pub work_dir: Option<PathBuf>,
    stream_rules: Vec<(crate::rules::StreamRule, regex::Regex)>,
    pub config: AgentConfig,
    pub provider_router: ProviderRouter,
    pub tool_dispatcher: crate::tool_dispatcher::ToolDispatcher,
    /// Optional journal for durability. When set, the agent loop records
    /// messages, tool intents, usage, and streaming state through this
    /// instead of the individual recorder callbacks.
    pub journal: Option<Arc<dyn AgentJournal>>,
}

impl AgentLoop {
    pub fn new(
        api_key: impl Into<String>,
        account_id: Option<String>,
        model: impl Into<String>,
    ) -> Self {
        Self::new_with_config(api_key, account_id, model, AgentConfig::default())
    }

    /// Creates an [`AgentLoop`] with a custom [`AgentConfig`].
    pub fn new_with_config(
        api_key: impl Into<String>,
        account_id: Option<String>,
        model: impl Into<String>,
        config: AgentConfig,
    ) -> Self {
        let (event_tx, _) = broadcast::channel(config.event_channel_capacity);
        let state = Arc::new(Mutex::new(AgentState::new(
            model,
            &config.default_system_prompt,
        )));
        let api_key = api_key.into();
        let provider_client = ProviderClient::new(api_key.clone(), account_id.clone());
        let hooks = HookRegistry::default();

        Self {
            state,
            api_key,
            account_id,
            provider_client,
            prompt_cache_key: None,
            tool_execution_mode: ToolExecutionMode::Parallel,
            allowed_tool_names: None,
            steering_queue: PendingMessageQueue::new(QueueMode::All),
            follow_up_queue: PendingMessageQueue::new(QueueMode::All),
            hook_registry: hooks.clone(),
            tool_intent_recorder: None,
            tool_completion_recorder: None,
            provider_usage_recorder: None,
            provider_discarded_usage_recorder: None,
            streaming_state_recorder: None,
            provider_hook_recorder: None,
            assistant_message_recorder: None,
            tool_message_recorder: None,
            session_id: String::new(),
            event_tx: event_tx.clone(),
            tool_executors: Vec::new(),
            extension_manager: None,
            work_dir: None,
            stream_rules: Vec::new(),
            config,
            provider_router: ProviderRouter::new(),
            tool_dispatcher: crate::tool_dispatcher::ToolDispatcher::new(event_tx, hooks),
            journal: None,
        }
    }

    pub fn set_prompt_cache_key(&mut self, key: Option<String>) {
        self.prompt_cache_key = key
            .map(|key| clamp_prompt_cache_key(&key))
            .filter(|key| !key.is_empty());
    }

    pub fn set_credentials(&mut self, api_key: impl Into<String>, account_id: Option<String>) {
        let api_key = api_key.into();
        self.provider_client = ProviderClient::new(api_key.clone(), account_id.clone());
        self.api_key = api_key;
        self.account_id = account_id;
    }

    /// Restricts both advertised and executable tools. `None` restores the
    /// default behavior where all registered, state, and core tools are available.
    pub fn set_allowed_tool_names(&mut self, allowed_tool_names: Option<HashSet<String>>) {
        self.allowed_tool_names = allowed_tool_names;
    }

    pub fn set_stream_rules(&mut self, rules: Vec<crate::rules::StreamRule>) {
        self.stream_rules = rules
            .into_iter()
            .filter_map(|rule| regex::Regex::new(&rule.pattern).ok().map(|re| (rule, re)))
            .collect();
    }

    /// Returns the core and registered executor schemas in provider order,
    /// after conflict deduplication and the active allowlist are applied.
    pub fn configured_tool_definitions(&self) -> Vec<AgentToolDefinition> {
        self.tool_dispatcher.configured_tool_definitions()
    }

    pub fn register_tool_executor(
        &mut self,
        executor: Arc<dyn ToolExecutor>,
    ) -> Result<(), AgentError> {
        // Keep both registries in sync: ToolDispatcher for execution,
        // self.tool_executors for payload building.
        self.tool_dispatcher
            .register_tool_executor(executor.clone())?;
        self.tool_executors.push(executor);
        Ok(())
    }

    pub fn tool_executor_count(&self) -> usize {
        self.tool_dispatcher.tool_executor_count()
    }

    fn compatibility_executor(&self) -> Option<Arc<dyn ToolExecutor>> {
        self.extension_manager.clone().filter(|compat| {
            !self
                .tool_executors
                .iter()
                .any(|reg| reg.executor_id() == compat.executor_id())
        })
    }

    async fn build_payload_helper(
        state_mutex: &Arc<Mutex<AgentState>>,
        tool_executors: &[Arc<dyn ToolExecutor>],
        allowed_tool_names: Option<&HashSet<String>>,
        compatibility_executor: Option<&Arc<dyn ToolExecutor>>,
        prompt_cache_key: Option<&str>,
        format: PayloadFormat,
    ) -> Value {
        let mut state = state_mutex.lock().await.clone();
        repair_interrupted_tool_turn(&mut state.messages);

        let mut definitions =
            collect_tool_definitions(&state.tools, tool_executors, compatibility_executor);
        if let Some(allowed_tool_names) = allowed_tool_names {
            definitions.retain(|definition| allowed_tool_names.contains(&definition.name));
        }

        match format {
            PayloadFormat::ChatCompletions => {
                let api_msgs = convert_to_llm(&state.messages);
                let tools: Vec<_> = definitions
                    .iter()
                    .map(AgentToolDefinition::to_chat_completions_tool)
                    .collect();
                let mut chat_payload = serde_json::json!({
                    "model": state.model,
                    "messages": api_msgs,
                    "tools": tools,
                    "stream": true,
                    "stream_options": { "include_usage": true }
                });
                if let Some(key) = prompt_cache_key {
                    chat_payload["prompt_cache_key"] = key.into();
                }
                if let Some(effort) = state.reasoning_effort.as_api_str() {
                    chat_payload["reasoning_effort"] = effort.into();
                }
                chat_payload
            }
            PayloadFormat::Codex => {
                let (instructions, codex_msgs) = convert_to_codex_llm(&state.messages);
                let codex_tools: Vec<_> = definitions
                    .iter()
                    .map(AgentToolDefinition::to_codex_responses_tool)
                    .collect();
                let mut codex_payload = serde_json::json!({
                    "model": state.model,
                    "instructions": instructions,
                    "input": codex_msgs,
                    "store": false,
                    "stream": true,
                    "tools": codex_tools
                });
                if let Some(key) = prompt_cache_key {
                    codex_payload["prompt_cache_key"] = key.into();
                }
                if let Some(effort) = state.reasoning_effort.as_api_str() {
                    codex_payload["reasoning"] = serde_json::json!({
                        "effort": effort,
                        "summary": "auto"
                    });
                }
                codex_payload
            }
        }
    }

    async fn build_chat_payload(&self) -> Value {
        Self::build_payload_helper(
            &self.state,
            &self.tool_executors,
            self.allowed_tool_names.as_ref(),
            self.compatibility_executor().as_ref(),
            self.prompt_cache_key.as_deref(),
            PayloadFormat::ChatCompletions,
        )
        .await
    }

    async fn build_codex_payload(&self) -> Value {
        Self::build_payload_helper(
            &self.state,
            &self.tool_executors,
            self.allowed_tool_names.as_ref(),
            self.compatibility_executor().as_ref(),
            self.prompt_cache_key.as_deref(),
            PayloadFormat::Codex,
        )
        .await
    }

    /// Builds both provider payloads without making a network request.
    pub async fn build_api_payloads(&self) -> (Value, Value) {
        (
            self.build_chat_payload().await,
            self.build_codex_payload().await,
        )
    }

    pub(crate) fn subscribe(&self) -> broadcast::Receiver<AgentEvent> {
        self.event_tx.subscribe()
    }

    pub(crate) fn steer(&mut self, message: AgentMessage) {
        self.steering_queue.enqueue(message);
    }

    pub(crate) fn follow_up(&mut self, message: AgentMessage) {
        self.follow_up_queue.enqueue(message);
    }

    pub(crate) async fn run_prompt(&mut self, prompt: &str) {
        self.run_prompt_message(AgentMessage::User {
            content: prompt.to_string(),
        })
        .await;
    }

    /// Runs a complete user message, preserving multimodal attachments exactly.
    ///
    /// Panics if `message` is not a user message.
    pub(crate) async fn run_prompt_message(&mut self, message: AgentMessage) {
        assert!(message.is_user(), "prompt message must have a user role");
        {
            let mut state = self.state.lock().await;
            repair_interrupted_tool_turn(&mut state.messages);
            state.messages.push(message);
        }
        self.run_queued_turns().await;
    }

    /// Runs messages already placed in the follow-up queue without adding an
    /// artificial prompt. This lets host schedulers start queued work while
    /// the agent is idle.
    pub(crate) async fn run_follow_up(&mut self) {
        if !self.follow_up_queue.has_items() {
            return;
        }
        let items = self.follow_up_queue.drain();
        let mut state = self.state.lock().await;
        repair_interrupted_tool_turn(&mut state.messages);
        state.messages.extend(items);
        drop(state);
        self.run_queued_turns().await;
    }

    pub(crate) async fn run_steer(&mut self) {
        if !self.steering_queue.has_items() {
            return;
        }
        let items = self.steering_queue.drain();
        let mut state = self.state.lock().await;
        repair_interrupted_tool_turn(&mut state.messages);
        state.messages.extend(items);
        drop(state);
        self.run_queued_turns().await;
    }

    async fn run_queued_turns(&mut self) {
        let _ = self.event_tx.send(AgentEvent::AgentStart);
        let mut turn_number = 0;
        let mut overflow_recovery_attempted = false;
        let mut total_usage = TokenUsage::default();

        'turn_loop: loop {
            turn_number += 1;

            // Drain steering queue items
            if self.steering_queue.has_items() {
                let items = self.steering_queue.drain();
                let mut state = self.state.lock().await;
                state.messages.extend(items);
            }

            if let Err(error) = run_provider_hook(
                self.provider_hook_recorder.as_ref(),
                crate::harness::HookKind::BeforeContext,
            )
            .await
            {
                let _ = self.event_tx.send(AgentEvent::AgentError { error });
                return;
            }

            {
                let mut state = self.state.lock().await;
                if should_auto_compact(&state.messages, &self.config) {
                    state.messages = compact_messages_to_token_budget(
                        &state.messages,
                        self.config.auto_compaction_keep_recent_tokens,
                    );
                }
            }

            let _ = self.event_tx.send(AgentEvent::TurnStart { turn_number });

            let model = {
                let state = self.state.lock().await;
                state.model.clone()
            };

            let state = self.state.clone();
            let tool_executors = self.tool_executors.clone();
            let allowed_tool_names = self.allowed_tool_names.clone();
            let compatibility_executor = self.compatibility_executor();
            let pc_key = self.prompt_cache_key.clone();
            let provider_hook_recorder = self.provider_hook_recorder.clone();
            let router = self.provider_router.clone();

            let payload_source = PayloadSource::lazy(model, move |format| {
                let state = state.clone();
                let tool_executors = tool_executors.clone();
                let allowed_tool_names = allowed_tool_names.clone();
                let compatibility_executor = compatibility_executor.clone();
                let pc_key = pc_key.clone();
                let provider_hook_recorder = provider_hook_recorder.clone();
                let router = router.clone();
                Box::pin(async move {
                    if let Err(error) = run_provider_hook(
                        provider_hook_recorder.as_ref(),
                        crate::harness::HookKind::BeforePayload,
                    )
                    .await
                    {
                        eprintln!("provider payload hook failed: {error}");
                    }
                    // Collect tools and build the payload through the provider router.
                    let mut state_snapshot = state.lock().await.clone();
                    repair_interrupted_tool_turn(&mut state_snapshot.messages);
                    let mut definitions = collect_tool_definitions(
                        &state_snapshot.tools,
                        &tool_executors,
                        compatibility_executor.as_ref(),
                    );
                    if let Some(ref allowed) = allowed_tool_names {
                        definitions.retain(|d| allowed.contains(&d.name));
                    }
                    let payload = router.build_payload(
                        format,
                        &state_snapshot,
                        &definitions,
                        pc_key.as_deref(),
                    );
                    if let Err(error) = run_provider_hook(
                        provider_hook_recorder.as_ref(),
                        crate::harness::HookKind::AfterPayload,
                    )
                    .await
                    {
                        eprintln!("provider payload hook failed: {error}");
                    }
                    payload
                })
            });

            let (stream_tx, mut stream_rx) = mpsc::channel(100);
            let client = self.provider_client.clone();
            let prompt_cache_key = self.prompt_cache_key.clone();

            if let Err(error) = run_provider_hook(
                self.provider_hook_recorder.as_ref(),
                crate::harness::HookKind::BeforeRequest,
            )
            .await
            {
                let _ = self.event_tx.send(AgentEvent::AgentError { error });
                return;
            }

            let _stream_task = AbortOnDrop::new(tokio::spawn(async move {
                client
                    .stream_chat_completion(payload_source, prompt_cache_key, stream_tx)
                    .await;
            }));

            if let Err(error) = run_provider_hook(
                self.provider_hook_recorder.as_ref(),
                crate::harness::HookKind::BeforeResponse,
            )
            .await
            {
                let _ = self.event_tx.send(AgentEvent::AgentError { error });
                return;
            }

            let _ = self.event_tx.send(AgentEvent::MessageStart {
                role: "assistant".into(),
            });

            if let Some(recorder) = &self.streaming_state_recorder {
                if let Err(error) = recorder(crate::harness::StreamingState::default()).await {
                    let _ = self.event_tx.send(AgentEvent::AgentError { error });
                    return;
                }
            }

            let mut current_turn_text = String::new();
            let mut current_turn_reasoning = String::new();
            let mut captured_tool_calls: Vec<ToolCall> = Vec::new();
            let mut provider_step = ProviderStepAccumulator::default();

            let mut stream_monitor =
                crate::rules::StreamRuleMonitor::new(self.stream_rules.clone(), &self.config);
            let mut rule_triggered = None;

            while let Some(evt) = stream_rx.recv().await {
                let accumulated = provider_step.push(&evt);
                match evt {
                    StreamEvent::ContentToken(token) => {
                        current_turn_text.push_str(&token);
                        if let Some(rule_match) = stream_monitor.push_chunk(&token) {
                            rule_triggered = Some(rule_match);
                            break;
                        }
                        let _ = self.event_tx.send(AgentEvent::MessageUpdate {
                            text_delta: Some(token),
                            reasoning_delta: None,
                            tool_call_name: None,
                        });
                        if let Some(recorder) = &self.streaming_state_recorder {
                            if let Err(error) = recorder(crate::harness::StreamingState {
                                assistant_text: current_turn_text.clone(),
                                reasoning: current_turn_reasoning.clone(),
                                ..Default::default()
                            })
                            .await
                            {
                                let _ = self.event_tx.send(AgentEvent::AgentError { error });
                                return;
                            }
                        }
                    }
                    StreamEvent::ReasoningToken(token) => {
                        current_turn_reasoning.push_str(&token);
                        let _ = self.event_tx.send(AgentEvent::MessageUpdate {
                            text_delta: None,
                            reasoning_delta: Some(token),
                            tool_call_name: None,
                        });
                        if let Some(recorder) = &self.streaming_state_recorder {
                            if let Err(error) = recorder(crate::harness::StreamingState {
                                assistant_text: current_turn_text.clone(),
                                reasoning: current_turn_reasoning.clone(),
                                ..Default::default()
                            })
                            .await
                            {
                                let _ = self.event_tx.send(AgentEvent::AgentError { error });
                                return;
                            }
                        }
                    }
                    StreamEvent::ToolCallStart { name, .. } => {
                        let _ = self.event_tx.send(AgentEvent::MessageUpdate {
                            text_delta: None,
                            reasoning_delta: None,
                            tool_call_name: Some(name),
                        });
                    }
                    StreamEvent::ToolCallArgsDelta { .. } => {}
                    StreamEvent::Finished { .. } => {
                        let Some(step) = accumulated.ok().flatten() else {
                            let _ = self.event_tx.send(AgentEvent::AgentError {
                                error: "provider step did not produce a final response".into(),
                            });
                            return;
                        };
                        let usage = step.usage.clone();
                        if let Some(recorder) = &self.provider_usage_recorder {
                            if let Err(error) = recorder(usage.clone()).await {
                                let _ = self.event_tx.send(AgentEvent::AgentError { error });
                                return;
                            }
                        }
                        if let Some(hook) = &self.provider_hook_recorder {
                            match hook(crate::harness::HookKind::AfterResponse).await {
                                Ok(failures) => {
                                    for failure in failures {
                                        eprintln!("provider after-response hook failed: {failure}");
                                    }
                                }
                                Err(error) => {
                                    let _ = self.event_tx.send(AgentEvent::AgentError { error });
                                    return;
                                }
                            }
                        }
                        captured_tool_calls = step.tool_calls;
                        if let Some(recorder) = &self.streaming_state_recorder {
                            if let Err(error) = recorder(crate::harness::StreamingState {
                                assistant_text: current_turn_text.clone(),
                                reasoning: current_turn_reasoning.clone(),
                                tool_call_ids: captured_tool_calls
                                    .iter()
                                    .map(|call| call.id.clone())
                                    .collect(),
                                ..Default::default()
                            })
                            .await
                            {
                                let _ = self.event_tx.send(AgentEvent::AgentError { error });
                                return;
                            }
                        }
                        total_usage.accumulate(&usage);
                        break;
                    }
                    StreamEvent::Error(err) => {
                        if let Some(recorder) = &self.provider_discarded_usage_recorder {
                            if let Err(error) = recorder(TokenUsage::default()).await {
                                let _ = self.event_tx.send(AgentEvent::AgentError { error });
                                return;
                            }
                        }
                        if !overflow_recovery_attempted && is_context_overflow_error(&err) {
                            let mut state = self.state.lock().await;
                            let compacted = compact_messages_to_token_budget(
                                &state.messages,
                                self.config.auto_compaction_keep_recent_tokens,
                            );
                            if compacted.len() < state.messages.len() {
                                state.messages = compacted;
                                overflow_recovery_attempted = true;
                                drop(state);
                                continue 'turn_loop;
                            }
                        }
                        let _ = self
                            .event_tx
                            .send(AgentEvent::AgentError { error: err.clone() });
                        return;
                    }
                }
            }

            if let Err(error) = run_provider_hook(
                self.provider_hook_recorder.as_ref(),
                crate::harness::HookKind::AfterRequest,
            )
            .await
            {
                let _ = self.event_tx.send(AgentEvent::AgentError { error });
                return;
            }

            if rule_triggered.is_none() && provider_step.finish().is_err() {
                let _ = self.event_tx.send(AgentEvent::AgentError {
                    error: "provider stream ended without a final response".into(),
                });
                return;
            }

            if let Some(rule_match) = rule_triggered {
                let _ = self.event_tx.send(AgentEvent::StreamRuleTriggered {
                    rule_id: rule_match.rule_id.clone(),
                    rule_name: rule_match.rule_name.clone(),
                    matched_text: rule_match.matched_text.clone(),
                    reminder: rule_match.reminder.clone(),
                });

                let reminder_msg = AgentMessage::System {
                    content: format!(
                        "⚠ STREAM RULE INJECTION [{}: {}]: Matched invalid pattern '{}'. Reminder: {}. Please adjust your output and try again.",
                        rule_match.rule_id, rule_match.rule_name, rule_match.matched_text, rule_match.reminder
                    ),
                };

                let mut state = self.state.lock().await;
                state.messages.push(reminder_msg);
                drop(state);

                continue 'turn_loop;
            }

            let assistant_msg = AgentMessage::Assistant {
                content: if current_turn_text.is_empty() {
                    None
                } else {
                    Some(current_turn_text.clone())
                },
                tool_calls: if captured_tool_calls.is_empty() {
                    None
                } else {
                    Some(captured_tool_calls.clone())
                },
                stop_reason: None,
                deferred_handle: None,
            };

            if !current_turn_reasoning.trim().is_empty() {
                let thinking = AgentMessage::Custom {
                    custom_type: "thinking".into(),
                    payload: serde_json::json!({ "text": current_turn_reasoning }),
                };
                if let Some(recorder) = &self.assistant_message_recorder {
                    if let Err(error) = recorder(thinking.clone()).await {
                        let _ = self.event_tx.send(AgentEvent::AgentError { error });
                        return;
                    }
                }
                self.state.lock().await.messages.push(thinking);
            }
            self.state.lock().await.messages.push(assistant_msg.clone());

            if let Some(recorder) = &self.assistant_message_recorder {
                if let Err(error) = recorder(assistant_msg.clone()).await {
                    let _ = self.event_tx.send(AgentEvent::AgentError { error });
                    return;
                }
            }

            let _ = self.event_tx.send(AgentEvent::MessageEnd {
                message: assistant_msg,
            });

            if captured_tool_calls.is_empty() {
                if let Some(recorder) = &self.streaming_state_recorder {
                    if let Err(error) = recorder(crate::harness::StreamingState::default()).await {
                        let _ = self.event_tx.send(AgentEvent::AgentError { error });
                        return;
                    }
                }
                let _ = self.event_tx.send(AgentEvent::TurnEnd {
                    turn_number,
                    tool_results: Vec::new(),
                });

                if self.follow_up_queue.has_items() {
                    let items = self.follow_up_queue.drain();
                    let mut state = self.state.lock().await;
                    state.messages.extend(items);
                    continue;
                }
                break;
            }

            // Tool Execution
            let tool_results = self.execute_tools(&captured_tool_calls).await;

            let should_terminate = tool_results.iter().any(|r| r.terminate);

            let mut state = self.state.lock().await;
            for r in &tool_results {
                state.messages.push(AgentMessage::Tool {
                    tool_call_id: r.tool_call_id.clone(),
                    name: r.name.clone(),
                    content: r.content.clone(),
                    is_error: r.is_error,
                    terminate: r.terminate,
                });
            }
            drop(state);

            if let Some(recorder) = &self.tool_message_recorder {
                for result in &tool_results {
                    let message = AgentMessage::Tool {
                        tool_call_id: result.tool_call_id.clone(),
                        name: result.name.clone(),
                        content: result.content.clone(),
                        is_error: result.is_error,
                        terminate: result.terminate,
                    };
                    if let Err(error) = recorder(message).await {
                        let _ = self.event_tx.send(AgentEvent::AgentError { error });
                        return;
                    }
                }
            }

            if let Some(recorder) = &self.streaming_state_recorder {
                if let Err(error) = recorder(crate::harness::StreamingState::default()).await {
                    let _ = self.event_tx.send(AgentEvent::AgentError { error });
                    return;
                }
            }

            let _ = self.event_tx.send(AgentEvent::TurnEnd {
                turn_number,
                tool_results: tool_results.clone(),
            });

            if should_terminate {
                break;
            }
        }

        let _ = self
            .event_tx
            .send(AgentEvent::AgentEnd { usage: total_usage });
    }

    pub(crate) async fn resume_pending_turn(&mut self) {
        self.run_queued_turns().await;
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

    /// Returns a ToolDispatcher snapshot synced with this AgentLoop's current state.
    fn synced_dispatcher(&self) -> crate::tool_dispatcher::ToolDispatcher {
        let mut td = self.tool_dispatcher.clone();
        td.tool_execution_mode = self.tool_execution_mode;
        td.hook_registry = self.hook_registry.clone();
        td.tool_intent_recorder = self.tool_intent_recorder.clone();
        td.tool_completion_recorder = self.tool_completion_recorder.clone();
        td.allowed_tool_names = self.allowed_tool_names.clone();
        td.work_dir = self.work_dir.clone();
        td.session_id = self.session_id.clone();
        td.set_extension_manager(self.extension_manager.clone());
        td
    }

    pub async fn execute_tools(&self, tool_calls: &[ToolCall]) -> Vec<AgentToolResult> {
        self.synced_dispatcher().execute_tools(tool_calls).await
    }

    pub async fn execute_tools_without_intent_recording(
        &self,
        tool_calls: &[ToolCall],
    ) -> Vec<AgentToolResult> {
        self.synced_dispatcher()
            .execute_tools_without_intent_recording(tool_calls)
            .await
    }

    /// Replays already-intended safe tools. The before hook is intentionally
    /// not rerun: the durable ToolStarted record is the clearance boundary.
    pub async fn execute_tools_for_replay(&self, tool_calls: &[ToolCall]) -> Vec<AgentToolResult> {
        self.synced_dispatcher()
            .execute_tools_for_replay(tool_calls)
            .await
    }
}

pub(crate) fn core_tool_definitions() -> Vec<AgentToolDefinition> {
    let mut seen = HashSet::new();
    get_available_tools()
        .into_iter()
        .chain(get_codex_tools())
        .filter_map(|schema| AgentToolDefinition::from_provider_schema(&schema).ok())
        .filter(|definition| seen.insert(definition.name.clone()))
        .collect()
}

pub(crate) fn collect_tool_definitions(
    state_tools: &[Value],
    registered_executors: &[Arc<dyn ToolExecutor>],
    compatibility_executor: Option<&Arc<dyn ToolExecutor>>,
) -> Vec<AgentToolDefinition> {
    let mut seen = HashSet::new();
    let mut definitions = Vec::new();

    for definition in core_tool_definitions()
        .into_iter()
        .chain(
            registered_executors
                .iter()
                .flat_map(|executor| executor.tool_definitions()),
        )
        .chain(
            compatibility_executor
                .into_iter()
                .flat_map(|executor| executor.tool_definitions()),
        )
        .chain(
            state_tools
                .iter()
                .filter_map(|schema| AgentToolDefinition::from_provider_schema(schema).ok()),
        )
    {
        if seen.insert(definition.name.clone()) {
            definitions.push(definition);
        }
    }

    definitions
}

pub(crate) fn normalize_tool_arguments_inner(
    name: &str,
    arguments: &str,
    work_dir: Option<&std::path::Path>,
) -> String {
    let Some(work_dir) = work_dir else {
        return arguments.to_string();
    };
    let Ok(mut value) = serde_json::from_str::<Value>(arguments) else {
        return arguments.to_string();
    };
    let workspace = work_dir.to_string_lossy().to_string();
    match (name, value.as_object_mut()) {
        ("read_file" | "write_file" | "edit_file" | "list_dir", Some(object))
            if object
                .get("path")
                .and_then(Value::as_str)
                .is_none_or(str::is_empty) =>
        {
            object.insert("path".into(), Value::String(workspace));
        }
        ("run_command", Some(object))
            if object
                .get("cwd")
                .and_then(Value::as_str)
                .is_none_or(str::is_empty) =>
        {
            object.insert("cwd".into(), Value::String(workspace));
        }
        _ => {}
    }

    serde_json::to_string(&value).unwrap_or_else(|_| arguments.to_string())
}

#[cfg(test)]
mod normalize_tool_arguments_tests {
    use super::*;
    use crate::harness::{HookEffect, HookKind};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Mutex as StdMutex;
    use threadlane_provider::openai::{ToolCall, ToolCallFunction};

    fn register_counting_hooks(
        agent: &mut AgentLoop,
        before_calls: Arc<AtomicUsize>,
        after_calls: Arc<AtomicUsize>,
    ) {
        let before_calls_for_handler = before_calls;
        agent
            .hook_registry
            .register(
                HookKind::BeforeTool,
                "test-before-tool",
                Arc::new(move |_context| {
                    let before_calls = before_calls_for_handler.clone();
                    Box::pin(async move {
                        before_calls.fetch_add(1, Ordering::SeqCst);
                        Ok(HookEffect::default())
                    })
                }),
            )
            .unwrap();
        agent
            .hook_registry
            .register(
                HookKind::AfterTool,
                "test-after-tool",
                Arc::new(move |_context| {
                    let after_calls = after_calls.clone();
                    Box::pin(async move {
                        after_calls.fetch_add(1, Ordering::SeqCst);
                        Ok(HookEffect::default())
                    })
                }),
            )
            .unwrap();
    }

    #[test]
    fn provider_step_accumulator_returns_one_stateless_result() {
        let mut step = ProviderStepAccumulator::default();
        step.push(&StreamEvent::ContentToken("answer".into()))
            .unwrap();
        step.push(&StreamEvent::ReasoningToken("thought".into()))
            .unwrap();
        let result = step
            .push(&StreamEvent::Finished {
                tool_calls: Vec::new(),
                usage: ProviderUsage {
                    input_tokens: 2,
                    output_tokens: 3,
                    cache_read_tokens: 0,
                    cache_write_tokens: 0,
                    total_tokens: 5,
                },
            })
            .unwrap()
            .unwrap();
        assert_eq!(result.text, "answer");
        assert_eq!(result.reasoning, "thought");
        assert_eq!(result.usage.total_tokens, 5);
        let finished = step.finish().unwrap();
        assert_eq!(finished.text, result.text);
        assert_eq!(finished.reasoning, result.reasoning);
        assert_eq!(finished.usage, result.usage);
    }

    #[test]
    fn provider_step_accumulator_preserves_stream_errors() {
        let mut step = ProviderStepAccumulator::default();
        assert_eq!(
            step.push(&StreamEvent::Error("temporary failure".into()))
                .unwrap_err(),
            "temporary failure"
        );
        assert!(step.finish().is_err());
    }

    #[test]
    fn provider_step_accumulator_rejects_incomplete_streams() {
        let mut step = ProviderStepAccumulator::default();
        step.push(&StreamEvent::ContentToken("partial".into()))
            .unwrap();
        assert_eq!(
            step.finish().unwrap_err(),
            "provider stream ended without a final response"
        );
    }

    #[test]
    fn fills_missing_file_paths_from_the_workspace() {
        let arguments = normalize_tool_arguments_inner(
            "read_file",
            "{}",
            Some(std::path::Path::new("/workspace")),
        );

        assert_eq!(arguments, r#"{"path":"/workspace"}"#);
    }

    #[test]
    fn normalizes_empty_tool_ids_by_tool_index() {
        let messages = vec![
            AgentMessage::Assistant {
                content: None,
                tool_calls: Some(vec![
                    ToolCall {
                        id: String::new(),
                        r#type: "function".into(),
                        function: ToolCallFunction {
                            name: "read_file".into(),
                            arguments: "{}".into(),
                        },
                        thought_signature: None,
                    },
                    ToolCall {
                        id: String::new(),
                        r#type: "function".into(),
                        function: ToolCallFunction {
                            name: "list_dir".into(),
                            arguments: "{}".into(),
                        },
                        thought_signature: None,
                    },
                ]),
                stop_reason: None,
                deferred_handle: None,
            },
            AgentMessage::Tool {
                tool_call_id: String::new(),
                name: "read_file".into(),
                content: "one".into(),
                is_error: false,
                terminate: false,
            },
            AgentMessage::Tool {
                tool_call_id: String::new(),
                name: "list_dir".into(),
                content: "two".into(),
                is_error: false,
                terminate: false,
            },
        ];

        let chat = convert_to_llm(&messages);
        assert_eq!(chat[1]["tool_call_id"], "call_0");
        assert_eq!(chat[2]["tool_call_id"], "call_1");

        let (_, codex) = convert_to_codex_llm(&messages);
        assert_eq!(codex[2]["call_id"], "call_0");
        assert_eq!(codex[3]["call_id"], "call_1");
    }

    #[test]
    fn normalizes_empty_tool_ids_after_explicit_ids() {
        let messages = vec![
            AgentMessage::Assistant {
                content: None,
                tool_calls: Some(vec![
                    ToolCall {
                        id: "provider-call".into(),
                        r#type: "function".into(),
                        function: ToolCallFunction {
                            name: "read_file".into(),
                            arguments: "{}".into(),
                        },
                        thought_signature: None,
                    },
                    ToolCall {
                        id: String::new(),
                        r#type: "function".into(),
                        function: ToolCallFunction {
                            name: "list_dir".into(),
                            arguments: "{}".into(),
                        },
                        thought_signature: None,
                    },
                ]),
                stop_reason: None,
                deferred_handle: None,
            },
            AgentMessage::Tool {
                tool_call_id: "provider-call".into(),
                name: "read_file".into(),
                content: "one".into(),
                is_error: false,
                terminate: false,
            },
            AgentMessage::Tool {
                tool_call_id: String::new(),
                name: "list_dir".into(),
                content: "two".into(),
                is_error: false,
                terminate: false,
            },
        ];

        let chat = convert_to_llm(&messages);
        assert_eq!(chat[2]["tool_call_id"], "call_1");
    }

    #[tokio::test]
    async fn tool_intent_recorder_sees_normalized_arguments_before_execution() {
        let dir = tempfile::tempdir().unwrap();
        let mut agent = AgentLoop::new("", None, "test");
        agent.work_dir = Some(dir.path().to_path_buf());
        let recorded = Arc::new(StdMutex::new(None));
        let recorded_for_callback = recorded.clone();
        agent.tool_intent_recorder = Some(Arc::new(move |id, name, arguments| {
            let recorded = recorded_for_callback.clone();
            let value = (id.to_string(), name.to_string(), arguments.to_string());
            Box::pin(async move {
                *recorded.lock().unwrap() = Some(value);
                Ok(())
            })
        }));

        let results = agent
            .execute_tools(&[ToolCall {
                id: "call-1".into(),
                r#type: "function".into(),
                function: ToolCallFunction {
                    name: "list_dir".into(),
                    arguments: "{}".into(),
                },
                thought_signature: None,
            }])
            .await;

        assert_eq!(
            recorded.lock().unwrap().as_ref(),
            Some(&(
                "call-1".into(),
                "list_dir".into(),
                format!(r#"{{"path":"{}"}}"#, dir.path().display())
            ))
        );
        assert!(!results[0].is_error);
    }

    #[tokio::test]
    async fn tool_intent_recorder_failure_prevents_execution() {
        let mut agent = AgentLoop::new("", None, "test");
        agent.tool_intent_recorder = Some(Arc::new(|_, _, _| {
            Box::pin(async { Err("intent append failed".into()) })
        }));
        let mut events = agent.event_tx.subscribe();

        let results = agent
            .execute_tools(&[ToolCall {
                id: "call-1".into(),
                r#type: "function".into(),
                function: ToolCallFunction {
                    name: "list_dir".into(),
                    arguments: "{}".into(),
                },
                thought_signature: None,
            }])
            .await;

        assert!(results[0].is_error);
        assert_eq!(results[0].content, "intent append failed");
        assert!(matches!(
            events.try_recv(),
            Ok(AgentEvent::ToolExecutionEnd { .. })
        ));
        assert!(events.try_recv().is_err());
    }

    #[tokio::test]
    async fn safe_replay_skips_before_tool_hook() {
        let mut agent = AgentLoop::new("", None, "test");
        let before_calls = Arc::new(AtomicUsize::new(0));
        let after_calls = Arc::new(AtomicUsize::new(0));
        register_counting_hooks(&mut agent, before_calls.clone(), after_calls.clone());
        let call = ToolCall {
            id: "call-1".into(),
            r#type: "function".into(),
            function: ToolCallFunction {
                name: "list_dir".into(),
                arguments: "{}".into(),
            },
            thought_signature: None,
        };

        let normal = agent.execute_tools(std::slice::from_ref(&call)).await;
        assert!(!normal[0].is_error);
        assert_eq!(before_calls.load(Ordering::SeqCst), 1);

        let replay = agent.execute_tools_for_replay(&[call]).await;
        assert!(!replay[0].is_error);
        assert_eq!(before_calls.load(Ordering::SeqCst), 1);
        assert_eq!(after_calls.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn parallel_tool_intents_are_recorded_in_source_order() {
        let mut agent = AgentLoop::new("", None, "test");
        let recorded = Arc::new(StdMutex::new(Vec::new()));
        let recorded_for_callback = recorded.clone();
        agent.tool_intent_recorder = Some(Arc::new(move |id, _, _| {
            let id = id.to_owned();
            let recorded = recorded_for_callback.clone();
            Box::pin(async move {
                if id == "call-1" {
                    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
                }
                recorded.lock().unwrap().push(id);
                Ok(())
            })
        }));

        let calls = ["call-1", "call-2"]
            .into_iter()
            .map(|id| ToolCall {
                id: id.into(),
                r#type: "function".into(),
                function: ToolCallFunction {
                    name: "list_dir".into(),
                    arguments: "{}".into(),
                },
                thought_signature: None,
            })
            .collect::<Vec<_>>();
        let results = agent.execute_tools(&calls).await;

        assert!(results.iter().all(|result| !result.is_error));
        assert_eq!(recorded.lock().unwrap().as_slice(), ["call-1", "call-2"]);
    }

    #[tokio::test]
    async fn tool_completion_recorder_runs_after_execution() {
        let mut agent = AgentLoop::new("", None, "test");
        let completed = Arc::new(StdMutex::new(Vec::new()));
        let completed_for_callback = completed.clone();
        agent.tool_completion_recorder = Some(Arc::new(move |id, terminate| {
            let completed = completed_for_callback.clone();
            let id = id.to_owned();
            Box::pin(async move {
                completed.lock().unwrap().push((id, terminate));
                Ok(())
            })
        }));

        let results = agent
            .execute_tools(&[ToolCall {
                id: "call-1".into(),
                r#type: "function".into(),
                function: ToolCallFunction {
                    name: "list_dir".into(),
                    arguments: "{}".into(),
                },
                thought_signature: None,
            }])
            .await;

        assert!(!results[0].is_error);
        assert_eq!(
            completed.lock().unwrap().as_slice(),
            [("call-1".into(), false)]
        );
    }

    #[test]
    fn set_credentials_updates_provider_routing() {
        let mut agent = AgentLoop::new("sk-openai", None, "test");
        assert_eq!(
            agent.provider_client.determine_format("gpt-5"),
            threadlane_provider::router::PayloadFormat::ChatCompletions
        );

        agent.set_credentials("codex-token", Some("account-id".into()));

        assert_eq!(agent.api_key, "codex-token");
        assert_eq!(agent.account_id.as_deref(), Some("account-id"));
        assert_eq!(
            agent.provider_client.determine_format("gpt-5"),
            threadlane_provider::router::PayloadFormat::Codex
        );
    }
}
