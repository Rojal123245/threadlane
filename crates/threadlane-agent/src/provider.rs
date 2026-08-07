//! Provider adapter abstraction.
//!
//! Each LLM provider (OpenAI Chat Completions, OpenAI Codex Responses, etc.)
//! has its own message format and API payload shape. The [`ProviderAdapter`]
//! trait encapsulates these differences so the agent loop can remain
//! provider-agnostic.
//!
//! The existing free functions `convert_to_llm` and `convert_to_codex_llm`
//! are re-exported for backward compatibility.

use crate::types::{AgentMessage, AgentState, AgentToolDefinition};
use async_trait::async_trait;
use serde_json::Value;
use std::fmt;
use std::sync::Arc;
use threadlane_provider::router::PayloadFormat;

/// Opaque provider-specific message representation.
///
/// Chat Completions providers receive `Vec<Value>` (array of message objects).
/// Codex Responses providers receive `(String, Vec<Value>)` (instructions + input items).
#[derive(Debug, Clone)]
pub enum ProviderMessages {
    ChatMessages(Vec<Value>),
    CodexMessages {
        instructions: String,
        input_items: Vec<Value>,
    },
}

/// Builds provider-specific API payloads from agent state.
///
/// Implementations encapsulate:
/// - Message format conversion (`AgentMessage` → provider messages)
/// - API payload structure (model, tools, streaming, reasoning, cache keys)
#[async_trait]
pub trait ProviderAdapter: fmt::Debug + Send + Sync {
    /// The [`PayloadFormat`] this adapter targets.
    fn format(&self) -> PayloadFormat;

    /// Converts agent messages into the provider's native message format.
    fn convert_messages(&self, messages: &[AgentMessage]) -> ProviderMessages;

    /// Builds a complete API payload from state, tool definitions, and an
    /// optional prompt cache key.
    ///
    /// The `state` is already locked by the caller; the adapter reads it
    /// but must not hold the lock across `.await`.
    fn build_payload(
        &self,
        state: &AgentState,
        tools: &[AgentToolDefinition],
        prompt_cache_key: Option<&str>,
    ) -> Value;
}

/// Chat Completions adapter (OpenAI, Antigravity, OpenCode).
#[derive(Debug, Clone, Default)]
pub struct ChatCompletionsAdapter;

#[async_trait]
impl ProviderAdapter for ChatCompletionsAdapter {
    fn format(&self) -> PayloadFormat {
        PayloadFormat::ChatCompletions
    }

    fn convert_messages(&self, messages: &[AgentMessage]) -> ProviderMessages {
        ProviderMessages::ChatMessages(convert_to_llm(messages))
    }

    fn build_payload(
        &self,
        state: &AgentState,
        tools: &[AgentToolDefinition],
        prompt_cache_key: Option<&str>,
    ) -> Value {
        let api_msgs = convert_to_llm(&state.messages);
        let tools: Vec<_> = tools
            .iter()
            .map(AgentToolDefinition::to_chat_completions_tool)
            .collect();
        let mut chat_payload = serde_json::json!({
            "model": state.model,
            "messages": api_msgs,
            "tools": tools,
            "stream": true,
            "stream_options": { "include_usage": true }
        });
        if let Some(key) = prompt_cache_key {
            chat_payload["prompt_cache_key"] = key.into();
        }
        if let Some(effort) = state.reasoning_effort.as_api_str() {
            chat_payload["reasoning_effort"] = effort.into();
        }
        chat_payload
    }
}

/// Codex Responses adapter.
#[derive(Debug, Clone, Default)]
pub struct CodexResponsesAdapter;

#[async_trait]
impl ProviderAdapter for CodexResponsesAdapter {
    fn format(&self) -> PayloadFormat {
        PayloadFormat::Codex
    }

    fn convert_messages(&self, messages: &[AgentMessage]) -> ProviderMessages {
        let (instructions, input_items) = convert_to_codex_llm(messages);
        ProviderMessages::CodexMessages {
            instructions,
            input_items,
        }
    }

    fn build_payload(
        &self,
        state: &AgentState,
        tools: &[AgentToolDefinition],
        prompt_cache_key: Option<&str>,
    ) -> Value {
        let (instructions, codex_msgs) = convert_to_codex_llm(&state.messages);
        let codex_tools: Vec<_> = tools
            .iter()
            .map(AgentToolDefinition::to_codex_responses_tool)
            .collect();
        let mut codex_payload = serde_json::json!({
            "model": state.model,
            "instructions": instructions,
            "input": codex_msgs,
            "store": false,
            "stream": true,
            "tools": codex_tools
        });
        if let Some(key) = prompt_cache_key {
            codex_payload["prompt_cache_key"] = key.into();
        }
        if let Some(effort) = state.reasoning_effort.as_api_str() {
            codex_payload["reasoning"] = serde_json::json!({
                "effort": effort,
                "summary": "auto"
            });
        }
        codex_payload
    }
}

