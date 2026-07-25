pub mod frontmatter;
pub mod agents;
pub mod capabilities;
pub mod coding_agent;
pub mod commands;
pub mod context;
pub mod extension_broker;
pub mod full_trust_extension;
pub mod packages;
pub mod prompt_templates;
pub mod skills;
pub mod policy;
pub mod supervisor;
pub mod system_prompt;
pub mod wasi_extension;

pub use agents::{discover_agents, AgentConfig, AgentDiscoveryResult, AgentScope, AgentSource};
pub use capabilities::{CapabilityCatalog, ExtensionMetadata};
pub use coding_agent::{CodingAgent, CodingAgentOptions, ExtensionBeforeToolHook};
pub use policy::ToolPolicy;
pub use commands::{execute_slash_command, parse_slash_command, CommandAction};
pub use context::{ProjectContext, ProjectInstruction};
pub use extension_broker::{
    BrokerDispatchResult, BrokerError, BrokerOperationResult, BrokerRequest, BrokerResponse,
    CapabilityDispatcher, CapabilityHandler, CapabilityPolicy, HostBrokerRequest,
    HostCapabilityGrantPolicy,
};
pub use full_trust_extension::{FullTrustRunner, TrustStore};
pub use packages::{PackageManifest, PackageRecord, PackageScope};
pub use prompt_templates::{
    expand_prompt_template, load_prompt_templates, parse_command_args, substitute_args, PromptTemplate,
};
pub use skills::{
    load_skill_tool_definition, LoadSkillToolExecutor, SkillDiscoveryOptions, SkillDiscoveryReport,
    SkillDiscoveryWarning, SkillDiscoveryWarningKind, SkillManager, SkillMetadata, SkillRegistry,
    SkillScope, LOAD_SKILL_TOOL_NAME,
};
pub use supervisor::{HarnessSupervisor, ProjectRecord, TaskAgentEvent, TaskRecord, TaskStatus};
pub use system_prompt::SystemPromptConfig;
pub use wasi_extension::{
    WasiCommandDefinition, WasiExtension, WasiExtensionCommandResult, WasiExtensionEvent,
    WasiExtensionManager, WasiExtensionManifest, WasiLegacyEffect, WasiToolDefinition,
};
