//! Turn loop driver for [`UnifiedAgent`].
//!
//! Encapsulates streaming, auto-compaction, stream rule monitoring, journal
//! recording, tool execution, and queue draining for an active turn sequence.

use crate::compaction::{
    compact_messages_to_token_budget, is_context_overflow_error, should_auto_compact,
};
use crate::config::AgentConfig;
use crate::events::AgentEvent;
use crate::journal::AgentJournal;
use crate::loop_engine::{AbortOnDrop, ProviderStepAccumulator};
use crate::provider::ProviderRouter;
use crate::rules::{StreamRule, StreamRuleMonitor};
use crate::tool_dispatcher::ToolDispatcher;
use crate::types::{AgentMessage, ToolExecutionMode, TurnState};
use regex::Regex;
use std::sync::Arc;
use threadlane_provider::openai::{StreamEvent, ToolCall};
use threadlane_provider::router::{PayloadSource, ProviderClient};
use tokio::sync::{broadcast, mpsc, Mutex};

pub struct TurnDriver<'a> {
    pub turn: Arc<Mutex<TurnState>>,
    pub provider_client: ProviderClient,
    pub provider_router: ProviderRouter,
    pub prompt_cache_key: Option<String>,
    pub tool_dispatcher: ToolDispatcher,
    pub config: AgentConfig,
    pub journal: Option<Arc<dyn AgentJournal>>,
    pub event_tx: broadcast::Sender<AgentEvent>,
    pub harness_event_hub: crate::harness::HarnessEventHub,
    pub stream_rules: Vec<(StreamRule, Regex)>,
    pub steering_queue: &'a mut Vec<AgentMessage>,
    pub follow_up_queue: &'a mut Vec<AgentMessage>,
}

impl<'a> TurnDriver<'a> {
    fn emit_event(&self, event: AgentEvent) {
        let _ = self.event_tx.send(event.clone());
        self.harness_event_hub.publish_agent_event(event);
    }

    pub async fn run_turns(&mut self) {
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

            self.emit_event(AgentEvent::TurnStart { turn_number });

            // --- Provider streaming ---
            let model = {
                let turn = self.turn.lock().await;
                turn.model.clone()
            };
            let (stream_tx, mut stream_rx) = mpsc::channel(100);
            let client = self.provider_client.clone();
            let pc_key = self.prompt_cache_key.clone();

            let payload_source = PayloadSource::lazy(model, {
                let turn_clone = self.turn.clone();
                let router = self.provider_router.clone();
                move |format| {
                    let turn = turn_clone.clone();
                    let router = router.clone();
                    Box::pin(async move {
                        let turn = turn.lock().await;
                        router.build_payload(format, &*turn, &[], None)
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
                        self.emit_event(AgentEvent::AgentError { error: err });
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

            self.emit_event(AgentEvent::MessageEnd {
                message: assistant_msg,
            });

            if captured_tool_calls.is_empty() {
                self.emit_event(AgentEvent::TurnEnd {
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
            let mut dispatcher = self.tool_dispatcher.clone();
            dispatcher.tool_execution_mode = ToolExecutionMode::Parallel;

            let tool_results = dispatcher.execute_tools(&captured_tool_calls).await;

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
