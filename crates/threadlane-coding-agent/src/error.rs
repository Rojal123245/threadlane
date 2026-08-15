//! Typed errors for the coding-agent crate.
//!
//! Replaces ad-hoc `Result<_, String>` with discriminated error variants.

use thiserror::Error;
use threadlane_agent::AgentError as AgentAgentError;

/// Errors from the coding agent harness and its subsystems.
#[derive(Debug, Error)]
pub enum CodingAgentError {
    /// WASI extension or broker error.
    #[error("WASI error: {0}")]
    Wasi(String),

    /// MCP server connection or tool error.
    #[error("MCP error: {0}")]
    Mcp(String),

    /// ACP agent error.
    #[error("ACP error: {0}")]
    Acp(String),

    /// Skill discovery or loading error.
    #[error("skill error: {0}")]
    Skill(String),

    /// Subagent lifecycle error.
    #[error("subagent error: {0}")]
    Subagent(String),

    /// Harness journal persistence error.
    #[error("harness journal error: {0}")]
    Journal(String),

    /// Initialization or configuration error.
    #[error("initialization error: {0}")]
    Init(String),

    /// Error from the underlying agent runtime.
    #[error(transparent)]
    Agent(#[from] AgentAgentError),

    /// A catch-all.
    #[error("{0}")]
    Other(String),
}
