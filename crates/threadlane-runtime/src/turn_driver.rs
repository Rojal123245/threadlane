//! Turn loop driver for [`UnifiedAgent`].
//!
//! Encapsulates streaming, auto-compaction, stream rule monitoring, journal
//! recording, tool execution, and queue draining for an active turn sequence.

use crate::compaction::{
    compact_messages_to_token_budget, is_context_overflow_error, should_auto_compact,
};
use crate::config::AgentConfig;
use crate::events::AgentEvent;
use crate::harness::{
    ContextItemSource, ContextItemStatus, ContextManifestItem, ErrorCategory,
    ProviderErrorSummary, ProviderOutcome, TraceString,
};
use crate::provider::{ProviderTraceEvent, ProviderTraceRecorder};
use crate::rules::{StreamRule, StreamRuleMonitor};
use crate::tool_dispatcher::ToolDispatcher;
use crate::types::{AgentMessage, TokenUsage, ToolExecutionMode, TurnState};
use crate::utils::AbortOnDrop;
use regex::Regex;
use sha2::{Digest, Sha256};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Instant;
use threadlane_protocol::{ProviderPort, RuntimeRequest, RuntimeStreamEvent as StreamEvent, RuntimeToolCall as ToolCall};

use tokio::sync::{broadcast, mpsc, Mutex};

const STREAM_CHECKPOINT_BYTES: usize = 64 * 1024;

async fn persist_messages_with(
    recorder: Option<&crate::provider::AssistantMessageRecorder>,
    messages: &[AgentMessage],
) -> Result<(), String> {
    let Some(recorder) = recorder else {
        return Ok(());
    };
    for message in messages {
        recorder(message.clone()).await?;
    }
    Ok(())
}

fn is_quota_or_rate_limit(error: &str) -> bool {
    let error = error.to_ascii_lowercase();
    error.contains("429") || error.contains("rate limit") || error.contains("rate_limit") || error.contains("quota") || error.contains("too many requests") || error.contains("resource_exhausted")
}

fn classify_provider_error(error: &str) -> ErrorCategory {
    let error = error.to_ascii_lowercase();
    if error.contains("401")
        || error.contains("authentication")
        || error.contains("invalid api key")
    {
        ErrorCategory::Authentication
    } else if error.contains("403") || error.contains("permission denied") {
        ErrorCategory::Authorization
    } else if is_quota_or_rate_limit(&error) {
        ErrorCategory::RateLimit
    } else if error.contains("timeout") || error.contains("timed out") {
        ErrorCategory::Timeout
    } else if error.contains("invalid request") || error.contains("400") {
        ErrorCategory::InvalidRequest
    } else if error.contains("unavailable") || error.contains("503") {
        ErrorCategory::Unavailable
    } else if error.contains("connection") || error.contains("transport") {
        ErrorCategory::Transport
    } else if error.contains("cancel") || error.contains("abort") {
        ErrorCategory::Cancelled
    } else {
        ErrorCategory::Unknown
    }
}

/// Captured result from one provider stream.
#[derive(Debug, Clone)]
#[cfg_attr(not(test), allow(dead_code))]
pub struct ProviderStepResult {
    pub(crate) text: String,
    pub(crate) reasoning: String,
    #[allow(dead_code)]
    pub(crate) tool_calls: Vec<ToolCall>,
    pub(crate) usage: TokenUsage,
}

/// Accumulates streaming deltas into a single [`ProviderStepResult`].
#[derive(Default)]
pub struct ProviderStepAccumulator {
    text: String,
    reasoning: String,
    result: Option<ProviderStepResult>,
}

