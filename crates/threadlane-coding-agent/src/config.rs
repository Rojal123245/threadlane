//! Centralized configuration for the coding agent harness.
//!
//! All tunable parameters for subagents, WASI extensions, network/proc timeout,
//! and recovery behavior live here rather than as scattered `const` items.

use serde::{Deserialize, Serialize};
use std::time::Duration;

/// Configuration for the coding agent harness.
///
/// Every field has a sensible default matching the prior hard-coded constants.
/// Use [`CodingAgentConfig::builder()`] or `CodingAgentConfig::default()`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodingAgentConfig {
    // ── Capability / WASI ───────────────────────────────────────────────
    /// Timeout for capability calls (network, process, etc.).
    pub capability_timeout: Duration,

    /// Maximum bytes buffered for a single capability response.
    pub max_capability_buffer_bytes: usize,

    /// Maximum timeout a WASI process can request (ms).
    pub max_process_timeout_ms: u64,

    /// Maximum output bytes a WASI process can produce.
    pub max_process_output_bytes: usize,

    /// Maximum number of concurrently managed processes.
    pub max_managed_processes: usize,

    /// Default recv timeout for managed processes (ms).
    pub default_recv_timeout_ms: u64,

    /// Maximum recv timeout for managed processes (ms).
    pub max_recv_timeout_ms: u64,

    /// Maximum stdout bytes buffered for a managed process.
    pub max_managed_stdout_bytes: usize,

    /// Maximum broker continuation rounds before failing with an error.
    pub max_broker_continuation_rounds: usize,

    // ── Subagents ───────────────────────────────────────────────────────
    /// Maximum number of tasks a subagent can accept.
    pub max_subagent_tasks: usize,

    /// Maximum characters for a subagent task description.
    pub max_subagent_task_chars: usize,

    /// Maximum concurrent subagents.
    pub subagent_concurrency_limit: usize,

    /// Overall timeout for a single subagent run.
    pub subagent_timeout: Duration,

    /// Prompt used when recovering a subagent from a checkpoint.
    pub subagent_recovery_prompt: String,
}

impl Default for CodingAgentConfig {
    fn default() -> Self {
        Self {
            capability_timeout: Duration::from_secs(2),
            max_capability_buffer_bytes: 64 * 1024,
            max_process_timeout_ms: 120_000,
            max_process_output_bytes: 8 * 1024 * 1024,
            max_managed_processes: 16,
            default_recv_timeout_ms: 5000,
            max_recv_timeout_ms: 30_000,
            max_managed_stdout_bytes: 16 * 1024 * 1024,
            max_broker_continuation_rounds: 4,
            max_subagent_tasks: 8,
            max_subagent_task_chars: 32_000,
            subagent_concurrency_limit: 4,
            subagent_timeout: Duration::from_secs(10 * 60),
            subagent_recovery_prompt:
                "Continue from the recovered checkpoint and finish the assigned task.".into(),
        }
    }
}

impl CodingAgentConfig {
    /// Creates a new [`CodingAgentConfigBuilder`].
    pub fn builder() -> CodingAgentConfigBuilder {
        CodingAgentConfigBuilder::default()
    }
}

/// Builder for [`CodingAgentConfig`].
#[derive(Debug, Clone, Default)]
pub struct CodingAgentConfigBuilder {
    config: CodingAgentConfig,
}

impl CodingAgentConfigBuilder {
    pub fn capability_timeout(mut self, value: Duration) -> Self {
        self.config.capability_timeout = value;
        self
    }

    pub fn max_capability_buffer_bytes(mut self, value: usize) -> Self {
        self.config.max_capability_buffer_bytes = value;
        self
    }

    pub fn max_process_timeout_ms(mut self, value: u64) -> Self {
        self.config.max_process_timeout_ms = value;
        self
    }

    pub fn max_process_output_bytes(mut self, value: usize) -> Self {
        self.config.max_process_output_bytes = value;
        self
    }

    pub fn max_managed_processes(mut self, value: usize) -> Self {
        self.config.max_managed_processes = value;
        self
    }

    pub fn default_recv_timeout_ms(mut self, value: u64) -> Self {
        self.config.default_recv_timeout_ms = value;
        self
    }

    pub fn max_recv_timeout_ms(mut self, value: u64) -> Self {
        self.config.max_recv_timeout_ms = value;
        self
    }

    pub fn max_managed_stdout_bytes(mut self, value: usize) -> Self {
        self.config.max_managed_stdout_bytes = value;
        self
    }

    pub fn max_broker_continuation_rounds(mut self, value: usize) -> Self {
        self.config.max_broker_continuation_rounds = value;
        self
    }

    pub fn max_subagent_tasks(mut self, value: usize) -> Self {
        self.config.max_subagent_tasks = value;
        self
    }

    pub fn max_subagent_task_chars(mut self, value: usize) -> Self {
        self.config.max_subagent_task_chars = value;
        self
    }

    pub fn subagent_concurrency_limit(mut self, value: usize) -> Self {
        self.config.subagent_concurrency_limit = value;
        self
    }

    pub fn subagent_timeout(mut self, value: Duration) -> Self {
        self.config.subagent_timeout = value;
        self
    }

    pub fn subagent_recovery_prompt(mut self, value: impl Into<String>) -> Self {
        self.config.subagent_recovery_prompt = value.into();
        self
    }

    pub fn build(self) -> CodingAgentConfig {
        self.config
    }
}
