use crate::config::AgentConfig;
use crate::types::AgentMessage;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum CompactionStrategy {
    #[default]
    TokenBudget,
    SemanticKeyframes,
}

#[derive(Debug, Clone)]
pub struct CompactionOptions {
    pub max_messages: usize,
    pub preserve_recent: usize,
}

#[derive(Debug, Clone)]
pub struct PreparedCompaction {
    pub messages: Vec<AgentMessage>,
    pub pre_tokens: usize,
    pub post_tokens: usize,
    pub compacted_messages: usize,
    pub retained_tail_tokens: usize,
}

impl Default for CompactionOptions {
    fn default() -> Self {
        Self {
            max_messages: 50,
            preserve_recent: 10,
        }
    }
}

fn estimate_message_tokens(message: &AgentMessage, config: &AgentConfig) -> usize {
    let chars = match message {
        AgentMessage::System { content } | AgentMessage::User { content } => content.len(),
        AgentMessage::UserWithImages { content, images } => {
            return content.len().div_ceil(4) + images.len() * config.estimated_image_tokens;
        }
        AgentMessage::Assistant {
            content,
            tool_calls,
            ..
        } => {
            content.as_deref().map_or(0, str::len)
                + tool_calls.as_ref().map_or(0, |calls| {
                    calls
                        .iter()
                        .map(|call| {
                            call.id.len()
                                + call.r#type.len()
                                + call.function.name.len()
                                + call.function.arguments.len()
                                + call.thought_signature.as_deref().map_or(0, str::len)
                        })
                        .sum()
                })
        }
        AgentMessage::Tool { name, content, .. } => name.len() + content.len(),
        AgentMessage::Custom { payload, .. } => payload.to_string().len(),
    };
    chars.div_ceil(4)
}

fn estimate_context_tokens(messages: &[AgentMessage], config: &AgentConfig) -> usize {
    messages
        .iter()
        .map(|m| estimate_message_tokens(m, config))
        .sum()
}

pub fn estimate_request_tokens(
    messages: &[AgentMessage],
    tool_schema_json: Option<&str>,
    config: &AgentConfig,
) -> usize {
    estimate_context_tokens(messages, config)
        .saturating_add(tool_schema_json.map_or(0, |tools| tools.len().div_ceil(4)))
}

pub fn compact_for_budget(
    messages: &[AgentMessage],
    tool_schema_json: Option<&str>,
    retained_tail_tokens: usize,
    config: &AgentConfig,
) -> Option<PreparedCompaction> {
    let pre_tokens = estimate_request_tokens(messages, tool_schema_json, config);
    let compacted = compact_messages_to_token_budget(messages, retained_tail_tokens);
    if compacted.len() == messages.len() {
        return None;
    }
    let post_tokens = estimate_request_tokens(&compacted, tool_schema_json, config);
    let compacted_messages = messages
        .len()
        .saturating_sub(compacted.len().saturating_sub(1));
    Some(PreparedCompaction {
        messages: compacted,
        pre_tokens,
        post_tokens,
        compacted_messages,
        retained_tail_tokens,
    })
}

pub(crate) fn should_auto_compact(messages: &[AgentMessage], config: &AgentConfig) -> bool {
    estimate_context_tokens(messages, config) > config.auto_compaction_threshold_tokens
}

pub(crate) fn is_context_overflow_error(error: &str) -> bool {
    let error = error.to_ascii_lowercase();
    error.contains("context_length_exceeded")
        || error.contains("context length exceeded")
        || error.contains("maximum context length")
        || error.contains("input exceeds the context window")
        || error.contains("too many tokens")
}

pub fn compaction_summary_text(message: &AgentMessage) -> Option<&str> {
    let AgentMessage::Custom {
        custom_type,
        payload,
    } = message
    else {
        return None;
    };
    if custom_type != "compaction_summary" {
        return None;
    }
    payload.get("summary").and_then(serde_json::Value::as_str)
}

pub fn compact_messages(
    messages: &[AgentMessage],
    options: &CompactionOptions,
) -> Vec<AgentMessage> {
    if messages.len() <= options.max_messages {
        return messages.to_vec();
    }

    let keep_count = options.preserve_recent.min(messages.len());
    compact_from_index(messages, messages.len().saturating_sub(keep_count))
}

pub(crate) fn compact_messages_to_token_budget(
    messages: &[AgentMessage],
    keep_recent_tokens: usize,
) -> Vec<AgentMessage> {
    if messages.len() <= 2 {
        return messages.to_vec();
    }

    let mut tokens = 0;
    let mut start = messages.len();
    for (index, message) in messages.iter().enumerate().rev() {
        if matches!(message, AgentMessage::System { .. }) {
            continue;
        }
        tokens += estimate_message_tokens(message, &AgentConfig::default());
        start = index;
        if tokens >= keep_recent_tokens {
            break;
        }
    }

    start = tool_boundary_safe_start(messages, start);

    compact_from_index(messages, start)
}

