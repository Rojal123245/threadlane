use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::UNIX_EPOCH;
use threadlane_agent::{AgentEvent, AgentMessage, ImageAttachment, SessionPlan, SessionTree};

use crate::adapters::agent_events::{adapt_agent_event, ChatAgentUpdate};
use crate::persistence::{load_project_registry, save_project_registry};
use crate::services::sessions::{SessionRuntime, SessionRuntimeStatus};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct AttachedProject {
    pub path: PathBuf,
    pub display_name: String,
    pub attached_at: u64,
    pub last_opened_at: u64,
    #[serde(default)]
    pub last_session_id: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SessionHealth {
    Healthy,
    Working,
    Warning,
}

#[derive(Clone, Debug)]
pub struct SessionInfo {
    pub id: String,
    pub title: String,
    pub work_dir: PathBuf,
    pub session_file: PathBuf,
    pub updated_at: u64,
    pub health: SessionHealth,
}

#[derive(Clone, Debug)]
pub struct ProjectInfo {
    pub name: String,
    pub work_dir: PathBuf,
    pub sessions: Vec<SessionInfo>,
    pub is_expanded: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MessageRole {
    User,
    Assistant,
    System,
    Error,
}

#[derive(Clone, Debug)]
pub struct ToolActivityInfo {
    pub id: String,
    pub category: String,
    pub title: String,
    pub summary: String,
    pub detail: String,
    pub is_expanded: bool,
}

#[derive(Clone, Debug)]
pub struct ChatMessageInfo {
    pub id: String,
    pub role: MessageRole,
    pub content: String,
    pub timestamp: String,
    pub tool_activities: Vec<ToolActivityInfo>,
    pub streaming: bool,
    pub reasoning_content: Option<String>,
    pub reasoning_expanded: bool,
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
    pub projects: Vec<ProjectInfo>,
    pub active_work_dir: Option<PathBuf>,
    pub active_session_id: Option<String>,
    pub is_new_task: bool,
    pub search_query: String,
    pub messages: Vec<ChatMessageInfo>,
    pub active_plan: SessionPlan,
    pub is_generating: bool,
    pub composer_text: String,
    pub session_status: Option<String>,
    pub pending_composer_messages: HashMap<String, String>,

    pub selected_model: String,
    pub workspace_page: WorkspacePage,
    pub openai_key: String,
    pub opencode_key: String,
    pub antigravity_connected: bool,
    pub auth_status_msg: Option<String>,
    pub update_status: threadlane_updater::UpdateStatus,
    pub update_notice_dismissed: bool,
    stream_tx: Sender<ChatStreamEvent>,
    stream_rx: Receiver<ChatStreamEvent>,
    pending_stream_event: Mutex<Option<ChatStreamEvent>>,
    session_refresh_tx: Sender<PathBuf>,
    session_refresh_rx: Receiver<(PathBuf, Vec<SessionInfo>)>,
    session_runtimes: HashMap<PathBuf, Arc<SessionRuntime>>,
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
    let sessions_dir = work_dir.join(".threadlane/sessions");
    let Ok(entries) = std::fs::read_dir(&sessions_dir) else {
        return Vec::new();
    };

    let canonical_work_dir =
        std::fs::canonicalize(work_dir).unwrap_or_else(|_| work_dir.to_path_buf());
    let mut sessions = Vec::new();

    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
            continue;
        }
        if path
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.ends_with(".harness.jsonl"))
        {
            continue;
        }
        let id = path
            .file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| "session".into());

        let (title, health, updated_at) = match SessionTree::load_from_file(&path) {
            Ok(tree) => {
                let title = extract_session_title(&tree, &id);
                let updated_at = file_mtime(&path);
                (title, SessionHealth::Healthy, updated_at)
            }
            Err(_) => (
                "Unreadable session".to_string(),
                SessionHealth::Warning,
                file_mtime(&path),
            ),
        };

        sessions.push(SessionInfo {
            id,
            title,
            work_dir: canonical_work_dir.clone(),
            session_file: path,
            updated_at,
            health,
        });
    }

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
    let Ok(tree) = SessionTree::load_from_file(session_file) else {
        return Vec::new();
    };

    let agent_messages = {
        let branch = tree.get_active_branch_messages();
        if branch.is_empty() {
            tree.get_persisted_messages()
        } else {
            branch
        }
    };

    project_agent_messages(agent_messages)
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
                result.push(ChatMessageInfo {
                    id: format!("msg_{msg_counter}"),
                    role: MessageRole::User,
                    content,
                    timestamp: String::new(),
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
                    timestamp: String::new(),
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
                    timestamp: String::new(),
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
                    timestamp: String::new(),
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
                    timestamp: String::new(),
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
                        timestamp: String::new(),
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
                    timestamp: String::new(),
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
) -> threadlane_coding_agent::CodingAgentOptions {
    let (api_key, account_id) = provider_credentials(&model);

    threadlane_coding_agent::CodingAgentOptions {
        api_key,
        account_id,
        model,
        work_dir,
        session_file: Some(session_file),
        system_prompt: Default::default(),
        agent_config: None,
        coding_config: None,
    }
}

impl Default for AppState {
    fn default() -> Self {
        Self::load()
    }
}

impl AppState {
    pub fn load() -> Self {
        let mut registry_projects = load_project_registry();

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
        let antigravity_connected =
            threadlane_provider::antigravity_auth::load_antigravity_credentials().is_some();

        let (stream_tx, stream_rx) = mpsc::channel();
        let (session_refresh_tx, session_refresh_requests) = mpsc::channel::<PathBuf>();
        let (session_refresh_results_tx, session_refresh_rx) = mpsc::channel();
        std::thread::spawn(move || {
            while let Ok(work_dir) = session_refresh_requests.recv() {
                let sessions = discover_sessions_in_project(&work_dir);
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

        let mut session_runtimes = HashMap::new();
        let mut session_status = None;
        let active_plan = active_session_file
            .as_deref()
            .map(load_session_plan)
            .unwrap_or_default();
        let messages = match (active_work_dir.as_ref(), active_session_file.as_ref()) {
            (Some(work_dir), Some(session_file)) => {
                let runtime = SessionRuntime::new(coding_agent_options(
                    work_dir.clone(),
                    session_file.clone(),
                    selected_model.clone(),
                ));
                let messages = project_agent_messages(runtime.initial_messages.clone());
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
            active_plan,
            is_generating: false,
            composer_text: String::new(),
            session_status,
            pending_composer_messages: HashMap::new(),
            selected_model,
            workspace_page: WorkspacePage::Chat,
            openai_key,
            opencode_key,
            antigravity_connected,
            auth_status_msg: None,
            update_status: threadlane_updater::UpdateStatus::Idle,
            update_notice_dismissed: false,
            stream_tx,
            stream_rx,
            pending_stream_event: Mutex::new(None),
            session_refresh_tx,
            session_refresh_rx,
            session_runtimes,
        }
    }

    fn invalidate_idle_runtimes(&mut self) {
        self.session_runtimes
            .retain(|_, runtime| runtime.is_generating());
    }

    pub fn invalidate_capability_runtimes(&mut self) {
        self.invalidate_idle_runtimes();
    }

    pub fn save_openai_key(&mut self, key: String) -> Result<(), String> {
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

    pub fn save_opencode_key(&mut self, key: String) -> Result<(), String> {
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

    pub fn reconcile_selected_model(&mut self) {
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

    pub fn set_selected_model(&mut self, model: String) {
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

    pub fn open_settings(&mut self) {
        self.workspace_page = WorkspacePage::Settings;
        self.auth_status_msg = None;
    }

    pub fn close_settings(&mut self) {
        self.workspace_page = WorkspacePage::Chat;
        self.auth_status_msg = None;
    }

    fn request_session_refresh(&self, work_dir: &Path) {
        let _ = self.session_refresh_tx.send(work_dir.to_path_buf());
    }

    pub fn apply_session_refreshes(&mut self) -> bool {
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

    pub fn refresh_active_session(&mut self) {
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
                self.messages = load_session_messages(&session_file);
                self.active_plan = load_session_plan(&session_file);
            }
            self.request_session_refresh(work_dir);
        }
    }

    pub fn begin_new_task(&mut self) {
        self.workspace_page = WorkspacePage::Chat;
        self.active_session_id = None;
        self.is_new_task = true;
        self.messages.clear();
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

    pub fn select_draft_project(&mut self, work_dir: PathBuf) {
        if self
            .projects
            .iter()
            .any(|project| project.work_dir == work_dir)
        {
            self.active_work_dir = Some(work_dir);
            self.active_session_id = None;
            self.is_new_task = true;
            self.messages.clear();
            self.active_plan = SessionPlan::default();
            self.is_generating = false;
            self.session_status = None;
        }
    }

    pub fn select_session(&mut self, work_dir: PathBuf, session_id: String) {
        self.workspace_page = WorkspacePage::Chat;
        self.active_work_dir = Some(work_dir.clone());
        self.active_session_id = Some(session_id.clone());
        self.is_new_task = false;

        let session_file = self.session_file(&work_dir, &session_id);
        let existed = self.session_runtimes.contains_key(&session_file);
        let runtime = self.ensure_session_runtime(work_dir, session_file.clone());
        self.messages = if existed {
            load_session_messages(&session_file)
        } else {
            project_agent_messages(runtime.initial_messages.clone())
        };
        self.active_plan = load_session_plan(&session_file);
        self.is_generating = runtime.is_generating();
        self.selected_model = runtime.selected_model.clone();
        self.session_status = runtime_status_text(runtime.status());
    }

    pub fn settle_session(&mut self, work_dir: PathBuf, session_id: String) -> Result<(), String> {
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

    pub fn remove_session(&mut self, work_dir: PathBuf, session_id: String) -> Result<(), String> {
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

    fn ensure_session_runtime(
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
        ));
        self.session_runtimes.insert(session_file, runtime.clone());
        runtime
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

    pub fn session_is_generating(&self, session_file: &Path) -> bool {
        self.session_runtimes
            .get(session_file)
            .is_some_and(|runtime| runtime.is_generating())
    }

    pub fn toggle_project_expanded(&mut self, work_dir: &Path) {
        if let Some(proj) = self.projects.iter_mut().find(|p| p.work_dir == work_dir) {
            proj.is_expanded = !proj.is_expanded;
        }
    }

    pub fn toggle_tool_activity(&mut self, tool_call_id: &str) {
        if let Some(activity) = self
            .messages
            .iter_mut()
            .flat_map(|message| message.tool_activities.iter_mut())
            .find(|activity| activity.id == tool_call_id)
        {
            activity.is_expanded = !activity.is_expanded;
        }
    }

    pub fn attach_project(&mut self, raw_path: PathBuf) -> Result<(), String> {
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

    pub fn create_new_session(&mut self) -> Result<String, String> {
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

    pub fn chat_stream_pending(&self) -> bool {
        let Ok(mut pending) = self.pending_stream_event.lock() else {
            return false;
        };
        if pending.is_none() {
            *pending = self.stream_rx.try_recv().ok();
        }
        pending.is_some()
    }

    pub fn drain_chat_stream(&mut self) -> bool {
        let first = self
            .pending_stream_event
            .lock()
            .ok()
            .and_then(|mut pending| pending.take());
        let events = first
            .into_iter()
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
                                    timestamp: String::new(),
                                    tool_activities: Vec::new(),
                                    streaming: true,
                                    reasoning_content: None,
                                    reasoning_expanded: false,
                                });
                            }
                        }
                        ChatAgentUpdate::ReasoningDelta(delta) => {
                            if let Some(message) = self.messages.last_mut().filter(|m| {
                                m.role == MessageRole::Assistant && m.streaming
                            }) {
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
                                    timestamp: String::new(),
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
                                    timestamp: String::new(),
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
                        ChatAgentUpdate::Error(error) => {
                            self.messages.push(ChatMessageInfo {
                                id: format!("stream-error-{session_id}"),
                                role: MessageRole::Error,
                                content: error.clone(),
                                timestamp: String::new(),
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
                    if self.active_session_id.as_deref() == Some(&session_id) {
                        self.messages = load_session_messages(&session_file);
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
                ChatStreamEvent::Agent { .. } => {}
            }
        }
        true
    }

    pub fn active_pending_composer_message(&self) -> Option<&str> {
        self.active_session_id
            .as_ref()
            .and_then(|session_id| self.pending_composer_messages.get(session_id))
            .map(String::as_str)
    }

    pub fn stage_busy_message(&mut self, text: String) -> Result<(), String> {
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

    pub fn queue_pending_message(&mut self) -> Result<(), String> {
        let (runtime, session_id, text) = self.pending_runtime_message()?;
        runtime
            .work_handle
            .try_queue_follow_up_with_images(text.clone(), Vec::new())?;
        self.pending_composer_messages.remove(&session_id);
        self.push_optimistic_follow_up(&session_id, text);
        self.session_status = Some("Message queued…".into());
        Ok(())
    }

    pub fn steer_pending_message(&mut self) -> Result<(), String> {
        let (runtime, session_id, text) = self.pending_runtime_message()?;
        runtime
            .work_handle
            .queue_steer_with_images(text.clone(), Vec::new())?;
        self.pending_composer_messages.remove(&session_id);
        self.push_optimistic_follow_up(&session_id, text);
        self.session_status = Some("Steering current turn…".into());
        Ok(())
    }

    pub fn dismiss_pending_message(&mut self) {
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
                timestamp: String::new(),
                tool_activities: Vec::new(),
                streaming: false,
                reasoning_content: None,
                reasoning_expanded: false,
            });
        }
    }

    pub fn send_prompt(&mut self, text: String) -> Result<(), String> {
        self.send_prompt_with_images(text, Vec::new())
    }

    pub fn send_prompt_with_images(
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
                timestamp: String::new(),
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
            self.stream_tx.clone(),
        )?;
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
            content: if images.is_empty() {
                text
            } else if text.is_empty() {
                format!("[{} image attachment(s)]", images.len())
            } else {
                format!("{text}\n[{} image attachment(s)]", images.len())
            },
            timestamp: String::new(),
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

    pub fn cancel_generation(&mut self) -> Result<(), String> {
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
}