impl ProviderStepAccumulator {
    pub(crate) fn push(
        &mut self,
        event: &StreamEvent,
    ) -> Result<Option<ProviderStepResult>, String> {
        match event {
            StreamEvent::ContentToken(token) => self.text.push_str(token),
            StreamEvent::ReasoningToken(token) => self.reasoning.push_str(token),
            StreamEvent::ToolCallStart { .. } | StreamEvent::ToolCallArgsDelta { .. } => {}
            StreamEvent::Finished { tool_calls, usage } => {
                let result = ProviderStepResult {
                    text: self.text.clone(),
                    reasoning: self.reasoning.clone(),
                    tool_calls: tool_calls.clone(),
                    usage: TokenUsage {
                        input_tokens: usage.input_tokens,
                        output_tokens: usage.output_tokens,
                        cache_read_tokens: usage.cache_read_tokens,
                        cache_write_tokens: usage.cache_write_tokens,
                        total_tokens: usage.total_tokens,
                    },
                };
                self.result = Some(result.clone());
                return Ok(Some(result));
            }
            StreamEvent::Error(error) => return Err(error.clone()),
        }
        Ok(None)
    }

    pub(crate) fn finish(&self) -> Result<ProviderStepResult, String> {
        self.result
            .clone()
            .ok_or_else(|| "provider stream ended without a final response".into())
    }
}

pub(crate) struct TurnDriver<'a> {
    pub(crate) turn: Arc<Mutex<TurnState>>,
    pub(crate) provider_client: Arc<dyn ProviderPort>,
    pub(crate) prompt_cache_key: Option<String>,
    pub(crate) tool_dispatcher: ToolDispatcher,
    pub(crate) config: AgentConfig,
    pub(crate) event_tx: broadcast::Sender<AgentEvent>,
    pub(crate) harness_event_hub: crate::harness::HarnessEventHub,
    pub(crate) provider_trace_recorder: Option<ProviderTraceRecorder>,
    /// Persists model-visible messages before they may affect another provider
    /// request. Durable runtimes install the canonical session-journal writer.
    pub(crate) message_recorder: Option<crate::provider::AssistantMessageRecorder>,
    pub(crate) stream_rules: Vec<(StreamRule, Regex)>,
    pub(crate) steering_queue: &'a mut Vec<AgentMessage>,
    pub(crate) follow_up_queue: &'a mut Vec<AgentMessage>,
    /// Cached serialized tool definitions JSON to avoid re-serializing on every turn.
    pub(crate) cached_tools_json: Option<String>,
}

impl<'a> TurnDriver<'a> {
    fn emit_event(&self, event: AgentEvent) {
        let _ = self.event_tx.send(event.clone());
        self.harness_event_hub.publish_agent_event(event);
    }

    async fn record_provider_trace(&self, event: ProviderTraceEvent) -> Result<(), String> {
        match &self.provider_trace_recorder {
            Some(recorder) => recorder(event).await,
            None => Ok(()),
        }
    }

    async fn persist_messages(&self, messages: &[AgentMessage]) -> Result<(), String> {
        persist_messages_with(self.message_recorder.as_ref(), messages).await
    }

