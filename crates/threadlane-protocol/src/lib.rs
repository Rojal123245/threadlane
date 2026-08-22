use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuntimeToolCallFunction {
    pub name: String,
    pub arguments: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuntimeToolCall {
    pub id: String,
    pub r#type: String,
    pub function: RuntimeToolCallFunction,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        rename = "thoughtSignature"
    )]
    pub thought_signature: Option<String>,
}
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RuntimeUsage {
    pub input_tokens: u32,
    pub output_tokens: u32,
    pub cache_read_tokens: u32,
    pub cache_write_tokens: u32,
    pub total_tokens: u32,
}
#[derive(Debug, Clone)]
pub enum RuntimeStreamEvent {
    ContentToken(String),
    ReasoningToken(String),
    ToolCallStart {
        name: String,
    },
    ToolCallArgsDelta {
        args_chunk: String,
    },
    Finished {
        tool_calls: Vec<RuntimeToolCall>,
        usage: RuntimeUsage,
    },
    Error(String),
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeferredResponse {
    Pending,
    Ready { content: String },
    Error { message: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuntimeRequest {
    pub model: String,
    pub messages: serde_json::Value,
    pub tools: serde_json::Value,
    pub prompt_cache_key: Option<String>,
    pub reasoning_effort: Option<String>,
}

#[async_trait::async_trait]
pub trait ProviderPort: Send + Sync {
    async fn stream_request(
        &self,
        request: RuntimeRequest,
        events: tokio::sync::mpsc::Sender<RuntimeStreamEvent>,
    );
    async fn fetch_deferred(
        &self,
        model: &str,
        handle_id: &str,
    ) -> Result<DeferredResponse, String>;
    async fn cancel_deferred(&self, model: &str, handle_id: &str) -> Result<(), String>;
    fn provider_kind(&self, model: &str) -> &'static str;
}