fn tool_boundary_safe_start(messages: &[AgentMessage], start: usize) -> usize {
    if start >= messages.len() {
        return start;
    }

    if matches!(messages[start], AgentMessage::Tool { .. }) {
        let mut tool_block_start = start;
        while tool_block_start > 0
            && matches!(messages[tool_block_start - 1], AgentMessage::Tool { .. })
        {
            tool_block_start -= 1;
        }
        if tool_block_start > 0
            && is_complete_tool_exchange(messages, tool_block_start - 1, tool_block_start)
        {
            return tool_block_start - 1;
        }

        let mut after_tools = start;
        while after_tools < messages.len()
            && matches!(messages[after_tools], AgentMessage::Tool { .. })
        {
            after_tools += 1;
        }
        return after_tools;
    }

    if has_tool_calls(&messages[start]) && !is_complete_tool_exchange(messages, start, start + 1) {
        let mut after_exchange = start + 1;
        while after_exchange < messages.len()
            && matches!(messages[after_exchange], AgentMessage::Tool { .. })
        {
            after_exchange += 1;
        }
        return after_exchange;
    }

    start
}

fn has_tool_calls(message: &AgentMessage) -> bool {
    matches!(
        message,
        AgentMessage::Assistant {
            tool_calls: Some(calls),
            ..
        } if !calls.is_empty()
    )
}

fn is_complete_tool_exchange(
    messages: &[AgentMessage],
    assistant_index: usize,
    first_tool_index: usize,
) -> bool {
    let AgentMessage::Assistant {
        tool_calls: Some(calls),
        ..
    } = &messages[assistant_index]
    else {
        return false;
    };
    if calls.is_empty() || first_tool_index >= messages.len() {
        return false;
    }

    let tool_results = messages[first_tool_index..]
        .iter()
        .take_while(|message| matches!(message, AgentMessage::Tool { .. }));
    let result_ids: Vec<&str> = tool_results
        .filter_map(|message| match message {
            AgentMessage::Tool { tool_call_id, .. } => Some(tool_call_id.as_str()),
            _ => None,
        })
        .collect();

    result_ids.len() == calls.len()
        && calls
            .iter()
            .all(|call| result_ids.contains(&call.id.as_str()))
}
fn compact_from_index(messages: &[AgentMessage], mut start: usize) -> Vec<AgentMessage> {
    while start < messages.len() && matches!(messages[start], AgentMessage::System { .. }) {
        start += 1;
    }

    let system_messages: Vec<_> = messages
        .iter()
        .filter(|message| matches!(message, AgentMessage::System { .. }))
        .cloned()
        .collect();
    let dropped: Vec<_> = messages[..start]
        .iter()
        .filter(|message| !matches!(message, AgentMessage::System { .. }))
        .cloned()
        .collect();

    if dropped.is_empty() {
        return messages.to_vec();
    }

    let mut compacted = Vec::new();
    compacted.extend(system_messages);
    compacted.push(AgentMessage::Custom {
        custom_type: "compaction_summary".to_string(),
        payload: serde_json::json!({
            "schema_version": 1,
            "summary": build_checkpoint(&dropped, &AgentConfig::default()),
            "compacted_messages": dropped.len(),
            "checkpoint_kind": "token_budget",
        }),
    });
    compacted.extend(
        messages[start..]
            .iter()
            .filter(|message| !matches!(message, AgentMessage::System { .. }))
            .cloned(),
    );
    compacted
}

pub fn compact_messages_with_strategy(
    messages: &[AgentMessage],
    target_tokens: usize,
    strategy: CompactionStrategy,
) -> Vec<AgentMessage> {
    match strategy {
        CompactionStrategy::TokenBudget => {
            compact_messages_to_token_budget(messages, target_tokens)
        }
        CompactionStrategy::SemanticKeyframes => {
            if messages.len() <= 2 {
                return messages.to_vec();
            }
            let mut keyframes = Vec::new();
            let mut user_keyframes = 0;
            for (idx, msg) in messages.iter().enumerate() {
                if idx == 0 && matches!(msg, AgentMessage::System { .. }) {
                    keyframes.push(msg.clone());
                } else if matches!(
                    msg,
                    AgentMessage::User { .. } | AgentMessage::UserWithImages { .. }
                ) && user_keyframes < 3
                {
                    keyframes.push(msg.clone());
                    user_keyframes += 1;
                }
            }
            let keyframe_tokens: usize = keyframes
                .iter()
                .map(|m| estimate_message_tokens(m, &AgentConfig::default()))
                .sum();
            let remaining_budget = target_tokens.saturating_sub(keyframe_tokens);

            let recent = compact_messages_to_token_budget(messages, remaining_budget);
            let mut result = keyframes;
            let mut result_json: std::collections::HashSet<String> = result
                .iter()
                .filter_map(|m| serde_json::to_string(m).ok())
                .collect();
            for msg in recent {
                if let Ok(json) = serde_json::to_string(&msg) {
                    if result_json.insert(json) {
                        result.push(msg);
                    }
                }
            }
            result
        }
    }
}