    pub(crate) async fn run_turns(&mut self) {
        let mut turn_number = 0;
        let mut overflow_recovery_attempted = false;
        let mut stream_rule_recovery_attempted = false;
        let mut provider_fallback_attempted = false;

        'turns: loop {
            turn_number += 1;

            // Drain steering queue into turn state.
            if !self.steering_queue.is_empty() {
                let items: Vec<_> = self.steering_queue.drain(..).collect();
                if let Err(error) = self.persist_messages(&items).await {
                    self.emit_event(AgentEvent::AgentError {
                        error: format!("failed to persist steering before provider work: {error}"),
                    });
                    return;
                }
                let mut turn = self.turn.lock().await;
                turn.messages.extend(items);
            }

            // Auto-compaction is only safe here for runtimes without a durable
            // journal. Durable coding sessions compact at their harness boundary
            // before starting a run, where the checkpoint branch is committed
            // before another provider request can observe it.
            if self.message_recorder.is_none() {
                let mut turn = self.turn.lock().await;
                if should_auto_compact(&turn.messages, &self.config) {
                    turn.messages = compact_messages_to_token_budget(
                        &turn.messages,
                        self.config.auto_compaction_keep_recent_tokens,
                    );
                }
            }

            self.emit_event(AgentEvent::TurnStart { turn_number });

            // --- Provider streaming ---
            let model = {
                let turn = self.turn.lock().await;
                // The session model is the user-facing base selection. The Task
                // role is the model that actually drives execution, including
                // queued turns and session switches.
                let task = self.config.model_roles.resolve_task(&turn.model);
                if provider_fallback_attempted {
                    self.config
                        .model_roles
                        .fallback_after(task)
                        .unwrap_or(task)
                        .to_string()
                } else {
                    task.to_string()
                }
            };
            let provider = self.provider_client.provider_kind(&model).to_string();
            static PROVIDER_REQUEST_COUNTER: AtomicU64 = AtomicU64::new(1);
            let request_id = format!(
                "provider-request-{}",
                PROVIDER_REQUEST_COUNTER.fetch_add(1, Ordering::Relaxed)
            );
            if let Err(error) = self
                .record_provider_trace(ProviderTraceEvent::Started {
                    attempt: turn_number as u32,
                    request_id: request_id.clone(),
                    model: model.clone(),
                    provider,
                })
                .await
            {
                self.emit_event(AgentEvent::AgentError {
                    error: format!("failed to persist provider request start: {error}"),
                });
                return;
            }
            let request_started_at = Instant::now();
            let mut provider_terminal_recorded = false;
            let (stream_tx, mut stream_rx) = mpsc::channel(100);
            let client = self.provider_client.clone();
            let pc_key = self.prompt_cache_key.clone();
            let configured_tool_definitions = self.tool_dispatcher.configured_tool_definitions();
            let query = {
                let turn = self.turn.lock().await;
                turn.messages.iter().rev().find_map(|message| match message {
                    AgentMessage::User { content } | AgentMessage::UserWithImages { content, .. } => {
                        Some(content.clone())
                    }
                    _ => None,
                })
            };
            let tool_definitions = crate::local_tool_router::shortlist_from_environment(
                &query.unwrap_or_default(),
                &configured_tool_definitions,
                self.config.needle_enabled,
            )
            .await;
            let payload_cache_key = pc_key.clone();

            let request = {
                let turn = self.turn.lock().await;
                RuntimeRequest {
                    model: model.clone(),
                    messages: serde_json::to_value(&turn.messages).unwrap_or_default(),
                    tools: serde_json::Value::Array(
                        tool_definitions
                            .iter()
                            .map(|tool| tool.to_chat_completions_tool())
                            .collect(),
                    ),
                    prompt_cache_key: payload_cache_key.clone(),
                    reasoning_effort: turn.reasoning_effort.as_api_str().map(str::to_owned),
                }
            };

            let manifest_items = {
                let turn = self.turn.lock().await;
                let mut items = Vec::new();
                for (idx, msg) in turn.messages.iter().enumerate() {
                    let role = msg.role_str();
                    let custom_serialized;
                    let content_str = match msg {
                        AgentMessage::User { content } => content.as_str(),
                        AgentMessage::UserWithImages { content, .. } => content.as_str(),
                        AgentMessage::Assistant { content, .. } => content.as_deref().unwrap_or(""),
                        AgentMessage::System { content } => content.as_str(),
                        AgentMessage::Tool { content, .. } => content.as_str(),
                        AgentMessage::Custom { payload, .. } => {
                            custom_serialized = payload.to_string();
                            custom_serialized.as_str()
                        }
                    };
                    let digest = format!("{:x}", Sha256::digest(content_str.as_bytes()));
                    let token_estimate = ((content_str.len() + 3) / 4) as u32;
                    let source = match msg {
                        AgentMessage::System { .. } => ContextItemSource::SystemPrompt,
                        AgentMessage::Tool { .. } => ContextItemSource::Skill,
                        _ => ContextItemSource::Message,
                    };
                    if let (Ok(role_trace), Ok(digest_trace)) = (
                        TraceString::new(role),
                        TraceString::new(digest),
                    ) {
                        items.push(ContextManifestItem {
                            position: idx,
                            source,
                            entry_id: None,
                            role: role_trace,
                            token_estimate,
                            status: ContextItemStatus::Active,
                            digest_sha256: digest_trace,
                            label: None,
                        });
                    }
                }
                if !tool_definitions.is_empty() {
                    let tools_json = self
                        .cached_tools_json
                        .as_deref()
                        .unwrap_or(&serde_json::to_string(&tool_definitions).unwrap_or_default())
                        .to_string();
                    let digest = format!("{:x}", Sha256::digest(tools_json.as_bytes()));
                    let token_estimate = ((tools_json.len() + 3) / 4) as u32;
                    if let (Ok(role_trace), Ok(digest_trace), Ok(label_trace)) = (
                        TraceString::new("tools"),
                        TraceString::new(digest),
                        TraceString::new(format!("{} tools", tool_definitions.len())),
                    ) {
                        items.push(ContextManifestItem {
                            position: items.len(),
                            source: ContextItemSource::ToolSchema,
                            entry_id: None,
                            role: role_trace,
                            token_estimate,
                            status: ContextItemStatus::Active,
                            digest_sha256: digest_trace,
                            label: Some(label_trace),
                        });
                    }
                }
                items
            };
            let total_estimated_tokens: u32 = manifest_items.iter().map(|item| item.token_estimate).sum();
            let _ = self
                .record_provider_trace(ProviderTraceEvent::ContextManifest {
                    attempt: turn_number as u32,
                    request_id: request_id.clone(),
                    total_estimated_tokens: Some(total_estimated_tokens),
                    items: manifest_items,
                })
                .await;

            let _stream_task = AbortOnDrop::new(tokio::spawn(async move {
                client.stream_request(request, stream_tx).await;
            }));

            self.emit_event(AgentEvent::MessageStart {
                role: "assistant".into(),
            });

            let mut current_text = String::new();
            let mut current_reasoning = String::new();
            let mut checkpoint_index = 0u32;
            let mut checkpointed_bytes = 0usize;
            let mut captured_tool_calls: Vec<ToolCall> = Vec::new();
            let mut provider_step = ProviderStepAccumulator::default();
            let mut monitor = StreamRuleMonitor::new(self.stream_rules.clone(), &self.config);
            let mut stream_rule_matched = false;

            let mut pending_evt = None;
            while let Some(evt) = match pending_evt.take() {
                Some(evt) => Some(evt),
                None => stream_rx.recv().await,
            } {
                let _ = provider_step.push(&evt);
                match evt {
                    StreamEvent::ContentToken(mut token) => {
                        while let Ok(next_evt) = stream_rx.try_recv() {
                            let _ = provider_step.push(&next_evt);
                            match next_evt {
                                StreamEvent::ContentToken(next_token) => {
                                    token.push_str(&next_token);
                                }
                                other => {
                                    pending_evt = Some(other);
                                    break;
                                }
                            }
                        }
                        current_text.push_str(&token);
                        if current_text.len().saturating_sub(checkpointed_bytes)
                            >= STREAM_CHECKPOINT_BYTES
                        {
                            checkpoint_index = checkpoint_index.saturating_add(1);
                            if let Err(error) = self
                                .record_provider_trace(ProviderTraceEvent::Checkpoint {
                                    attempt: turn_number as u32,
                                    request_id: request_id.clone(),
                                    checkpoint_index,
                                    text: current_text.clone(),
                                    reasoning: None,
                                })
                                .await
                            {
                                self.emit_event(AgentEvent::AgentError {
                                    error: format!("failed to persist stream checkpoint: {error}"),
                                });
                                return;
                            }
                            checkpointed_bytes = current_text.len();
                        }
                        if let Some(matched) = monitor.push_chunk(&token) {
                            tracing::warn!(
                                rule_id = %matched.rule_id,
                                "stream rule matched; aborting current response"
                            );
                            self.emit_event(AgentEvent::MessageEnd {
                                message: AgentMessage::Assistant {
                                    content: None,
                                    tool_calls: None,
                                    stop_reason: Some("stream_rule_abort".into()),
                                    deferred_handle: None,
                                },
                            });
                            self.turn.lock().await.messages.push(AgentMessage::user(
                                format!(
                                    "System reminder from rule '{}': {}",
                                    matched.rule_name, matched.reminder
                                ),
                                Vec::new(),
                            ));
                            monitor.reset();
                            stream_rule_matched = true;
                            if current_text.len() > checkpointed_bytes {
                                checkpoint_index = checkpoint_index.saturating_add(1);
                                let _ = self
                                    .record_provider_trace(ProviderTraceEvent::Checkpoint {
                                        attempt: turn_number as u32,
                                        request_id: request_id.clone(),
                                        checkpoint_index,
                                        text: current_text.clone(),
                                        reasoning: None,
                                    })
                                    .await;
                            }
                            let _ = self
                                .record_provider_trace(ProviderTraceEvent::Finished {
                                    attempt: turn_number as u32,
                                    request_id: request_id.clone(),
                                    outcome: ProviderOutcome::Aborted,
                                    error: Some(ProviderErrorSummary {
                                        category: ErrorCategory::Cancelled,
                                        code: TraceString::new("stream_rule_abort").ok(),
                                        retryable: true,
                                    }),
                                    duration_ms: request_started_at.elapsed().as_millis() as u64,
                                    usage: None,
                                })
                                .await;
                            provider_terminal_recorded = true;
                            break;
                        }
                        self.emit_event(AgentEvent::MessageUpdate {
                            text_delta: Some(token),
                            reasoning_delta: None,
                            tool_call_name: None,
                        });
                    }
                    StreamEvent::ReasoningToken(mut token) => {
                        while let Ok(next_evt) = stream_rx.try_recv() {
                            let _ = provider_step.push(&next_evt);
                            match next_evt {
                                StreamEvent::ReasoningToken(next_token) => {
                                    token.push_str(&next_token);
                                }
                                other => {
                                    pending_evt = Some(other);
                                    break;
                                }
                            }
                        }
                        current_reasoning.push_str(&token);
                        self.emit_event(AgentEvent::MessageUpdate {
                            text_delta: None,
                            reasoning_delta: Some(token),
                            tool_call_name: None,
                        });
                    }
                    StreamEvent::ToolCallStart { name, .. } => {
                        self.emit_event(AgentEvent::MessageUpdate {
                            text_delta: None,
                            reasoning_delta: None,
                            tool_call_name: Some(name),
                        });
                    }
                    StreamEvent::ToolCallArgsDelta { .. } => {}
                    StreamEvent::Finished { tool_calls, usage } => {
                        captured_tool_calls = tool_calls;
                        let usage = TokenUsage {
                            input_tokens: usage.input_tokens,
                            output_tokens: usage.output_tokens,
                            cache_read_tokens: usage.cache_read_tokens,
                            cache_write_tokens: usage.cache_write_tokens,
                            total_tokens: usage.total_tokens,
                        };
                        tracing::info!(
                            turn = turn_number,
                            tool_calls = captured_tool_calls.len(),
                            total_tokens = usage.total_tokens,
                            "provider turn finished"
                        );
                        if let Err(error) = self
                            .record_provider_trace(ProviderTraceEvent::Finished {
                                attempt: turn_number as u32,
                                request_id: request_id.clone(),
                                outcome: ProviderOutcome::Completed,
                                error: None,
                                duration_ms: request_started_at.elapsed().as_millis() as u64,
                                usage: Some(usage),
                            })
                            .await
                        {
                            self.emit_event(AgentEvent::AgentError {
                                error: format!(
                                    "failed to persist provider request finish: {error}"
                                ),
                            });
                            return;
                        }
                        provider_terminal_recorded = true;
                        break;
                    }
                    StreamEvent::Error(err) => {
                        tracing::error!(turn = turn_number, error = %err, "provider turn failed");
                        if current_text.len() > checkpointed_bytes {
                            checkpoint_index = checkpoint_index.saturating_add(1);
                            if let Err(error) = self
                                .record_provider_trace(ProviderTraceEvent::Checkpoint {
                                    attempt: turn_number as u32,
                                    request_id: request_id.clone(),
                                    checkpoint_index,
                                    text: current_text.clone(),
                                    reasoning: None,
                                })
                                .await
                            {
                                self.emit_event(AgentEvent::AgentError {
                                    error: format!("failed to persist error checkpoint: {error}"),
                                });
                                return;
                            }
                        }
                        let category = classify_provider_error(&err);
                        let retryable = matches!(
                            category,
                            ErrorCategory::RateLimit
                                | ErrorCategory::Timeout
                                | ErrorCategory::Transport
                                | ErrorCategory::Unavailable
                        );
                        // Store the full provider error text so the trajectory
                        // inspector can show it inline with the request row.
                        let error_code =
                            TraceString::new(err.chars().take(2048).collect::<String>()).ok();
                        if let Err(error) = self
                            .record_provider_trace(ProviderTraceEvent::Finished {
                                attempt: turn_number as u32,
                                request_id: request_id.clone(),
                                outcome: ProviderOutcome::Failed,
                                error: Some(ProviderErrorSummary {
                                    category,
                                    code: error_code,
                                    retryable,
                                }),
                                duration_ms: request_started_at.elapsed().as_millis() as u64,
                                usage: None,
                            })
                            .await
                        {
                            self.emit_event(AgentEvent::AgentError {
                                error: format!(
                                    "failed to persist provider request failure: {error}"
                                ),
                            });
                            return;
                        }
                        if !overflow_recovery_attempted
                            && self.message_recorder.is_none()
                            && is_context_overflow_error(&err)
                        {
                            let mut turn = self.turn.lock().await;
                            turn.messages = compact_messages_to_token_budget(
                                &turn.messages,
                                self.config.auto_compaction_keep_recent_tokens,
                            );
                            overflow_recovery_attempted = true;
                            continue 'turns;
                        }
                        if !provider_fallback_attempted
                            && current_text.is_empty()
                            && is_quota_or_rate_limit(&err)
                        {
                            provider_fallback_attempted = true;
                            let fallback = self.config.model_roles.fallback_after(&model);
                            let reminder = match fallback {
                                Some(fallback) => format!("System: primary provider is rate-limited; retry this identical turn using fallback model {fallback}."),
                                None => "System: primary provider is rate-limited; retry this identical turn using the configured fallback route.".into(),
                            };
                            self.turn
                                .lock()
                                .await
                                .messages
                                .push(AgentMessage::user(reminder, Vec::new()));
                            continue 'turns;
                        }
                        self.emit_event(AgentEvent::AgentError { error: err });
                        return;
                    }
                }
            }

            if !provider_terminal_recorded {
                if let Err(error) = self
                    .record_provider_trace(ProviderTraceEvent::Finished {
                        attempt: turn_number as u32,
                        request_id: request_id.clone(),
                        outcome: ProviderOutcome::Failed,
                        error: Some(ProviderErrorSummary {
                            category: ErrorCategory::Protocol,
                            code: TraceString::new("stream_closed_without_terminal_event").ok(),
                            retryable: true,
                        }),
                        duration_ms: request_started_at.elapsed().as_millis() as u64,
                        usage: None,
                    })
                    .await
                {
                    self.emit_event(AgentEvent::AgentError {
                        error: format!("failed to persist incomplete provider request: {error}"),
                    });
                    return;
                }
            }

            if stream_rule_matched {
                if stream_rule_recovery_attempted {
                    self.emit_event(AgentEvent::AgentError {
                        error: "stream rule matched again after corrective retry".into(),
                    });
                    return;
                }
                stream_rule_recovery_attempted = true;
                // Do not persist or emit the partial completion. The injected reminder
                // already entered canonical turn state; continue creates the corrected retry.
                continue;
            }

            if current_text.trim().is_empty() && captured_tool_calls.is_empty() {
                let error = match provider_step.finish() {
                    Ok(_) => {
                        let phase = if turn_number > 1 {
                            " after tool execution"
                        } else {
                            ""
                        };
                        format!("Provider returned an empty response{phase} (turn {turn_number})")
                    }
                    Err(error) => format!(
                        "Provider stream ended without a final response (turn {turn_number}): {error}"
                    ),
                };
                tracing::warn!(turn = turn_number, error = %error, "provider returned no usable response");
                self.emit_event(AgentEvent::AgentError { error });
                return;
            }

            // Record assistant message in turn state.
            let assistant_msg = AgentMessage::Assistant {
                content: if current_text.is_empty() {
                    None
                } else {
                    Some(current_text.clone())
                },
                tool_calls: if captured_tool_calls.is_empty() {
                    None
                } else {
                    Some(captured_tool_calls.clone())
                },
                stop_reason: None,
                deferred_handle: None,
            };

            let mut step_messages = Vec::new();
            if !current_reasoning.trim().is_empty() {
                let thinking = AgentMessage::Custom {
                    custom_type: "thinking".into(),
                    payload: serde_json::json!({ "text": current_reasoning }),
                };
                step_messages.push(thinking);
            }

            step_messages.push(assistant_msg.clone());

            // Persist the typed assistant transition before exposing it to the
            // next continuation. The transient turn copy is updated only
            // after the canonical commit succeeds.
            if let Err(error) = self.persist_messages(&step_messages).await {
                self.emit_event(AgentEvent::AgentError {
                    error: format!("failed to persist assistant step before continuation: {error}"),
                });
                return;
            }
            if let Err(error) = self
                .record_provider_trace(ProviderTraceEvent::AssistantReady {
                    attempt: turn_number as u32,
                    request_id: request_id.clone(),
                    reasoning: (!current_reasoning.trim().is_empty())
                        .then(|| current_reasoning.clone()),
                    message: assistant_msg.clone(),
                })
                .await
            {
                self.emit_event(AgentEvent::AgentError {
                    error: format!("failed to persist provider assistant result: {error}"),
                });
                return;
            }
            self.turn
                .lock()
                .await
                .messages
                .extend(step_messages.iter().cloned());

            self.emit_event(AgentEvent::MessageEnd {
                message: assistant_msg,
            });

            if captured_tool_calls.is_empty() {
                self.emit_event(AgentEvent::TurnEnd {
                    turn_number,
                    tool_results: Vec::new(),
                });

                if !self.steering_queue.is_empty() {
                    let items: Vec<_> = self.steering_queue.drain(..).collect();
                    if let Err(error) = self.persist_messages(&items).await {
                        self.emit_event(AgentEvent::AgentError {
                            error: format!("failed to persist steering before retry: {error}"),
                        });
                        return;
                    }
                    self.turn.lock().await.messages.extend(items);
                    continue;
                }

                if !self.follow_up_queue.is_empty() {
                    let items: Vec<_> = self.follow_up_queue.drain(..).collect();
                    if let Err(error) = self.persist_messages(&items).await {
                        self.emit_event(AgentEvent::AgentError {
                            error: format!(
                                "failed to persist follow-up before provider work: {error}"
                            ),
                        });
                        return;
                    }
                    self.turn.lock().await.messages.extend(items);
                    continue;
                }
                break;
            }

            // Persist a bounded handoff checkpoint before external tool work.
            if current_text.len() > checkpointed_bytes {
                checkpoint_index = checkpoint_index.saturating_add(1);
                if let Err(error) = self
                    .record_provider_trace(ProviderTraceEvent::Checkpoint {
                        attempt: turn_number as u32,
                        request_id: request_id.clone(),
                        checkpoint_index,
                        text: current_text.clone(),
                        reasoning: None,
                    })
                    .await
                {
                    self.emit_event(AgentEvent::AgentError {
                        error: format!("failed to persist pre-tool checkpoint: {error}"),
                    });
                    return;
                }
            }

            // Execute tools.
            let mut dispatcher = self.tool_dispatcher.clone();
            dispatcher.tool_execution_mode = ToolExecutionMode::Parallel;

            let tool_results = dispatcher.execute_tools(&captured_tool_calls).await;

            // Persist tool results before they can affect the continuation
            // request. Tool lifecycle recorders may enrich the same durable
            // operation, while this recorder guarantees model visibility.
            let tool_messages = tool_results
                .iter()
                .map(|result| AgentMessage::Tool {
                    tool_call_id: result.tool_call_id.clone(),
                    name: result.name.clone(),
                    content: result.content.clone(),
                    is_error: result.is_error,
                    terminate: result.terminate,
                })
                .collect::<Vec<_>>();
            if let Err(error) = self.persist_messages(&tool_messages).await {
                self.emit_event(AgentEvent::AgentError {
                    error: format!("failed to persist tool results before continuation: {error}"),
                });
                return;
            }
            self.turn.lock().await.messages.extend(tool_messages);

            self.emit_event(AgentEvent::TurnEnd {
                turn_number,
                tool_results: tool_results.clone(),
            });

            if tool_results.iter().any(|r| r.terminate) {
                break;
            }
        }
    }
}
