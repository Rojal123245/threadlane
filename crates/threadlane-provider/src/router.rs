use crate::antigravity::AntigravityClient;
use crate::openai::{OpenAIClient, StreamEvent};
use serde_json::Value;
use tokio::sync::mpsc;

pub const ANTIGRAVITY_MODEL_PREFIX: &str = "antigravity/";

pub fn is_antigravity_model(model: &str) -> bool {
    model.starts_with(ANTIGRAVITY_MODEL_PREFIX)
}

#[derive(Clone)]
pub struct ProviderClient {
    openai: OpenAIClient,
    antigravity: AntigravityClient,
}

impl ProviderClient {
    pub fn new(api_key: impl Into<String>, account_id: Option<String>) -> Self {
        Self {
            openai: OpenAIClient::new(api_key.into(), account_id),
            antigravity: AntigravityClient::new(),
        }
    }

    pub async fn stream_chat_completion(
        &self,
        api_payload: Value,
        codex_payload: Value,
        prompt_cache_key: Option<String>,
        event_tx: mpsc::Sender<StreamEvent>,
    ) {
        let model = api_payload
            .get("model")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if is_antigravity_model(model) {
            self.antigravity
                .clone()
                .stream_chat_completion(api_payload, event_tx)
                .await;
            return;
        }

        self.openai
            .stream_chat_completion(api_payload, codex_payload, prompt_cache_key, event_tx)
            .await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn routes_only_prefixed_models_to_antigravity() {
        assert!(is_antigravity_model("antigravity/gemini-3.6-flash"));
        assert!(!is_antigravity_model("gpt-5.6-luna"));
        assert!(!is_antigravity_model("gemini-3.6-flash"));
    }
}
