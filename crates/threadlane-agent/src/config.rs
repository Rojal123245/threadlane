//! Centralized agent configuration.
//!
//! All tunable parameters for the agent execution loop, compaction, and
//! stream rules live here rather than as scattered `const` items.

use crate::types::ModelRoles;
use serde::{Deserialize, Serialize};
use std::time::Duration;

/// Configuration for the agent execution loop, compaction, and stream rules.
///
/// Every field has a sensible default. Use [`AgentConfig::builder()`] or
/// `AgentConfig::default()` as a starting point and override only what you
/// need.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentConfig {
    // ── Compaction ──────────────────────────────────────────────────────
    /// Estimated token threshold above which auto-compaction triggers.
    pub auto_compaction_threshold_tokens: usize,

    /// Number of tokens to retain from the most recent messages during
    /// token-budget compaction.
    pub auto_compaction_keep_recent_tokens: usize,

    /// Maximum characters for a compaction checkpoint excerpt.
    pub max_checkpoint_chars: usize,

    /// Estimated tokens per image attachment (used for token counting).
    pub estimated_image_tokens: usize,

    // ── Stream Rules ────────────────────────────────────────────────────
    /// Maximum bytes of accumulated streaming text to retain for regex
    /// matching. Text beyond this window is discarded.
    pub stream_rule_max_window_bytes: usize,

    // ── Provider ────────────────────────────────────────────────────────
    /// Default system prompt used when none is explicitly set.
    pub default_system_prompt: String,

    // ── Model Roles ─────────────────────────────────────────────────────
    /// Assigned models for specialized roles (Task, Plan, Advisor).
    #[serde(default)]
    pub model_roles: ModelRoles,

    // ── Tool Execution ──────────────────────────────────────────────────
    /// Timeout for individual tool executions. `None` means no timeout.
    pub tool_execution_timeout: Option<Duration>,

    /// Maximum tool output length in bytes before truncation. `None` means
    /// no limit.
    pub max_tool_output_bytes: Option<usize>,

    // ── Event Channel ───────────────────────────────────────────────────
    /// Capacity of the broadcast channel for [`AgentEvent`]s.
    pub event_channel_capacity: usize,
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            auto_compaction_threshold_tokens: 96_000,
            auto_compaction_keep_recent_tokens: 20_000,
            max_checkpoint_chars: 12_000,
            estimated_image_tokens: 1_200,
            stream_rule_max_window_bytes: 4096,
            default_system_prompt: "You are threadlane AI coding agent.".into(),
            model_roles: ModelRoles::default(),
            tool_execution_timeout: None,
            max_tool_output_bytes: None,
            event_channel_capacity: 500,
        }
    }
}

impl AgentConfig {
    /// Creates a new [`AgentConfigBuilder`].
    pub fn builder() -> AgentConfigBuilder {
        AgentConfigBuilder::default()
    }
}

/// Builder for [`AgentConfig`].
///
/// # Example
///
/// ```ignore
/// let config = AgentConfig::builder()
///     .auto_compaction_threshold_tokens(128_000)
///     .tool_execution_timeout(Duration::from_secs(30))
///     .build();
/// ```
#[derive(Debug, Clone, Default)]
pub struct AgentConfigBuilder {
    config: AgentConfig,
}

impl AgentConfigBuilder {
    pub fn auto_compaction_threshold_tokens(mut self, value: usize) -> Self {
        self.config.auto_compaction_threshold_tokens = value;
        self
    }

    pub fn auto_compaction_keep_recent_tokens(mut self, value: usize) -> Self {
        self.config.auto_compaction_keep_recent_tokens = value;
        self
    }

    pub fn max_checkpoint_chars(mut self, value: usize) -> Self {
        self.config.max_checkpoint_chars = value;
        self
    }

    pub fn estimated_image_tokens(mut self, value: usize) -> Self {
        self.config.estimated_image_tokens = value;
        self
    }

    pub fn stream_rule_max_window_bytes(mut self, value: usize) -> Self {
        self.config.stream_rule_max_window_bytes = value;
        self
    }

    pub fn default_system_prompt(mut self, value: impl Into<String>) -> Self {
        self.config.default_system_prompt = value.into();
        self
    }

    pub fn model_roles(mut self, value: ModelRoles) -> Self {
        self.config.model_roles = value;
        self
    }

    pub fn tool_execution_timeout(mut self, value: Duration) -> Self {
        self.config.tool_execution_timeout = Some(value);
        self
    }

    pub fn max_tool_output_bytes(mut self, value: usize) -> Self {
        self.config.max_tool_output_bytes = Some(value);
        self
    }

    pub fn event_channel_capacity(mut self, value: usize) -> Self {
        self.config.event_channel_capacity = value;
        self
    }

    pub fn build(self) -> AgentConfig {
        self.config
    }
}
