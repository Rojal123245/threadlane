use crate::openai::StreamEvent;
use crate::router::PayloadSource;
use async_trait::async_trait;
use tokio::sync::mpsc;

#[async_trait]
pub trait ModelProvider: Send + Sync {
    /// Identifier for the model provider (e.g., "openai", "codex", "antigravity").
    fn provider_id(&self) -> &'static str;

    /// Checks if this provider handles the given model identifier.
    fn supports_model(&self, model: &str) -> bool;

    /// Streams a chat completion response over `event_tx`.
    async fn stream_chat_completion(
        &self,
        payload_source: PayloadSource,
        prompt_cache_key: Option<String>,
        event_tx: mpsc::Sender<StreamEvent>,
    );
}
