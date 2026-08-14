use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::Mutex;
use std::time::UNIX_EPOCH;
use threadlane_agent::{AgentEvent, AgentMessage, SessionTree};

use crate::persistence::{load_project_registry, save_project_registry};

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
    pub is_generating: bool,
    pub composer_text: String,

    pub selected_model: String,
    pub workspace_page: WorkspacePage,
    pub openai_key: String,
    pub opencode_key: String,
    pub antigravity_connected: bool,
    pub auth_status_msg: Option<String>,
    stream_tx: Sender<ChatStreamEvent>,
    stream_rx: Receiver<ChatStreamEvent>,
    pending_stream_event: Mutex<Option<ChatStreamEvent>>,
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
                });
            }
            AgentMessage::UserWithImages { content, .. } => {
                result.push(ChatMessageInfo {
                    id: format!("msg_{msg_counter}"),
                    role: MessageRole::User,
                    content,
                    timestamp: String::new(),
                    tool_activities: Vec::new(),
                });
            }
            AgentMessage::Assistant {
                content,
                tool_calls,
                ..
            } => {
                let mut tool_activities = Vec::new();
                if let Some(calls) = tool_calls {
                    for (i, call) in calls.iter().enumerate() {
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
                            id: format!("tool_{msg_counter}_{i}"),
                            category,
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
                let tool_info = ToolActivityInfo {
                    id: tool_call_id,
                    category,
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
                });
            }
            AgentMessage::Custom {
                custom_type,
                payload,
            } => {
                let is_error_type = custom_type == "error" || custom_type == "agent_error";
                let err_msg = payload
                    .get("error")
                    .and_then(|v| v.as_str())
                    .map(ToString::to_string)
                    .unwrap_or_else(|| payload.to_string());
                result.push(ChatMessageInfo {
                    id: format!("msg_{msg_counter}"),
                    role: if is_error_type {
                        MessageRole::Error
                    } else {
                        MessageRole::System
                    },
                    content: err_msg,
                    timestamp: String::new(),
                    tool_activities: Vec::new(),
                });
            }
        }
    }
    result
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

        let messages = if let Some(ref file) = active_session_file {
            load_session_messages(file)
        } else {
            Vec::new()
        };

        let openai_key = threadlane_auth::openai_auth::load_openai_api_key()
            .or_else(|| std::env::var("OPENAI_API_KEY").ok())
            .unwrap_or_default();
        let opencode_key =
            threadlane_auth::opencode_auth::load_opencode_api_key().unwrap_or_default();
        let antigravity_connected =
            threadlane_provider::antigravity_auth::load_antigravity_credentials().is_some();

        let (stream_tx, stream_rx) = mpsc::channel();
        let selected_model = if !openai_key.is_empty() {
            "gpt-4o".to_string()
        } else if antigravity_connected {
            "antigravity/gemini-3.6-flash".to_string()
        } else if !opencode_key.is_empty() {
            "opencode-go/claude-3-5-sonnet".to_string()
        } else {
            "gpt-4o".to_string()
        };

        Self {
            projects: project_infos,
            active_work_dir,
            is_new_task: active_session_id.is_none(),
            active_session_id,
            search_query: String::new(),
            messages,
            is_generating: false,
            composer_text: String::new(),
            selected_model,
            workspace_page: WorkspacePage::Chat,
            openai_key,
            opencode_key,
            antigravity_connected,
            auth_status_msg: None,
            stream_tx,
            stream_rx,
            pending_stream_event: Mutex::new(None),
        }
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
        Ok(())
    }

    pub fn set_selected_model(&mut self, model: String) {
        self.selected_model = model.clone();
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

    pub fn refresh_active_session(&mut self) {
        if let (Some(work_dir), Some(session_id)) = (
            &self.active_work_dir.clone(),
            &self.active_session_id.clone(),
        ) {
            let session_file = work_dir
                .join(".threadlane/sessions")
                .join(format!("{session_id}.jsonl"));
            self.messages = load_session_messages(&session_file);
            if let Some(proj) = self.projects.iter_mut().find(|p| &p.work_dir == work_dir) {
                proj.sessions = discover_sessions_in_project(work_dir);
            }
        }
    }

    pub fn begin_new_task(&mut self) {
        self.workspace_page = WorkspacePage::Chat;
        self.active_session_id = None;
        self.is_new_task = true;
        self.messages.clear();
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
        }
    }

    pub fn select_session(&mut self, work_dir: PathBuf, session_id: String) {
        self.workspace_page = WorkspacePage::Chat;
        self.active_work_dir = Some(work_dir.clone());
        self.active_session_id = Some(session_id.clone());
        self.is_new_task = false;

        if let Some(proj) = self.projects.iter().find(|p| p.work_dir == work_dir) {
            if let Some(sess) = proj.sessions.iter().find(|s| s.id == session_id) {
                self.messages = load_session_messages(&sess.session_file);
            }
        }
    }

    pub fn settle_session(&mut self, work_dir: PathBuf, session_id: String) -> Result<(), String> {
        let session_file = self.session_file(&work_dir, &session_id);
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
        std::fs::remove_file(session_file).map_err(|error| error.to_string())?;
        self.finish_session_removal(&work_dir, &session_id);
        Ok(())
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

    pub fn toggle_project_expanded(&mut self, work_dir: &Path) {
        if let Some(proj) = self.projects.iter_mut().find(|p| p.work_dir == work_dir) {
            proj.is_expanded = !proj.is_expanded;
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

        *self = Self::load();
        self.active_work_dir = Some(canonical);
        self.active_session_id = None;
        self.is_new_task = true;
        self.messages.clear();
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

        *self = Self::load();
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
                    match event {
                        AgentEvent::MessageUpdate {
                            text_delta: Some(delta),
                            ..
                        } => {
                            if let Some(message) = self.messages.last_mut().filter(|message| {
                                message.role == MessageRole::Assistant
                                    && message.id == format!("streaming-{session_id}")
                            }) {
                                message.content.push_str(&delta);
                            } else {
                                self.messages.push(ChatMessageInfo {
                                    id: format!("streaming-{session_id}"),
                                    role: MessageRole::Assistant,
                                    content: delta,
                                    timestamp: String::new(),
                                    tool_activities: Vec::new(),
                                });
                            }
                        }
                        AgentEvent::MessageUpdate {
                            reasoning_delta: Some(delta),
                            ..
                        } => {
                            let activity_id = format!("reasoning-{session_id}");
                            if let Some(activity) = self
                                .messages
                                .iter_mut()
                                .rev()
                                .flat_map(|message| message.tool_activities.iter_mut().rev())
                                .find(|activity| activity.id == activity_id)
                            {
                                activity.detail.push_str(&delta);
                            } else {
                                let activity = ToolActivityInfo {
                                    id: activity_id,
                                    category: "Thinking".into(),
                                    title: "Reasoning".into(),
                                    detail: delta,
                                    is_expanded: false,
                                };
                                if let Some(message) = self
                                    .messages
                                    .last_mut()
                                    .filter(|message| message.role == MessageRole::Assistant)
                                {
                                    message.tool_activities.push(activity);
                                } else {
                                    self.messages.push(ChatMessageInfo {
                                        id: format!("streaming-{session_id}"),
                                        role: MessageRole::Assistant,
                                        content: String::new(),
                                        timestamp: String::new(),
                                        tool_activities: vec![activity],
                                    });
                                }
                            }
                        }
                        AgentEvent::ToolExecutionStart {
                            tool_call_id,
                            name,
                            arguments,
                        } => {
                            let activity = ToolActivityInfo {
                                id: tool_call_id,
                                category: "Working".into(),
                                title: name,
                                detail: arguments,
                                is_expanded: false,
                            };
                            if let Some(message) = self
                                .messages
                                .last_mut()
                                .filter(|message| message.role == MessageRole::Assistant)
                            {
                                message.tool_activities.push(activity);
                            } else {
                                self.messages.push(ChatMessageInfo {
                                    id: format!("streaming-{session_id}"),
                                    role: MessageRole::Assistant,
                                    content: String::new(),
                                    timestamp: String::new(),
                                    tool_activities: vec![activity],
                                });
                            }
                        }
                        AgentEvent::ToolExecutionUpdate {
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
                        AgentEvent::ToolExecutionEnd {
                            tool_call_id,
                            result,
                            ..
                        } => {
                            if let Some(activity) = self
                                .messages
                                .iter_mut()
                                .rev()
                                .flat_map(|message| message.tool_activities.iter_mut().rev())
                                .find(|activity| activity.id == tool_call_id)
                            {
                                activity.category = if result.is_error {
                                    "Error".into()
                                } else {
                                    "Completed".into()
                                };
                                activity.detail = result.content;
                            }
                        }
                        AgentEvent::AgentError { error } => {
                            self.messages.push(ChatMessageInfo {
                                id: format!("stream-error-{session_id}"),
                                role: MessageRole::Error,
                                content: error,
                                timestamp: String::new(),
                                tool_activities: Vec::new(),
                            });
                            self.is_generating = false;
                        }
                        _ => {}
                    }
                }
                ChatStreamEvent::Finished {
                    session_id,
                    session_file,
                } => {
                    if self.active_session_id.as_deref() == Some(&session_id) {
                        self.messages = load_session_messages(&session_file);
                    }
                    self.is_generating = false;
                    if let Some(work_dir) = session_file
                        .parent()
                        .and_then(Path::parent)
                        .and_then(Path::parent)
                    {
                        if let Some(project) = self
                            .projects
                            .iter_mut()
                            .find(|project| project.work_dir == work_dir)
                        {
                            project.sessions = discover_sessions_in_project(work_dir);
                        }
                    }
                }
                ChatStreamEvent::Agent { .. } => {}
            }
        }
        true
    }

    pub fn send_prompt(&mut self, text: String) -> Result<(), String> {
        let text = text.trim().to_string();
        if text.is_empty() {
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

        // 1. Load or create SessionTree with file_path bound
        let mut tree = match SessionTree::load_from_file(&session_file) {
            Ok(mut t) => {
                t.file_path = Some(session_file.clone());
                t
            }
            Err(_) => {
                let mut t = SessionTree::new(&session_id);
                t.file_path = Some(session_file.clone());
                t
            }
        };

        // Present the accepted prompt immediately. CodingAgent owns durable
        // persistence; writing it through SessionTree here would duplicate it.
        self.messages.push(ChatMessageInfo {
            id: format!("pending-user-{session_id}-{}", self.messages.len()),
            role: MessageRole::User,
            content: text.clone(),
            timestamp: String::new(),
            tool_activities: Vec::new(),
        });

        // Resolve API credentials for selected model
        let model = self.selected_model.clone();
        let api_key = if threadlane_provider::router::is_antigravity_model(&model) {
            threadlane_provider::antigravity_auth::load_antigravity_credentials()
                .map(|c| c.access_token)
                .unwrap_or_default()
        } else if threadlane_provider::router::is_opencode_model(&model) {
            threadlane_auth::opencode_auth::load_opencode_api_key().unwrap_or_default()
        } else {
            threadlane_auth::openai_auth::load_openai_api_key()
                .or_else(|| std::env::var("OPENAI_API_KEY").ok())
                .unwrap_or_default()
        };

        if api_key.is_empty() {
            let err_msg = AgentMessage::Custom {
                custom_type: "error".to_string(),
                payload: serde_json::json!({
                    "error": format!(
                        "No API key configured for model `{model}`.\n\nPlease click the model badge in the top right to open Provider Settings and save your API key."
                    )
                }),
            };
            tree.add_message(err_msg);
            self.messages = load_session_messages(&session_file);
            return Ok(());
        }

        self.is_generating = true;

        let options = threadlane_coding_agent::CodingAgentOptions {
            api_key: api_key.clone(),
            account_id: None,
            model: model.clone(),
            work_dir: work_dir.clone(),
            session_file: Some(session_file.clone()),
            system_prompt: Default::default(),
            agent_config: None,
            coding_config: None,
        };

        let text_to_agent = text.clone();
        let session_file_bg = session_file.clone();
        let session_id_bg = session_id.clone();
        let stream_tx = self.stream_tx.clone();
        std::thread::spawn(move || {
            let rt = match tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
            {
                Ok(rt) => rt,
                Err(e) => {
                    eprintln!("Failed to build tokio runtime: {e}");
                    return;
                }
            };

            rt.block_on(async move {
                let mut agent = threadlane_coding_agent::CodingAgent::new(options);
                let mut events = agent.subscribe();
                let run = agent.handle_input_with_images(&text_to_agent, Vec::new());
                tokio::pin!(run);

                loop {
                    tokio::select! {
                        result = &mut run => {
                            if let Some(Err(err)) = result {
                                let _ = stream_tx.send(ChatStreamEvent::Agent {
                                    session_id: session_id_bg.clone(),
                                    event: AgentEvent::AgentError { error: err.to_string() },
                                });
                            }
                            while let Ok(event) = events.try_recv() {
                                let _ = stream_tx.send(ChatStreamEvent::Agent {
                                    session_id: session_id_bg.clone(),
                                    event,
                                });
                            }
                            break;
                        }
                        event = events.recv() => {
                            match event {
                                Ok(event) => {
                                    let _ = stream_tx.send(ChatStreamEvent::Agent {
                                        session_id: session_id_bg.clone(),
                                        event,
                                    });
                                }
                                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                            }
                        }
                    }
                }
                let _ = stream_tx.send(ChatStreamEvent::Finished {
                    session_id: session_id_bg,
                    session_file: session_file_bg,
                });
            });
        });

        // Refresh project sessions & reload messages
        if let Some(proj) = self.projects.iter_mut().find(|p| p.work_dir == work_dir) {
            proj.sessions = discover_sessions_in_project(&work_dir);
        }
        self.composer_text.clear();
        Ok(())
    }
}
