use crate::antigravity::AntigravityClient;
use crate::openai::{OpenAIClient, StreamEvent};
use futures_util::future::BoxFuture;
use serde_json::Value;
use std::sync::Arc;
use tokio::sync::mpsc;

const ANTIGRAVITY_MODEL_PREFIX: &str = "antigravity/";

pub fn is_antigravity_model(model: &str) -> bool {
    model.starts_with(ANTIGRAVITY_MODEL_PREFIX)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PayloadFormat {
    ChatCompletions,
    Codex,
}

pub type LazyPayloadBuilder = Arc<dyn Fn(PayloadFormat) -> BoxFuture<'static, Value> + Send + Sync>;

pub enum PayloadSource {
    Eager {
        chat_payload: Value,
        codex_payload: Value,
    },
    ChatCompletions(Value),
    Codex(Value),
    Lazy {
        model: String,
        builder: LazyPayloadBuilder,
    },
}

impl PayloadSource {
    pub fn lazy<F>(model: impl Into<String>, builder: F) -> Self
    where
        F: Fn(PayloadFormat) -> BoxFuture<'static, Value> + Send + Sync + 'static,
    {
        Self::Lazy {
            model: model.into(),
            builder: Arc::new(builder),
        }
    }

    fn model(&self) -> &str {
        match self {
            PayloadSource::Eager { chat_payload, .. } => chat_payload
                .get("model")
                .and_then(Value::as_str)
                .unwrap_or_default(),
            PayloadSource::ChatCompletions(payload) | PayloadSource::Codex(payload) => payload
                .get("model")
                .and_then(Value::as_str)
                .unwrap_or_default(),
            PayloadSource::Lazy { model, .. } => model.as_str(),
        }
    }

    async fn resolve(self, format: PayloadFormat) -> Value {
        match self {
            PayloadSource::Eager {
                chat_payload,
                codex_payload,
            } => match format {
                PayloadFormat::ChatCompletions => chat_payload,
                PayloadFormat::Codex => codex_payload,
            },
            PayloadSource::ChatCompletions(payload) | PayloadSource::Codex(payload) => payload,
            PayloadSource::Lazy { builder, .. } => builder(format).await,
        }
    }
}

impl From<(Value, Value)> for PayloadSource {
    fn from((chat_payload, codex_payload): (Value, Value)) -> Self {
        Self::Eager {
            chat_payload,
            codex_payload,
        }
    }
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

    fn determine_format(&self, model: &str) -> PayloadFormat {
        if is_antigravity_model(model) {
            PayloadFormat::ChatCompletions
        } else if self.openai.is_codex() {
            PayloadFormat::Codex
        } else {
            PayloadFormat::ChatCompletions
        }
    }

    pub async fn stream_chat_completion(
        &self,
        payload_source: impl Into<PayloadSource>,
        prompt_cache_key: Option<String>,
        event_tx: mpsc::Sender<StreamEvent>,
    ) {
        let source = payload_source.into();
        let format = self.determine_format(source.model());
        let payload = source.resolve(format).await;

        let model = payload
            .get("model")
            .and_then(Value::as_str)
            .unwrap_or_default();

        if is_antigravity_model(model) {
            self.antigravity
                .clone()
                .stream_chat_completion(payload, event_tx)
                .await;
            return;
        }

        self.openai
            .stream_chat_completion(payload, prompt_cache_key, event_tx)
            .await;
    }

    /// Generate a short commit subject from a Git diff without adding a message to the chat.
    pub async fn generate_commit_message(&self, model: &str, diff: &str) -> Result<String, String> {
        let model = model.to_owned();
        let instructions = concat!(
            "You are an expert software engineer generating a Git commit message.\n",
            "Follow Conventional Commits format (`<type>: <description>` or `<type>(<scope>): <description>`).\n",
            "Rules:\n",
            "1. Use imperative, present tense: 'add', 'fix', 'refactor', 'update', 'remove' (not 'added', 'fixed').\n",
            "2. Common types: feat, fix, refactor, style, perf, docs, test, chore.\n",
            "3. Keep the entire commit subject under 72 characters.\n",
            "4. Do not end the subject line with a period.\n",
            "5. Output ONLY the raw commit subject line. Do NOT include quotes, backticks, bullet points, or markdown formatting."
        );
        let prompt = Arc::new(format!(
            "{instructions}\n\nHere is the diff of the changes:\n\n{diff}"
        ));
        let instructions_str = instructions.to_string();
        let model_for_payload = model.clone();
        let payload = PayloadSource::lazy(model.clone(), move |format| {
            let prompt = Arc::clone(&prompt);
            let model = model_for_payload.clone();
            let instructions_str = instructions_str.clone();
            Box::pin(async move {
                match format {
                    PayloadFormat::Codex => serde_json::json!({
                        "model": model,
                        "instructions": instructions_str,
                        "input": [{
                            "type": "message",
                            "role": "user",
                            "content": [{"type": "input_text", "text": prompt.as_str()}]
                        }],
                        "store": false,
                        "stream": true
                    }),
                    PayloadFormat::ChatCompletions => serde_json::json!({
                        "model": model,
                        "messages": [
                            {"role": "system", "content": instructions_str},
                            {"role": "user", "content": prompt.as_str()}
                        ],
                        "max_tokens": 96,
                        "stream": true
                    }),
                }
            })
        });
        let (event_tx, mut event_rx) = mpsc::channel(128);
        let client = self.clone();
        let stream_task = tokio::spawn(async move {
            client
                .stream_chat_completion(payload, None, event_tx)
                .await;
        });

        let mut text = String::new();
        let mut error = None;
        while let Some(event) = event_rx.recv().await {
            match event {
                StreamEvent::ContentToken(token) => text.push_str(&token),
                StreamEvent::Error(message) => error = Some(message),
                StreamEvent::Finished { .. }
                | StreamEvent::ReasoningToken(_)
                | StreamEvent::ToolCallStart { .. }
                | StreamEvent::ToolCallArgsDelta { .. } => {}
            }
        }
        if stream_task.await.is_err() && error.is_none() {
            return Err("commit message generation stream terminated unexpectedly".to_owned());
        }
        if let Some(error) = error {
            return Err(error);
        }
        let text = text
            .trim()
            .trim_matches('`')
            .trim_matches('"')
            .trim_matches('\'')
            .trim()
            .to_owned();
        if text.is_empty() {
            Err("The model returned an empty commit message".to_owned())
        } else {
            Ok(text)
        }
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

    #[test]
    fn determines_payload_format_correctly() {
        let client_std = ProviderClient::new("sk-test", None);
        assert_eq!(
            client_std.determine_format("gpt-4o"),
            PayloadFormat::ChatCompletions
        );
        assert_eq!(
            client_std.determine_format("antigravity/gemini-3.6-flash"),
            PayloadFormat::ChatCompletions
        );

        let client_codex = ProviderClient::new("ey-test", Some("acc-123".into()));
        assert_eq!(
            client_codex.determine_format("gpt-4o"),
            PayloadFormat::Codex
        );
        assert_eq!(
            client_codex.determine_format("antigravity/gemini-3.6-flash"),
            PayloadFormat::ChatCompletions
        );
    }
}
