pub mod acp;
pub mod agents;
pub mod capabilities;
pub mod coding_agent;
pub mod commands;
pub mod context;
pub mod extension_broker;
pub mod frontmatter;
pub mod mcp;
pub mod packages;
mod plan;
pub mod policy;
pub mod prompt_templates;
pub mod skills;
pub mod supervisor;
pub mod system_prompt;
pub mod wasi_extension;

pub use acp::{
    AcpAgentCapabilities, AcpAgentConfig, AcpAgentRecord, AcpAgentStatus, AcpAuthMethod,
    AcpClientHandler, AcpConnection, AcpContentBlock, AcpInitializeResult, AcpManager,
    AcpNewSessionResult, AcpPermissionOption, AcpPermissionOptionKind, AcpPermissionOutcome,
    AcpPermissionPolicy, AcpPermissionRequest, AcpPlanEntry, AcpProbeClient,
    AcpReadTextFileRequest, AcpScope, AcpSession, AcpSessionNotification, AcpSessionUpdate,
    AcpSettings, AcpStopReason, AcpToolCall, AcpToolCallStatus, AcpToolKind, AcpWorkspaceClient,
    AcpWriteTextFileRequest, ACP_PROTOCOL_VERSION,
};
pub use agents::{discover_agents, AgentConfig, AgentDiscoveryResult, AgentScope, AgentSource};
pub use capabilities::CapabilityCatalog;
pub use coding_agent::{
    cancel_open_subagent_operations, CodingAgent, CodingAgentOptions, CodingAgentWorkHandle,
    ExtensionBeforeToolHook,
};
pub use commands::{execute_slash_command, parse_slash_command, CommandAction};
pub use context::{ProjectContext, ProjectInstruction};
pub use extension_broker::{
    BrokerDispatchResult, BrokerError, BrokerOperationResult, BrokerRequest, BrokerResponse,
    CapabilityDispatcher, CapabilityHandler, CapabilityPolicy, HostBrokerRequest,
    HostCapabilityGrantPolicy,
};
pub use mcp::{
    McpManager, McpScope, McpServerConfig, McpServerRecord, McpServerStatus, McpSettings,
    McpToolExecutor, McpTransport,
};
pub use packages::{
    default_global_threadlane_dir, ExtensionManager, ExtensionRecord, ExtensionScope,
};
pub use policy::ToolPolicy;
pub use prompt_templates::{
    expand_prompt_template, load_prompt_templates, parse_command_args, substitute_args,
    PromptTemplate,
};
pub use skills::{
    load_skill_tool_definition, LoadSkillToolExecutor, SkillDiscoveryOptions, SkillDiscoveryReport,
    SkillDiscoveryWarning, SkillDiscoveryWarningKind, SkillManager, SkillMetadata, SkillRegistry,
    SkillScope, SkillSettings, LOAD_SKILL_TOOL_NAME,
};
pub use supervisor::{
    HarnessSupervisor, ProjectRecord, TaskAgentEvent, TaskKind, TaskRecord, TaskStatus,
};
pub use system_prompt::SystemPromptConfig;
pub use wasi_extension::{
    WasiCommandDefinition, WasiExtension, WasiExtensionCommandResult, WasiExtensionEvent,
    WasiExtensionManager, WasiExtensionManifest, WasiLegacyEffect, WasiToolDefinition,
};
