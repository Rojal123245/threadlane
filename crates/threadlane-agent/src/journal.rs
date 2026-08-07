//! Journal bridge between the agent loop and the harness.
//!
//! [`AgentJournal`] replaces the 8 optional recorder callbacks on
//! [`crate::AgentLoop`] with a single trait. Implementations persist
//! agent operations to a durable store (typically a harness journal).

use crate::harness::{HookKind, StreamingState};
use crate::types::{AgentMessage, TokenUsage};
use async_trait::async_trait;

/// A journal that records agent operations for durability.
///
/// The agent loop calls these methods at well-defined points during a turn.
/// A no-op implementation is provided for sessions without persistence.
#[async_trait]
pub trait AgentJournal: Send + Sync {
    /// Called when an assistant message is finalized (text or thinking).
    async fn record_assistant_message(&self, message: AgentMessage) -> Result<(), String>;

    /// Called when a tool result message is appended.
    async fn record_tool_message(&self, message: AgentMessage) -> Result<(), String>;

    /// Called before tool execution to record the intent.
    async fn record_tool_intent(
        &self,
        tool_call_id: &str,
        tool_name: &str,
        arguments: &str,
    ) -> Result<(), String>;

    /// Called after tool execution to record completion.
    async fn record_tool_completion(
        &self,
        tool_call_id: &str,
        terminate: bool,
    ) -> Result<(), String>;

    /// Called when token usage is reported by the provider.
    async fn record_provider_usage(&self, usage: TokenUsage) -> Result<(), String>;

    /// Called when a provider request is discarded (e.g., error before response).
    async fn record_discarded_usage(&self, usage: TokenUsage) -> Result<(), String>;

    /// Called during streaming to update the journal with current state.
    async fn record_streaming_state(&self, state: StreamingState) -> Result<(), String>;

    /// Called before/after provider lifecycle hooks.
    async fn run_provider_hook(&self, kind: HookKind) -> Result<(), String>;
}

/// A journal that does nothing — for draft sessions without a file.
pub struct NoopJournal;

#[async_trait]
impl AgentJournal for NoopJournal {
    async fn record_assistant_message(&self, _message: AgentMessage) -> Result<(), String> {
        Ok(())
    }
    async fn record_tool_message(&self, _message: AgentMessage) -> Result<(), String> {
        Ok(())
    }
    async fn record_tool_intent(
        &self,
        _tool_call_id: &str,
        _tool_name: &str,
        _arguments: &str,
    ) -> Result<(), String> {
        Ok(())
    }
    async fn record_tool_completion(
        &self,
        _tool_call_id: &str,
        _terminate: bool,
    ) -> Result<(), String> {
        Ok(())
    }
    async fn record_provider_usage(&self, _usage: TokenUsage) -> Result<(), String> {
        Ok(())
    }
    async fn record_discarded_usage(&self, _usage: TokenUsage) -> Result<(), String> {
        Ok(())
    }
    async fn record_streaming_state(&self, _state: StreamingState) -> Result<(), String> {
        Ok(())
    }
    async fn run_provider_hook(&self, _kind: HookKind) -> Result<(), String> {
        Ok(())
    }
}