/// Squeezes historical tool outputs older than `keep_recent_tool_turns` to save input tokens.
pub fn prune_historical_tool_outputs(
    messages: &[AgentMessage],
    keep_recent_tool_turns: usize,
) -> Vec<AgentMessage> {
    const INLINE_TOOL_OUTPUT_LIMIT: usize = 200;
    let mut tool_seen_count = 0;
    let mut result = Vec::with_capacity(messages.len());

    let mut keep_full = vec![false; messages.len()];
    for (i, msg) in messages.iter().enumerate().rev() {
        if matches!(msg, AgentMessage::Tool { .. }) {
            tool_seen_count += 1;
            if tool_seen_count <= keep_recent_tool_turns {
                keep_full[i] = true;
            }
        }
    }

    for (i, msg) in messages.iter().enumerate() {
        match msg {
            AgentMessage::Tool {
                tool_call_id,
                name,
                content,
                is_error,
                terminate,
            } => {
                if keep_full[i] || content.len() <= INLINE_TOOL_OUTPUT_LIMIT {
                    result.push(msg.clone());
                } else {
                    let pruned_content = format!(
                        "[Historical tool output truncated for '{name}' ({} bytes)]",
                        content.len()
                    );
                    result.push(AgentMessage::Tool {
                        tool_call_id: tool_call_id.clone(),
                        name: name.clone(),
                        content: pruned_content,
                        is_error: *is_error,
                        terminate: *terminate,
                    });
                }
            }
            _ => result.push(msg.clone()),
        }
    }

    result
}

/// Prepares a token-optimal message context for model invocation by squeezing historical tool outputs
/// and applying semantic keyframe compaction.
pub fn prepare_token_optimal_context(
    messages: &[AgentMessage],
    target_tokens: usize,
) -> Vec<AgentMessage> {
    let pruned = prune_historical_tool_outputs(messages, 3);
    compact_messages_with_strategy(
        &pruned,
        target_tokens,
        CompactionStrategy::SemanticKeyframes,
    )
}

fn build_checkpoint(messages: &[AgentMessage], config: &AgentConfig) -> String {
    let mut excerpts = Vec::new();
    let mut used_chars = 0;

    for message in messages.iter().rev() {
        let Some(excerpt) = message_excerpt(message) else {
            continue;
        };
        if used_chars + excerpt.len() > config.max_checkpoint_chars {
            break;
        }
        used_chars += excerpt.len();
        excerpts.push(excerpt);
    }
    excerpts.reverse();

    format!(
        "Context checkpoint from {} earlier messages. Continue the same task using the retained recent messages and these earlier excerpts:\n\n{}",
        messages.len(),
        excerpts.join("\n\n")
    )
}

fn message_excerpt(message: &AgentMessage) -> Option<String> {
    match message {
        AgentMessage::User { content } => Some(format!("User: {content}")),
        AgentMessage::UserWithImages { content, images } => Some(format!(
            "User: {content}\n[{} image attachment(s)]",
            images.len()
        )),
        AgentMessage::Assistant { content, .. } => content
            .as_ref()
            .filter(|content| !content.trim().is_empty())
            .map(|content| format!("Assistant: {content}")),
        AgentMessage::Tool { name, content, .. } => {
            let truncated_content = if content.len() > 400 {
                let head: String = content.chars().take(200).collect();
                let tail_chars: Vec<char> = content.chars().rev().take(150).collect();
                let tail: String = tail_chars.into_iter().rev().collect();
                format!("{head} ... [truncated] ... {tail}")
            } else {
                content.clone()
            };
            Some(format!("Tool {name}: {truncated_content}"))
        }
        AgentMessage::Custom { .. } => compaction_summary_text(message).map(str::to_string),
        AgentMessage::System { .. } => None,
    }
}

