use crate::types::{AgentToolCall, AgentToolDefinition};
use async_trait::async_trait;

#[async_trait]
pub trait ToolExecutor: Send + Sync {
    /// Stable identity used for deterministic registration and diagnostics.
    fn executor_id(&self) -> &str {
        std::any::type_name::<Self>()
    }

    /// Provider-neutral definitions for tools handled by this executor.
    fn tool_definitions(&self) -> Vec<AgentToolDefinition> {
        self.get_tool_schemas()
            .iter()
            .filter_map(|schema| AgentToolDefinition::from_provider_schema(schema).ok())
            .collect()
    }

    /// Legacy Chat Completions schemas. Prefer `tool_definitions` for new executors.
    fn get_tool_schemas(&self) -> Vec<serde_json::Value> {
        Vec::new()
    }

    async fn execute_tool(&self, name: &str, args: &str) -> Option<Result<String, String>>;

    async fn execute_tool_with_call(
        &self,
        call: &AgentToolCall,
        args: &str,
    ) -> Option<Result<String, String>> {
        self.execute_tool(&call.name, args).await
    }
}
