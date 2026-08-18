//! Advisor and reviewer model runtime.
//!
//! Evaluates active turns on an isolated context to identify asides,
//! concerns, or hard blockers, allowing the main agent to course-correct.

use crate::types::{AdvisorNote, AdvisorSeverity, AgentMessage};
use serde::{Deserialize, Serialize};
use threadlane_provider::openai::StreamEvent;
use threadlane_provider::router::{PayloadFormat, PayloadSource, ProviderClient};
use tokio::sync::mpsc;

/// System prompt supplied to the Advisor model.
pub const ADVISOR_SYSTEM_PROMPT: &str = r#"You are the AI Advisor and Reviewer for an AI coding assistant.
You monitor the main agent's actions on every turn to catch mistakes, risky code, regressions, or deviations from the objective.

Evaluate the agent's latest turn, reasoning, tool executions, and results.
Respond with ONLY valid JSON with this exact schema:
{
  "severity": "none" | "aside" | "concern" | "blocker",
  "summary": "Short headline (or empty if none)",
  "details": "Specific, actionable note for the agent to course-correct (or empty if none)"
}

Severity levels:
- "none": Everything is progressing accurately and cleanly. No intervention needed.
- "aside": Helpful tip, subtle context, or minor stylistic note (non-blocking).
- "concern": Potential edge case, unhandled error, missing test, or deviation from user intent.
- "blocker": Critical flaw, destructive command, broken build/test, or major regression requiring immediate correction.

Be concise, precise, and practical. Do not output markdown code fences around JSON if possible."#;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct RawAdvisorResponse {
    pub severity: String,
    #[serde(default)]
    pub summary: String,
    #[serde(default)]
    pub details: String,
}

/// Evaluates a turn and produces an optional [`AdvisorNote`].
pub struct AdvisorEvaluator {
    provider_client: ProviderClient,
    model: String,
}

impl AdvisorEvaluator {
    pub fn new(provider_client: ProviderClient, model: impl Into<String>) -> Self {
        Self {
            provider_client,
            model: model.into(),
        }
    }

    /// Evaluates the recent messages in a turn.
    pub async fn evaluate_turn(&self, messages: &[AgentMessage]) -> Option<AdvisorNote> {
        if messages.is_empty() {
            return None;
        }

        // Filter and construct evaluation summary from recent messages (up to 8 most recent messages).
        let recent_slice = if messages.len() > 8 {
            &messages[messages.len() - 8..]
        } else {
            messages
        };

        let mut turn_transcript = String::new();
        for msg in recent_slice {
            match msg {
                AgentMessage::User { content } => {
                    turn_transcript.push_str(&format!("[User Input]:\n{}\n\n", content.trim()));
                }
                AgentMessage::UserWithImages { content, .. } => {
                    turn_transcript.push_str(&format!("[User Input (with images)]:\n{}\n\n", content.trim()));
                }
                AgentMessage::Assistant { content, tool_calls, .. } => {
                    if let Some(text) = content {
                        turn_transcript.push_str(&format!("[Agent Response]:\n{}\n\n", text.trim()));
                    }
                    if let Some(calls) = tool_calls {
                        for call in calls {
                            turn_transcript.push_str(&format!(
                                "[Agent Tool Call]: {} with args: {}\n\n",
                                call.function.name, call.function.arguments
                            ));
                        }
                    }
                }
                AgentMessage::Tool { name, content, is_error, .. } => {
                    let preview = if content.len() > 1500 {
                        format!("{}... [truncated]", &content[..1500])
                    } else {
                        content.clone()
                    };
                    turn_transcript.push_str(&format!(
                        "[Tool Result for {}{}]]:\n{}\n\n",
                        name,
                        if *is_error { " (ERROR)" } else { "" },
                        preview.trim()
                    ));
                }
                AgentMessage::Custom { custom_type, payload } => {
                    if custom_type == "thinking" {
                        if let Some(text) = payload.get("text").and_then(|t| t.as_str()) {
                            let preview = if text.len() > 800 {
                                format!("{}...", &text[..800])
                            } else {
                                text.to_string()
                            };
                            turn_transcript.push_str(&format!("[Agent Thinking]:\n{}\n\n", preview.trim()));
                        }
                    }
                }
                AgentMessage::System { content } => {
                    turn_transcript.push_str(&format!("[System Context]:\n{}\n\n", content.trim()));
                }
            }
        }

        let eval_prompt = format!(
            "Review the following recent turn transcript and assess if advice/blocker is needed:\n\n{}",
            turn_transcript
        );

        let model_str = self.model.clone();
        let eval_prompt_clone = eval_prompt.clone();
        let payload_source = PayloadSource::lazy(model_str.clone(), {
            let model = model_str.clone();
            let eval_prompt = eval_prompt_clone.clone();
            move |format| {
                let model = model.clone();
                let eval_prompt = eval_prompt.clone();
                Box::pin(async move {
                    match format {
                        PayloadFormat::Codex => serde_json::json!({
                            "model": model,
                            "instructions": ADVISOR_SYSTEM_PROMPT,
                            "input": [{
                                "type": "message",
                                "role": "user",
                                "content": [{"type": "input_text", "text": eval_prompt.as_str()}]
                            }],
                            "store": false,
                            "stream": true,
                        }),
                        PayloadFormat::ChatCompletions => serde_json::json!({
                            "model": model,
                            "messages": [
                                {"role": "system", "content": ADVISOR_SYSTEM_PROMPT},
                                {"role": "user", "content": eval_prompt.as_str()}
                            ],
                            "temperature": 0.2,
                            "stream": true,
                        }),
                    }
                })
            }
        });

        let (tx, mut rx) = mpsc::channel(50);
        let client = self.provider_client.clone();
        tokio::spawn(async move {
            client.stream_chat_completion(payload_source, None, tx).await;
        });

        let mut output = String::new();
        while let Some(evt) = rx.recv().await {
            match evt {
                StreamEvent::ContentToken(token) => {
                    output.push_str(&token);
                }
                StreamEvent::ReasoningToken(token) => {
                    // Ignore reasoning tokens in final JSON parse
                    let _ = token;
                }
                StreamEvent::Error(err) => {
                    log::warn!("Advisor model evaluation error: {err}");
                }
                _ => {}
            }
        }

        parse_advisor_response(&output)
    }
}