#[cfg_attr(not(test), allow(dead_code))]
fn extract_session_insights(messages: &[AgentMessage]) -> (Vec<String>, Vec<String>, Vec<String>) {
    let mut verification = Vec::new();
    let mut gotchas = Vec::new();
    let mut architecture = Vec::new();

    for msg in messages {
        match msg {
            AgentMessage::Tool {
                name,
                content,
                is_error,
                ..
            } => {
                if name == "run_command" {
                    if content.contains("cargo test") && !content.contains("error:") {
                        let line = "cargo test --workspace";
                        if !verification.contains(&line.to_string()) {
                            verification.push(line.to_string());
                        }
                    }
                    if content.contains("cargo check") && !content.contains("error:") {
                        let line = "cargo check -p threadlane-gpui";
                        if !verification.contains(&line.to_string()) {
                            verification.push(line.to_string());
                        }
                    }
                }
                if *is_error
                    && (content.contains("Access denied")
                        || content.contains("Operation not permitted"))
                {
                    let line = "Command execution in restricted environments may require BypassSandbox mode.";
                    if !gotchas.contains(&line.to_string()) {
                        gotchas.push(line.to_string());
                    }
                }
            }
            AgentMessage::Assistant {
                content: Some(text),
                ..
            } if text.contains("GPUI") && text.contains("component") => {
                let line = "UI components in crates/threadlane-gpui/src must use GPUI components.";
                if !architecture.contains(&line.to_string()) {
                    architecture.push(line.to_string());
                }
            }
            _ => {}
        }
    }

    (architecture, gotchas, verification)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ImageAttachment;
    use std::collections::HashSet;
    use threadlane_protocol::{
        RuntimeToolCall as ToolCall, RuntimeToolCallFunction as ToolCallFunction,
    };

    fn tool_exchange_fixture(historical_chars: usize) -> Vec<AgentMessage> {
        vec![
            AgentMessage::System {
                content: "system".into(),
            },
            AgentMessage::User {
                content: "older request".into(),
            },
            AgentMessage::User {
                content: "x".repeat(historical_chars),
            },
            AgentMessage::Assistant {
                content: None,
                tool_calls: Some(vec![ToolCall {
                    id: "call_1".into(),
                    r#type: "function".into(),
                    function: ToolCallFunction {
                        name: "read_file".into(),
                        arguments: "{}".into(),
                    },
                    thought_signature: None,
                }]),
                stop_reason: None,
                deferred_handle: None,
            },
            AgentMessage::Tool {
                tool_call_id: "call_1".into(),
                name: "read_file".into(),
                content: "result".repeat(1_000),
                is_error: false,
                terminate: false,
            },
            AgentMessage::User {
                content: "continue".into(),
            },
        ]
    }

    fn assert_valid_tool_pairs(messages: &[AgentMessage]) {
        let mut pending = HashSet::new();
        for message in messages {
            match message {
                AgentMessage::Assistant {
                    tool_calls: Some(calls),
                    ..
                } if !calls.is_empty() => {
                    assert!(pending.is_empty(), "tool call missing its result");
                    pending.extend(calls.iter().map(|call| call.id.as_str()));
                }
                AgentMessage::Tool { tool_call_id, .. } => {
                    assert!(
                        pending.remove(tool_call_id.as_str()),
                        "tool result missing its assistant tool call"
                    );
                }
                _ => assert!(pending.is_empty(), "tool call missing its result"),
            }
        }
        assert!(pending.is_empty(), "tool call missing its result");
    }

    #[test]
    fn request_estimator_includes_tool_schema_and_images() {
        let config = AgentConfig::default();
        let messages = vec![AgentMessage::UserWithImages {
            content: "x".repeat(400),
            images: vec![ImageAttachment {
                display_name: "image.png".into(),
                data_url: "data:image/png;base64,AA==".into(),
            }],
        }];
        assert_eq!(
            estimate_request_tokens(&messages, Some(&"t".repeat(400)), &config),
            1_400
        );
    }

    #[test]
    fn budget_compaction_retains_complete_tool_exchange() {
        let messages = tool_exchange_fixture(12_000);
        let result = compact_for_budget(&messages, None, 1_000, &AgentConfig::default()).unwrap();
        assert!(compaction_summary_text(&result.messages[1]).is_some());
        assert_valid_tool_pairs(&result.messages);
        assert!(result.post_tokens < result.pre_tokens);
        assert!(result.compacted_messages > 0);
    }
    #[test]
    fn test_compact_messages() {
        let mut msgs = vec![AgentMessage::System {
            content: "System prompt".into(),
        }];
        for i in 0..100 {
            msgs.push(AgentMessage::User {
                content: format!("User message {i}"),
            });
        }

        let compacted = compact_messages(
            &msgs,
            &CompactionOptions {
                max_messages: 20,
                preserve_recent: 5,
            },
        );
        assert!(compacted.len() <= 10);
        assert_eq!(compacted[0].role_str(), "system");
        assert!(compaction_summary_text(&compacted[1]).is_some());
    }

    #[test]
    fn token_compaction_keeps_tool_call_before_tool_result() {
        let mut msgs = vec![AgentMessage::System {
            content: "system".into(),
        }];
        msgs.push(AgentMessage::User {
            content: "older request".into(),
        });
        msgs.push(AgentMessage::Assistant {
            content: None,
            tool_calls: Some(vec![ToolCall {
                id: "call_1".into(),
                r#type: "function".into(),
                function: ToolCallFunction {
                    name: "read_file".into(),
                    arguments: "{}".into(),
                },
                thought_signature: None,
            }]),
            stop_reason: None,
            deferred_handle: None,
        });
        msgs.push(AgentMessage::Tool {
            tool_call_id: "call_1".into(),
            name: "read_file".into(),
            content: "x".repeat(1_000),
            is_error: false,
            terminate: false,
        });

        let compacted = compact_messages_to_token_budget(&msgs, 1);
        assert!(matches!(compacted[2], AgentMessage::Assistant { .. }));
        assert!(matches!(compacted[3], AgentMessage::Tool { .. }));
        assert_valid_tool_pairs(&compacted);
    }

    #[test]
    fn detects_provider_context_overflow_errors() {
        assert!(is_context_overflow_error(
            "OpenAI SSE Error [context_length_exceeded]: input exceeds the context window"
        ));
        assert!(!is_context_overflow_error("rate limit exceeded"));
    }

    #[test]
    fn test_extract_session_insights() {
        let msgs = vec![
            AgentMessage::Tool {
                tool_call_id: "1".into(),
                name: "run_command".into(),
                content: "running cargo test ... finished cleanly".into(),
                is_error: false,
                terminate: false,
            },
            AgentMessage::Assistant {
                content: Some("GPUI component guidelines must be followed.".into()),
                tool_calls: None,
                stop_reason: None,
                deferred_handle: None,
            },
        ];

        let (arch, _gotchas, verify) = extract_session_insights(&msgs);
        assert!(!arch.is_empty());
        assert!(!verify.is_empty());
        assert!(verify.contains(&"cargo test --workspace".to_string()));
    }

    #[test]
    fn test_semantic_keyframe_compaction() {
        let msgs = vec![
            AgentMessage::System {
                content: "System Goal".into(),
            },
            AgentMessage::User {
                content: "Initial User Goal".into(),
            },
            AgentMessage::Assistant {
                content: Some("Intermediate reasoning".into()),
                tool_calls: None,
                stop_reason: None,
                deferred_handle: None,
            },
            AgentMessage::User {
                content: "Latest prompt".into(),
            },
        ];

        let compacted =
            compact_messages_with_strategy(&msgs, 200, CompactionStrategy::SemanticKeyframes);
        assert!(!compacted.is_empty());
        assert_eq!(compacted[0].role_str(), "system");
    }

    #[test]
    fn test_prune_historical_tool_outputs_and_optimal_context() {
        let mut msgs = vec![
            AgentMessage::System {
                content: "system prompt".into(),
            },
            AgentMessage::User {
                content: "initial goal prompt".into(),
            },
        ];

        for i in 0..10 {
            msgs.push(AgentMessage::Assistant {
                content: Some(format!("step {i}")),
                tool_calls: None,
                stop_reason: None,
                deferred_handle: None,
            });
            msgs.push(AgentMessage::Tool {
                tool_call_id: format!("call_{i}"),
                name: "view_file".into(),
                content: "a".repeat(5_000),
                is_error: false,
                terminate: false,
            });
        }

        let pruned = prune_historical_tool_outputs(&msgs, 3);
        assert_eq!(pruned.len(), msgs.len());

        let full_count = pruned
            .iter()
            .filter(
                |m| matches!(m, AgentMessage::Tool { content, .. } if content.contains("aaaaa")),
            )
            .count();
        assert_eq!(full_count, 3);

        let truncated_count = pruned
            .iter()
            .filter(|m| matches!(m, AgentMessage::Tool { content, .. } if content.contains("Historical tool output truncated")))
            .count();
        assert_eq!(truncated_count, 7);

        let optimal = prepare_token_optimal_context(&msgs, 10_000);
        assert!(!optimal.is_empty());
        assert_eq!(optimal[0].role_str(), "system");
    }
}
