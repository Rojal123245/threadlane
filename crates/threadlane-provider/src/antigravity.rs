use crate::antigravity_auth::{
    get_valid_antigravity_token, load_antigravity_credentials,
};
use futures_util::StreamExt;
use reqwest::header::{AUTHORIZATION, CONTENT_TYPE};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::mpsc;

const DEFAULT_BASE_URL: &str = "https://cloudaicompanion.googleapis.com";
const PUBLIC_BASE_URL: &str = "https://generativelanguage.googleapis.com";

#[derive(Debug, Clone)]
pub enum AntigravityStreamEvent {
    ContentDelta(String),
    ThinkingDelta(String),
    ToolCall {
        id: String,
        name: String,
        arguments: String,
    },
    FinishReason(String),
    Usage {
        prompt_tokens: u32,
        completion_tokens: u32,
    },
    Error(String),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GeminiPart {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", rename = "functionCall")]
    pub function_call: Option<GeminiFunctionCall>,
    #[serde(skip_serializing_if = "Option::is_none", rename = "functionResponse")]
    pub function_response: Option<GeminiFunctionResponse>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GeminiFunctionCall {
    pub name: String,
    pub args: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GeminiFunctionResponse {
    pub name: String,
    pub response: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GeminiContent {
    pub role: String,
    pub parts: Vec<GeminiPart>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GeminiFunctionDecl {
    pub name: String,
    pub description: String,
    pub parameters: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GeminiTool {
    #[serde(rename = "functionDeclarations")]
    pub function_declarations: Vec<GeminiFunctionDecl>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GeminiRequest {
    pub contents: Vec<GeminiContent>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<GeminiTool>>,
    #[serde(skip_serializing_if = "Option::is_none", rename = "systemInstruction")]
    pub system_instruction: Option<GeminiContent>,
    #[serde(skip_serializing_if = "Option::is_none", rename = "generationConfig")]
    pub generation_config: Option<Value>,
}

pub fn build_gemini_request(
    system_prompt: &str,
    messages: &[Value], // Standard JSON representation of messages
    tools: &[Value],    // Standard tool schemas
) -> GeminiRequest {
    let mut contents = Vec::new();

    for msg in messages {
        let role = msg.get("role").and_then(|r| r.as_str()).unwrap_or("user");
        let gemini_role = if role == "assistant" || role == "model" {
            "model".to_string()
        } else {
            "user".to_string()
        };

        let mut parts = Vec::new();
        if let Some(content) = msg.get("content").and_then(|c| c.as_str()) {
            if !content.trim().is_empty() {
                parts.push(GeminiPart {
                    text: Some(content.to_string()),
                    function_call: None,
                    function_response: None,
                });
            }
        }

        // Handle tool calls in assistant message
        if let Some(tool_calls) = msg.get("tool_calls").and_then(|t| t.as_array()) {
            for call in tool_calls {
                if let Some(func) = call.get("function") {
                    let name = func.get("name").and_then(|n| n.as_str()).unwrap_or("").to_string();
                    let args_val = func.get("arguments").and_then(|a| {
                        if let Some(s) = a.as_str() {
                            serde_json::from_str::<Value>(s).ok()
                        } else {
                            Some(a.clone())
                        }
                    }).unwrap_or_else(|| serde_json::json!({}));

                    parts.push(GeminiPart {
                        text: None,
                        function_call: Some(GeminiFunctionCall { name, args: args_val }),
                        function_response: None,
                    });
                }
            }
        }

        // Handle tool response message
        if role == "tool" {
            let name = msg.get("name").and_then(|n| n.as_str()).unwrap_or("tool").to_string();
            let content_str = msg.get("content").and_then(|c| c.as_str()).unwrap_or("");
            let response_val = serde_json::json!({ "result": content_str });

            parts.push(GeminiPart {
                text: None,
                function_call: None,
                function_response: Some(GeminiFunctionResponse { name, response: response_val }),
            });
        }

        if !parts.is_empty() {
            contents.push(GeminiContent {
                role: gemini_role,
                parts,
            });
        }
    }

    let gemini_tools = if !tools.is_empty() {
        let decls = tools
            .iter()
            .filter_map(|t| {
                let name = t.get("name")?.as_str()?.to_string();
                let description = t.get("description").and_then(|d| d.as_str()).unwrap_or("").to_string();
                let parameters = t.get("parameters").cloned().unwrap_or_else(|| serde_json::json!({}));
                Some(GeminiFunctionDecl { name, description, parameters })
            })
            .collect::<Vec<_>>();

        if !decls.is_empty() {
            Some(vec![GeminiTool { function_declarations: decls }])
        } else {
            None
        }
    } else {
        None
    };

    let system_instruction = if !system_prompt.trim().is_empty() {
        Some(GeminiContent {
            role: "user".to_string(),
            parts: vec![GeminiPart {
                text: Some(system_prompt.to_string()),
                function_call: None,
                function_response: None,
            }],
        })
    } else {
        None
    };

    GeminiRequest {
        contents,
        tools: gemini_tools,
        system_instruction,
        generation_config: None,
    }
}

pub struct AntigravityClient {
    client: reqwest::Client,
}

impl AntigravityClient {
    pub fn new() -> Self {
        Self {
            client: reqwest::Client::new(),
        }
    }

    pub async fn stream_generate(
        &self,
        model_id: &str,
        request: GeminiRequest,
    ) -> Result<mpsc::Receiver<AntigravityStreamEvent>, String> {
        let token = get_valid_antigravity_token().await?;
        let creds = load_antigravity_credentials();
        let project_id = creds
            .as_ref()
            .and_then(|c| c.project_id.clone())
            .unwrap_or_else(|| "default".to_string());

        // Strip "antigravity/" prefix if present
        let clean_model = model_id.strip_prefix("antigravity/").unwrap_or(model_id);

        let url = if clean_model.starts_with("gemini-") {
            format!("{DEFAULT_BASE_URL}/v1alpha/projects/{project_id}/locations/global/publishers/google/models/{clean_model}:streamGenerateContent?alt=sse")
        } else {
            format!("{PUBLIC_BASE_URL}/v1beta/models/{clean_model}:streamGenerateContent?alt=sse")
        };

        let response = self
            .client
            .post(&url)
            .header(AUTHORIZATION, format!("Bearer {token}"))
            .header(CONTENT_TYPE, "application/json")
            .header("X-Goog-Api-Client", "antigravity-threadlane/0.1.0")
            .json(&request)
            .send()
            .await
            .map_err(|e| format!("Failed to send Antigravity request: {e}"))?;

        if !response.status().is_success() {
            let status = response.status();
            let err_body = response.text().await.unwrap_or_default();
            return Err(format!("Antigravity API error ({status}): {err_body}"));
        }

        let (tx, rx) = mpsc::channel(100);
        let mut stream = response.bytes_stream();

        tokio::spawn(async move {
            let mut buffer = String::new();

            while let Some(chunk_result) = stream.next().await {
                match chunk_result {
                    Ok(chunk) => {
                        let text = String::from_utf8_lossy(&chunk);
                        buffer.push_str(&text);

                        while let Some(line_end) = buffer.find('\n') {
                            let line = buffer[..line_end].trim().to_string();
                            buffer = buffer[line_end + 1..].to_string();

                            if line.starts_with("data: ") {
                                let json_str = &line["data: ".len()..];
                                if json_str == "[DONE]" {
                                    let _ = tx.send(AntigravityStreamEvent::FinishReason("stop".to_string())).await;
                                    break;
                                }

                                if let Ok(val) = serde_json::from_str::<Value>(json_str) {
                                    parse_and_emit_events(&val, &tx).await;
                                }
                            }
                        }
                    }
                    Err(e) => {
                        let _ = tx
                            .send(AntigravityStreamEvent::Error(format!("Stream error: {e}")))
                            .await;
                        break;
                    }
                }
            }
        });

        Ok(rx)
    }

    pub async fn run_diagnostics(&self) -> String {
        let mut report = vec![];
        report.push("=== Antigravity Doctor Diagnostics ===".to_string());

        // Check 1: Credentials file
        let creds_opt = load_antigravity_credentials();
        match creds_opt {
            Some(ref creds) => {
                report.push("✓ Credentials file found.".to_string());
                if let Some(ref email) = creds.account_email {
                    report.push(format!("  Authenticated Email: {email}"));
                }
                if let Some(ref proj) = creds.project_id {
                    report.push(format!("  GCP Project ID: {proj}"));
                }
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs();
                if creds.expires_at > now {
                    let remaining = creds.expires_at - now;
                    report.push(format!("  Access Token Valid for: {remaining} seconds"));
                } else {
                    report.push("  Access Token is EXPIRED (will auto-refresh on request).".to_string());
                }
                if creds.refresh_token.is_some() {
                    report.push("✓ Refresh Token present.".to_string());
                } else {
                    report.push("⚠️ Refresh Token missing (re-login recommended).".to_string());
                }
            }
            None => {
                report.push("❌ No stored credentials found under ~/.threadlane/antigravity_credentials.json".to_string());
                report.push("   Run /login antigravity to authenticate.".to_string());
                return report.join("\n");
            }
        }

        // Check 2: Active token retrieval
        match get_valid_antigravity_token().await {
            Ok(token) => {
                report.push("✓ Successfully retrieved valid access token.".to_string());

                // Check 3: Google User Info ping
                let res = self
                    .client
                    .get("https://www.googleapis.com/oauth2/v2/userinfo")
                    .bearer_auth(&token)
                    .send()
                    .await;

                match res {
                    Ok(resp) if resp.status().is_success() => {
                        report.push("✓ Google OAuth Token verified against Google API.".to_string());
                    }
                    Ok(resp) => {
                        report.push(format!("⚠️ Google Token check returned status: {}", resp.status()));
                    }
                    Err(e) => {
                        report.push(format!("❌ Network connectivity issue reaching Google API: {e}"));
                    }
                }
            }
            Err(e) => {
                report.push(format!("❌ Failed to refresh/get valid access token: {e}"));
            }
        }

        report.push("\nDiagnostics complete.".to_string());
        report.join("\n")
    }
}

async fn parse_and_emit_events(val: &Value, tx: &mpsc::Sender<AntigravityStreamEvent>) {
    if let Some(candidates) = val.get("candidates").and_then(|v| v.as_array()) {
        for candidate in candidates {
            if let Some(parts) = candidate.get("content").and_then(|c| c.get("parts")).and_then(|p| p.as_array()) {
                for part in parts {
                    // Text delta
                    if let Some(text) = part.get("text").and_then(|t| t.as_str()) {
                        if !text.is_empty() {
                            let _ = tx.send(AntigravityStreamEvent::ContentDelta(text.to_string())).await;
                        }
                    }

                    // Thought / Reasoning delta
                    if let Some(thought) = part.get("thought").and_then(|t| t.as_str()) {
                        if !thought.is_empty() {
                            let _ = tx.send(AntigravityStreamEvent::ThinkingDelta(thought.to_string())).await;
                        }
                    }

                    // Function Call
                    if let Some(fc) = part.get("functionCall") {
                        let name = fc.get("name").and_then(|n| n.as_str()).unwrap_or("").to_string();
                        let args = fc.get("args").map(|a| a.to_string()).unwrap_or_else(|| "{}".to_string());
                        let id = format!("call_{}", rand_id());
                        let _ = tx.send(AntigravityStreamEvent::ToolCall { id, name, arguments: args }).await;
                    }
                }
            }

            if let Some(finish_reason) = candidate.get("finishReason").and_then(|r| r.as_str()) {
                let _ = tx.send(AntigravityStreamEvent::FinishReason(finish_reason.to_string())).await;
            }
        }
    }

    if let Some(usage) = val.get("usageMetadata") {
        let prompt_tokens = usage.get("promptTokenCount").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
        let completion_tokens = usage.get("candidatesTokenCount").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
        let _ = tx.send(AntigravityStreamEvent::Usage { prompt_tokens, completion_tokens }).await;
    }
}

fn rand_id() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!("{:x}", nanos)
}