/// Parses the raw JSON response from an Advisor model into an [`AdvisorNote`].
pub fn parse_advisor_response(raw: &str) -> Option<AdvisorNote> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }

    // Strip markdown code fences if model enclosed JSON
    let json_text = if let Some(stripped) = trimmed.strip_prefix("```json") {
        stripped.strip_suffix("```").unwrap_or(stripped).trim()
    } else if let Some(stripped) = trimmed.strip_prefix("```") {
        stripped.strip_suffix("```").unwrap_or(stripped).trim()
    } else {
        trimmed
    };

    let parsed: RawAdvisorResponse = serde_json::from_str(json_text).ok()?;
    let severity = match parsed.severity.trim().to_lowercase().as_str() {
        "aside" => AdvisorSeverity::Aside,
        "concern" => AdvisorSeverity::Concern,
        "blocker" => AdvisorSeverity::Blocker,
        _ => return None,
    };

    let summary = parsed.summary.trim().to_string();
    let details = parsed.details.trim().to_string();

    if summary.is_empty() && details.is_empty() {
        return None;
    }

    Some(AdvisorNote {
        severity,
        summary: if summary.is_empty() { "Advisor Observation".into() } else { summary },
        details: if details.is_empty() { "Please review the previous action.".into() } else { details },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_valid_aside_json() {
        let json = r#"{
            "severity": "aside",
            "summary": "Consider using std::path::PathBuf",
            "details": "String paths can fail on non-UTF8 filenames."
        }"#;
        let note = parse_advisor_response(json).expect("should parse");
        assert_eq!(note.severity, AdvisorSeverity::Aside);
        assert_eq!(note.summary, "Consider using std::path::PathBuf");
        assert_eq!(note.details, "String paths can fail on non-UTF8 filenames.");
    }

    #[test]
    fn parses_valid_concern_with_fences() {
        let json = "```json\n{\n  \"severity\": \"concern\",\n  \"summary\": \"Missing error handling\",\n  \"details\": \"unwrap() may panic on non-existent file.\"\n}\n```";
        let note = parse_advisor_response(json).expect("should parse");
        assert_eq!(note.severity, AdvisorSeverity::Concern);
        assert_eq!(note.summary, "Missing error handling");
    }

    #[test]
    fn parses_blocker_json() {
        let json = r#"{
            "severity": "blocker",
            "summary": "Infinite recursion detected",
            "details": "The loop condition never updates the counter."
        }"#;
        let note = parse_advisor_response(json).expect("should parse");
        assert_eq!(note.severity, AdvisorSeverity::Blocker);
    }

    #[test]
    fn ignores_none_severity() {
        let json = r#"{
            "severity": "none",
            "summary": "",
            "details": ""
        }"#;
        assert!(parse_advisor_response(json).is_none());
    }
}
