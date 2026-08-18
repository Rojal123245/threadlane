use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};
use threadlane_agent::{
    AgentEvent, AgentMessage, ImageAttachment, ReasoningEffort, SessionPlan, SessionTree,
    TokenUsage,
};

use crate::adapters::agent_events::{adapt_agent_event, ChatAgentUpdate};
use crate::persistence::{load_project_registry, save_project_registry};
use crate::services::sessions::{SessionRuntime, SessionRuntimeStatus};

const CHAT_HISTORY_PAGE_SIZE: usize = 40;
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct AttachedProject {
    pub(crate) path: PathBuf,
    display_name: String,
    attached_at: u64,
    last_opened_at: u64,
    #[serde(default)]
    last_session_id: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SessionHealth {
    Healthy,
    Working,
    Warning,
}

#[derive(Clone, Debug)]
pub struct SessionInfo {
    pub(crate) id: String,
    pub(crate) title: String,
    pub(crate) work_dir: PathBuf,
    pub(crate) session_file: PathBuf,
    pub(crate) updated_at: u64,
    pub(crate) health: SessionHealth,
}

#[derive(Clone, Debug)]
pub struct ProjectInfo {
    pub(crate) name: String,
    pub(crate) work_dir: PathBuf,
    pub(crate) sessions: Vec<SessionInfo>,
    is_expanded: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MessageRole {
    User,
    Assistant,
    System,
    Advisor(threadlane_agent::AdvisorSeverity),
    Error,
}

#[derive(Clone, Debug)]
pub struct ToolActivityInfo {
    pub(crate) id: String,
    pub(crate) category: String,
    pub(crate) title: String,
    pub(crate) summary: String,
    pub(crate) detail: String,
    pub(crate) is_expanded: bool,
}

#[derive(Clone, Debug, serde::Serialize)]
pub struct TrajectoryEntry {
    pub(crate) seq: Option<u64>,
    pub(crate) run_id: Option<String>,
    pub(crate) turn: Option<u32>,
    pub(crate) category: String,
    pub(crate) summary: String,
    pub(crate) detail: String,
    pub(crate) lane: Option<String>,
    pub(crate) correlation_id: Option<String>,
}

#[derive(Clone, Debug, Default)]
pub struct SessionMetricsInfo {
    pub(crate) turns: usize,
    pub(crate) tool_calls: usize,
    pub(crate) input_tokens: u64,
    pub(crate) output_tokens: u64,
}

#[derive(Clone, Debug)]
pub struct ChatMessageInfo {
    pub(crate) id: String,
    pub(crate) role: MessageRole,
    pub(crate) content: String,
    pub(crate) tool_activities: Vec<ToolActivityInfo>,
    pub(crate) streaming: bool,
    pub(crate) reasoning_content: Option<String>,
    pub(crate) reasoning_expanded: bool,
}

#[derive(Clone, Debug)]
pub enum ChatStreamEvent {
    Agent {
        session_id: String,
        event: AgentEvent,
    },
    Finished {
        session_id: String,
        session_file: PathBuf,
    },
    TitleGenerated {
        session_id: String,
        session_file: PathBuf,
    },
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum WorkspacePage {
    #[default]
    Chat,
    Settings,
}

pub struct AppState {
    pub(crate) projects: Vec<ProjectInfo>,
    pub(crate) active_work_dir: Option<PathBuf>,
    pub(crate) active_session_id: Option<String>,
    pub(crate) is_new_task: bool,
    pub(crate) search_query: String,
    pub(crate) messages: Vec<ChatMessageInfo>,
    history_session_file: Option<PathBuf>,
    history_start: usize,
    history_has_older: bool,
    pub(crate) active_plan: SessionPlan,
    pub(crate) is_generating: bool,
    composer_text: String,
    pub(crate) session_status: Option<String>,
    pending_composer_messages: HashMap<String, String>,
    session_token_usage: HashMap<String, TokenUsage>,
    trajectory_by_session: HashMap<String, Vec<TrajectoryEntry>>,
    session_metrics: HashMap<String, SessionMetricsInfo>,
    stashed_prompts: HashMap<String, String>,
    pub(crate) pending_permissions: HashMap<String, threadlane_agent::PermissionRequest>,

    pub(crate) selected_model: String,
    pub(crate) model_roles: threadlane_agent::ModelRoles,
    pub(crate) reasoning_effort: ReasoningEffort,
    pub(crate) workspace_page: WorkspacePage,
    pub(crate) openai_key: String,
    pub(crate) opencode_key: String,
    pub(crate) auth_status_msg: Option<String>,
    pub(crate) update_status: threadlane_updater::UpdateStatus,
    pub(crate) update_notice_dismissed: bool,
    stream_tx: Sender<ChatStreamEvent>,
    stream_rx: Receiver<ChatStreamEvent>,
    pending_stream_event: Mutex<Option<ChatStreamEvent>>,
    session_refresh_tx: Sender<PathBuf>,
    session_refresh_rx: Receiver<(PathBuf, Vec<SessionInfo>)>,
    session_runtimes: HashMap<PathBuf, Arc<SessionRuntime>>,
    deferred_stream_events: HashMap<String, Vec<ChatStreamEvent>>,
}

#[derive(Default)]
struct SessionDiscoveryCache {
    entries: HashMap<PathBuf, SessionDiscoveryCacheEntry>,
}

struct SessionDiscoveryCacheEntry {
    len: u64,
    modified: Option<SystemTime>,
    info: SessionInfo,
}
fn file_mtime(path: &Path) -> u64 {
    std::fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn extract_session_title(tree: &SessionTree, fallback_id: &str) -> String {
    if let Some(ref name) = tree.name {
        if !name.trim().is_empty() {
            return name.clone();
        }
    }
    let messages = {
        let active = tree.get_active_branch_messages();
        if active.is_empty() {
            tree.get_persisted_messages()
        } else {
            active
        }
    };

    for msg in &messages {
        match msg {
            AgentMessage::User { content } | AgentMessage::UserWithImages { content, .. } => {
                let trimmed = content.trim();
                if !trimmed.is_empty() {
                    let first_line = trimmed.lines().next().unwrap_or(trimmed);
                    let mut char_count = 0;
                    let mut result = String::new();
                    for ch in first_line.chars() {
                        if char_count >= 40 {
                            result.push('…');
                            break;
                        }
                        result.push(ch);
                        char_count += 1;
                    }
                    return result;
                }
            }
            _ => {}
        }
    }
    fallback_id.to_string()
}

pub fn discover_sessions_in_project(work_dir: &Path) -> Vec<SessionInfo> {
    let mut cache = SessionDiscoveryCache::default();
    discover_sessions_in_project_cached(work_dir, &mut cache)
}

fn discover_sessions_in_project_cached(
    work_dir: &Path,
    cache: &mut SessionDiscoveryCache,
) -> Vec<SessionInfo> {
    let sessions_dir = work_dir.join(".threadlane/sessions");
    let Ok(entries) = std::fs::read_dir(&sessions_dir) else {
        return Vec::new();
    };

    let canonical_work_dir =
        std::fs::canonicalize(work_dir).unwrap_or_else(|_| work_dir.to_path_buf());
    let mut sessions = Vec::new();
    let mut seen_paths = std::collections::HashSet::new();

    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("jsonl")
            || path
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.ends_with(".harness.jsonl"))
        {
            continue;
        }
        seen_paths.insert(path.clone());
        let metadata = std::fs::metadata(&path).ok();
        let len = metadata.as_ref().map_or(0, |metadata| metadata.len());
        let modified = metadata.and_then(|metadata| metadata.modified().ok());
        let info = match cache.entries.get(&path) {
            Some(cached) if cached.len == len && cached.modified == modified => cached.info.clone(),
            _ => {
                let id = path
                    .file_stem()
                    .map(|s| s.to_string_lossy().to_string())
                    .unwrap_or_else(|| "session".into());
                let (title, health, updated_at) = match SessionTree::load_from_file(&path) {
                    Ok(tree) => (
                        extract_session_title(&tree, &id),
                        SessionHealth::Healthy,
                        file_mtime(&path),
                    ),
                    Err(_) => (
                        "Unreadable session".to_string(),
                        SessionHealth::Warning,
                        file_mtime(&path),
                    ),
                };
                let info = SessionInfo {
                    id,
                    title,
                    work_dir: canonical_work_dir.clone(),
                    session_file: path.clone(),
                    updated_at,
                    health,
                };
                cache.entries.insert(
                    path.clone(),
                    SessionDiscoveryCacheEntry {
                        len,
                        modified,
                        info: info.clone(),
                    },
                );
                info
            }
        };
        sessions.push(info);
    }

    cache.entries.retain(|path, _| seen_paths.contains(path));
    sessions.sort_by(|a, b| {
        b.updated_at
            .cmp(&a.updated_at)
            .then_with(|| a.title.cmp(&b.title))
    });
    sessions
}

pub fn load_session_plan(session_file: &Path) -> SessionPlan {
    SessionTree::load_from_file(session_file)
        .map(|tree| tree.plan().clone())
        .unwrap_or_default()
}

pub fn load_session_messages(session_file: &Path) -> Vec<ChatMessageInfo> {
    load_session_message_page(session_file, usize::MAX).0
}

fn load_session_projection(
    session_file: &Path,
) -> (SessionPlan, Vec<ChatMessageInfo>, usize, bool) {
    let Ok(tree) = SessionTree::load_from_file(session_file) else {
        return (SessionPlan::default(), Vec::new(), 0, false);
    };
    let agent_messages = {
        let branch = tree.get_active_branch_messages();
        if branch.is_empty() {
            tree.get_persisted_messages()
        } else {
            branch
        }
    };
    let projected = project_agent_messages(agent_messages);
    let end = projected.len();
    let start = end.saturating_sub(CHAT_HISTORY_PAGE_SIZE);
    (
        tree.plan().clone(),
        projected[start..end].to_vec(),
        start,
        start > 0,
    )
}

fn load_session_message_page(
    session_file: &Path,
    end: usize,
) -> (Vec<ChatMessageInfo>, usize, bool) {
    let Ok(tree) = SessionTree::load_from_file(session_file) else {
        return (Vec::new(), 0, false);
    };
    let agent_messages = {
        let branch = tree.get_active_branch_messages();
        if branch.is_empty() {
            tree.get_persisted_messages()
        } else {
            branch
        }
    };
    let projected = project_agent_messages(agent_messages);
    let end = end.min(projected.len());
    let start = end.saturating_sub(CHAT_HISTORY_PAGE_SIZE);
    (projected[start..end].to_vec(), start, start > 0)
}

fn tool_activity_summary(name: &str, arguments: &str) -> String {
    let display_name = name.replace('_', " ");
    let Ok(arguments) = serde_json::from_str::<serde_json::Value>(arguments) else {
        return display_name;
    };
    let context = ["path", "file_path", "regex", "query", "glob", "command"]
        .iter()
        .find_map(|key| arguments.get(key).and_then(|value| value.as_str()));
    context
        .filter(|value| !value.is_empty())
        .map(|value| format!("{display_name} {value}"))
        .unwrap_or(display_name)
}

fn project_agent_messages(agent_messages: Vec<AgentMessage>) -> Vec<ChatMessageInfo> {
    let mut result = Vec::new();
    let mut msg_counter = 0;

    for msg in agent_messages {
        msg_counter += 1;
        match msg {
            AgentMessage::User { content } => {
                let role = if content.starts_with("[ADVISOR NOTE (Aside)]") {
                    MessageRole::Advisor(threadlane_agent::AdvisorSeverity::Aside)
                } else if content.starts_with("[ADVISOR NOTE (Concern)]") {
                    MessageRole::Advisor(threadlane_agent::AdvisorSeverity::Concern)
                } else if content.starts_with("[ADVISOR NOTE (CRITICAL BLOCKER)]") {
                    MessageRole::Advisor(threadlane_agent::AdvisorSeverity::Blocker)
                } else {
                    MessageRole::User
                };
                result.push(ChatMessageInfo {
                    id: format!("msg_{msg_counter}"),
                    role,
                    content,
                    tool_activities: Vec::new(),
                    streaming: false,
                    reasoning_content: None,
                    reasoning_expanded: false,
                });
            }
            AgentMessage::UserWithImages { content, .. } => {
                result.push(ChatMessageInfo {
                    id: format!("msg_{msg_counter}"),
                    role: MessageRole::User,
                    content,
                    tool_activities: Vec::new(),
                    streaming: false,
                    reasoning_content: None,
                    reasoning_expanded: false,
                });
            }
            AgentMessage::Assistant {
                content,
                tool_calls,
                ..
            } => {
                let mut tool_activities = Vec::new();
                if let Some(calls) = tool_calls {
                    for call in calls {
                        let category = match call.function.name.as_str() {
                            "write_file"
                            | "replace_file_content"
                            | "multi_replace_file_content" => "Edited".into(),
                            "create_file" => "Created".into(),
                            "run_command" | "execute" => "Ran".into(),
                            "read_file" | "list_dir" => "Loaded".into(),
                            _ => "Explored".into(),
                        };
                        let detail = call.function.arguments.clone();
                        let title = call.function.name.clone();
                        tool_activities.push(ToolActivityInfo {
                            id: call.id.clone(),
                            category,
                            summary: tool_activity_summary(&title, &detail),
                            title,
                            detail,
                            is_expanded: false,
                        });
                    }
                }
                result.push(ChatMessageInfo {
                    id: format!("msg_{msg_counter}"),
                    role: MessageRole::Assistant,
                    content: content.unwrap_or_default(),
                    tool_activities,
                    streaming: false,
                    reasoning_content: None,
                    reasoning_expanded: false,
                });
            }
            AgentMessage::Tool {
                tool_call_id,
                name,
                content,
                is_error,
                ..
            } => {
                let category = if is_error {
                    "Error".into()
                } else {
                    "Result".into()
                };
                if let Some(activity) = result
                    .iter_mut()
                    .rev()
                    .flat_map(|message| message.tool_activities.iter_mut().rev())
                    .find(|activity| activity.id == tool_call_id)
                {
                    activity.category = category;
                    activity.detail = content;
                    continue;
                }
                let tool_info = ToolActivityInfo {
                    id: tool_call_id,
                    category,
                    summary: tool_activity_summary(&name, ""),
                    title: name,
                    detail: content,
                    is_expanded: false,
                };
                if let Some(last) = result.last_mut() {
                    if last.role == MessageRole::Assistant {
                        last.tool_activities.push(tool_info);
                        continue;
                    }
                }
                result.push(ChatMessageInfo {
                    id: format!("msg_{msg_counter}"),
                    role: MessageRole::Assistant,
                    content: String::new(),
                    tool_activities: vec![tool_info],
                    streaming: false,
                    reasoning_content: None,
                    reasoning_expanded: false,
                });
            }
            AgentMessage::System { content } => {
                let role = if content.to_lowercase().contains("error")
                    || content.to_lowercase().contains("failed")
                {
                    MessageRole::Error
                } else {
                    MessageRole::System
                };
                result.push(ChatMessageInfo {
                    id: format!("msg_{msg_counter}"),
                    role,
                    content,
                    tool_activities: Vec::new(),
                    streaming: false,
                    reasoning_content: None,
                    reasoning_expanded: false,
                });
            }
            AgentMessage::Custom {
                custom_type,
                payload,
            } => {
                let text = payload
                    .get("text")
                    .or_else(|| payload.get("error"))
                    .and_then(|value| value.as_str())
                    .map(ToString::to_string)
                    .unwrap_or_else(|| payload.to_string());
                if custom_type == "thinking" {
                    result.push(ChatMessageInfo {
                        id: format!("msg_{msg_counter}"),
                        role: MessageRole::Assistant,
                        content: String::new(),
                        tool_activities: Vec::new(),
                        streaming: false,
                        reasoning_content: Some(text),
                        reasoning_expanded: false,
                    });
                    continue;
                }

                let is_error_type = custom_type == "error" || custom_type == "agent_error";
                result.push(ChatMessageInfo {
                    id: format!("msg_{msg_counter}"),
                    role: if is_error_type {
                        MessageRole::Error
                    } else {
                        MessageRole::System
                    },
                    content: text,
                    tool_activities: Vec::new(),
                    streaming: false,
                    reasoning_content: None,
                    reasoning_expanded: false,
                });
            }
        }
    }
    result
}

fn runtime_status_text(status: SessionRuntimeStatus) -> Option<String> {
    match status {
        SessionRuntimeStatus::Ready => None,
        SessionRuntimeStatus::Working => Some("Working…".into()),
        SessionRuntimeStatus::Interrupted => {
            Some("Turn interrupted · Safe replay checkpoints available".into())
        }
        SessionRuntimeStatus::Error(error) => Some(error),
    }
}

pub(crate) fn provider_credentials(model: &str) -> (String, Option<String>) {
    if threadlane_provider::router::is_antigravity_model(model) {
        return (
            threadlane_provider::antigravity_auth::load_antigravity_credentials()
                .map(|credentials| credentials.access_token)
                .unwrap_or_default(),
            None,
        );
    }
    if threadlane_provider::router::is_opencode_model(model) {
        return (
            threadlane_auth::opencode_auth::load_opencode_api_key().unwrap_or_default(),
            None,
        );
    }
    if let Some(api_key) =
        threadlane_auth::openai_auth::load_openai_api_key().filter(|key| !key.trim().is_empty())
    {
        return (api_key, None);
    }
    if let Some(credentials) = threadlane_auth::openai_auth::load_credentials()
        .filter(|credentials| threadlane_auth::openai_auth::is_own_source(&credentials.source))
    {
        return (credentials.access_token, credentials.account_id);
    }
    (std::env::var("OPENAI_API_KEY").unwrap_or_default(), None)
}

fn coding_agent_options(
    work_dir: PathBuf,
    session_file: PathBuf,
    model: String,
    model_roles: threadlane_agent::ModelRoles,
) -> threadlane_coding_agent::CodingAgentOptions {
    let (api_key, account_id) = provider_credentials(&model);
    let mut agent_config = threadlane_agent::AgentConfig::default();
    agent_config.model_roles = model_roles;

    threadlane_coding_agent::CodingAgentOptions {
        api_key,
        account_id,
        model,
        work_dir,
        session_file: Some(session_file),
        system_prompt: Default::default(),
        agent_config: Some(agent_config),
        coding_config: None,
    }
}

impl Default for AppState {
    fn default() -> Self {
        Self::load()
    }
}

impl AppState {
    pub(crate) fn load() -> Self {
        Self::load_from_registry(load_project_registry())
    }

    fn load_from_registry(mut registry_projects: Vec<AttachedProject>) -> Self {
        if registry_projects.is_empty() {
            if let Ok(curr) = std::env::current_dir().and_then(std::fs::canonicalize) {
                let display_name = curr
                    .file_name()
                    .map(|s| s.to_string_lossy().to_string())
                    .unwrap_or_else(|| "project".into());
                let now = std::time::SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs();
                let proj = AttachedProject {
                    path: curr,
                    display_name,
                    attached_at: now,
                    last_opened_at: now,
                    last_session_id: None,
                };
                registry_projects.push(proj.clone());
                let _ = save_project_registry(&registry_projects);
            }
        }

        let mut project_infos = Vec::new();
        let mut active_work_dir = None;
        let mut active_session_id = None;
        let mut active_session_file = None;

        for (i, p) in registry_projects.iter().enumerate() {
            let sessions = discover_sessions_in_project(&p.path);
            let is_first = i == 0;

            if is_first || active_work_dir.is_none() {
                active_work_dir = Some(p.path.clone());
                if let Some(target_session) = p
                    .last_session_id
                    .as_deref()
                    .and_then(|id| sessions.iter().find(|s| s.id == id))
                    .or_else(|| sessions.first())
                {
                    active_session_id = Some(target_session.id.clone());
                    active_session_file = Some(target_session.session_file.clone());
                }
            }

            project_infos.push(ProjectInfo {
                name: p.display_name.clone(),
                work_dir: p.path.clone(),
                sessions,
                is_expanded: true,
            });
        }

        let openai_key = threadlane_auth::openai_auth::load_openai_api_key()
            .or_else(|| std::env::var("OPENAI_API_KEY").ok())
            .unwrap_or_default();
        let opencode_key =
            threadlane_auth::opencode_auth::load_opencode_api_key().unwrap_or_default();

        let (stream_tx, stream_rx) = mpsc::channel();
        let (session_refresh_tx, session_refresh_requests) = mpsc::channel::<PathBuf>();
        let (session_refresh_results_tx, session_refresh_rx) = mpsc::channel();
        std::thread::spawn(move || {
            let mut discovery_cache = SessionDiscoveryCache::default();
            while let Ok(work_dir) = session_refresh_requests.recv() {
                let sessions = discover_sessions_in_project_cached(&work_dir, &mut discovery_cache);
                if session_refresh_results_tx
                    .send((work_dir, sessions))
                    .is_err()
                {
                    break;
                }
            }
        });
        let mut selected_model =
            crate::model_catalog::default_model_for_project(active_work_dir.as_deref())
                .unwrap_or_default();

        let model_roles = threadlane_agent::ModelRoles::default();
        let mut session_runtimes = HashMap::new();
        let mut session_status = None;
        let (active_plan, initial_messages, initial_history_start, initial_history_has_older) =
            active_session_file
                .as_deref()
                .map(load_session_projection)
                .unwrap_or_default();
        let history_start = initial_history_start;
        let history_has_older = initial_history_has_older;
        let messages = match (active_work_dir.as_ref(), active_session_file.as_ref()) {
            (Some(work_dir), Some(session_file)) => {
                let runtime = SessionRuntime::new(coding_agent_options(
                    work_dir.clone(),
                    session_file.clone(),
                    selected_model.clone(),
                    model_roles.clone(),
                ));
                let messages = initial_messages.clone();
                selected_model = runtime.selected_model.clone();
                session_status = runtime_status_text(runtime.status());
                session_runtimes.insert(session_file.clone(), runtime);
                messages
            }
            _ => Vec::new(),
        };

        Self {
            projects: project_infos,
            active_work_dir,
            is_new_task: active_session_id.is_none(),
            active_session_id,
            search_query: String::new(),
            messages,
            history_session_file: active_session_file.clone(),
            history_start,
            history_has_older,
            active_plan,
            is_generating: false,
            composer_text: String::new(),
            session_status,
            pending_composer_messages: HashMap::new(),
            session_token_usage: HashMap::new(),
            trajectory_by_session: HashMap::new(),
            session_metrics: HashMap::new(),
            stashed_prompts: HashMap::new(),
            selected_model,
            model_roles: threadlane_agent::ModelRoles::default(),
            reasoning_effort: ReasoningEffort::default(),
            workspace_page: WorkspacePage::Chat,
            openai_key,
            opencode_key,
            auth_status_msg: None,
            update_status: threadlane_updater::UpdateStatus::Idle,
            update_notice_dismissed: false,
            stream_tx,
            stream_rx,
            pending_stream_event: Mutex::new(None),
            session_refresh_tx,
            session_refresh_rx,
            session_runtimes,
            deferred_stream_events: HashMap::new(),
            pending_permissions: HashMap::new(),
        }
    }

    pub(crate) fn current_session_token_usage(&self) -> TokenUsage {
        if let Some(session_id) = &self.active_session_id {
            if let Some(usage) = self.session_token_usage.get(session_id) {
                return usage.clone();
            }
        }
        let chars: usize = self.messages.iter().map(|m| m.content.len()).sum();
        let approx_tokens = (chars / 4) as u32;
        TokenUsage {
            total_tokens: approx_tokens,
            input_tokens: approx_tokens,
            ..Default::default()
        }
    }

    pub(crate) fn stash_prompt(&mut self, session_id: &str, text: String) {
        if !text.trim().is_empty() {
            self.stashed_prompts.insert(session_id.to_string(), text);
        }
    }

    pub(crate) fn pop_stashed_prompt(&mut self, session_id: &str) -> Option<String> {
        self.stashed_prompts.remove(session_id)
    }

    pub(crate) fn get_stashed_prompt(&self, session_id: &str) -> Option<&String> {
        self.stashed_prompts.get(session_id)
    }

    pub(crate) fn clear_stashed_prompt(&mut self, session_id: &str) {
        self.stashed_prompts.remove(session_id);
    }

    fn invalidate_idle_runtimes(&mut self) {
        self.session_runtimes
            .retain(|_, runtime| runtime.is_generating());
    }

    pub(crate) fn invalidate_capability_runtimes(&mut self) {
        self.invalidate_idle_runtimes();
    }

    pub(crate) fn save_openai_key(&mut self, key: String) -> Result<(), String> {
        let key = key.trim().to_string();
        if !key.is_empty() {
            threadlane_auth::openai_auth::save_openai_api_key(&key)?;
            self.openai_key = key;
            self.auth_status_msg = Some("OpenAI API key saved successfully!".into());
        } else {
            let _ = threadlane_auth::openai_auth::remove_credentials();
            self.openai_key.clear();
            self.auth_status_msg = Some("OpenAI API key removed.".into());
        }
        self.invalidate_idle_runtimes();
        self.reconcile_selected_model();
        Ok(())
    }

    pub(crate) fn save_opencode_key(&mut self, key: String) -> Result<(), String> {
        let key = key.trim().to_string();
        if !key.is_empty() {
            threadlane_auth::opencode_auth::save_opencode_api_key(&key)?;
            self.opencode_key = key;
            self.auth_status_msg = Some("Opencode API key saved successfully!".into());
        } else {
            let _ = threadlane_auth::opencode_auth::clear_opencode_api_key();
            self.opencode_key.clear();
            self.auth_status_msg = Some("Opencode API key removed.".into());
        }
        self.invalidate_idle_runtimes();
        self.reconcile_selected_model();
        Ok(())
    }

    pub(crate) fn reconcile_selected_model(&mut self) {
        if !crate::model_catalog::is_available_for_project(
            &self.selected_model,
            self.active_work_dir.as_deref(),
        ) {
            self.selected_model =
                crate::model_catalog::default_model_for_project(self.active_work_dir.as_deref())
                    .unwrap_or_default();
            self.invalidate_idle_runtimes();
        }
    }

    pub(crate) fn set_selected_model(&mut self, model: String) {
        if !crate::model_catalog::is_available_for_project(&model, self.active_work_dir.as_deref())
        {
            return;
        }
        self.selected_model = model.clone();
        if let (Some(work_dir), Some(session_id)) = (
            self.active_work_dir.as_ref(),
            self.active_session_id.as_ref(),
        ) {
            let session_file = work_dir
                .join(".threadlane/sessions")
                .join(format!("{session_id}.jsonl"));
            if self
                .session_runtimes
                .get(&session_file)
                .is_some_and(|runtime| !runtime.is_generating())
            {
                self.session_runtimes.remove(&session_file);
            }
        }
        self.auth_status_msg = Some(format!("Model switched to {model}"));
    }

    pub(crate) fn set_reasoning_effort(&mut self, effort: ReasoningEffort) {
        self.reasoning_effort = effort;
    }

    pub(crate) fn open_settings(&mut self) {
        self.workspace_page = WorkspacePage::Settings;
        self.auth_status_msg = None;
    }

    pub(crate) fn close_settings(&mut self) {
        self.workspace_page = WorkspacePage::Chat;
        self.auth_status_msg = None;
    }

    fn request_session_refresh(&self, work_dir: &Path) {
        let _ = self.session_refresh_tx.send(work_dir.to_path_buf());
    }

    pub(crate) fn apply_session_refreshes(&mut self) -> bool {
        let mut changed = false;
        for (work_dir, sessions) in self.session_refresh_rx.try_iter() {
            if let Some(project) = self
                .projects
                .iter_mut()
                .find(|project| project.work_dir == work_dir)
            {
                project.sessions = sessions;
                changed = true;
            }
        }
        changed
    }

    fn refresh_active_session(&mut self) {
        if let (Some(work_dir), Some(session_id)) = (
            &self.active_work_dir.clone(),
            &self.active_session_id.clone(),
        ) {
            let session_file = work_dir
                .join(".threadlane/sessions")
                .join(format!("{session_id}.jsonl"));
            let is_generating = self
                .session_runtimes
                .get(&session_file)
                .is_some_and(|runtime| runtime.is_generating());
            if !is_generating {
                self.replace_visible_history(&session_file);
                self.active_plan = load_session_plan(&session_file);
            }
            self.request_session_refresh(work_dir);
        }
    }

    pub(crate) fn begin_new_task(&mut self) {
        self.workspace_page = WorkspacePage::Chat;
        self.active_session_id = None;
        self.is_new_task = true;
        self.messages.clear();
        self.history_session_file = None;
        self.history_start = 0;
        self.history_has_older = false;
        self.active_plan = SessionPlan::default();
        self.is_generating = false;
        self.session_status = None;
        if self.active_work_dir.is_none() {
            self.active_work_dir = self
                .projects
                .first()
                .map(|project| project.work_dir.clone());
        }
    }

    pub(crate) fn select_draft_project(&mut self, work_dir: PathBuf) {
        if self
            .projects
            .iter()
            .any(|project| project.work_dir == work_dir)
        {
            self.active_work_dir = Some(work_dir);
            self.active_session_id = None;
            self.is_new_task = true;
            self.messages.clear();
            self.history_session_file = None;
            self.history_start = 0;
            self.history_has_older = false;
            self.active_plan = SessionPlan::default();
            self.is_generating = false;
            self.session_status = None;
        }
    }

    fn replace_visible_history(&mut self, session_file: &Path) {
        let (messages, start, has_older) = load_session_message_page(session_file, usize::MAX);
        self.messages = messages;
        self.history_session_file = Some(session_file.to_path_buf());
        self.history_start = start;
        self.history_has_older = has_older;
    }
    pub(crate) fn has_older_messages(&self) -> bool {
        self.history_has_older
    }

    pub(crate) fn load_older_messages(&mut self) -> usize {
        if !self.history_has_older {
            return 0;
        }
        let Some(session_file) = self.history_session_file.as_deref() else {
            return 0;
        };
        let (older, start, has_older) = load_session_message_page(session_file, self.history_start);
        let added = older.len();
        if added > 0 {
            self.messages.splice(0..0, older);
            self.history_start = start;
            self.history_has_older = has_older;
        } else {
            self.history_has_older = false;
        }
        added
    }
    pub(crate) fn select_session(&mut self, work_dir: PathBuf, session_id: String) {
        self.workspace_page = WorkspacePage::Chat;
        self.active_work_dir = Some(work_dir.clone());
        self.active_session_id = Some(session_id.clone());
        self.is_new_task = false;

        let session_file = self.session_file(&work_dir, &session_id);
        let completed_while_away =
            self.deferred_stream_events
                .get(&session_id)
                .is_some_and(|events| {
                    events
                        .iter()
                        .any(|event| matches!(event, ChatStreamEvent::Finished { .. }))
                });
        let completed_events = completed_while_away
            .then(|| self.deferred_stream_events.remove(&session_id))
            .flatten()
            .unwrap_or_default();
        let runtime = self.ensure_session_runtime(work_dir, session_file.clone());
        if !self.trajectory_by_session.contains_key(&session_id) {
            if let Err(error) = self.hydrate_session_projection(&session_id, &session_file) {
                self.trajectory_by_session.insert(
                    session_id.clone(),
                    vec![TrajectoryEntry {
                        seq: None,
                        run_id: None,
                        turn: None,
                        category: "Error".into(),
                        summary: "Could not load durable trajectory".into(),
                        detail: error,
                        lane: Some("main".into()),
                        correlation_id: None,
                    }],
                );
            }
        }
        for event in completed_events {
            if let ChatStreamEvent::Agent { event, .. } = event {
                self.record_trajectory(&session_id, &event);
            }
        }
        let (messages, start, has_older) = load_session_message_page(&session_file, usize::MAX);
        self.messages = messages;
        self.history_session_file = Some(session_file.clone());
        self.history_start = start;
        self.history_has_older = has_older;
        self.active_plan = load_session_plan(&session_file);
        self.is_generating = runtime.is_generating();
        self.selected_model = runtime.selected_model.clone();
        self.session_status = runtime_status_text(runtime.status());
    }

    pub(crate) fn settle_session(
        &mut self,
        work_dir: PathBuf,
        session_id: String,
    ) -> Result<(), String> {
        let session_file = self.session_file(&work_dir, &session_id);
        if self
            .session_runtimes
            .get(&session_file)
            .is_some_and(|runtime| runtime.is_generating())
        {
            return Err("Stop the running generation before archiving this session".into());
        }
        let archive_dir = work_dir.join(".threadlane/sessions/archive");
        std::fs::create_dir_all(&archive_dir).map_err(|error| error.to_string())?;
        let file_name = session_file
            .file_name()
            .ok_or_else(|| "Session file has no file name".to_string())?;
        std::fs::rename(&session_file, archive_dir.join(file_name))
            .map_err(|error| error.to_string())?;
        self.finish_session_removal(&work_dir, &session_id);
        Ok(())
    }

    pub(crate) fn remove_session(
        &mut self,
        work_dir: PathBuf,
        session_id: String,
    ) -> Result<(), String> {
        let session_file = self.session_file(&work_dir, &session_id);
        if self
            .session_runtimes
            .get(&session_file)
            .is_some_and(|runtime| runtime.is_generating())
        {
            return Err("Stop the running generation before deleting this session".into());
        }
        std::fs::remove_file(session_file).map_err(|error| error.to_string())?;
        self.finish_session_removal(&work_dir, &session_id);
        Ok(())
    }

    pub(crate) fn update_model_roles(&mut self, roles: threadlane_agent::ModelRoles) {
        self.model_roles = roles.clone();
        for runtime in self.session_runtimes.values() {
            let runtime = runtime.clone();
            let roles = roles.clone();
            tokio::spawn(async move {
                runtime.set_model_roles(roles).await;
            });
        }
    }

    pub(crate) fn ensure_session_runtime(
        &mut self,
        work_dir: PathBuf,
        session_file: PathBuf,
    ) -> Arc<SessionRuntime> {
        if let Some(runtime) = self.session_runtimes.get(&session_file) {
            return runtime.clone();
        }
        let runtime = SessionRuntime::new(coding_agent_options(
            work_dir,
            session_file.clone(),
            self.selected_model.clone(),
            self.model_roles.clone(),
        ));
        self.session_runtimes.insert(session_file, runtime.clone());
        runtime
    }

    pub(crate) fn resolve_active_permission(
        &mut self,
        request_id: &str,
        decision: threadlane_coding_agent::PermissionDecision,
    ) -> bool {
        let Some(session_id) = self.active_session_id.clone() else {
            return false;
        };
        let Some(work_dir) = self.active_work_dir.clone() else {
            return false;
        };
        let session_file = self.session_file(&work_dir, &session_id);
        let resolved = self
            .session_runtimes
            .get(&session_file)
            .is_some_and(|runtime| runtime.resolve_permission(request_id, decision));
        if resolved {
            self.pending_permissions.remove(&session_id);
        }
        resolved
    }

    fn session_file(&self, work_dir: &Path, session_id: &str) -> PathBuf {
        self.projects
            .iter()
            .find(|project| project.work_dir == work_dir)
            .and_then(|project| {
                project
                    .sessions
                    .iter()
                    .find(|session| session.id == session_id)
            })
            .map(|session| session.session_file.clone())
            .unwrap_or_else(|| {
                work_dir
                    .join(".threadlane/sessions")
                    .join(format!("{session_id}.jsonl"))
            })
    }

    fn finish_session_removal(&mut self, work_dir: &Path, session_id: &str) {
        let session_file = self.session_file(work_dir, session_id);
        self.session_runtimes.remove(&session_file);
        self.pending_composer_messages.remove(session_id);
        if let Some(project) = self
            .projects
            .iter_mut()
            .find(|project| project.work_dir == work_dir)
        {
            project.sessions = discover_sessions_in_project(work_dir);
        }

        let removed_active = self.active_work_dir.as_deref() == Some(work_dir)
            && self.active_session_id.as_deref() == Some(session_id);
        if !removed_active {
            return;
        }

        self.active_session_id = None;
        self.is_new_task = true;
        self.messages.clear();
        self.active_plan = SessionPlan::default();
        self.is_generating = false;
        self.session_status = None;
        let next_session = self
            .projects
            .iter()
            .flat_map(|project| project.sessions.iter())
            .next()
            .map(|session| (session.work_dir.clone(), session.id.clone()));
        if let Some((next_work_dir, next_session_id)) = next_session {
            self.select_session(next_work_dir, next_session_id);
        }
    }

    pub(crate) fn session_is_generating(&self, session_file: &Path) -> bool {
        self.session_runtimes
            .get(session_file)
            .is_some_and(|runtime| runtime.is_generating())
    }

    pub(crate) fn toggle_project_expanded(&mut self, work_dir: &Path) {
        if let Some(proj) = self.projects.iter_mut().find(|p| p.work_dir == work_dir) {
            proj.is_expanded = !proj.is_expanded;
        }
    }

    pub(crate) fn accept_edit_proposal(&mut self, proposal_id: &str) -> Result<(), String> {
        let work_dir = self
            .active_work_dir
            .as_deref()
            .ok_or_else(|| "Select a project before accepting an edit proposal".to_string())?;
        let response = threadlane_tools::execute_tool_in_workspace(
            "accept_edit",
            &serde_json::json!({ "proposal_id": proposal_id }).to_string(),
            work_dir,
        );
        if response.starts_with("Error:") {
            return Err(response);
        }
        self.session_status = Some(response);
        Ok(())
    }

    pub(crate) fn toggle_tool_activity(&mut self, tool_call_id: &str) {
        if let Some(activity) = self
            .messages
            .iter_mut()
            .flat_map(|message| message.tool_activities.iter_mut())
            .find(|activity| activity.id == tool_call_id)
        {
            activity.is_expanded = !activity.is_expanded;
        }
    }

    pub(crate) fn attach_project(&mut self, raw_path: PathBuf) -> Result<(), String> {
        let canonical = std::fs::canonicalize(&raw_path).map_err(|e| e.to_string())?;
        if !canonical.is_dir() {
            return Err("Selected path is not a directory".into());
        }

        let mut registry = load_project_registry();
        if !registry.iter().any(|p| p.path == canonical) {
            let name = canonical
                .file_name()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_else(|| "project".into());
            let now = std::time::SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();
            registry.push(AttachedProject {
                path: canonical.clone(),
                display_name: name,
                attached_at: now,
                last_opened_at: now,
                last_session_id: None,
            });
            save_project_registry(&registry)?;
        }

        if !self
            .projects
            .iter()
            .any(|project| project.work_dir == canonical)
        {
            let name = canonical
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| "project".into());
            self.projects.push(ProjectInfo {
                name,
                sessions: discover_sessions_in_project(&canonical),
                work_dir: canonical.clone(),
                is_expanded: true,
            });
        }
        self.active_work_dir = Some(canonical);
        self.active_session_id = None;
        self.is_new_task = true;
        self.messages.clear();
        self.active_plan = SessionPlan::default();
        self.is_generating = false;
        self.session_status = None;
        Ok(())
    }

    fn create_new_session(&mut self) -> Result<String, String> {
        let Some(work_dir) = self.active_work_dir.clone() else {
            return Err("No active project directory".into());
        };
        let sessions_dir = work_dir.join(".threadlane/sessions");
        std::fs::create_dir_all(&sessions_dir).map_err(|e| e.to_string())?;

        let now_nanos = std::time::SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let session_id = format!("session_{now_nanos}");
        let session_file = sessions_dir.join(format!("{session_id}.jsonl"));

        let mut tree = SessionTree::new(&session_id);
        tree.file_path = Some(session_file.clone());
        let _ = tree.append_passive_branch(None, Vec::new());

        let _ = save_project_registry(
            &self
                .projects
                .iter()
                .map(|p| AttachedProject {
                    path: p.work_dir.clone(),
                    display_name: p.name.clone(),
                    attached_at: 0,
                    last_opened_at: 0,
                    last_session_id: if p.work_dir == work_dir {
                        Some(session_id.clone())
                    } else {
                        None
                    },
                })
                .collect::<Vec<_>>(),
        );

        if let Some(project) = self
            .projects
            .iter_mut()
            .find(|project| project.work_dir == work_dir)
        {
            project.sessions = discover_sessions_in_project(&work_dir);
        }
        self.select_session(work_dir, session_id.clone());
        self.is_new_task = false;
        Ok(session_id)
    }

    fn hydrate_session_projection(
        &mut self,
        session_id: &str,
        session_file: &Path,
    ) -> Result<(), String> {
        let store = threadlane_agent::harness::JsonlStore::open_read_only(session_file)
            .map_err(|error| error.to_string())?;
        let mut trajectory: Vec<TrajectoryEntry> = Vec::new();
        let mut metrics = SessionMetricsInfo::default();
        let entries_by_id = store
            .entries()
            .iter()
            .map(|entry| (entry.id.as_str(), &entry.message))
            .collect::<HashMap<_, _>>();
        let mut tool_rows = HashMap::<String, usize>::new();
        for record in store.records() {
            use threadlane_agent::harness::Record;
            let entry = match record {
                Record::OperationStarted {
                    seq,
                    lane,
                    id,
                    intent,
                    ..
                } => Some(TrajectoryEntry {
                    seq: Some(*seq),
                    run_id: Some(id.clone()),
                    turn: None,
                    category: "Operation".into(),
                    summary: format!("{intent:?} started"),
                    detail: String::new(),
                    lane: Some(lane.clone()),
                    correlation_id: None,
                }),
                Record::OperationFinished {
                    seq,
                    lane,
                    run_id,
                    outcome,
                    error,
                    ..
                } => Some(TrajectoryEntry {
                    seq: Some(*seq),
                    run_id: Some(run_id.clone()),
                    turn: None,
                    category: "Operation".into(),
                    summary: format!("Operation {outcome:?}"),
                    detail: error.clone().unwrap_or_default(),
                    lane: Some(lane.clone()),
                    correlation_id: None,
                }),
                Record::StepAttempt {
                    seq,
                    lane,
                    run_id,
                    attempt,
                    ..
                } => {
                    metrics.turns = metrics.turns.saturating_add(1);
                    Some(TrajectoryEntry {
                        seq: Some(*seq),
                        run_id: Some(run_id.clone()),
                        turn: Some(*attempt),
                        category: "Step".into(),
                        summary: format!("Step attempt {attempt}"),
                        detail: String::new(),
                        lane: Some(lane.clone()),
                        correlation_id: None,
                    })
                }
                Record::RetryScheduled {
                    seq,
                    lane,
                    run_id,
                    attempt,
                    reason,
                    ..
                } => Some(TrajectoryEntry {
                    seq: Some(*seq),
                    run_id: Some(run_id.clone()),
                    turn: Some(*attempt),
                    category: "Retry".into(),
                    summary: format!("Retry {attempt} scheduled"),
                    detail: reason.clone(),
                    lane: Some(lane.clone()),
                    correlation_id: None,
                }),
                Record::ToolStarted {
                    seq,
                    lane,
                    run_id,
                    tool_call_id,
                    tool_name,
                    effective_args,
                    ..
                } => {
                    metrics.tool_calls = metrics.tool_calls.saturating_add(1);
                    tool_rows.insert(tool_call_id.clone(), trajectory.len());
                    Some(TrajectoryEntry {
                        seq: Some(*seq),
                        run_id: Some(run_id.clone()),
                        turn: None,
                        category: "Tool".into(),
                        summary: format!("{tool_name} running"),
                        detail: effective_args.to_string(),
                        lane: Some(lane.clone()),
                        correlation_id: Some(tool_call_id.clone()),
                    })
                }
                Record::ToolFinished {
                    tool_call_id,
                    result_entry_id,
                    ..
                } => {
                    if let Some(index) = tool_rows.get(tool_call_id).copied() {
                        if let Some(AgentMessage::Tool {
                            name,
                            content,
                            is_error,
                            ..
                        }) = entries_by_id.get(result_entry_id.as_str()).copied()
                        {
                            trajectory[index].summary =
                                format!("{name} {}", if *is_error { "failed" } else { "finished" });
                            trajectory[index].detail = content.clone();
                        } else {
                            trajectory[index].summary = "Tool finished".into();
                        }
                    }
                    None
                }
                Record::Usage { usage, .. } => {
                    metrics.input_tokens = metrics
                        .input_tokens
                        .saturating_add(u64::from(usage.input_tokens));
                    metrics.output_tokens = metrics
                        .output_tokens
                        .saturating_add(u64::from(usage.output_tokens));
                    None
                }
                _ => None,
            };
            if let Some(entry) = entry {
                trajectory.push(entry);
            }
        }
        for entry in store.entries() {
            if let AgentMessage::Tool {
                tool_call_id,
                name,
                content,
                is_error,
                ..
            } = &entry.message
            {
                if !tool_rows.contains_key(tool_call_id) {
                    trajectory.push(TrajectoryEntry {
                        seq: Some(entry.seq),
                        run_id: None,
                        turn: None,
                        category: "Tool".into(),
                        summary: format!(
                            "{name} {}",
                            if *is_error { "failed" } else { "finished" }
                        ),
                        detail: content.clone(),
                        lane: Some(entry.lane.clone()),
                        correlation_id: Some(tool_call_id.clone()),
                    });
                }
                continue;
            }
            let projected = match &entry.message {
                AgentMessage::User { content } | AgentMessage::UserWithImages { content, .. } => {
                    Some(("Input", "User input", content.clone()))
                }
                AgentMessage::Assistant {
                    content: Some(content),
                    ..
                } if !content.trim().is_empty() => {
                    Some(("Assistant", "Assistant response", content.clone()))
                }
                AgentMessage::Custom {
                    custom_type,
                    payload,
                } if matches!(custom_type.as_str(), "thinking" | "goal_round") => {
                    Some(("Context", custom_type.as_str(), payload.to_string()))
                }
                _ => None,
            };
            if let Some((category, summary, detail)) = projected {
                trajectory.push(TrajectoryEntry {
                    seq: Some(entry.seq),
                    run_id: None,
                    turn: None,
                    category: category.into(),
                    summary: summary.into(),
                    detail,
                    lane: Some(entry.lane.clone()),
                    correlation_id: None,
                });
            }
        }
        trajectory.sort_by_key(|entry| entry.seq.unwrap_or(u64::MAX));
        self.trajectory_by_session
            .insert(session_id.into(), trajectory);
        self.session_metrics.insert(session_id.into(), metrics);
        Ok(())
    }

    fn record_trajectory(&mut self, session_id: &str, event: &AgentEvent) {
        let entry = match event {
            AgentEvent::TurnStart { turn_number } => Some((
                "Turn",
                format!("Turn {turn_number} started"),
                String::new(),
                None,
            )),
            AgentEvent::TurnEnd {
                turn_number,
                tool_results,
            } => Some((
                "Turn",
                format!("Turn {turn_number} finished"),
                format!("{} tool result(s)", tool_results.len()),
                None,
            )),
            AgentEvent::ToolExecutionStart {
                tool_call_id,
                name,
                arguments,
            } => Some((
                "Tool",
                format!("{name} started"),
                format!("{tool_call_id}\n{arguments}"),
                None,
            )),
            AgentEvent::ToolExecutionEnd {
                tool_call_id,
                name,
                result,
            } => Some((
                "Tool",
                format!(
                    "{name} {}",
                    if result.is_error {
                        "failed"
                    } else {
                        "finished"
                    }
                ),
                format!("{tool_call_id}\n{}", result.content),
                None,
            )),
            AgentEvent::SubagentQueued {
                task_index,
                agent,
                task,
                ..
            } => Some((
                "Subagent",
                format!("{agent} queued"),
                format!("Task {task_index}: {task}"),
                Some(agent.clone()),
            )),
            AgentEvent::SubagentStarted {
                journal_run_id,
                task_index,
                ..
            } => Some((
                "Subagent",
                format!("Subagent {task_index} started"),
                journal_run_id.clone(),
                Some(journal_run_id.clone()),
            )),
            AgentEvent::SubagentFinished {
                journal_run_id,
                task_index,
                succeeded,
                error,
                ..
            } => Some((
                "Subagent",
                format!(
                    "Subagent {task_index} {}",
                    if *succeeded { "finished" } else { "failed" }
                ),
                error.clone().unwrap_or_else(|| journal_run_id.clone()),
                Some(journal_run_id.clone()),
            )),
            AgentEvent::SubagentRecovery {
                run_id,
                status,
                detail,
            } => Some((
                "Recovery",
                format!("{status:?}"),
                detail.clone().unwrap_or_else(|| run_id.clone()),
                Some(run_id.clone()),
            )),
            AgentEvent::AgentError { error } => {
                Some(("Error", "Agent error".into(), error.clone(), None))
            }
            AgentEvent::StreamRuleTriggered {
                rule_name,
                reminder,
                ..
            } => Some((
                "Rule",
                format!("{rule_name} triggered"),
                reminder.clone(),
                None,
            )),
            _ => None,
        };
        if let Some((category, summary, detail, lane)) = entry {
            self.trajectory_by_session
                .entry(session_id.into())
                .or_default()
                .push(TrajectoryEntry {
                    seq: None,
                    run_id: lane.clone(),
                    turn: match event {
                        AgentEvent::TurnStart { turn_number }
                        | AgentEvent::TurnEnd { turn_number, .. } => {
                            u32::try_from(*turn_number).ok()
                        }
                        _ => None,
                    },
                    category: category.into(),
                    summary,
                    detail,
                    lane,
                    correlation_id: match event {
                        AgentEvent::ToolExecutionStart { tool_call_id, .. }
                        | AgentEvent::ToolExecutionEnd { tool_call_id, .. } => {
                            Some(tool_call_id.clone())
                        }
                        _ => None,
                    },
                });
        }
    }

    pub(crate) fn active_trajectory(&self) -> &[TrajectoryEntry] {
        self.active_session_id
            .as_ref()
            .and_then(|id| self.trajectory_by_session.get(id))
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }

    pub(crate) fn session_trajectory(&self, session_id: &str) -> &[TrajectoryEntry] {
        self.trajectory_by_session
            .get(session_id)
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }

    pub(crate) fn active_session_metrics(&self) -> SessionMetricsInfo {
        self.active_session_id
            .as_ref()
            .and_then(|id| self.session_metrics.get(id))
            .cloned()
            .unwrap_or_default()
    }

    pub(crate) fn chat_stream_pending(&self) -> bool {
        let Ok(mut pending) = self.pending_stream_event.lock() else {
            return false;
        };
        if pending.is_none() {
            *pending = self.stream_rx.try_recv().ok();
        }
        pending.is_some()
    }

    pub(crate) fn drain_chat_stream(&mut self) -> bool {
        let first = self
            .pending_stream_event
            .lock()
            .ok()
            .and_then(|mut pending| pending.take());
        let deferred = self
            .active_session_id
            .as_ref()
            .and_then(|session_id| self.deferred_stream_events.remove(session_id))
            .unwrap_or_default();
        let events = deferred
            .into_iter()
            .chain(first)
            .chain(self.stream_rx.try_iter())
            .collect::<Vec<_>>();
        if events.is_empty() {
            return false;
        }

        for event in events {
            match event {
                ChatStreamEvent::Agent { session_id, event }
                    if self.active_session_id.as_deref() == Some(&session_id) =>
                {
                    self.record_trajectory(&session_id, &event);
                    let metrics = self.session_metrics.entry(session_id.clone()).or_default();
                    match &event {
                        AgentEvent::TurnStart { .. } => {
                            metrics.turns = metrics.turns.saturating_add(1)
                        }
                        AgentEvent::ToolExecutionStart { .. } => {
                            metrics.tool_calls = metrics.tool_calls.saturating_add(1)
                        }
                        AgentEvent::AgentEnd { usage } => {
                            metrics.input_tokens = metrics
                                .input_tokens
                                .saturating_add(u64::from(usage.input_tokens));
                            metrics.output_tokens = metrics
                                .output_tokens
                                .saturating_add(u64::from(usage.output_tokens));
                        }
                        _ => {}
                    }
                    match adapt_agent_event(event) {
                        ChatAgentUpdate::TextDelta(delta) => {
                            let stream_prefix = format!("streaming-{session_id}-");
                            if let Some(message) = self.messages.last_mut().filter(|message| {
                                message.role == MessageRole::Assistant
                                    && message.id.starts_with(&stream_prefix)
                                    && message.tool_activities.is_empty()
                            }) {
                                message.content.push_str(&delta);
                            } else {
                                self.messages.push(ChatMessageInfo {
                                    id: format!("streaming-{session_id}-{}", self.messages.len()),
                                    role: MessageRole::Assistant,
                                    content: delta,
                                    tool_activities: Vec::new(),
                                    streaming: true,
                                    reasoning_content: None,
                                    reasoning_expanded: false,
                                });
                            }
                        }
                        ChatAgentUpdate::ReasoningDelta(delta) => {
                            if let Some(message) = self
                                .messages
                                .last_mut()
                                .filter(|m| m.role == MessageRole::Assistant && m.streaming)
                            {
                                match &mut message.reasoning_content {
                                    Some(content) => content.push_str(&delta),
                                    None => message.reasoning_content = Some(delta),
                                }
                            } else {
                                let segment = self.messages.len();
                                self.messages.push(ChatMessageInfo {
                                    id: format!("streaming-{session_id}-{segment}"),
                                    role: MessageRole::Assistant,
                                    content: String::new(),
                                    tool_activities: Vec::new(),
                                    streaming: true,
                                    reasoning_content: Some(delta),
                                    reasoning_expanded: false,
                                });
                            }
                        }
                        ChatAgentUpdate::ToolStarted {
                            tool_call_id,
                            name,
                            arguments,
                        } => {
                            let activity = ToolActivityInfo {
                                id: tool_call_id,
                                category: "Working".into(),
                                summary: tool_activity_summary(&name, &arguments),
                                title: name,
                                detail: arguments,
                                is_expanded: false,
                            };
                            if let Some(message) = self.messages.last_mut().filter(|message| {
                                message.role == MessageRole::Assistant && message.content.is_empty()
                            }) {
                                message.tool_activities.push(activity);
                            } else {
                                self.messages.push(ChatMessageInfo {
                                    id: format!("streaming-{session_id}-{}", self.messages.len()),
                                    role: MessageRole::Assistant,
                                    content: String::new(),
                                    tool_activities: vec![activity],
                                    streaming: true,
                                    reasoning_content: None,
                                    reasoning_expanded: false,
                                });
                            }
                        }
                        ChatAgentUpdate::ToolUpdated {
                            tool_call_id,
                            partial_result,
                        } => {
                            if let Some(activity) = self
                                .messages
                                .iter_mut()
                                .rev()
                                .flat_map(|message| message.tool_activities.iter_mut().rev())
                                .find(|activity| activity.id == tool_call_id)
                            {
                                activity.detail = partial_result;
                            }
                        }
                        ChatAgentUpdate::ToolFinished {
                            tool_call_id,
                            content,
                            is_error,
                        } => {
                            if let Some(activity) = self
                                .messages
                                .iter_mut()
                                .rev()
                                .flat_map(|message| message.tool_activities.iter_mut().rev())
                                .find(|activity| activity.id == tool_call_id)
                            {
                                activity.category = if is_error {
                                    "Error".into()
                                } else {
                                    "Completed".into()
                                };
                                activity.detail = content;
                            }
                        }
                        ChatAgentUpdate::PlanUpdated(plan) => {
                            self.active_plan = plan;
                        }
                        ChatAgentUpdate::AdvisorNote(note) => {
                            let note_id =
                                format!("advisor-note-{session_id}-{}", self.messages.len());
                            self.messages.push(ChatMessageInfo {
                                id: note_id,
                                role: MessageRole::Advisor(note.severity),
                                content: format!("**{}**\n\n{}", note.summary, note.details),
                                tool_activities: Vec::new(),
                                streaming: false,
                                reasoning_content: None,
                                reasoning_expanded: false,
                            });
                        }
                        ChatAgentUpdate::ModelRolesUpdated(roles) => {
                            self.model_roles = roles;
                        }
                        ChatAgentUpdate::Usage(usage) => {
                            let entry = self
                                .session_token_usage
                                .entry(session_id.clone())
                                .or_default();
                            entry.accumulate(&usage);
                        }
                        ChatAgentUpdate::PermissionRequested(request) => {
                            self.pending_permissions.insert(session_id.clone(), request);
                        }
                        ChatAgentUpdate::Error(error) => {
                            self.messages.push(ChatMessageInfo {
                                id: format!("stream-error-{session_id}"),
                                role: MessageRole::Error,
                                content: error.clone(),
                                tool_activities: Vec::new(),
                                streaming: false,
                                reasoning_content: None,
                                reasoning_expanded: false,
                            });
                            self.is_generating = false;
                            self.session_status = Some(error);
                        }
                        ChatAgentUpdate::Ignore => {}
                    }
                }
                ChatStreamEvent::Finished {
                    session_id,
                    session_file,
                } => {
                    if self.active_session_id.as_deref() != Some(&session_id) {
                        self.deferred_stream_events
                            .entry(session_id.clone())
                            .or_default()
                            .push(ChatStreamEvent::Finished {
                                session_id,
                                session_file,
                            });
                        continue;
                    }
                    if self.active_session_id.as_deref() == Some(&session_id) {
                        self.pending_permissions.remove(&session_id);
                        self.replace_visible_history(&session_file);
                        self.active_plan = load_session_plan(&session_file);
                        self.is_generating = false;
                        self.session_status = self
                            .session_runtimes
                            .get(&session_file)
                            .and_then(|runtime| runtime_status_text(runtime.status()));
                    }
                    let runtime_is_stale =
                        self.session_runtimes
                            .get(&session_file)
                            .is_some_and(|runtime| {
                                !runtime.is_generating()
                                    && runtime.selected_model != self.selected_model
                            });
                    if runtime_is_stale {
                        self.session_runtimes.remove(&session_file);
                    }
                    if let Some(work_dir) = session_file
                        .parent()
                        .and_then(Path::parent)
                        .and_then(Path::parent)
                    {
                        self.request_session_refresh(work_dir);
                    }
                }
                ChatStreamEvent::TitleGenerated {
                    session_id,
                    session_file,
                } => {
                    if let Some(work_dir) = session_file
                        .parent()
                        .and_then(Path::parent)
                        .and_then(Path::parent)
                    {
                        self.request_session_refresh(work_dir);
                    }
                    if self.active_session_id.as_deref() == Some(&session_id) {
                        self.refresh_active_session();
                    }
                }
                ChatStreamEvent::Agent { session_id, event } => {
                    self.deferred_stream_events
                        .entry(session_id.clone())
                        .or_default()
                        .push(ChatStreamEvent::Agent { session_id, event });
                }
            }
        }
        true
    }

    pub(crate) fn active_pending_composer_message(&self) -> Option<&str> {
        self.active_session_id
            .as_ref()
            .and_then(|session_id| self.pending_composer_messages.get(session_id))
            .map(String::as_str)
    }

    pub(crate) fn stage_busy_message(&mut self, text: String) -> Result<(), String> {
        let text = text.trim().to_string();
        if text.is_empty() {
            return Ok(());
        }
        let session_id = self
            .active_session_id
            .clone()
            .ok_or_else(|| "No active session".to_string())?;
        if !self.is_generating {
            return Err("The session is no longer generating".into());
        }
        self.pending_composer_messages.insert(session_id, text);
        Ok(())
    }

    pub(crate) fn queue_pending_message(&mut self) -> Result<(), String> {
        let (runtime, session_id, text) = self.pending_runtime_message()?;
        runtime
            .work_handle
            .try_queue_follow_up_with_images(text.clone(), Vec::new())?;
        self.pending_composer_messages.remove(&session_id);
        self.push_optimistic_follow_up(&session_id, text);
        self.session_status = Some("Message queued…".into());
        Ok(())
    }

    pub(crate) fn steer_pending_message(&mut self) -> Result<(), String> {
        let (runtime, session_id, text) = self.pending_runtime_message()?;
        runtime
            .work_handle
            .queue_steer_with_images(text.clone(), Vec::new())?;
        self.pending_composer_messages.remove(&session_id);
        self.push_optimistic_follow_up(&session_id, text);
        self.session_status = Some("Steering current turn…".into());
        Ok(())
    }

    pub(crate) fn dismiss_pending_message(&mut self) {
        if let Some(session_id) = self.active_session_id.as_ref() {
            self.pending_composer_messages.remove(session_id);
        }
    }

    fn pending_runtime_message(&self) -> Result<(Arc<SessionRuntime>, String, String), String> {
        let session_id = self
            .active_session_id
            .clone()
            .ok_or_else(|| "No active session".to_string())?;
        let work_dir = self
            .active_work_dir
            .as_ref()
            .ok_or_else(|| "No active project".to_string())?;
        let session_file = work_dir
            .join(".threadlane/sessions")
            .join(format!("{session_id}.jsonl"));
        let runtime = self
            .session_runtimes
            .get(&session_file)
            .cloned()
            .ok_or_else(|| "Session runtime is unavailable".to_string())?;
        let text = self
            .pending_composer_messages
            .get(&session_id)
            .cloned()
            .ok_or_else(|| "No pending composer message".to_string())?;
        Ok((runtime, session_id, text))
    }

    fn push_optimistic_follow_up(&mut self, session_id: &str, text: String) {
        if self.active_session_id.as_deref() == Some(session_id) {
            self.messages.push(ChatMessageInfo {
                id: format!("queued-user-{session_id}-{}", self.messages.len()),
                role: MessageRole::User,
                content: text,
                tool_activities: Vec::new(),
                streaming: false,
                reasoning_content: None,
                reasoning_expanded: false,
            });
        }
    }

    pub(crate) fn send_prompt(&mut self, text: String) -> Result<(), String> {
        self.send_prompt_with_images(text, Vec::new())
    }

    pub(crate) fn send_prompt_with_images(
        &mut self,
        text: String,
        images: Vec<ImageAttachment>,
    ) -> Result<(), String> {
        let text = text.trim().to_string();
        if text.is_empty() && images.is_empty() {
            return Ok(());
        }

        if self.active_session_id.is_none() || self.active_work_dir.is_none() {
            self.create_new_session()?;
        }

        let (work_dir, session_id) =
            match (self.active_work_dir.clone(), self.active_session_id.clone()) {
                (Some(w), Some(s)) => (w, s),
                _ => return Err("Failed to ensure active session".into()),
            };

        let session_file = work_dir
            .join(".threadlane/sessions")
            .join(format!("{session_id}.jsonl"));
        if self
            .session_runtimes
            .get(&session_file)
            .is_some_and(|runtime| runtime.is_generating())
        {
            return Err("A generation is already running for this session".into());
        }

        // Resolve credentials using the same provider routing as the runtime and title task.
        let model = self.selected_model.clone();
        let (api_key, account_id) = provider_credentials(&model);

        if api_key.is_empty() {
            self.messages.push(ChatMessageInfo {
                id: format!("credential-error-{session_id}"),
                role: MessageRole::Error,
                content: format!(
                    "No API key configured for model `{model}`. Open Settings and save the provider credential."
                ),
                tool_activities: Vec::new(),
                streaming: false,
                reasoning_content: None,
                reasoning_expanded: false,
            });
            return Ok(());
        }

        let runtime = self.ensure_session_runtime(work_dir.clone(), session_file.clone());
        crate::services::chat::execute_prompt(
            runtime,
            session_id.clone(),
            text.clone(),
            images.clone(),
            self.reasoning_effort,
            self.stream_tx.clone(),
        )?;
        let prompt_detail = if images.is_empty() {
            text.clone()
        } else if text.is_empty() {
            format!("[{} image attachment(s)]", images.len())
        } else {
            format!("{text}\n[{} image attachment(s)]", images.len())
        };
        self.trajectory_by_session
            .entry(session_id.clone())
            .or_default()
            .push(TrajectoryEntry {
                seq: None,
                run_id: None,
                turn: None,
                category: "Input".into(),
                summary: "User input".into(),
                detail: prompt_detail.clone(),
                lane: Some("main".into()),
                correlation_id: None,
            });
        if !threadlane_provider::router::is_antigravity_model(&model) {
            crate::services::chat::spawn_session_title(
                session_file,
                session_id.clone(),
                text.clone(),
                api_key,
                account_id,
                model,
                self.stream_tx.clone(),
            );
        }

        // Present the accepted prompt immediately. CodingAgent owns durable
        // persistence; writing it through SessionTree here would duplicate it.
        self.messages.push(ChatMessageInfo {
            id: format!("pending-user-{session_id}-{}", self.messages.len()),
            role: MessageRole::User,
            content: prompt_detail,
            tool_activities: Vec::new(),
            streaming: false,
            reasoning_content: None,
            reasoning_expanded: false,
        });

        self.is_generating = true;
        self.session_status = Some("Working…".into());

        // Refresh project sessions without blocking the UI thread.
        self.request_session_refresh(&work_dir);
        self.composer_text.clear();
        Ok(())
    }

    pub(crate) fn cancel_generation(&mut self) -> Result<(), String> {
        let (Some(work_dir), Some(session_id)) = (
            self.active_work_dir.as_ref(),
            self.active_session_id.as_ref(),
        ) else {
            return Ok(());
        };
        let session_file = work_dir
            .join(".threadlane/sessions")
            .join(format!("{session_id}.jsonl"));
        let Some(runtime) = self.session_runtimes.get(&session_file).cloned() else {
            return Ok(());
        };
        crate::services::chat::cancel_prompt(runtime, session_id.clone(), self.stream_tx.clone())?;
        self.is_generating = false;
        self.session_status = Some("Generation cancelled".into());
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn persisted_thinking_message_projects_as_reasoning_content() {
        let messages = project_agent_messages(vec![AgentMessage::Custom {
            custom_type: "thinking".into(),
            payload: serde_json::json!({"text": "Planning codebase inspection"}),
        }]);

        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].role, MessageRole::Assistant);
        assert!(messages[0].content.is_empty());
        assert_eq!(
            messages[0].reasoning_content.as_deref(),
            Some("Planning codebase inspection")
        );
        assert!(messages[0].tool_activities.is_empty());
    }

    #[test]
    fn app_state_startup_hydrates_complete_initial_session_history() {
        use threadlane_provider::openai::{ToolCall, ToolCallFunction};

        let unique = std::time::SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let work_dir = std::env::temp_dir().join(format!(
            "threadlane-gpui-session-hydration-{}-{unique}",
            std::process::id()
        ));
        let session_file = work_dir.join(".threadlane/sessions/hydration-test.jsonl");
        std::fs::create_dir_all(session_file.parent().unwrap()).unwrap();

        let mut tree = SessionTree::new("hydration-test");
        tree.file_path = Some(session_file.clone());
        tree.add_message(AgentMessage::User {
            content: "Inspect the project".into(),
        });
        tree.add_message(AgentMessage::Custom {
            custom_type: "thinking".into(),
            payload: serde_json::json!({"text": "Reading the relevant files"}),
        });
        tree.add_message(AgentMessage::Assistant {
            content: Some("The issue is fixed.".into()),
            tool_calls: Some(vec![ToolCall {
                id: "call-read".into(),
                r#type: "function".into(),
                function: ToolCallFunction {
                    name: "read_file".into(),
                    arguments: r#"{"path":"src/main.rs"}"#.into(),
                },
                thought_signature: None,
            }]),
            stop_reason: None,
            deferred_handle: None,
        });
        tree.add_message(AgentMessage::Tool {
            tool_call_id: "call-read".into(),
            name: "read_file".into(),
            content: "file contents".into(),
            is_error: false,
            terminate: false,
        });

        let state = AppState::load_from_registry(vec![AttachedProject {
            path: work_dir.clone(),
            display_name: "hydration-test".into(),
            attached_at: 0,
            last_opened_at: 0,
            last_session_id: Some("hydration-test".into()),
        }]);

        assert_eq!(state.active_session_id.as_deref(), Some("hydration-test"));
        assert_eq!(state.active_work_dir.as_deref(), Some(work_dir.as_path()));
        assert!(!state.is_new_task);
        let messages = &state.messages;

        assert_eq!(messages.len(), 3);
        assert_eq!(
            messages[1].reasoning_content.as_deref(),
            Some("Reading the relevant files")
        );
        assert_eq!(messages[2].content, "The issue is fixed.");
        assert_eq!(messages[2].tool_activities.len(), 1);
        assert_eq!(messages[2].tool_activities[0].id, "call-read");
        assert_eq!(messages[2].tool_activities[0].detail, "file contents");

        drop(state);
        let _ = std::fs::remove_dir_all(work_dir);
    }

    #[test]
    fn session_switch_preserves_live_trajectory_and_applies_deferred_events() {
        let dir = tempfile::tempdir().unwrap();
        let work_dir = dir.path().to_path_buf();
        let session_id = "live-session".to_string();
        let session_file = work_dir
            .join(".threadlane/sessions")
            .join(format!("{session_id}.jsonl"));
        std::fs::create_dir_all(session_file.parent().unwrap()).unwrap();
        std::fs::write(&session_file, "").unwrap();

        let mut state = AppState::load_from_registry(Vec::new());
        state.projects.push(ProjectInfo {
            name: "project".into(),
            work_dir: work_dir.clone(),
            sessions: vec![SessionInfo {
                id: session_id.clone(),
                title: "Live".into(),
                work_dir: work_dir.clone(),
                session_file: session_file.clone(),
                updated_at: 0,
                health: SessionHealth::Working,
            }],
            is_expanded: true,
        });
        state.trajectory_by_session.insert(
            session_id.clone(),
            vec![TrajectoryEntry {
                seq: None,
                run_id: None,
                turn: None,
                category: "Tool".into(),
                summary: "live tool".into(),
                detail: String::new(),
                lane: Some("main".into()),
                correlation_id: Some("call-1".into()),
            }],
        );
        state.deferred_stream_events.insert(
            session_id.clone(),
            vec![
                ChatStreamEvent::Agent {
                    session_id: session_id.clone(),
                    event: AgentEvent::TurnStart { turn_number: 2 },
                },
                ChatStreamEvent::Finished {
                    session_id: session_id.clone(),
                    session_file,
                },
            ],
        );

        state.select_session(work_dir, session_id.clone());

        let trajectory = &state.trajectory_by_session[&session_id];
        assert_eq!(trajectory[0].summary, "live tool");
        assert!(trajectory
            .iter()
            .any(|entry| entry.summary == "Turn 2 started"));
    }

    #[test]
    fn durable_trajectory_hydrates_after_session_switch() {
        use threadlane_agent::harness::SessionStore;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.jsonl");
        std::fs::write(&path, "").unwrap();
        let store = threadlane_agent::harness::JsonlStore::open(&path).unwrap();
        let mut harness = threadlane_agent::harness::AgentHarness::new(store);
        harness
            .accept_prompt("run-1", AgentMessage::user("old prompt", vec![]))
            .unwrap();
        harness.drive_to_completion().unwrap();
        let parent_id = harness.store().entries().last().unwrap().id.clone();
        let seq = harness.store().next_sequence();
        harness
            .store_mut()
            .append_entry(threadlane_agent::harness::Entry {
                id: "legacy-tool-result".into(),
                parent_id: Some(parent_id),
                lane: "main".into(),
                seq,
                timestamp: seq,
                message: AgentMessage::Tool {
                    tool_call_id: "legacy-call".into(),
                    name: "read_file".into(),
                    content: "legacy output".into(),
                    is_error: false,
                    terminate: false,
                },
                terminate: false,
            })
            .unwrap();
        drop(harness);

        let mut state = AppState::load_from_registry(Vec::new());
        state
            .hydrate_session_projection("old-session", &path)
            .unwrap();

        let trajectory = &state.trajectory_by_session["old-session"];
        assert!(trajectory.iter().any(|entry| entry.category == "Operation"));
        assert!(trajectory
            .iter()
            .any(|entry| { entry.category == "Input" && entry.detail == "old prompt" }));
        assert!(trajectory.iter().any(|entry| entry.category == "Step"));
        assert!(trajectory.iter().any(|entry| {
            entry.category == "Tool"
                && entry.correlation_id.as_deref() == Some("legacy-call")
                && entry.detail == "legacy output"
        }));
    }

    #[test]
    fn trajectory_projection_is_session_scoped_and_preserves_tool_details() {
        let mut state = AppState::load_from_registry(Vec::new());
        state.record_trajectory(
            "session-a",
            &AgentEvent::ToolExecutionStart {
                tool_call_id: "call-1".into(),
                name: "read_file".into(),
                arguments: r#"{"path":"src/lib.rs"}"#.into(),
            },
        );
        state.record_trajectory(
            "session-b",
            &AgentEvent::SubagentQueued {
                run_id: 7,
                task_index: 2,
                agent: "reviewer".into(),
                task: "Review the patch".into(),
            },
        );
        assert_eq!(state.trajectory_by_session["session-a"].len(), 1);
        assert_eq!(state.trajectory_by_session["session-a"][0].category, "Tool");
        assert!(state.trajectory_by_session["session-a"][0]
            .detail
            .contains("src/lib.rs"));
        assert_eq!(
            state.trajectory_by_session["session-a"][0]
                .correlation_id
                .as_deref(),
            Some("call-1")
        );
        assert_eq!(
            state.trajectory_by_session["session-b"][0].lane.as_deref(),
            Some("reviewer")
        );
        state.record_trajectory("session-a", &AgentEvent::TurnStart { turn_number: 12 });
        assert_eq!(state.trajectory_by_session["session-a"][1].turn, Some(12));
    }

    #[test]
    fn accepting_a_staged_edit_proposal_updates_the_active_project_file() {
        let dir = tempfile::tempdir().unwrap();
        let work_dir = dir.path().to_path_buf();
        let file = work_dir.join("note.txt");
        std::fs::write(&file, "before\n").unwrap();
        let proposal = threadlane_tools::execute_tool_in_workspace(
            "edit_file_hashline",
            &serde_json::json!({
                "path": "note.txt",
                "interactive": true,
                "edits": [{"start_anchor":"1:f3e", "end_anchor":"1:f3e", "action":"replace", "new_content":"after"}]
            }).to_string(),
            &work_dir,
        );
        assert!(proposal.starts_with("Proposed edit"), "{proposal}");
        let proposal_id = proposal
            .split('\'')
            .nth(1)
            .expect("interactive edit returns proposal id");
        let mut state = AppState::load_from_registry(Vec::new());
        state.active_work_dir = Some(work_dir);
        state.accept_edit_proposal(proposal_id).unwrap();
        assert_eq!(std::fs::read_to_string(file).unwrap(), "after\n");
        assert!(state
            .session_status
            .as_deref()
            .unwrap_or_default()
            .contains("Accepted"));
    }

    #[test]
    fn accepting_an_unknown_edit_proposal_does_not_mutate_files() {
        let dir = tempfile::tempdir().unwrap();
        let work_dir = dir.path().to_path_buf();
        let file = work_dir.join("note.txt");
        std::fs::write(&file, "unchanged\n").unwrap();
        let mut state = AppState::load_from_registry(Vec::new());
        state.active_work_dir = Some(work_dir);
        assert!(state.accept_edit_proposal("missing-proposal").is_err());
        assert_eq!(std::fs::read_to_string(file).unwrap(), "unchanged\n");
    }

    #[test]
    fn inactive_session_stream_events_replay_after_switching_back() {
        let mut state = AppState::load_from_registry(Vec::new());
        state.messages.clear();
        state.active_session_id = Some("foreground-session".into());
        state.is_new_task = false;

        for event in [
            AgentEvent::MessageUpdate {
                text_delta: None,
                reasoning_delta: Some("reasoning while away".into()),
                tool_call_name: None,
            },
            AgentEvent::MessageUpdate {
                text_delta: Some("generated while away".into()),
                reasoning_delta: None,
                tool_call_name: None,
            },
            AgentEvent::ToolExecutionStart {
                tool_call_id: "call-away".into(),
                name: "read_file".into(),
                arguments: r#"{"path":"src/main.rs"}"#.into(),
            },
            AgentEvent::ToolExecutionUpdate {
                tool_call_id: "call-away".into(),
                partial_result: "tool output while away".into(),
            },
        ] {
            state
                .stream_tx
                .send(ChatStreamEvent::Agent {
                    session_id: "background-session".into(),
                    event,
                })
                .unwrap();
        }

        assert!(state.drain_chat_stream());
        assert!(state.messages.is_empty());
        assert_eq!(state.deferred_stream_events.len(), 1);

        state.active_session_id = Some("background-session".into());
        assert!(state.drain_chat_stream());
        assert_eq!(state.messages.len(), 2);
        assert_eq!(state.messages[0].content, "generated while away");
        assert_eq!(
            state.messages[0].reasoning_content.as_deref(),
            Some("reasoning while away")
        );
        assert!(state.messages[0].streaming);
        assert_eq!(state.messages[1].tool_activities.len(), 1);
        assert_eq!(state.messages[1].tool_activities[0].id, "call-away");
        assert_eq!(
            state.messages[1].tool_activities[0].detail,
            "tool output while away"
        );
        assert!(state.deferred_stream_events.is_empty());
    }
    #[test]
    fn session_message_page_returns_newest_window_and_older_cursor() {
        let unique = std::time::SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "threadlane-gpui-history-page-{}-{unique}",
            std::process::id()
        ));
        let path = root.join("session.jsonl");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let mut tree = SessionTree::new("paged");
        tree.file_path = Some(path.clone());
        for index in 0..45 {
            tree.add_message(AgentMessage::User {
                content: format!("message-{index}"),
            });
        }

        let (messages, start, has_older) = load_session_message_page(&path, usize::MAX);
        assert_eq!(messages.len(), CHAT_HISTORY_PAGE_SIZE);
        assert_eq!(messages.first().unwrap().content, "message-5");
        assert_eq!(messages.last().unwrap().content, "message-44");
        assert_eq!(start, 5);
        assert!(has_older);

        let (older, older_start, has_more) = load_session_message_page(&path, start);
        assert_eq!(older.len(), 5);
        assert_eq!(older.first().unwrap().content, "message-0");
        assert_eq!(older_start, 0);
        assert!(!has_more);

        let _ = std::fs::remove_dir_all(root);
    }
}
