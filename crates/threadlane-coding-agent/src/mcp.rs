use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{BTreeSet, HashMap};
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, RwLock};
use std::time::Duration;
use threadlane_agent::{AgentToolDefinition, ToolExecutor};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;
use tokio::sync::Mutex as TokioMutex;

const MCP_SETTINGS_FILE: &str = "mcp.json";
const MCP_PROJECT_SETTINGS_RELATIVE_PATH: &str = ".threadlane/mcp.json";
const MAX_MCP_SETTINGS_BYTES: usize = 512 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum McpScope {
    Global,
    Project,
}

impl McpScope {
    pub fn display_name(self) -> &'static str {
        match self {
            McpScope::Global => "Global (~/.threadlane)",
            McpScope::Project => "Project (.threadlane)",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum McpTransport {
    Stdio {
        command: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        args: Vec<String>,
        #[serde(default, skip_serializing_if = "HashMap::is_empty")]
        env: HashMap<String, String>,
    },
    Sse {
        url: String,
        #[serde(default, skip_serializing_if = "HashMap::is_empty")]
        headers: HashMap<String, String>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct McpServerConfig {
    pub id: String,
    pub name: String,
    pub transport: McpTransport,
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    pub scope: McpScope,
}

fn default_enabled() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct McpSettingsFile {
    #[serde(default)]
    servers: Vec<McpServerConfig>,
}

#[derive(Debug, Clone, Default)]
pub struct McpSettings {
    servers: Vec<McpServerConfig>,
}

impl McpSettings {
    pub fn servers(&self) -> &[McpServerConfig] {
        &self.servers
    }

    pub fn load_global(global_dir: Option<&Path>) -> Vec<McpServerConfig> {
        let Some(dir) = global_dir else {
            return Vec::new();
        };
        let path = dir.join(MCP_SETTINGS_FILE);
        Self::load_file(&path, McpScope::Global)
    }

    pub fn load_project(project_root: Option<&Path>) -> Vec<McpServerConfig> {
        let Some(root) = project_root else {
            return Vec::new();
        };
        let path = root.join(MCP_PROJECT_SETTINGS_RELATIVE_PATH);
        Self::load_file(&path, McpScope::Project)
    }

    fn load_file(path: &Path, scope: McpScope) -> Vec<McpServerConfig> {
        let bytes = match fs::read(path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == ErrorKind::NotFound => return Vec::new(),
            Err(_) => return Vec::new(),
        };
        if bytes.len() > MAX_MCP_SETTINGS_BYTES {
            return Vec::new();
        }
        let parsed: McpSettingsFile = match serde_json::from_slice(&bytes) {
            Ok(data) => data,
            Err(_) => return Vec::new(),
        };
        parsed
            .servers
            .into_iter()
            .map(|mut config| {
                config.scope = scope;
                config
            })
            .collect()
    }

    pub fn save_global(dir: &Path, servers: &[McpServerConfig]) -> Result<(), String> {
        Self::save_file(&dir.join(MCP_SETTINGS_FILE), servers)
    }

    pub fn save_project(root: &Path, servers: &[McpServerConfig]) -> Result<(), String> {
        Self::save_file(&root.join(MCP_PROJECT_SETTINGS_RELATIVE_PATH), servers)
    }

    fn save_file(file_path: &Path, servers: &[McpServerConfig]) -> Result<(), String> {
        if let Some(parent) = file_path.parent() {
            fs::create_dir_all(parent)
                .map_err(|e| format!("Failed to create settings directory: {e}"))?;
        }
        let file_data = McpSettingsFile {
            servers: servers.to_vec(),
        };
        let bytes = serde_json::to_vec_pretty(&file_data)
            .map_err(|e| format!("Failed to serialize MCP settings: {e}"))?;
        fs::write(&file_path, bytes).map_err(|e| format!("Failed to write MCP settings: {e}"))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum McpServerStatus {
    Disconnected,
    Connecting,
    Connected { tool_count: usize },
    Error(String),
}

impl McpServerStatus {
    pub fn display_status(&self) -> String {
        match self {
            McpServerStatus::Disconnected => "Disconnected".to_string(),
            McpServerStatus::Connecting => "Connecting...".to_string(),
            McpServerStatus::Connected { tool_count } => {
                format!(
                    "Connected ({} tool{})",
                    tool_count,
                    if *tool_count == 1 { "" } else { "s" }
                )
            }
            McpServerStatus::Error(err) => format!("Error: {err}"),
        }
    }
}

#[derive(Debug, Clone)]
pub struct McpToolInfo {
    pub server_id: String,
    pub tool_name: String,
    pub full_name: String,
    pub definition: AgentToolDefinition,
}

#[derive(Debug, Clone)]
pub struct McpServerRecord {
    pub config: McpServerConfig,
    pub status: McpServerStatus,
    pub tools: Vec<McpToolInfo>,
}

pub struct McpManager {
    global_dir: Option<PathBuf>,
    project_root: Option<PathBuf>,
    servers: TokioMutex<Vec<McpServerRecord>>,
    cached_tool_defs: RwLock<Vec<AgentToolDefinition>>,
}

impl McpManager {
    pub fn new(global_dir: Option<PathBuf>, project_root: Option<PathBuf>) -> Self {
        Self {
            global_dir,
            project_root,
            servers: TokioMutex::new(Vec::new()),
            cached_tool_defs: RwLock::new(Vec::new()),
        }
    }

    pub async fn discover_and_connect(&self) -> Vec<McpServerRecord> {
        let global_configs = McpSettings::load_global(self.global_dir.as_deref());
        let project_configs = McpSettings::load_project(self.project_root.as_deref());

        let mut all_configs = Vec::new();
        let mut seen_ids = BTreeSet::new();

        for config in project_configs
            .into_iter()
            .chain(global_configs.into_iter())
        {
            if seen_ids.insert(config.id.clone()) {
                all_configs.push(config);
            }
        }

        let mut records = Vec::new();
        let mut tool_defs = Vec::new();

        for config in all_configs {
            if !config.enabled {
                records.push(McpServerRecord {
                    config,
                    status: McpServerStatus::Disconnected,
                    tools: Vec::new(),
                });
                continue;
            }

            let (status, tools) = Self::connect_server(&config).await;
            for t in &tools {
                tool_defs.push(t.definition.clone());
            }
            records.push(McpServerRecord {
                config,
                status,
                tools,
            });
        }

        let mut guard = self.servers.lock().await;
        *guard = records.clone();
        if let Ok(mut cached) = self.cached_tool_defs.write() {
            *cached = tool_defs;
        }
        records
    }

    async fn connect_server(config: &McpServerConfig) -> (McpServerStatus, Vec<McpToolInfo>) {
        match &config.transport {
            McpTransport::Stdio { command, args, env } => {
                let mut cmd = Command::new(command);
                cmd.args(args);
                for (k, v) in env {
                    cmd.env(k, v);
                }
                cmd.stdin(Stdio::piped())
                    .stdout(Stdio::piped())
                    .stderr(Stdio::null());

                let mut child = match cmd.spawn() {
                    Ok(c) => c,
                    Err(e) => {
                        return (
                            McpServerStatus::Error(format!("Failed to spawn process: {e}")),
                            Vec::new(),
                        )
                    }
                };

                let mut stdin = match child.stdin.take() {
                    Some(s) => s,
                    None => {
                        return (
                            McpServerStatus::Error("Failed to open stdin".into()),
                            Vec::new(),
                        )
                    }
                };
                let stdout = match child.stdout.take() {
                    Some(s) => s,
                    None => {
                        return (
                            McpServerStatus::Error("Failed to open stdout".into()),
                            Vec::new(),
                        )
                    }
                };

                let mut reader = BufReader::new(stdout);

                // Send initialize JSON-RPC request
                let init_req = json!({
                    "jsonrpc": "2.0",
                    "id": 1,
                    "method": "initialize",
                    "params": {
                        "protocolVersion": "2024-11-05",
                        "capabilities": {},
                        "clientInfo": {
                            "name": "threadlane",
                            "version": env!("CARGO_PKG_VERSION")
                        }
                    }
                });

                let mut line = String::new();
                let write_res = stdin.write_all(format!("{}\n", init_req).as_bytes()).await;
                if let Err(e) = write_res {
                    let _ = child.kill().await;
                    return (
                        McpServerStatus::Error(format!("Failed to send initialize: {e}")),
                        Vec::new(),
                    );
                }

                let read_res =
                    tokio::time::timeout(Duration::from_secs(5), reader.read_line(&mut line)).await;
                if read_res.is_err() || line.trim().is_empty() {
                    let _ = child.kill().await;
                    return (
                        McpServerStatus::Error("Initialize response timed out".into()),
                        Vec::new(),
                    );
                }

                // Send initialized notification
                let init_notif = json!({
                    "jsonrpc": "2.0",
                    "method": "notifications/initialized"
                });
                let _ = stdin
                    .write_all(format!("{}\n", init_notif).as_bytes())
                    .await;

                // Send tools/list request
                let list_req = json!({
                    "jsonrpc": "2.0",
                    "id": 2,
                    "method": "tools/list",
                    "params": {}
                });
                let _ = stdin.write_all(format!("{}\n", list_req).as_bytes()).await;

                line.clear();
                let list_res =
                    tokio::time::timeout(Duration::from_secs(5), reader.read_line(&mut line)).await;
                let _ = child.kill().await;

                if list_res.is_err() || line.trim().is_empty() {
                    return (
                        McpServerStatus::Error("tools/list response timed out".into()),
                        Vec::new(),
                    );
                }

                let response_json: Value = match serde_json::from_str(line.trim()) {
                    Ok(val) => val,
                    Err(e) => {
                        return (
                            McpServerStatus::Error(format!("Failed to parse response: {e}")),
                            Vec::new(),
                        )
                    }
                };

                let mut mcp_tools = Vec::new();
                if let Some(tools_arr) = response_json
                    .get("result")
                    .and_then(|r| r.get("tools"))
                    .and_then(|t| t.as_array())
                {
                    for tool_val in tools_arr {
                        if let Some(name) = tool_val.get("name").and_then(|n| n.as_str()) {
                            let description = tool_val
                                .get("description")
                                .and_then(|d| d.as_str())
                                .unwrap_or("MCP tool");
                            let input_schema = tool_val
                                .get("inputSchema")
                                .cloned()
                                .unwrap_or_else(|| json!({"type": "object", "properties": {}}));

                            let full_name = format!("mcp__{}__{}", config.id, name);
                            let definition = AgentToolDefinition::new(
                                full_name.clone(),
                                format!("[MCP: {}] {}", config.name, description),
                                input_schema,
                            );

                            mcp_tools.push(McpToolInfo {
                                server_id: config.id.clone(),
                                tool_name: name.to_string(),
                                full_name,
                                definition,
                            });
                        }
                    }
                }

                let count = mcp_tools.len();
                (McpServerStatus::Connected { tool_count: count }, mcp_tools)
            }
            McpTransport::Sse { url, .. } => (
                McpServerStatus::Error(format!("SSE transport ({url}) not active")),
                Vec::new(),
            ),
        }
    }

    pub fn get_tools_sync(&self) -> Vec<AgentToolDefinition> {
        self.cached_tool_defs
            .read()
            .map(|defs| defs.clone())
            .unwrap_or_default()
    }

    pub async fn execute_tool(
        &self,
        full_name: &str,
        args: &str,
    ) -> Option<Result<String, String>> {
        let guard = self.servers.lock().await;
        let mut target = None;
        for server in guard.iter() {
            if !server.config.enabled {
                continue;
            }
            for tool in &server.tools {
                if tool.full_name == full_name || tool.tool_name == full_name {
                    target = Some((server.config.clone(), tool.tool_name.clone()));
                    break;
                }
            }
            if target.is_some() {
                break;
            }
        }
        drop(guard);

        let (config, tool_name) = target?;
        let parsed_args: Value = match serde_json::from_str(args) {
            Ok(v) => v,
            Err(e) => return Some(Err(format!("Invalid JSON tool arguments: {e}"))),
        };

        match config.transport {
            McpTransport::Stdio { command, args, env } => {
                let mut cmd = Command::new(&command);
                cmd.args(&args);
                for (k, v) in &env {
                    cmd.env(k, v);
                }
                cmd.stdin(Stdio::piped())
                    .stdout(Stdio::piped())
                    .stderr(Stdio::null());

                let mut child = match cmd.spawn() {
                    Ok(c) => c,
                    Err(e) => return Some(Err(format!("Failed to spawn MCP server: {e}"))),
                };

                let mut stdin = child.stdin.take().unwrap();
                let stdout = child.stdout.take().unwrap();
                let mut reader = BufReader::new(stdout);

                // Handshake
                let init_req = json!({
                    "jsonrpc": "2.0",
                    "id": 1,
                    "method": "initialize",
                    "params": {
                        "protocolVersion": "2024-11-05",
                        "capabilities": {},
                        "clientInfo": { "name": "threadlane", "version": env!("CARGO_PKG_VERSION") }
                    }
                });
                let _ = stdin.write_all(format!("{}\n", init_req).as_bytes()).await;

                let mut line = String::new();
                let _ =
                    tokio::time::timeout(Duration::from_secs(5), reader.read_line(&mut line)).await;

                let init_notif = json!({ "jsonrpc": "2.0", "method": "notifications/initialized" });
                let _ = stdin
                    .write_all(format!("{}\n", init_notif).as_bytes())
                    .await;

                // Execute call
                let call_req = json!({
                    "jsonrpc": "2.0",
                    "id": 2,
                    "method": "tools/call",
                    "params": {
                        "name": tool_name,
                        "arguments": parsed_args
                    }
                });
                let _ = stdin.write_all(format!("{}\n", call_req).as_bytes()).await;

                line.clear();
                let read_res =
                    tokio::time::timeout(Duration::from_secs(15), reader.read_line(&mut line))
                        .await;
                let _ = child.kill().await;

                if read_res.is_err() || line.trim().is_empty() {
                    return Some(Err("MCP tool execution timed out".into()));
                }

                let response_json: Value = match serde_json::from_str(line.trim()) {
                    Ok(v) => v,
                    Err(e) => {
                        return Some(Err(format!("Failed to parse tool execution response: {e}")))
                    }
                };

                if let Some(err) = response_json.get("error") {
                    let msg = err
                        .get("message")
                        .and_then(|m| m.as_str())
                        .unwrap_or("MCP error");
                    return Some(Err(msg.to_string()));
                }

                let mut output = String::new();
                if let Some(content) = response_json
                    .get("result")
                    .and_then(|r| r.get("content"))
                    .and_then(|c| c.as_array())
                {
                    for item in content {
                        if let Some(text) = item.get("text").and_then(|t| t.as_str()) {
                            if !output.is_empty() {
                                output.push('\n');
                            }
                            output.push_str(text);
                        }
                    }
                }

                if output.is_empty() {
                    output = serde_json::to_string_pretty(&response_json).unwrap_or_default();
                }

                Some(Ok(output))
            }
            McpTransport::Sse { url, .. } => Some(Err(format!(
                "SSE transport for {url} not currently supported"
            ))),
        }
    }
}

pub struct McpToolExecutor {
    manager: Arc<McpManager>,
}

impl McpToolExecutor {
    pub fn new(manager: Arc<McpManager>) -> Self {
        Self { manager }
    }
}

#[async_trait]
impl ToolExecutor for McpToolExecutor {
    fn executor_id(&self) -> &str {
        "threadlane.mcp_tools"
    }

    fn tool_definitions(&self) -> Vec<AgentToolDefinition> {
        self.manager.get_tools_sync()
    }

    async fn execute_tool(&self, name: &str, args: &str) -> Option<Result<String, String>> {
        self.manager.execute_tool(name, args).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_mcp_server_config_serialization() {
        let config = McpServerConfig {
            id: "fs".to_string(),
            name: "Filesystem".to_string(),
            transport: McpTransport::Stdio {
                command: "npx".to_string(),
                args: vec![
                    "-y".to_string(),
                    "@modelcontextprotocol/server-filesystem".to_string(),
                ],
                env: HashMap::new(),
            },
            enabled: true,
            scope: McpScope::Global,
        };

        let json_str = serde_json::to_string_pretty(&config).unwrap();
        assert!(json_str.contains("Filesystem"));
        assert!(json_str.contains("npx"));

        let deserialized: McpServerConfig = serde_json::from_str(&json_str).unwrap();
        assert_eq!(deserialized, config);
    }
}
