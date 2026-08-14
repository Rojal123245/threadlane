use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;
use threadlane_agent::{AgentMessage, SessionTree};

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
pub struct AppState {
    pub projects: Vec<ProjectInfo>,
    pub active_work_dir: Option<PathBuf>,
    pub active_session_id: Option<String>,
    pub search_query: String,
    pub messages: Vec<ChatMessageInfo>,
    pub is_generating: bool,
    pub composer_text: String,
}

pub fn global_threadlane_dir() -> PathBuf {
    threadlane_coding_agent::default_global_threadlane_dir()
        .unwrap_or_else(|| dirs::home_dir().unwrap_or_else(|| PathBuf::from(".")).join(".threadlane"))
}

pub fn load_project_registry() -> Vec<AttachedProject> {
    let path = global_threadlane_dir().join("gui").join("projects.json");
    if let Ok(contents) = std::fs::read(&path) {
        if let Ok(projects) = serde_json::from_slice::<Vec<AttachedProject>>(&contents) {
            let mut seen = HashSet::new();
            return projects
                .into_iter()
                .filter_map(|mut p| {
                    p.path = std::fs::canonicalize(&p.path).unwrap_or(p.path);
                    if seen.insert(p.path.clone()) {
                        Some(p)
                    } else {
                        None
                    }
                })
                .collect();
        }
    }
    Vec::new()
}

pub fn save_project_registry(projects: &[AttachedProject]) -> Result<(), String> {
    let dir = global_threadlane_dir().join("gui");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let path = dir.join("projects.json");
    let json = serde_json::to_string_pretty(projects).map_err(|e| e.to_string())?;
    std::fs::write(&path, json).map_err(|e| e.to_string())?;
    Ok(())
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
    let messages = tree.get_active_branch_messages();
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

    let canonical_work_dir = std::fs::canonicalize(work_dir).unwrap_or_else(|_| work_dir.to_path_buf());
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

    sessions.sort_by(|a, b| b.updated_at.cmp(&a.updated_at).then_with(|| a.title.cmp(&b.title)));
    sessions
}

pub fn load_session_messages(session_file: &Path) -> Vec<ChatMessageInfo> {
    let Ok(tree) = SessionTree::load_from_file(session_file) else {
        return Vec::new();
    };

    let agent_messages = tree.get_active_branch_messages();
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
            AgentMessage::Assistant { content, tool_calls, .. } => {
                let mut tool_activities = Vec::new();
                if let Some(calls) = tool_calls {
                    for (i, call) in calls.iter().enumerate() {
                        let category = match call.function.name.as_str() {
                            "write_file" | "replace_file_content" | "multi_replace_file_content" => {
                                "Edited".into()
                            }
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
                let category = if is_error { "Error".into() } else { "Result".into() };
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
                result.push(ChatMessageInfo {
                    id: format!("msg_{msg_counter}"),
                    role: MessageRole::System,
                    content,
                    timestamp: String::new(),
                    tool_activities: Vec::new(),
                });
            }
            _ => {}
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

        Self {
            projects: project_infos,
            active_work_dir,
            active_session_id,
            search_query: String::new(),
            messages,
            is_generating: false,
            composer_text: String::new(),
        }
    }

    pub fn select_session(&mut self, work_dir: PathBuf, session_id: String) {
        self.active_work_dir = Some(work_dir.clone());
        self.active_session_id = Some(session_id.clone());

        if let Some(proj) = self.projects.iter().find(|p| p.work_dir == work_dir) {
            if let Some(sess) = proj.sessions.iter().find(|s| s.id == session_id) {
                self.messages = load_session_messages(&sess.session_file);
            }
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
        Ok(session_id)
    }

    pub fn send_prompt(&mut self, text: String) -> Result<(), String> {
        let text = text.trim().to_string();
        if text.is_empty() {
            return Ok(());
        }

        if self.active_session_id.is_none() || self.active_work_dir.is_none() {
            self.create_new_session()?;
        }

        let (work_dir, session_id) = match (self.active_work_dir.clone(), self.active_session_id.clone()) {
            (Some(w), Some(s)) => (w, s),
            _ => return Err("Failed to ensure active session".into()),
        };

        let session_file = work_dir.join(".threadlane/sessions").join(format!("{session_id}.jsonl"));

        // 1. Load or create SessionTree
        let mut tree = match SessionTree::load_from_file(&session_file) {
            Ok(t) => t,
            Err(_) => {
                let mut t = SessionTree::new(&session_id);
                t.file_path = Some(session_file.clone());
                t
            }
        };

        // 2. Append User Message
        let user_msg = AgentMessage::User { content: text.clone() };
        let leaf_id = tree.active_node_id().map(str::to_owned);
        let _ = tree.append_passive_branch(leaf_id.as_deref(), vec![user_msg]);

        // Auto-set title if default
        if tree.name.is_none() {
            tree.name = Some(extract_session_title(&tree, &session_id));
        }

        // 3. Resolve API credentials
        let api_key = std::env::var("OPENAI_API_KEY")
            .ok()
            .or_else(|| {
                threadlane_provider::antigravity_auth::load_antigravity_credentials()
                    .map(|c| c.access_token)
            })
            .or_else(|| threadlane_provider::opencode_auth::load_opencode_api_key())
            .unwrap_or_default();

        let reply_content = if api_key.is_empty() {
            format!(
                "Received: \"{text}\"\n\nNote: No API key or provider credentials were found. Please set `OPENAI_API_KEY` or sign in with `/login antigravity` to enable live model responses."
            )
        } else {
            format!("Received prompt: \"{text}\"\n\nProcessed prompt successfully.")
        };

        // 4. Append Assistant Message
        let assistant_msg = AgentMessage::Assistant {
            content: Some(reply_content),
            tool_calls: None,
            stop_reason: None,
            deferred_handle: None,
        };

        let mut tree = SessionTree::load_from_file(&session_file).unwrap_or(tree);
        let leaf_id = tree.active_node_id().map(str::to_owned);
        let _ = tree.append_passive_branch(leaf_id.as_deref(), vec![assistant_msg]);

        // 5. Reload session messages & project list
        if let Some(proj) = self.projects.iter_mut().find(|p| p.work_dir == work_dir) {
            proj.sessions = discover_sessions_in_project(&work_dir);
        }
        self.messages = load_session_messages(&session_file);
        self.composer_text.clear();
        Ok(())
    }
}
