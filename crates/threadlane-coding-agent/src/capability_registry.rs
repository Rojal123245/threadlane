//! Capability registry for agent subsystems.
//!
//! Each subsystem (MCP, WASI, skills, subagents, plan) that provides tools
//! or hooks implements [`Capability`]. The [`CapabilityRegistry`] collects
//! them and wires them into the agent loop declaratively.
//!
//! This replaces the imperative tool/hook registration previously scattered
//! across `CodingAgent::new()`.

use crate::error::CodingAgentError;
use std::sync::Arc;
use threadlane_agent::harness::{HookHandler, HookKind};
use threadlane_agent::ToolExecutor;

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
    pub fn wire_all(&self, agent: &mut threadlane_agent::Agent) -> (usize, Vec<CodingAgentError>) {
        let mut tool_count = 0;
        let mut hook_count = 0;
        let mut errors = Vec::new();

        for executor in self.all_tool_executors() {
            match agent.loop_engine.register_tool_executor(executor) {
                Ok(()) => tool_count += 1,
                Err(error) => errors.push(CodingAgentError::Init(format!(
                    "tool registration failed: {error}"
                ))),
            }
        }

        for (kind, id, handler) in self.all_hooks() {
            match agent.loop_engine.hook_registry.replace(kind, id, handler) {
                Ok(()) => hook_count += 1,
                Err(error) => errors.push(CodingAgentError::Init(format!(
                    "hook '{id}' registration failed: {error:?}"
                ))),
            }
        }

        (tool_count + hook_count, errors)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use threadlane_agent::harness::HookKind;
    use threadlane_agent::ToolExecutor;

    struct StubCapability {
        tools: Vec<Arc<dyn ToolExecutor>>,
        hooks: Vec<(HookKind, &'static str, HookHandler)>,
    }

    impl Capability for StubCapability {
        fn id(&self) -> &str {
            "stub"
        }

        fn tool_executors(&self) -> Vec<Arc<dyn ToolExecutor>> {
            self.tools.clone()
        }

        fn hooks(&self) -> Vec<(HookKind, &str, HookHandler)> {
            self.hooks.clone()
        }
    }

    struct DummyExecutor;
    #[async_trait]
    impl ToolExecutor for DummyExecutor {
        fn executor_id(&self) -> &str {
            "dummy"
        }
        async fn execute_tool(&self, _name: &str, _args: &str) -> Option<Result<String, String>> {
            None
        }
    }

    #[test]
    fn empty_registry_has_no_tools_or_hooks() {
        let registry = CapabilityRegistry::new();
        assert!(registry.all_tool_executors().is_empty());
        assert!(registry.all_hooks().is_empty());
    }

    #[test]
    fn registry_collects_tools_and_hooks() {
        let mut registry = CapabilityRegistry::new();
        let dummy: Arc<dyn ToolExecutor> = Arc::new(DummyExecutor);
        let hook: HookHandler = Arc::new(|_ctx| Box::pin(async { Ok(Default::default()) }));

        registry.register(Box::new(StubCapability {
            tools: vec![dummy.clone()],
            hooks: vec![(HookKind::BeforeTool, "test-hook", hook.clone())],
        }));

        assert_eq!(registry.all_tool_executors().len(), 1);
        assert_eq!(registry.all_hooks().len(), 1);
    }

    #[test]
    fn multiple_capabilities_aggregate() {
        let mut registry = CapabilityRegistry::new();
        let dummy: Arc<dyn ToolExecutor> = Arc::new(DummyExecutor);
        let hook: HookHandler = Arc::new(|_ctx| Box::pin(async { Ok(Default::default()) }));

        registry.register(Box::new(StubCapability {
            tools: vec![dummy.clone()],
            hooks: vec![],
        }));
        registry.register(Box::new(StubCapability {
            tools: vec![],
            hooks: vec![(HookKind::AfterTool, "hook1", hook.clone())],
        }));
        registry.register(Box::new(StubCapability {
            tools: vec![dummy.clone()],
            hooks: vec![(HookKind::BeforeTool, "hook2", hook.clone())],
        }));

        assert_eq!(registry.all_tool_executors().len(), 2);
        assert_eq!(registry.all_hooks().len(), 2);
    }
}
