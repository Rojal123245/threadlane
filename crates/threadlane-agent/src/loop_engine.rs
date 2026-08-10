use crate::compaction::compaction_summary_text;
use crate::types::{AgentMessage, TokenUsage};
use serde_json::Value;
use std::collections::HashSet;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use threadlane_provider::openai::{ProviderUsage, StreamEvent, ToolCall};

pub(crate) use crate::utils::AbortOnDrop;

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

pub use crate::turn_driver::{ProviderStepAccumulator, ProviderStepResult};

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

#[cfg(test)]
mod normalize_tool_arguments_tests {
    use super::*;
    use threadlane_provider::openai::ToolCallFunction;

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
}