/// A router that selects the correct [`ProviderAdapter`] for a given model.
///
/// # Example
///
/// ```ignore
/// let router = ProviderRouter::default();
/// let adapter = router.select_for_model("gpt-5.6-luna");
/// let payload = adapter.build_payload(&state, &tools, None);
/// ```
pub struct ProviderRouter {
    adapters: Vec<Arc<dyn ProviderAdapter>>,
}

impl fmt::Debug for ProviderRouter {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ProviderRouter")
            .field("adapter_count", &self.adapters.len())
            .finish()
    }
}

impl Clone for ProviderRouter {
    fn clone(&self) -> Self {
        Self {
            adapters: self.adapters.clone(),
        }
    }
}

impl Default for ProviderRouter {
    fn default() -> Self {
        Self {
            adapters: Vec::new(),
        }
    }
}

impl ProviderRouter {
    /// Creates a router with the default adapters (Chat Completions + Codex).
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers a custom adapter. Later registrations take priority over
    /// earlier ones when selecting by model.
    pub fn register(&mut self, adapter: Arc<dyn ProviderAdapter>) {
        self.adapters.push(adapter);
    }

    /// Returns the first adapter whose format matches the given format.
    pub fn select(&self, format: PayloadFormat) -> Arc<dyn ProviderAdapter> {
        self.adapters
            .iter()
            .find(|adapter| adapter.format() == format)
            .cloned()
            .unwrap_or_else(|| default_adapter_for(format))
    }

    /// Builds a complete payload for the given format, reading state and tools
    /// from the caller.
    pub fn build_payload(
        &self,
        format: PayloadFormat,
        state: &AgentState,
        tools: &[AgentToolDefinition],
        prompt_cache_key: Option<&str>,
    ) -> Value {
        self.select(format)
            .build_payload(state, tools, prompt_cache_key)
    }
}

fn default_adapter_for(format: PayloadFormat) -> Arc<dyn ProviderAdapter> {
    match format {
        PayloadFormat::ChatCompletions => Arc::new(ChatCompletionsAdapter),
        PayloadFormat::Codex => Arc::new(CodexResponsesAdapter),
    }
}

// ── Backward-compatible free-function re-exports ─────────────────────────
// These remain available as standalone functions used by tests and callers
// that convert messages without going through an adapter.
pub use crate::loop_engine::{convert_to_codex_llm, convert_to_llm};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ReasoningEffort;

    #[test]
    fn default_router_has_both_formats() {
        let router = ProviderRouter::default();
        assert!(
            router.select(PayloadFormat::ChatCompletions).format()
                == PayloadFormat::ChatCompletions
        );
        assert!(router.select(PayloadFormat::Codex).format() == PayloadFormat::Codex);
    }

    #[test]
    fn chat_adapter_builds_payload_with_reasoning() {
        let adapter = ChatCompletionsAdapter;
        let mut state = AgentState::new("gpt-4o", "system");
        state.reasoning_effort = ReasoningEffort::High;
        let payload = adapter.build_payload(&state, &[], None);
        assert_eq!(payload["model"], "gpt-4o");
        assert_eq!(payload["reasoning_effort"], "high");
        assert!(payload["stream"].as_bool().unwrap());
    }

    #[test]
    fn codex_adapter_builds_payload_with_reasoning() {
        let adapter = CodexResponsesAdapter;
        let mut state = AgentState::new("gpt-5.6-luna", "system");
        state.reasoning_effort = ReasoningEffort::Low;
        let payload = adapter.build_payload(&state, &[], None);
        assert_eq!(payload["model"], "gpt-5.6-luna");
        assert_eq!(payload["reasoning"]["effort"], "low");
        assert_eq!(payload["reasoning"]["summary"], "auto");
    }

    #[test]
    fn router_builds_correct_payload_per_format() {
        let router = ProviderRouter::default();
        let state = AgentState::new("test-model", "instructions");

        let chat = router.build_payload(PayloadFormat::ChatCompletions, &state, &[], None);
        assert!(chat.get("messages").is_some());

        let codex = router.build_payload(PayloadFormat::Codex, &state, &[], None);
        assert!(codex.get("instructions").is_some());
        assert!(codex.get("input").is_some());
    }
}
