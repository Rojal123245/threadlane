//! Turn loop driver for [`UnifiedAgent`].
//!
//! Encapsulates streaming, auto-compaction, stream rule monitoring, journal
//! recording, tool execution, and queue draining for an active turn sequence.

use crate::compaction::{
    compact_messages_to_token_budget, is_context_overflow_error, should_auto_compact,
};
use crate::config::AgentConfig;
use crate::events::AgentEvent;
use crate::harness::{ErrorCategory, ProviderErrorSummary, ProviderOutcome, TraceString};
use crate::provider::{ProviderRouter, ProviderTraceEvent, ProviderTraceRecorder};
use crate::rules::{StreamRule, StreamRuleMonitor};
use crate::tool_dispatcher::ToolDispatcher;
use crate::types::{AgentMessage, TokenUsage, ToolExecutionMode, TurnState};
use crate::utils::AbortOnDrop;
use regex::Regex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Instant;
use threadlane_provider::openai::{StreamEvent, ToolCall};
use threadlane_provider::router::{PayloadSource, ProviderClient};
use tokio::sync::{broadcast, mpsc, Mutex};

const STREAM_CHECKPOINT_BYTES: usize = 16 * 1024;

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

fn classify_provider_error(error: &str) -> ErrorCategory {
    let error = error.to_ascii_lowercase();
    if error.contains("401")
        || error.contains("authentication")
        || error.contains("invalid api key")
    {
        ErrorCategory::Authentication
    } else if error.contains("403") || error.contains("permission denied") {
        ErrorCategory::Authorization
    } else if threadlane_provider::router::is_quota_or_rate_limit(&error) {
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
    tool_calls: Vec<ToolCall>,
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
    pub(crate) provider_client: ProviderClient,
    pub(crate) provider_router: ProviderRouter,
    pub(crate) prompt_cache_key: Option<String>,
    pub(crate) tool_dispatcher: ToolDispatcher,
    pub(crate) config: AgentConfig,
    pub(crate) event_tx: broadcast::Sender<AgentEvent>,
    pub(crate) harness_event_hub: crate::harness::HarnessEventHub,
    pub(crate) provider_trace_recorder: Option<ProviderTraceRecorder>,
    /// Persists model-visible messages before they may affect another provider
    /// request. Durable runtimes install the canonical session-journal writer.
    pub(crate) message_recorder: Option<crate::provider::AssistantMessageRecorder>,
    pub(crate) model_context_refresh: Option<crate::provider::ModelContextRefresh>,
    pub(crate) stream_rules: Vec<(StreamRule, Regex)>,
    pub(crate) steering_queue: &'a mut Vec<AgentMessage>,
    pub(crate) follow_up_queue: &'a mut Vec<AgentMessage>,
}

impl<'a> TurnDriver<'a> {
    fn emit_event(&self, event: AgentEvent) {
        let _ = self.event_tx.send(event.clone());
        self.harness_event_hub.publish_agent_event(event);
    }

    async fn record_provider_trace(&self, event: ProviderTraceEvent) -> Result<(), String> {
        match self.provider_trace_recorder.as_ref() {
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

            // A durable recorder is the canonical projection boundary. Refresh
            // the transient request copy before every provider attempt so
            // continuations cannot rely on stale in-memory history.
            if let Some(refresh) = self.model_context_refresh.as_ref() {
                if let Err(error) = refresh(self.turn.clone()).await {
                    self.emit_event(AgentEvent::AgentError {
                        error: format!("failed to refresh canonical model context: {error}"),
                    });
                    return;
                }
            }

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
            let tool_definitions = self.tool_dispatcher.configured_tool_definitions();
            let payload_cache_key = pc_key.clone();

            let payload_source = PayloadSource::lazy(model.clone(), {
                let turn_clone = self.turn.clone();
                let router = self.provider_router.clone();
                move |format| {
                    let turn = turn_clone.clone();
                    let router = router.clone();
                    let tool_definitions = tool_definitions.clone();
                    let prompt_cache_key = payload_cache_key.clone();
                    Box::pin(async move {
                        let turn = turn.lock().await;
                        router.build_payload(
                            format,
                            &turn,
                            &tool_definitions,
                            prompt_cache_key.as_deref(),
                        )
                    })
                }
            });

            let _stream_task = AbortOnDrop::new(tokio::spawn(async move {
                client
                    .stream_chat_completion(payload_source, pc_key, stream_tx)
                    .await;
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

            while let Some(evt) = stream_rx.recv().await {
                let _ = provider_step.push(&evt);
                match evt {
                    StreamEvent::ContentToken(token) => {
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
                            log::warn!(
                                "stream rule '{}' matched; aborting current response",
                                matched.rule_id
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
                    StreamEvent::ReasoningToken(token) => {
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
                        if let Err(error) = self
                            .record_provider_trace(ProviderTraceEvent::Finished {
                                attempt: turn_number as u32,
                                request_id: request_id.clone(),
                                outcome: ProviderOutcome::Failed,
                                error: Some(ProviderErrorSummary {
                                    category,
                                    code: None,
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
                            && threadlane_provider::router::is_quota_or_rate_limit(&err)
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
                log::warn!("{error}");
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
            self.turn.lock().await.messages.extend(step_messages.iter().cloned());

            self.emit_event(AgentEvent::MessageEnd {
                message: assistant_msg,
            });

            if captured_tool_calls.is_empty() {
                self.emit_event(AgentEvent::TurnEnd {
                    turn_number,
                    tool_results: Vec::new(),
                });

                // Run Advisor watcher if enabled
                if self.config.model_roles.advisor_enabled {
                    let advisor_model = self.config.model_roles.resolve_advisor(&model).to_string();
                    let evaluator = crate::advisor::AdvisorEvaluator::new(
                        self.provider_client.clone(),
                        advisor_model,
                    );
                    let current_messages = {
                        let turn = self.turn.lock().await;
                        turn.messages.clone()
                    };
                    if let Some(note) = evaluator.evaluate_turn(&current_messages).await {
                        self.emit_event(AgentEvent::AdvisorNote { note: note.clone() });
                        let prompt = note.to_steering_prompt();
                        self.steering_queue
                            .push(AgentMessage::user(prompt, Vec::new()));
                    }
                }

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
                            error: format!("failed to persist follow-up before provider work: {error}"),
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

            // Run Advisor watcher after tool executions if enabled
            if self.config.model_roles.advisor_enabled {
                let advisor_model = self.config.model_roles.resolve_advisor(&model).to_string();
                let evaluator = crate::advisor::AdvisorEvaluator::new(
                    self.provider_client.clone(),
                    advisor_model,
                );
                let current_messages = {
                    let turn = self.turn.lock().await;
                    turn.messages.clone()
                };
                if let Some(note) = evaluator.evaluate_turn(&current_messages).await {
                    self.emit_event(AgentEvent::AdvisorNote { note: note.clone() });
                    let prompt = note.to_steering_prompt();
                    self.steering_queue
                        .push(AgentMessage::user(prompt, Vec::new()));
                }
            }

            if tool_results.iter().any(|r| r.terminate) {
                break;
            }
        }
    }
}
