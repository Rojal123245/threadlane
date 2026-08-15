//! Declarative capability registration for the agent loop.
//!
//! Each subsystem (MCP, WASI, skills, subagents, plan) that provides tools
//! or hooks implements [`Capability`]. The [`CapabilityRegistry`] collects
//! them and wires them into the agent loop declaratively.

use crate::harness::{HookHandler, HookKind};
use crate::tool_executor::ToolExecutor;
use std::sync::Arc;

/// A subsystem that contributes tools and/or hooks to the agent.
pub trait Capability: Send + Sync {
    /// Stable identifier for diagnostics.
    fn id(&self) -> &str;

    /// Tool executors to register with the agent loop.
    fn tool_executors(&self) -> Vec<Arc<dyn ToolExecutor>> {
        Vec::new()
    }

    /// Hooks to register. Each tuple is `(hook_kind, stable_id, handler)`.
    fn hooks(&self) -> Vec<(HookKind, &str, HookHandler)> {
        Vec::new()
    }
}

/// Collects capabilities and wires them into an agent loop.
#[derive(Default)]
pub struct CapabilityRegistry {
    capabilities: Vec<Box<dyn Capability>>,
}

impl CapabilityRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers a capability. Registration order determines hook execution
    /// order and tool deduplication priority (later wins for hooks, first
    /// wins for tool names).
    pub fn register(&mut self, capability: Box<dyn Capability>) {
        self.capabilities.push(capability);
    }

    /// Returns all tool executors from all registered capabilities.
    pub fn all_tool_executors(&self) -> Vec<Arc<dyn ToolExecutor>> {
        self.capabilities
            .iter()
            .flat_map(|cap| cap.tool_executors())
            .collect()
    }

    /// Returns all hooks from all registered capabilities.
    pub fn all_hooks(&self) -> Vec<(HookKind, &str, HookHandler)> {
        self.capabilities
            .iter()
            .flat_map(|cap| cap.hooks())
            .collect()
    }

    /// Returns true if no capabilities are registered.
    pub fn is_empty(&self) -> bool {
        self.capabilities.is_empty()
    }

    /// Number of registered capabilities.
    pub fn len(&self) -> usize {
        self.capabilities.len()
    }

    /// Wires all capabilities into the given agent loop: registers tool
    /// executors and hooks. Returns the count of successfully registered
    /// items and any errors encountered.
    pub fn wire_all(&self, agent: &mut crate::unified::UnifiedAgent) -> (usize, Vec<String>) {
        let mut tool_count = 0;
        let mut hook_count = 0;
        let mut errors = Vec::new();

        for executor in self.all_tool_executors() {
            match agent.register_tool_executor(executor) {
                Ok(()) => tool_count += 1,
                Err(error) => errors.push(format!("tool registration failed: {error}")),
            }
        }

        for (kind, id, handler) in self.all_hooks() {
            match agent.hook_registry.replace(kind, id, handler) {
                Ok(()) => hook_count += 1,
                Err(error) => errors.push(format!("hook '{id}' registration failed: {error:?}")),
            }
        }

        (tool_count + hook_count, errors)
    }
}
