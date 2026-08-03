//! Agent Client Protocol (ACP) client support.
//!
//! Threadlane acts as an ACP *client*: it launches an external agent as a
//! subprocess and speaks newline-delimited JSON-RPC 2.0 over its stdio pipes.
//! The agent streams `session/update` notifications back and may call into the
//! client for filesystem access and tool permission decisions.
//!
//! This module owns the protocol and configuration layers only. It deliberately
//! contains no UI wiring: callers supply an [`AcpClientHandler`] and drive
//! [`AcpSession`] or [`AcpConnection`] directly.
//!
//! Configuration mirrors [`crate::mcp`]: `acp.json` in the global Threadlane
//! directory and in `<project>/.threadlane/`, with project entries shadowing
//! global entries that share an id.

use async_trait::async_trait;
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::{json, Value};
use std::collections::{BTreeSet, HashMap};
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::{mpsc, oneshot, Mutex as TokioMutex};
use tokio::task::JoinHandle;

/// ACP major protocol version implemented by this client.
pub const ACP_PROTOCOL_VERSION: u16 = 1;

const ACP_SETTINGS_FILE: &str = "acp.json";
const ACP_PROJECT_SETTINGS_RELATIVE_PATH: &str = ".threadlane/acp.json";
const MAX_ACP_SETTINGS_BYTES: usize = 512 * 1024;
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

// ---------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum AcpScope {
    #[default]
    Global,
    Project,
}

/// A configured external ACP agent.
///
/// ACP is defined over stdio only, so an agent is always a spawnable command.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AcpAgentConfig {
    pub id: String,
    pub name: String,
    pub command: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub args: Vec<String>,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub env: HashMap<String, String>,
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    #[serde(default)]
    pub scope: AcpScope,
}

fn default_enabled() -> bool {
    true
}

impl AcpAgentConfig {
    /// Builds a config from a display name and a single shell-style command
    /// line such as `npx -y @zed-industries/claude-code-acp`.
    ///
    /// Returns `None` when the name or command line is blank.
    pub fn from_command_line(name: &str, command_line: &str, scope: AcpScope) -> Option<Self> {
        let name = name.trim();
        let command_line = command_line.trim();
        if name.is_empty() || command_line.is_empty() {
            return None;
        }
        let mut parts = command_line.split_whitespace();
        let command = parts.next()?.to_string();
        let args: Vec<String> = parts.map(String::from).collect();
        Some(Self {
            id: slugify_id(name),
            name: name.to_string(),
            command,
            args,
            env: HashMap::new(),
            enabled: true,
            scope,
        })
    }

    /// Human-readable `command args...` summary for settings rows.
    pub fn command_line(&self) -> String {
        if self.args.is_empty() {
            self.command.clone()
        } else {
            format!("{} {}", self.command, self.args.join(" "))
        }
    }
}

