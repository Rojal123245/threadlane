use crate::compaction::{
    compact_messages_to_token_budget, compaction_summary_text, is_context_overflow_error,
    should_auto_compact, AUTO_COMPACTION_KEEP_RECENT_TOKENS,
};
use crate::events::AgentEvent;
use crate::hooks::{
    AfterToolCallHook, BeforeToolCallHook, ShouldStopAfterTurnHook, ToolExecutor,
    TransformContextHook,
};
use crate::queue::PendingMessageQueue;
use crate::types::{
    AgentMessage, AgentState, AgentToolCall, AgentToolDefinition, AgentToolResult, QueueMode,
    TokenUsage, ToolExecutionMode,
};
use serde_json::Value;
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;
use threadlane_provider::openai::{clamp_prompt_cache_key, ProviderUsage, StreamEvent, ToolCall};
use threadlane_provider::router::{PayloadFormat, PayloadSource, ProviderClient};
use threadlane_tools::{
    execute_tool, execute_tool_in_workspace, get_available_tools, get_codex_tools,
};
use tokio::sync::{broadcast, mpsc, Mutex};

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
            } => {
                let normalized = normalized_tool_call_id(tool_call_id, tool_index);
                tool_index += 1;
                AgentMessage::Tool {
                    tool_call_id: normalized,
                    name: name.clone(),
                    content: content.clone(),
                    is_error: *is_error,
                }
            }
            other => {
                tool_index = 0;
                other.clone()
            }
        })
        .collect()
}

#[derive(Clone)]
struct ToolExecutorRoute {
    executor: Arc<dyn ToolExecutor>,
    tool_names: HashSet<String>,
}

pub type ToolIntentRecorder = Arc<dyn Fn(&str, &str, &str) -> Result<(), String> + Send + Sync>;

#[derive(Clone)]
struct ToolRunContext {
    before_hook: Option<Arc<dyn BeforeToolCallHook>>,
    after_hook: Option<Arc<dyn AfterToolCallHook>>,
    intent_recorder: Option<ToolIntentRecorder>,
    event_tx: broadcast::Sender<AgentEvent>,
    state: Arc<Mutex<AgentState>>,
    tool_routes: Vec<ToolExecutorRoute>,
    allowed_tool_names: Option<HashSet<String>>,
    work_dir: Option<PathBuf>,
}

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
    pub before_tool_call_hook: Option<Arc<dyn BeforeToolCallHook>>,
    pub after_tool_call_hook: Option<Arc<dyn AfterToolCallHook>>,
    pub tool_intent_recorder: Option<ToolIntentRecorder>,
    transform_context_hook: Option<Arc<dyn TransformContextHook>>,
    should_stop_hook: Option<Arc<dyn ShouldStopAfterTurnHook>>,
    pub event_tx: broadcast::Sender<AgentEvent>,
    tool_executors: Vec<Arc<dyn ToolExecutor>>,
    /// Compatibility slot for existing callers. New code should use
    /// `register_tool_executor` so ordering and schema conflicts are validated.
    extension_manager: Option<Arc<dyn ToolExecutor>>,
    pub work_dir: Option<PathBuf>,
    stream_rules: Vec<crate::rules::StreamRule>,
}

