//! Shared runtime services created once per process.
//!
//! [`AppRuntime`] owns the Tokio runtime, provider router, and global
//! registries. All sessions share one `AppRuntime`.

use crate::config::AgentConfig;
use crate::harness::HookRegistry;
use crate::provider::ProviderRouter;
use std::path::PathBuf;
use tokio::runtime::Runtime as TokioRuntime;

/// Services shared across all agent sessions in one process.
pub struct AppRuntime {
    /// Shared async runtime for all sessions.
    pub tokio: TokioRuntime,
    /// Provider routing (model → endpoint resolution).
    pub provider_router: ProviderRouter,
    /// Global before/after hooks applied to every tool call.
    pub global_hooks: HookRegistry,
    /// Global Threadlane directory (`~/.threadlane`).
    pub global_dir: PathBuf,
    /// Default agent configuration (compaction, stream rules, etc.).
    pub default_agent_config: AgentConfig,
}

impl AppRuntime {
    /// Initialise shared services.
    ///
    /// `global_dir` is typically `~/.threadlane`.
    pub fn new(global_dir: PathBuf) -> Self {
        let tokio = TokioRuntime::new().expect("Failed to create Tokio runtime");
        let provider_router = ProviderRouter::new();
        let global_hooks = HookRegistry::default();
        let default_agent_config = AgentConfig::default();

        // Ensure the global directory exists.
        let _ = std::fs::create_dir_all(&global_dir);

        Self {
            tokio,
            provider_router,
            global_hooks,
            global_dir,
            default_agent_config,
        }
    }
}

impl Default for AppRuntime {
    fn default() -> Self {
        Self::new(
            crate::dirs_home()
                .map(|home| home.join(".threadlane"))
                .unwrap_or_else(|| PathBuf::from(".threadlane")),
        )
    }
}

// The `engine` module's `get_runtime()` returns a process-wide singleton;
// `AppRuntime::tokio` is the canonical reference for new code.