fn slugify_id(name: &str) -> String {
    let slug: String = name
        .trim()
        .to_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect();
    let slug = slug.trim_matches('_').to_string();
    if slug.is_empty() {
        "agent".to_string()
    } else {
        slug
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct AcpSettingsFile {
    #[serde(default)]
    agents: Vec<AcpAgentConfig>,
}

/// Load/save helpers for `acp.json` at global and project scope.
pub struct AcpSettings;

impl AcpSettings {
    pub fn load_global(global_dir: Option<&Path>) -> Vec<AcpAgentConfig> {
        let Some(dir) = global_dir else {
            return Vec::new();
        };
        Self::load_file(&dir.join(ACP_SETTINGS_FILE), AcpScope::Global)
    }

    pub fn load_project(project_root: Option<&Path>) -> Vec<AcpAgentConfig> {
        let Some(root) = project_root else {
            return Vec::new();
        };
        Self::load_file(
            &root.join(ACP_PROJECT_SETTINGS_RELATIVE_PATH),
            AcpScope::Project,
        )
    }

    fn load_file(path: &Path, scope: AcpScope) -> Vec<AcpAgentConfig> {
        let bytes = match fs::read(path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == ErrorKind::NotFound => return Vec::new(),
            Err(_) => return Vec::new(),
        };
        if bytes.len() > MAX_ACP_SETTINGS_BYTES {
            return Vec::new();
        }
        let parsed: AcpSettingsFile = match serde_json::from_slice(&bytes) {
            Ok(data) => data,
            Err(_) => return Vec::new(),
        };
        parsed
            .agents
            .into_iter()
            .map(|mut config| {
                config.scope = scope;
                config
            })
            .collect()
    }

    pub fn save_global(dir: &Path, agents: &[AcpAgentConfig]) -> Result<(), String> {
        Self::save_file(&dir.join(ACP_SETTINGS_FILE), agents)
    }

    pub fn save_project(root: &Path, agents: &[AcpAgentConfig]) -> Result<(), String> {
        Self::save_file(&root.join(ACP_PROJECT_SETTINGS_RELATIVE_PATH), agents)
    }

    fn save_file(file_path: &Path, agents: &[AcpAgentConfig]) -> Result<(), String> {
        if let Some(parent) = file_path.parent() {
            fs::create_dir_all(parent)
                .map_err(|e| format!("Failed to create settings directory: {e}"))?;
        }
        let file_data = AcpSettingsFile {
            agents: agents.to_vec(),
        };
        let bytes = serde_json::to_vec_pretty(&file_data)
            .map_err(|e| format!("Failed to serialize ACP settings: {e}"))?;
        fs::write(file_path, bytes).map_err(|e| format!("Failed to write ACP settings: {e}"))
    }
}

// ---------------------------------------------------------------------------
// Protocol types
// ---------------------------------------------------------------------------

/// Deserializes a value that must always succeed, falling back to the type's
/// default when the agent sends an enum variant this client does not know.
fn lenient<'de, D, T>(deserializer: D) -> Result<T, D::Error>
where
    D: Deserializer<'de>,
    T: serde::de::DeserializeOwned + Default,
{
    let value = Value::deserialize(deserializer)?;
    Ok(serde_json::from_value(value).unwrap_or_default())
}

/// Same as [`lenient`], but for optional fields: an unknown variant becomes
/// `None` rather than failing the surrounding message.
fn lenient_option<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: Deserializer<'de>,
    T: serde::de::DeserializeOwned,
{
    let value = Value::deserialize(deserializer)?;
    Ok(serde_json::from_value(value).ok())
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AcpImplementation {
    pub name: String,
    pub version: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct AcpFileSystemCapabilities {
    #[serde(default)]
    pub read_text_file: bool,
    #[serde(default)]
    pub write_text_file: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct AcpClientCapabilities {
    #[serde(default)]
    pub fs: AcpFileSystemCapabilities,
    #[serde(default)]
    pub terminal: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct AcpPromptCapabilities {
    #[serde(default)]
    pub image: bool,
    #[serde(default)]
    pub audio: bool,
    #[serde(default)]
    pub embedded_context: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct AcpAgentCapabilities {
    #[serde(default)]
    pub load_session: bool,
    #[serde(default)]
    pub prompt_capabilities: AcpPromptCapabilities,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AcpAuthMethod {
    pub id: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AcpInitializeResult {
    pub protocol_version: u16,
    #[serde(default)]
    pub agent_capabilities: AcpAgentCapabilities,
    #[serde(default)]
    pub auth_methods: Vec<AcpAuthMethod>,
    #[serde(default)]
    pub agent_info: Option<AcpImplementation>,
}

impl AcpInitializeResult {
    /// Display name reported by the agent, falling back to a generic label.
    pub fn agent_display_name(&self) -> String {
        self.agent_info
            .as_ref()
            .map(|info| info.name.clone())
            .unwrap_or_else(|| "ACP agent".to_string())
    }

    pub fn requires_authentication(&self) -> bool {
        !self.auth_methods.is_empty()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AcpSessionMode {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AcpSessionModeState {
    pub current_mode_id: String,
    #[serde(default)]
    pub available_modes: Vec<AcpSessionMode>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AcpNewSessionResult {
    pub session_id: String,
    #[serde(default)]
    pub modes: Option<AcpSessionModeState>,
}

/// A single block of prompt or response content.
///
/// Unknown block types deserialize to [`AcpContentBlock::Unknown`] so a newer
/// agent cannot break an in-flight turn.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AcpContentBlock {
    Text {
        text: String,
    },
    #[serde(rename_all = "camelCase")]
    Image {
        data: String,
        mime_type: String,
    },
    #[serde(rename_all = "camelCase")]
    Audio {
        data: String,
        mime_type: String,
    },
    #[serde(rename_all = "camelCase")]
    ResourceLink {
        uri: String,
        #[serde(default)]
        name: Option<String>,
    },
    Resource {
        resource: Value,
    },
    #[serde(other)]
    Unknown,
}

impl AcpContentBlock {
    pub fn text(text: impl Into<String>) -> Self {
        Self::Text { text: text.into() }
    }

    /// Plain-text projection used for transcript rendering.
    pub fn as_text(&self) -> Option<&str> {
        match self {
            Self::Text { text } => Some(text),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AcpToolCallStatus {
    Pending,
    InProgress,
    Completed,
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum AcpToolKind {
    Read,
    Edit,
    Delete,
    Move,
    Search,
    Execute,
    Think,
    Fetch,
    SwitchMode,
    #[default]
    Other,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AcpToolCallLocation {
    pub path: String,
    #[serde(default)]
    pub line: Option<u64>,
}

/// `tool_call` and `tool_call_update` payloads share one shape here; only
/// `toolCallId` is guaranteed present on an update.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AcpToolCall {
    pub tool_call_id: String,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default, deserialize_with = "lenient_option")]
    pub kind: Option<AcpToolKind>,
    #[serde(default, deserialize_with = "lenient_option")]
    pub status: Option<AcpToolCallStatus>,
    #[serde(default)]
    pub content: Option<Vec<Value>>,
    #[serde(default)]
    pub locations: Option<Vec<AcpToolCallLocation>>,
    #[serde(default)]
    pub raw_input: Option<Value>,
    #[serde(default)]
    pub raw_output: Option<Value>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AcpPlanEntryPriority {
    High,
    Medium,
    Low,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AcpPlanEntryStatus {
    Pending,
    InProgress,
    Completed,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct AcpPlanEntry {
    pub content: String,
    pub priority: AcpPlanEntryPriority,
    pub status: AcpPlanEntryStatus,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct AcpAvailableCommand {
    pub name: String,
    #[serde(default)]
    pub description: String,
}

/// Decoded `session/update` payload.
///
/// The protocol keeps adding update kinds, so anything unrecognized is kept as
/// [`AcpSessionUpdate::Other`] instead of failing the whole notification.
#[derive(Debug, Clone, PartialEq)]
pub enum AcpSessionUpdate {
    UserMessageChunk(AcpContentBlock),
    AgentMessageChunk(AcpContentBlock),
    AgentThoughtChunk(AcpContentBlock),
    ToolCall(AcpToolCall),
    ToolCallUpdate(AcpToolCall),
    Plan(Vec<AcpPlanEntry>),
    AvailableCommandsUpdate(Vec<AcpAvailableCommand>),
    CurrentModeUpdate { current_mode_id: String },
    Other { kind: String, payload: Value },
}

impl AcpSessionUpdate {
    fn from_value(value: Value) -> Self {
        let kind = value
            .get("sessionUpdate")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();

        fn chunk(value: &Value) -> Option<AcpContentBlock> {
            serde_json::from_value(value.get("content")?.clone()).ok()
        }

        let decoded = match kind.as_str() {
            "user_message_chunk" => chunk(&value).map(Self::UserMessageChunk),
            "agent_message_chunk" => chunk(&value).map(Self::AgentMessageChunk),
            "agent_thought_chunk" => chunk(&value).map(Self::AgentThoughtChunk),
            "tool_call" => serde_json::from_value(value.clone())
                .ok()
                .map(Self::ToolCall),
            "tool_call_update" => serde_json::from_value(value.clone())
                .ok()
                .map(Self::ToolCallUpdate),
            "plan" => value
                .get("entries")
                .and_then(|entries| serde_json::from_value(entries.clone()).ok())
                .map(Self::Plan),
            "available_commands_update" => value
                .get("availableCommands")
                .and_then(|commands| serde_json::from_value(commands.clone()).ok())
                .map(Self::AvailableCommandsUpdate),
            "current_mode_update" => value
                .get("currentModeId")
                .and_then(Value::as_str)
                .map(|id| Self::CurrentModeUpdate {
                    current_mode_id: id.to_string(),
                }),
            _ => None,
        };

        decoded.unwrap_or(Self::Other {
            kind,
            payload: value,
        })
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct AcpSessionNotification {
    pub session_id: String,
    pub update: AcpSessionUpdate,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum AcpStopReason {
    EndTurn,
    MaxTokens,
    MaxTurnRequests,
    Refusal,
    Cancelled,
    #[default]
    Unknown,
}

impl AcpStopReason {
    /// Decodes a stop reason, treating an unrecognized value as
    /// [`AcpStopReason::Unknown`] rather than failing the turn.
    pub fn from_value(value: &Value) -> Self {
        serde_json::from_value(value.clone()).unwrap_or_default()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum AcpPermissionOptionKind {
    AllowOnce,
    AllowAlways,
    RejectOnce,
    RejectAlways,
    #[default]
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AcpPermissionOption {
    pub option_id: String,
    pub name: String,
    #[serde(default, deserialize_with = "lenient")]
    pub kind: AcpPermissionOptionKind,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AcpPermissionRequest {
    pub session_id: String,
    #[serde(default)]
    pub tool_call: Option<AcpToolCall>,
    #[serde(default)]
    pub options: Vec<AcpPermissionOption>,
}

/// Client answer to `session/request_permission`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AcpPermissionOutcome {
    Selected { option_id: String },
    Cancelled,
}

impl AcpPermissionOutcome {
    fn to_json(&self) -> Value {
        match self {
            Self::Selected { option_id } => json!({
                "outcome": {
                    "outcome": "selected",
                    "optionId": option_id,
                }
            }),
            Self::Cancelled => json!({ "outcome": { "outcome": "cancelled" } }),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AcpReadTextFileRequest {
    pub session_id: String,
    pub path: String,
    #[serde(default)]
    pub line: Option<u32>,
    #[serde(default)]
    pub limit: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AcpWriteTextFileRequest {
    pub session_id: String,
    pub path: String,
    pub content: String,
}

// ---------------------------------------------------------------------------
// Client handler
// ---------------------------------------------------------------------------

/// Client-side half of ACP: everything the agent may ask Threadlane to do.
#[async_trait]
pub trait AcpClientHandler: Send + Sync {
    async fn on_session_update(&self, notification: AcpSessionNotification);

    async fn request_permission(&self, request: AcpPermissionRequest) -> AcpPermissionOutcome;

    async fn read_text_file(&self, request: AcpReadTextFileRequest) -> Result<String, String>;

    async fn write_text_file(&self, request: AcpWriteTextFileRequest) -> Result<(), String>;
}

/// How an unattended client answers `session/request_permission`.
///
/// The default is [`AcpPermissionPolicy::Reject`]: without a UI in the loop
/// there is no informed consent, so nothing is auto-approved.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AcpPermissionPolicy {
    #[default]
    Reject,
    AllowOnce,
    AllowAlways,
}

impl AcpPermissionPolicy {
    fn select(&self, options: &[AcpPermissionOption]) -> AcpPermissionOutcome {
        let preferred: &[AcpPermissionOptionKind] = match self {
            Self::Reject => &[
                AcpPermissionOptionKind::RejectOnce,
                AcpPermissionOptionKind::RejectAlways,
            ],
            Self::AllowOnce => &[
                AcpPermissionOptionKind::AllowOnce,
                AcpPermissionOptionKind::AllowAlways,
            ],
            Self::AllowAlways => &[
                AcpPermissionOptionKind::AllowAlways,
                AcpPermissionOptionKind::AllowOnce,
            ],
        };
        for kind in preferred {
            if let Some(option) = options.iter().find(|option| option.kind == *kind) {
                return AcpPermissionOutcome::Selected {
                    option_id: option.option_id.clone(),
                };
            }
        }
        AcpPermissionOutcome::Cancelled
    }
}

/// Default handler: workspace-scoped filesystem access, a fixed permission
/// policy, and session updates forwarded on a channel.
pub struct AcpWorkspaceClient {
    workspace_root: PathBuf,
    permission_policy: AcpPermissionPolicy,
    updates: Option<mpsc::UnboundedSender<AcpSessionNotification>>,
}

impl AcpWorkspaceClient {
    pub fn new(workspace_root: PathBuf) -> Self {
        Self {
            workspace_root,
            permission_policy: AcpPermissionPolicy::default(),
            updates: None,
        }
    }

    pub fn with_permission_policy(mut self, policy: AcpPermissionPolicy) -> Self {
        self.permission_policy = policy;
        self
    }

    pub fn with_update_sender(
        mut self,
        sender: mpsc::UnboundedSender<AcpSessionNotification>,
    ) -> Self {
        self.updates = Some(sender);
        self
    }

    fn resolve(&self, path: &str) -> Result<PathBuf, String> {
        threadlane_tools::validate_path_in_workspace(path, &self.workspace_root)
    }
}

#[async_trait]
impl AcpClientHandler for AcpWorkspaceClient {
    async fn on_session_update(&self, notification: AcpSessionNotification) {
        if let Some(sender) = &self.updates {
            let _ = sender.send(notification);
        }
    }

    async fn request_permission(&self, request: AcpPermissionRequest) -> AcpPermissionOutcome {
        self.permission_policy.select(&request.options)
    }

    async fn read_text_file(&self, request: AcpReadTextFileRequest) -> Result<String, String> {
        let path = self.resolve(&request.path)?;
        let content = tokio::fs::read_to_string(&path)
            .await
            .map_err(|e| format!("Failed to read '{}': {e}", path.display()))?;

        // `line` is 1-based; `limit` counts lines from there.
        if request.line.is_none() && request.limit.is_none() {
            return Ok(content);
        }
        let start = request.line.unwrap_or(1).saturating_sub(1) as usize;
        let mut lines: Vec<&str> = content.lines().skip(start).collect();
        if let Some(limit) = request.limit {
            lines.truncate(limit as usize);
        }
        Ok(lines.join("\n"))
    }

    async fn write_text_file(&self, request: AcpWriteTextFileRequest) -> Result<(), String> {
        let path = self.resolve(&request.path)?;
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(|e| format!("Failed to create '{}': {e}", parent.display()))?;
        }
        tokio::fs::write(&path, request.content)
            .await
            .map_err(|e| format!("Failed to write '{}': {e}", path.display()))
    }
}

/// Handler for connections that exist only to complete a handshake.
///
/// A probe has no session and no user watching, so it grants nothing: every
/// filesystem method is refused and every permission request is cancelled. This
/// is what keeps `AcpManager::discover_and_connect` from handing an unproven
/// third-party binary access to whatever directory the app happens to be in.
pub struct AcpProbeClient;

#[async_trait]
impl AcpClientHandler for AcpProbeClient {
    async fn on_session_update(&self, _notification: AcpSessionNotification) {}

    async fn request_permission(&self, _request: AcpPermissionRequest) -> AcpPermissionOutcome {
        AcpPermissionOutcome::Cancelled
    }

    async fn read_text_file(&self, _request: AcpReadTextFileRequest) -> Result<String, String> {
        Err("Filesystem access is not available while probing an ACP agent".to_string())
    }

    async fn write_text_file(&self, _request: AcpWriteTextFileRequest) -> Result<(), String> {
        Err("Filesystem access is not available while probing an ACP agent".to_string())
    }
}

// ---------------------------------------------------------------------------
// Connection
// ---------------------------------------------------------------------------

type PendingMap = HashMap<u64, oneshot::Sender<Result<Value, String>>>;
type PendingResponses = Arc<Mutex<PendingMap>>;
type BoxedWriter = Box<dyn AsyncWrite + Unpin + Send>;

fn lock_pending(pending: &PendingResponses) -> std::sync::MutexGuard<'_, PendingMap> {
    // A panic while dispatching must not poison the connection for good.
    pending.lock().unwrap_or_else(|error| error.into_inner())
}

/// A live JSON-RPC connection to one ACP agent process.
///
/// Dropping the connection kills the child process and fails any in-flight
/// requests.
pub struct AcpConnection {
    writer: Arc<TokioMutex<BoxedWriter>>,
    pending: PendingResponses,
    next_id: AtomicU64,
    reader_task: JoinHandle<()>,
    child: Option<TokioMutex<Child>>,
}

impl AcpConnection {
    /// Spawns `config` as a subprocess and drives ACP over its stdio pipes.
    pub async fn spawn(
        config: &AcpAgentConfig,
        cwd: Option<&Path>,
        handler: Arc<dyn AcpClientHandler>,
    ) -> Result<Self, String> {
        let mut command = Command::new(&config.command);
        command.args(&config.args);
        for (key, value) in &config.env {
            command.env(key, value);
        }
        if let Some(dir) = cwd {
            command.current_dir(dir);
        }
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true);

        let mut child = command
            .spawn()
            .map_err(|e| format!("Failed to spawn ACP agent '{}': {e}", config.command))?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| "Failed to open ACP agent stdin".to_string())?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| "Failed to open ACP agent stdout".to_string())?;

        Ok(Self::from_streams(stdin, stdout, handler, Some(child)))
    }

    /// Builds a connection over arbitrary byte streams. Used by [`Self::spawn`]
    /// and by tests that pair the client with an in-process stub agent.
    pub fn from_streams<W, R>(
        writer: W,
        reader: R,
        handler: Arc<dyn AcpClientHandler>,
        child: Option<Child>,
    ) -> Self
    where
        W: AsyncWrite + Unpin + Send + 'static,
        R: AsyncRead + Unpin + Send + 'static,
    {
        let writer: Arc<TokioMutex<BoxedWriter>> = Arc::new(TokioMutex::new(Box::new(writer)));
        let pending: PendingResponses = Arc::new(Mutex::new(HashMap::new()));
        let reader_task = tokio::spawn(read_loop(
            BufReader::new(reader),
            Arc::clone(&pending),
            Arc::clone(&writer),
            handler,
        ));

        Self {
            writer,
            pending,
            next_id: AtomicU64::new(1),
            reader_task,
            child: child.map(TokioMutex::new),
        }
    }

    async fn send_line(&self, message: &Value) -> Result<(), String> {
        let mut line = serde_json::to_string(message)
            .map_err(|e| format!("Failed to encode ACP message: {e}"))?;
        line.push('\n');
        let mut writer = self.writer.lock().await;
        writer
            .write_all(line.as_bytes())
            .await
            .map_err(|e| format!("Failed to write to ACP agent: {e}"))?;
        writer
            .flush()
            .await
            .map_err(|e| format!("Failed to flush ACP agent stdin: {e}"))
    }

    /// Sends a request and awaits its response. `timeout` of `None` waits
    /// indefinitely, which is what a prompt turn needs.
    pub async fn request(
        &self,
        method: &str,
        params: Value,
        timeout: Option<Duration>,
    ) -> Result<Value, String> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        lock_pending(&self.pending).insert(id, tx);

        let message = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        });
        if let Err(error) = self.send_line(&message).await {
            lock_pending(&self.pending).remove(&id);
            return Err(error);
        }

        let received = match timeout {
            Some(duration) => match tokio::time::timeout(duration, rx).await {
                Ok(received) => received,
                Err(_) => {
                    lock_pending(&self.pending).remove(&id);
                    return Err(format!("ACP request '{method}' timed out"));
                }
            },
            None => rx.await,
        };

        match received {
            Ok(result) => result,
            Err(_) => Err(format!(
                "ACP agent closed the connection while handling '{method}'"
            )),
        }
    }

    pub async fn notify(&self, method: &str, params: Value) -> Result<(), String> {
        self.send_line(&json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params,
        }))
        .await
    }

    pub async fn initialize(&self) -> Result<AcpInitializeResult, String> {
        let params = json!({
            "protocolVersion": ACP_PROTOCOL_VERSION,
            "clientCapabilities": {
                "fs": { "readTextFile": true, "writeTextFile": true },
                "terminal": false,
            },
            "clientInfo": {
                "name": "threadlane",
                "version": env!("CARGO_PKG_VERSION"),
            },
        });
        let result = self
            .request("initialize", params, Some(HANDSHAKE_TIMEOUT))
            .await?;
        let initialized: AcpInitializeResult = serde_json::from_value(result)
            .map_err(|e| format!("Invalid ACP initialize response: {e}"))?;
        if initialized.protocol_version > ACP_PROTOCOL_VERSION {
            return Err(format!(
                "ACP agent requires protocol version {} but this client implements {ACP_PROTOCOL_VERSION}",
                initialized.protocol_version
            ));
        }
        Ok(initialized)
    }

    pub async fn authenticate(&self, method_id: &str) -> Result<(), String> {
        self.request(
            "authenticate",
            json!({ "methodId": method_id }),
            Some(HANDSHAKE_TIMEOUT),
        )
        .await
        .map(|_| ())
    }

    pub async fn new_session(
        &self,
        cwd: &Path,
        mcp_servers: Vec<Value>,
    ) -> Result<AcpNewSessionResult, String> {
        let params = json!({
            "cwd": cwd.to_string_lossy(),
            "mcpServers": mcp_servers,
        });
        let result = self
            .request("session/new", params, Some(HANDSHAKE_TIMEOUT))
            .await?;
        serde_json::from_value(result).map_err(|e| format!("Invalid ACP session/new response: {e}"))
    }

    /// Runs one prompt turn. Resolves when the agent reports a stop reason;
    /// use [`Self::cancel`] to interrupt it.
    pub async fn prompt(
        &self,
        session_id: &str,
        blocks: Vec<AcpContentBlock>,
    ) -> Result<AcpStopReason, String> {
        let params = json!({
            "sessionId": session_id,
            "prompt": blocks,
        });
        let result = self.request("session/prompt", params, None).await?;
        let stop_reason = result
            .get("stopReason")
            .ok_or_else(|| "ACP session/prompt response is missing stopReason".to_string())?;
        Ok(AcpStopReason::from_value(stop_reason))
    }

    pub async fn cancel(&self, session_id: &str) -> Result<(), String> {
        self.notify("session/cancel", json!({ "sessionId": session_id }))
            .await
    }

    pub async fn set_session_mode(&self, session_id: &str, mode_id: &str) -> Result<(), String> {
        self.request(
            "session/set_mode",
            json!({ "sessionId": session_id, "modeId": mode_id }),
            Some(HANDSHAKE_TIMEOUT),
        )
        .await
        .map(|_| ())
    }

    /// Terminates the agent process and stops the reader task.
    pub async fn shutdown(&self) {
        if let Some(child) = &self.child {
            let _ = child.lock().await.kill().await;
        }
        self.reader_task.abort();
        for (_, sender) in lock_pending(&self.pending).drain() {
            let _ = sender.send(Err("ACP connection was shut down".to_string()));
        }
    }
}

impl Drop for AcpConnection {
    fn drop(&mut self) {
        self.reader_task.abort();
        // `abort` only schedules cancellation, so the reader's own clone of the
        // pending map may outlive this call. Fail the waiters here instead of
        // relying on the channel senders being dropped at some later point.
        for (_, sender) in lock_pending(&self.pending).drain() {
            let _ = sender.send(Err("ACP connection was dropped".to_string()));
        }
    }
}

async fn read_loop<R>(
    mut reader: BufReader<R>,
    pending: PendingResponses,
    writer: Arc<TokioMutex<BoxedWriter>>,
    handler: Arc<dyn AcpClientHandler>,
) where
    R: AsyncRead + Unpin + Send + 'static,
{
    let mut line = String::new();
    loop {
        line.clear();
        match reader.read_line(&mut line).await {
            Ok(0) | Err(_) => break,
            Ok(_) => {}
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let Ok(message) = serde_json::from_str::<Value>(trimmed) else {
            continue;
        };

        let has_method = message.get("method").and_then(Value::as_str).is_some();
        if has_method {
            tokio::spawn(dispatch_incoming(
                message,
                Arc::clone(&writer),
                Arc::clone(&handler),
            ));
            continue;
        }

        let Some(id) = message.get("id").and_then(Value::as_u64) else {
            continue;
        };
        let Some(sender) = lock_pending(&pending).remove(&id) else {
            continue;
        };
        let payload = if let Some(error) = message.get("error") {
            Err(format_rpc_error(error))
        } else {
            Ok(message.get("result").cloned().unwrap_or(Value::Null))
        };
        let _ = sender.send(payload);
    }

    for (_, sender) in lock_pending(&pending).drain() {
        let _ = sender.send(Err("ACP agent closed the connection".to_string()));
    }
}

fn format_rpc_error(error: &Value) -> String {
    let message = error
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or("ACP agent returned an error");
    match error.get("code").and_then(Value::as_i64) {
        Some(code) => format!("{message} (code {code})"),
        None => message.to_string(),
    }
}

/// Handles one agent-initiated request or notification.
async fn dispatch_incoming(
    message: Value,
    writer: Arc<TokioMutex<BoxedWriter>>,
    handler: Arc<dyn AcpClientHandler>,
) {
    let method = message
        .get("method")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let params = message.get("params").cloned().unwrap_or(Value::Null);
    let id = message.get("id").cloned();

    if method == "session/update" {
        if let Some(notification) = parse_session_notification(params) {
            handler.on_session_update(notification).await;
        }
        return;
    }

    // Every remaining method is a request that owes a response.
    let Some(id) = id else {
        return;
    };

    let outcome: Result<Value, (i64, String)> = match method.as_str() {
        "session/request_permission" => match serde_json::from_value(params) {
            Ok(request) => Ok(handler.request_permission(request).await.to_json()),
            Err(error) => Err((-32602, format!("Invalid permission request: {error}"))),
        },
        "fs/read_text_file" => match serde_json::from_value(params) {
            Ok(request) => handler
                .read_text_file(request)
                .await
                .map(|content| json!({ "content": content }))
                .map_err(|error| (-32603, error)),
            Err(error) => Err((-32602, format!("Invalid read request: {error}"))),
        },
        "fs/write_text_file" => match serde_json::from_value(params) {
            Ok(request) => handler
                .write_text_file(request)
                .await
                .map(|_| json!({}))
                .map_err(|error| (-32603, error)),
            Err(error) => Err((-32602, format!("Invalid write request: {error}"))),
        },
        other => Err((-32601, format!("Method '{other}' is not supported"))),
    };

    let response = match outcome {
        Ok(result) => json!({ "jsonrpc": "2.0", "id": id, "result": result }),
        Err((code, message)) => json!({
            "jsonrpc": "2.0",
            "id": id,
            "error": { "code": code, "message": message },
        }),
    };

    if let Ok(mut encoded) = serde_json::to_string(&response) {
        encoded.push('\n');
        let mut writer = writer.lock().await;
        let _ = writer.write_all(encoded.as_bytes()).await;
        let _ = writer.flush().await;
    }
}

fn parse_session_notification(params: Value) -> Option<AcpSessionNotification> {
    let session_id = params.get("sessionId")?.as_str()?.to_string();
    let update = params.get("update")?.clone();
    Some(AcpSessionNotification {
        session_id,
        update: AcpSessionUpdate::from_value(update),
    })
}

// ---------------------------------------------------------------------------
// Session
// ---------------------------------------------------------------------------

/// A connected agent with one open ACP session.
pub struct AcpSession {
    connection: Arc<AcpConnection>,
    session_id: String,
    agent: AcpInitializeResult,
    modes: Option<AcpSessionModeState>,
}

impl AcpSession {
    /// Spawns the agent, performs the handshake, and opens a session rooted at
    /// `cwd`.
    pub async fn start(
        config: &AcpAgentConfig,
        cwd: &Path,
        handler: Arc<dyn AcpClientHandler>,
    ) -> Result<Self, String> {
        let connection = Arc::new(AcpConnection::spawn(config, Some(cwd), handler).await?);
        let agent = match connection.initialize().await {
            Ok(agent) => agent,
            Err(error) => {
                connection.shutdown().await;
                return Err(error);
            }
        };
        let session = match connection.new_session(cwd, Vec::new()).await {
            Ok(session) => session,
            Err(error) => {
                connection.shutdown().await;
                return Err(error);
            }
        };
        Ok(Self {
            connection,
            session_id: session.session_id,
            agent,
            modes: session.modes,
        })
    }

    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    pub fn agent(&self) -> &AcpInitializeResult {
        &self.agent
    }

    pub fn modes(&self) -> Option<&AcpSessionModeState> {
        self.modes.as_ref()
    }

    pub fn connection(&self) -> &Arc<AcpConnection> {
        &self.connection
    }

    pub async fn prompt_text(&self, text: &str) -> Result<AcpStopReason, String> {
        self.connection
            .prompt(&self.session_id, vec![AcpContentBlock::text(text)])
            .await
    }

    pub async fn prompt(&self, blocks: Vec<AcpContentBlock>) -> Result<AcpStopReason, String> {
        self.connection.prompt(&self.session_id, blocks).await
    }

    pub async fn cancel(&self) -> Result<(), String> {
        self.connection.cancel(&self.session_id).await
    }

    pub async fn shutdown(&self) {
        self.connection.shutdown().await;
    }
}

// ---------------------------------------------------------------------------
// Manager
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AcpAgentStatus {
    Disconnected,
    Connecting,
    Connected {
        agent_name: String,
        protocol_version: u16,
        auth_required: bool,
    },
    Error(String),
}

impl AcpAgentStatus {
    pub fn display_status(&self) -> String {
        match self {
            Self::Disconnected => "Disconnected".to_string(),
            Self::Connecting => "Connecting...".to_string(),
            Self::Connected {
                agent_name,
                protocol_version,
                auth_required,
            } => {
                if *auth_required {
                    format!("Connected to {agent_name} (ACP v{protocol_version}, sign-in required)")
                } else {
                    format!("Connected to {agent_name} (ACP v{protocol_version})")
                }
            }
            Self::Error(error) => format!("Error: {error}"),
        }
    }
}

#[derive(Debug, Clone)]
pub struct AcpAgentRecord {
    pub config: AcpAgentConfig,
    pub status: AcpAgentStatus,
}

/// Discovers configured ACP agents and probes them for availability.
pub struct AcpManager {
    global_dir: Option<PathBuf>,
    project_root: Option<PathBuf>,
    agents: TokioMutex<Vec<AcpAgentRecord>>,
}

impl AcpManager {
    pub fn new(global_dir: Option<PathBuf>, project_root: Option<PathBuf>) -> Self {
        Self {
            global_dir,
            project_root,
            agents: TokioMutex::new(Vec::new()),
        }
    }

    /// Merges project and global configuration, with project entries winning on
    /// a shared id.
    pub fn configs(&self) -> Vec<AcpAgentConfig> {
        let global = AcpSettings::load_global(self.global_dir.as_deref());
        let project = AcpSettings::load_project(self.project_root.as_deref());

        let mut merged = Vec::new();
        let mut seen = BTreeSet::new();
        for config in project.into_iter().chain(global) {
            if seen.insert(config.id.clone()) {
                merged.push(config);
            }
        }
        merged
    }

    /// Probes every enabled agent by completing an ACP handshake and then
    /// terminating the process. Disabled agents are reported without spawning.
    pub async fn discover_and_connect(&self) -> Vec<AcpAgentRecord> {
        let mut records = Vec::new();
        for config in self.configs() {
            let status = if config.enabled {
                Self::probe(&config, self.project_root.as_deref()).await
            } else {
                AcpAgentStatus::Disconnected
            };
            records.push(AcpAgentRecord { config, status });
        }

        *self.agents.lock().await = records.clone();
        records
    }

    pub async fn records(&self) -> Vec<AcpAgentRecord> {
        self.agents.lock().await.clone()
    }

    /// Completes a handshake and terminates the process.
    ///
    /// The probe runs with [`AcpProbeClient`], so an agent that issues
    /// filesystem or permission requests during `initialize` is refused rather
    /// than handed access to the current directory.
    async fn probe(config: &AcpAgentConfig, cwd: Option<&Path>) -> AcpAgentStatus {
        let handler: Arc<dyn AcpClientHandler> = Arc::new(AcpProbeClient);
        let connection = match AcpConnection::spawn(config, cwd, handler).await {
            Ok(connection) => connection,
            Err(error) => return AcpAgentStatus::Error(error),
        };
        let status = match connection.initialize().await {
            Ok(result) => AcpAgentStatus::Connected {
                agent_name: result.agent_display_name(),
                protocol_version: result.protocol_version,
                auth_required: result.requires_authentication(),
            },
            Err(error) => AcpAgentStatus::Error(error),
        };
        connection.shutdown().await;
        status
    }

    /// Opens a working session against the configured agent `id`.
    pub async fn start_session(
        &self,
        id: &str,
        cwd: &Path,
        handler: Arc<dyn AcpClientHandler>,
    ) -> Result<AcpSession, String> {
        let config = self
            .configs()
            .into_iter()
            .find(|config| config.id == id)
            .ok_or_else(|| format!("No ACP agent configured with id '{id}'"))?;
        if !config.enabled {
            return Err(format!("ACP agent '{}' is disabled", config.name));
        }
        AcpSession::start(&config, cwd, handler).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agent_config_round_trips_through_settings_file() {
        let config = AcpAgentConfig {
            id: "gemini".to_string(),
            name: "Gemini CLI".to_string(),
            command: "gemini".to_string(),
            args: vec!["--experimental-acp".to_string()],
            env: HashMap::new(),
            enabled: true,
            scope: AcpScope::Global,
        };
        let encoded = serde_json::to_string_pretty(&config).unwrap();
        assert!(encoded.contains("Gemini CLI"));
        let decoded: AcpAgentConfig = serde_json::from_str(&encoded).unwrap();
        assert_eq!(decoded, config);
    }

    #[test]
    fn agent_config_defaults_enabled_and_global_scope() {
        let file: AcpSettingsFile = serde_json::from_str(
            r#"{ "agents": [{ "id": "claude", "name": "Claude Code", "command": "claude-code-acp" }] }"#,
        )
        .unwrap();
        assert_eq!(file.agents.len(), 1);
        assert!(file.agents[0].enabled);
        assert_eq!(file.agents[0].scope, AcpScope::Global);
        assert!(file.agents[0].args.is_empty());
    }

    #[test]
    fn from_command_line_splits_command_and_args() {
        let config = AcpAgentConfig::from_command_line(
            "Claude Code",
            "  npx -y claude-code-acp  ",
            AcpScope::Project,
        )
        .unwrap();
        assert_eq!(config.id, "claude_code");
        assert_eq!(config.command, "npx");
        assert_eq!(config.args, vec!["-y", "claude-code-acp"]);
        assert_eq!(config.scope, AcpScope::Project);
        assert_eq!(config.command_line(), "npx -y claude-code-acp");
    }

    #[test]
    fn from_command_line_rejects_blank_input() {
        assert!(AcpAgentConfig::from_command_line("", "gemini", AcpScope::Global).is_none());
        assert!(AcpAgentConfig::from_command_line("Gemini", "   ", AcpScope::Global).is_none());
    }

    #[test]
    fn settings_save_and_load_round_trip_per_scope() {
        let dir = tempfile::tempdir().unwrap();
        let agents = vec![AcpAgentConfig {
            id: "gemini".to_string(),
            name: "Gemini".to_string(),
            command: "gemini".to_string(),
            args: vec!["--experimental-acp".to_string()],
            env: HashMap::new(),
            enabled: false,
            scope: AcpScope::Global,
        }];

        AcpSettings::save_global(dir.path(), &agents).unwrap();
        let loaded = AcpSettings::load_global(Some(dir.path()));
        assert_eq!(loaded.len(), 1);
        assert!(!loaded[0].enabled);
        assert_eq!(loaded[0].scope, AcpScope::Global);

        AcpSettings::save_project(dir.path(), &agents).unwrap();
        let project = AcpSettings::load_project(Some(dir.path()));
        assert_eq!(project.len(), 1);
        // Scope is derived from the file the entry came from, not its contents.
        assert_eq!(project[0].scope, AcpScope::Project);
    }

    #[test]
    fn missing_settings_files_load_as_empty() {
        let dir = tempfile::tempdir().unwrap();
        assert!(AcpSettings::load_global(Some(dir.path())).is_empty());
        assert!(AcpSettings::load_project(Some(dir.path())).is_empty());
        assert!(AcpSettings::load_global(None).is_empty());
        assert!(AcpSettings::load_project(None).is_empty());
    }

    #[test]
    fn project_agents_shadow_global_agents_with_the_same_id() {
        let global = tempfile::tempdir().unwrap();
        let project = tempfile::tempdir().unwrap();
        AcpSettings::save_global(
            global.path(),
            &[
                AcpAgentConfig::from_command_line("Shared", "global-cmd", AcpScope::Global)
                    .unwrap(),
                AcpAgentConfig::from_command_line("Global Only", "other", AcpScope::Global)
                    .unwrap(),
            ],
        )
        .unwrap();
        AcpSettings::save_project(
            project.path(),
            &[
                AcpAgentConfig::from_command_line("Shared", "project-cmd", AcpScope::Project)
                    .unwrap(),
            ],
        )
        .unwrap();

        let manager = AcpManager::new(
            Some(global.path().to_path_buf()),
            Some(project.path().to_path_buf()),
        );
        let configs = manager.configs();
        assert_eq!(configs.len(), 2);
        let shared = configs.iter().find(|c| c.id == "shared").unwrap();
        assert_eq!(shared.command, "project-cmd");
        assert_eq!(shared.scope, AcpScope::Project);
    }

    #[test]
    fn session_update_decodes_known_variants() {
        let agent_chunk = AcpSessionUpdate::from_value(json!({
            "sessionUpdate": "agent_message_chunk",
            "content": { "type": "text", "text": "hello" },
        }));
        assert_eq!(
            agent_chunk,
            AcpSessionUpdate::AgentMessageChunk(AcpContentBlock::text("hello"))
        );

        let thought = AcpSessionUpdate::from_value(json!({
            "sessionUpdate": "agent_thought_chunk",
            "content": { "type": "text", "text": "thinking" },
        }));
        assert!(matches!(thought, AcpSessionUpdate::AgentThoughtChunk(_)));

        let tool_call = AcpSessionUpdate::from_value(json!({
            "sessionUpdate": "tool_call",
            "toolCallId": "call_1",
            "title": "Read main.rs",
            "kind": "read",
            "status": "pending",
        }));
        let AcpSessionUpdate::ToolCall(call) = tool_call else {
            panic!("expected a tool call update");
        };
        assert_eq!(call.tool_call_id, "call_1");
        assert_eq!(call.kind, Some(AcpToolKind::Read));
        assert_eq!(call.status, Some(AcpToolCallStatus::Pending));

        let plan = AcpSessionUpdate::from_value(json!({
            "sessionUpdate": "plan",
            "entries": [{ "content": "step", "priority": "high", "status": "pending" }],
        }));
        let AcpSessionUpdate::Plan(entries) = plan else {
            panic!("expected a plan update");
        };
        assert_eq!(entries.len(), 1);

        let mode = AcpSessionUpdate::from_value(json!({
            "sessionUpdate": "current_mode_update",
            "currentModeId": "ask",
        }));
        assert_eq!(
            mode,
            AcpSessionUpdate::CurrentModeUpdate {
                current_mode_id: "ask".to_string()
            }
        );
    }

    #[test]
    fn session_update_keeps_unknown_variants() {
        let update = AcpSessionUpdate::from_value(json!({
            "sessionUpdate": "usage_update",
            "tokens": 42,
        }));
        let AcpSessionUpdate::Other { kind, payload } = update else {
            panic!("expected an unknown update to be preserved");
        };
        assert_eq!(kind, "usage_update");
        assert_eq!(payload.get("tokens").and_then(Value::as_u64), Some(42));
    }

    #[test]
    fn unknown_content_block_type_does_not_fail() {
        let block: AcpContentBlock =
            serde_json::from_value(json!({ "type": "future_kind", "data": 1 })).unwrap();
        assert_eq!(block, AcpContentBlock::Unknown);
        assert!(block.as_text().is_none());
    }

    #[test]
    fn prompt_blocks_serialize_in_protocol_form() {
        let encoded = serde_json::to_value(vec![AcpContentBlock::text("hi")]).unwrap();
        assert_eq!(encoded, json!([{ "type": "text", "text": "hi" }]));
    }

    #[test]
    fn permission_policy_prefers_matching_option_kind() {
        let options = vec![
            AcpPermissionOption {
                option_id: "allow".to_string(),
                name: "Allow".to_string(),
                kind: AcpPermissionOptionKind::AllowOnce,
            },
            AcpPermissionOption {
                option_id: "always".to_string(),
                name: "Always allow".to_string(),
                kind: AcpPermissionOptionKind::AllowAlways,
            },
            AcpPermissionOption {
                option_id: "no".to_string(),
                name: "Reject".to_string(),
                kind: AcpPermissionOptionKind::RejectOnce,
            },
        ];

        assert_eq!(
            AcpPermissionPolicy::default().select(&options),
            AcpPermissionOutcome::Selected {
                option_id: "no".to_string()
            }
        );
        assert_eq!(
            AcpPermissionPolicy::AllowOnce.select(&options),
            AcpPermissionOutcome::Selected {
                option_id: "allow".to_string()
            }
        );
        assert_eq!(
            AcpPermissionPolicy::AllowAlways.select(&options),
            AcpPermissionOutcome::Selected {
                option_id: "always".to_string()
            }
        );
    }

    #[test]
    fn permission_policy_cancels_when_no_option_matches() {
        let options = vec![AcpPermissionOption {
            option_id: "weird".to_string(),
            name: "Weird".to_string(),
            kind: AcpPermissionOptionKind::Unknown,
        }];
        assert_eq!(
            AcpPermissionPolicy::AllowOnce.select(&options),
            AcpPermissionOutcome::Cancelled
        );
    }

    #[test]
    fn permission_outcome_serializes_to_protocol_shape() {
        assert_eq!(
            AcpPermissionOutcome::Selected {
                option_id: "allow".to_string()
            }
            .to_json(),
            json!({ "outcome": { "outcome": "selected", "optionId": "allow" } })
        );
        assert_eq!(
            AcpPermissionOutcome::Cancelled.to_json(),
            json!({ "outcome": { "outcome": "cancelled" } })
        );
    }

    #[test]
    fn stop_reason_decodes_known_and_unknown_values() {
        assert_eq!(
            AcpStopReason::from_value(&json!("end_turn")),
            AcpStopReason::EndTurn
        );
        assert_eq!(
            AcpStopReason::from_value(&json!("cancelled")),
            AcpStopReason::Cancelled
        );
        assert_eq!(
            AcpStopReason::from_value(&json!("something_new")),
            AcpStopReason::Unknown
        );
    }

    #[test]
    fn tool_call_tolerates_unknown_kind_and_status() {
        let call: AcpToolCall = serde_json::from_value(json!({
            "toolCallId": "call_1",
            "kind": "teleport",
            "status": "vibing",
        }))
        .unwrap();
        assert_eq!(call.tool_call_id, "call_1");
        assert_eq!(call.kind, None);
        assert_eq!(call.status, None);
    }

    #[test]
    fn permission_option_tolerates_unknown_kind() {
        let option: AcpPermissionOption = serde_json::from_value(json!({
            "optionId": "maybe",
            "name": "Maybe",
            "kind": "ask_later",
        }))
        .unwrap();
        assert_eq!(option.kind, AcpPermissionOptionKind::Unknown);
    }

    #[test]
    fn agent_status_display_reports_auth_requirement() {
        let connected = AcpAgentStatus::Connected {
            agent_name: "Gemini".to_string(),
            protocol_version: 1,
            auth_required: true,
        };
        assert_eq!(
            connected.display_status(),
            "Connected to Gemini (ACP v1, sign-in required)"
        );
        assert_eq!(
            AcpAgentStatus::Error("boom".to_string()).display_status(),
            "Error: boom"
        );
        assert_eq!(
            AcpAgentStatus::Disconnected.display_status(),
            "Disconnected"
        );
    }
}