impl AgentLoop {
    pub fn new(
        api_key: impl Into<String>,
        account_id: Option<String>,
        model: impl Into<String>,
    ) -> Self {
        let (event_tx, _) = broadcast::channel(500);
        let state = Arc::new(Mutex::new(AgentState::new(
            model,
            "You are threadlane AI coding agent.",
        )));
        let api_key = api_key.into();
        let provider_client = ProviderClient::new(api_key.clone(), account_id.clone());

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
            before_tool_call_hook: None,
            after_tool_call_hook: None,
            tool_intent_recorder: None,
            transform_context_hook: None,
            should_stop_hook: None,
            event_tx,
            tool_executors: Vec::new(),
            extension_manager: None,
            work_dir: None,
            stream_rules: Vec::new(),
        }
    }

    pub fn set_prompt_cache_key(&mut self, key: Option<String>) {
        self.prompt_cache_key = key
            .map(|key| clamp_prompt_cache_key(&key))
            .filter(|key| !key.is_empty());
    }

    /// Restricts both advertised and executable tools. `None` restores the
    /// default behavior where all registered, state, and core tools are available.
    pub fn set_allowed_tool_names(&mut self, allowed_tool_names: Option<HashSet<String>>) {
        self.allowed_tool_names = allowed_tool_names;
    }

    /// Returns the core and registered executor schemas in provider order,
    /// after conflict deduplication and the active allowlist are applied.
    pub fn configured_tool_definitions(&self) -> Vec<AgentToolDefinition> {
        let mut definitions = collect_tool_definitions(
            &[],
            &self.tool_executors,
            self.compatibility_executor().as_ref(),
        );
        if let Some(allowed_tool_names) = &self.allowed_tool_names {
            definitions.retain(|definition| allowed_tool_names.contains(&definition.name));
        }
        definitions
    }

    pub fn register_tool_executor(
        &mut self,
        executor: Arc<dyn ToolExecutor>,
    ) -> Result<(), String> {
        let executor_id = executor.executor_id().trim();
        if executor_id.is_empty() {
            return Err("Tool executor id must not be empty".into());
        }
        if self
            .ordered_tool_executors()
            .iter()
            .any(|registered| registered.executor_id() == executor_id)
        {
            return Err(format!(
                "Tool executor '{executor_id}' is already registered"
            ));
        }

        let mut known_names: HashSet<String> = core_tool_definitions()
            .into_iter()
            .map(|definition| definition.name)
            .collect();
        for registered in self.ordered_tool_executors() {
            known_names.extend(
                registered
                    .tool_definitions()
                    .into_iter()
                    .map(|definition| definition.name),
            );
        }
        for definition in executor.tool_definitions() {
            if definition.name.trim().is_empty() {
                return Err(format!(
                    "Tool executor '{executor_id}' provided an empty tool name"
                ));
            }
            if !known_names.insert(definition.name.clone()) {
                return Err(format!(
                    "Tool schema '{}' from executor '{executor_id}' conflicts with an existing schema",
                    definition.name
                ));
            }
        }

        self.tool_executors.push(executor);
        Ok(())
    }

    pub fn tool_executor_count(&self) -> usize {
        self.ordered_tool_executors().len()
    }

    fn compatibility_executor(&self) -> Option<Arc<dyn ToolExecutor>> {
        self.extension_manager.clone().filter(|compatibility| {
            !self
                .tool_executors
                .iter()
                .any(|registered| registered.executor_id() == compatibility.executor_id())
        })
    }

    fn ordered_tool_executors(&self) -> Vec<Arc<dyn ToolExecutor>> {
        self.tool_executors
            .iter()
            .cloned()
            .chain(self.compatibility_executor())
            .collect()
    }

    async fn tool_execution_routes(&self) -> Vec<ToolExecutorRoute> {
        let state_tools = self.state.lock().await.tools.clone();
        let mut claimed_names: HashSet<String> = core_tool_definitions()
            .into_iter()
            .map(|definition| definition.name)
            .collect();
        let mut routes = Vec::new();

        for executor in &self.tool_executors {
            let tool_names = executor
                .tool_definitions()
                .into_iter()
                .filter_map(|definition| {
                    claimed_names
                        .insert(definition.name.clone())
                        .then_some(definition.name)
                })
                .collect();
            routes.push(ToolExecutorRoute {
                executor: executor.clone(),
                tool_names,
            });
        }

        if let Some(executor) = self.compatibility_executor() {
            let tool_names = executor
                .tool_definitions()
                .into_iter()
                .map(|definition| definition.name)
                .chain(state_tools.iter().filter_map(|schema| {
                    AgentToolDefinition::from_provider_schema(schema)
                        .ok()
                        .map(|definition| definition.name)
                }))
                .filter(|name| claimed_names.insert(name.clone()))
                .collect();
            routes.push(ToolExecutorRoute {
                executor,
                tool_names,
            });
        }

        routes
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

            // Apply context transformation hook if set
            if let Some(ref hook) = self.transform_context_hook {
                let msgs = {
                    let state = self.state.lock().await;
                    state.messages.clone()
                };
                let transformed = hook.transform_context(msgs).await;
                let mut state = self.state.lock().await;
                state.messages = transformed;
            }

            {
                let mut state = self.state.lock().await;
                if should_auto_compact(&state.messages) {
                    state.messages = compact_messages_to_token_budget(
                        &state.messages,
                        AUTO_COMPACTION_KEEP_RECENT_TOKENS,
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

            let payload_source = PayloadSource::lazy(model, move |format| {
                let state = state.clone();
                let tool_executors = tool_executors.clone();
                let allowed_tool_names = allowed_tool_names.clone();
                let compatibility_executor = compatibility_executor.clone();
                let pc_key = pc_key.clone();
                Box::pin(async move {
                    Self::build_payload_helper(
                        &state,
                        &tool_executors,
                        allowed_tool_names.as_ref(),
                        compatibility_executor.as_ref(),
                        pc_key.as_deref(),
                        format,
                    )
                    .await
                })
            });

            let (stream_tx, mut stream_rx) = mpsc::channel(100);
            let client = self.provider_client.clone();
            let prompt_cache_key = self.prompt_cache_key.clone();

            tokio::spawn(async move {
                client
                    .stream_chat_completion(payload_source, prompt_cache_key, stream_tx)
                    .await;
            });

            let _ = self.event_tx.send(AgentEvent::MessageStart {
                role: "assistant".into(),
            });

            let mut current_turn_text = String::new();
            let mut current_turn_reasoning = String::new();
            let mut captured_tool_calls: Vec<ToolCall> = Vec::new();

            let mut stream_monitor =
                crate::rules::StreamRuleMonitor::new(self.stream_rules.clone());
            let mut rule_triggered = None;

            while let Some(evt) = stream_rx.recv().await {
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
                    }
                    StreamEvent::ReasoningToken(token) => {
                        current_turn_reasoning.push_str(&token);
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
                    StreamEvent::Finished { tool_calls, usage } => {
                        captured_tool_calls = tool_calls;
                        total_usage.accumulate(&token_usage_from_provider(usage));
                        break;
                    }
                    StreamEvent::Error(err) => {
                        if !overflow_recovery_attempted && is_context_overflow_error(&err) {
                            let mut state = self.state.lock().await;
                            let compacted = compact_messages_to_token_budget(
                                &state.messages,
                                AUTO_COMPACTION_KEEP_RECENT_TOKENS,
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

            {
                let mut state = self.state.lock().await;
                if !current_turn_reasoning.trim().is_empty() {
                    state.messages.push(AgentMessage::Custom {
                        custom_type: "thinking".into(),
                        payload: serde_json::json!({ "text": current_turn_reasoning }),
                    });
                }
                state.messages.push(assistant_msg.clone());
            }

            let _ = self.event_tx.send(AgentEvent::MessageEnd {
                message: assistant_msg,
            });

            if captured_tool_calls.is_empty() {
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
                });
            }
            drop(state);

            let _ = self.event_tx.send(AgentEvent::TurnEnd {
                turn_number,
                tool_results: tool_results.clone(),
            });

            if let Some(ref hook) = self.should_stop_hook {
                let state = self.state.lock().await.clone();
                if hook
                    .should_stop_after_turn(turn_number, &tool_results, &state)
                    .await
                {
                    break;
                }
            }

            if should_terminate {
                break;
            }
        }

        let _ = self
            .event_tx
            .send(AgentEvent::AgentEnd { usage: total_usage });
    }

    pub async fn execute_tools(&self, tool_calls: &[ToolCall]) -> Vec<AgentToolResult> {
        let mut results = Vec::new();
        let tool_routes = self.tool_execution_routes().await;
        let allowed_tool_names = self.allowed_tool_names.clone();

        if self.tool_execution_mode == ToolExecutionMode::Sequential {
            for tc in tool_calls {
                let res = self
                    .execute_single_tool(tc, tool_routes.clone(), allowed_tool_names.clone())
                    .await;
                results.push(res);
            }
        } else {
            // Parallel execution
            let mut handles = Vec::new();
            for tc in tool_calls {
                let tc_clone = tc.clone();
                let context = ToolRunContext {
                    before_hook: self.before_tool_call_hook.clone(),
                    after_hook: self.after_tool_call_hook.clone(),
                    intent_recorder: self.tool_intent_recorder.clone(),
                    event_tx: self.event_tx.clone(),
                    state: self.state.clone(),
                    tool_routes: tool_routes.clone(),
                    allowed_tool_names: allowed_tool_names.clone(),
                    work_dir: self.work_dir.clone(),
                };

                let handle_tool_call = tc.clone();
                let handle =
                    tokio::spawn(async move { Self::run_tool_with_hooks(tc_clone, context).await });
                handles.push((handle_tool_call, handle));
            }

            for (tool_call, handle) in handles {
                match handle.await {
                    Ok(result) => results.push(result),
                    Err(error) => {
                        let result = AgentToolResult {
                            tool_call_id: tool_call.id.clone(),
                            name: tool_call.function.name.clone(),
                            content: format!("Tool execution task failed: {error}"),
                            is_error: true,
                            terminate: false,
                        };
                        let _ = self.event_tx.send(AgentEvent::ToolExecutionEnd {
                            tool_call_id: tool_call.id,
                            name: tool_call.function.name,
                            result: result.clone(),
                        });
                        results.push(result);
                    }
                }
            }
        }

        results
    }

    async fn execute_single_tool(
        &self,
        tc: &ToolCall,
        tool_routes: Vec<ToolExecutorRoute>,
        allowed_tool_names: Option<HashSet<String>>,
    ) -> AgentToolResult {
        Self::run_tool_with_hooks(
            tc.clone(),
            ToolRunContext {
                before_hook: self.before_tool_call_hook.clone(),
                after_hook: self.after_tool_call_hook.clone(),
                intent_recorder: self.tool_intent_recorder.clone(),
                event_tx: self.event_tx.clone(),
                state: self.state.clone(),
                tool_routes,
                allowed_tool_names,
                work_dir: self.work_dir.clone(),
            },
        )
        .await
    }

    async fn run_tool_with_hooks(tc: ToolCall, context: ToolRunContext) -> AgentToolResult {
        let arguments = normalize_tool_arguments(
            &tc.function.name,
            &tc.function.arguments,
            context.work_dir.as_deref(),
        );
        let agent_tool_call = AgentToolCall {
            id: tc.id.clone(),
            name: tc.function.name.clone(),
            arguments: arguments.clone(),
        };

        if context
            .allowed_tool_names
            .as_ref()
            .is_some_and(|allowed| !allowed.contains(&tc.function.name))
        {
            let result = AgentToolResult {
                tool_call_id: tc.id.clone(),
                name: tc.function.name.clone(),
                content: format!(
                    "Tool '{}' is not allowed by the current agent policy",
                    tc.function.name
                ),
                is_error: true,
                terminate: false,
            };
            let _ = context.event_tx.send(AgentEvent::ToolExecutionEnd {
                tool_call_id: tc.id,
                name: tc.function.name,
                result: result.clone(),
            });
            return result;
        }

        if let Some(ref hook) = context.before_hook {
            let state_snapshot = context.state.lock().await.clone();
            let check = hook
                .before_tool_call(&agent_tool_call, &state_snapshot)
                .await;
            if check.block {
                let res = AgentToolResult {
                    tool_call_id: tc.id.clone(),
                    name: tc.function.name.clone(),
                    content: check
                        .reason
                        .unwrap_or_else(|| "Tool execution blocked by hook".into()),
                    is_error: true,
                    terminate: false,
                };
                let _ = context.event_tx.send(AgentEvent::ToolExecutionEnd {
                    tool_call_id: tc.id.clone(),
                    name: tc.function.name.clone(),
                    result: res.clone(),
                });
                return res;
            }
        }

        if let Some(recorder) = &context.intent_recorder {
            if let Err(error) = recorder(&tc.id, &tc.function.name, &arguments) {
                let result = AgentToolResult {
                    tool_call_id: tc.id.clone(),
                    name: tc.function.name.clone(),
                    content: error,
                    is_error: true,
                    terminate: false,
                };
                let _ = context.event_tx.send(AgentEvent::ToolExecutionEnd {
                    tool_call_id: tc.id,
                    name: tc.function.name,
                    result: result.clone(),
                });
                return result;
            }
        }

        let _ = context.event_tx.send(AgentEvent::ToolExecutionStart {
            tool_call_id: tc.id.clone(),
            name: tc.function.name.clone(),
            arguments: arguments.clone(),
        });

        let mut execution_result = None;
        for route in context.tool_routes {
            if !route.tool_names.contains(&tc.function.name) {
                continue;
            }
            if let Some(result) = route
                .executor
                .execute_tool(&tc.function.name, &arguments)
                .await
            {
                execution_result = Some(result);
                break;
            }
        }
        let execution_result = execution_result.unwrap_or_else(|| {
            Ok(match context.work_dir.as_deref() {
                Some(dir) => execute_tool_in_workspace(&tc.function.name, &arguments, dir),
                None => execute_tool(&tc.function.name, &arguments),
            })
        });
        let (content, is_error) = match execution_result {
            Ok(content) => (content, false),
            Err(error) => (format!("Tool executor error: {error}"), true),
        };
        let mut final_result = AgentToolResult {
            tool_call_id: tc.id.clone(),
            name: tc.function.name.clone(),
            content,
            is_error,
            terminate: false,
        };

        if let Some(ref hook) = context.after_hook {
            let state_snapshot = context.state.lock().await.clone();
            let override_res = hook
                .after_tool_call(&agent_tool_call, &final_result, &state_snapshot)
                .await;
            if let Some(c) = override_res.override_content {
                final_result.content = c;
            }
            if let Some(err) = override_res.override_is_error {
                final_result.is_error = err;
            }
            if let Some(term) = override_res.terminate {
                final_result.terminate = term;
            }
        }

        let _ = context.event_tx.send(AgentEvent::ToolExecutionEnd {
            tool_call_id: tc.id.clone(),
            name: tc.function.name.clone(),
            result: final_result.clone(),
        });

        final_result
    }
}

fn core_tool_definitions() -> Vec<AgentToolDefinition> {
    let mut seen = HashSet::new();
    get_available_tools()
        .into_iter()
        .chain(get_codex_tools())
        .filter_map(|schema| AgentToolDefinition::from_provider_schema(&schema).ok())
        .filter(|definition| seen.insert(definition.name.clone()))
        .collect()
}

fn collect_tool_definitions(
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

fn normalize_tool_arguments(
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
    use std::sync::Mutex as StdMutex;
    use threadlane_provider::openai::{ToolCall, ToolCallFunction};

    #[test]
    fn fills_missing_file_paths_from_the_workspace() {
        let arguments =
            normalize_tool_arguments("read_file", "{}", Some(std::path::Path::new("/workspace")));

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
            },
            AgentMessage::Tool {
                tool_call_id: String::new(),
                name: "list_dir".into(),
                content: "two".into(),
                is_error: false,
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
            },
            AgentMessage::Tool {
                tool_call_id: String::new(),
                name: "list_dir".into(),
                content: "two".into(),
                is_error: false,
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
            *recorded_for_callback.lock().unwrap() =
                Some((id.to_string(), name.to_string(), arguments.to_string()));
            Ok(())
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
        agent.tool_intent_recorder = Some(Arc::new(|_, _, _| Err("intent append failed".into())));
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
}
