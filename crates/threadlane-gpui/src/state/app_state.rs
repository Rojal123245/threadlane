use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};
use threadlane_session::harness::{JsonlStore, SessionStore};
use threadlane_session::{
    AgentEvent, AgentMessage, ImageAttachment, ReasoningEffort, SessionPlan, TokenUsage,
};

use crate::adapters::agent_events::{adapt_agent_event, ChatAgentUpdate};
use crate::persistence::load_project_registry;
use crate::services::sessions::{ExecutionMode, SessionRuntime, SessionRuntimeStatus};

const CHAT_HISTORY_PAGE_SIZE: usize = 40;
pub type AttachedProject = threadlane_session::ProjectRecord;

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
    Advisor(threadlane_session::AdvisorSeverity),
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
    pub(crate) cache_read_tokens: u64,
    pub(crate) cache_write_tokens: u64,
}

impl SessionMetricsInfo {
    pub(crate) fn billed_input_tokens(&self) -> u64 {
        self.input_tokens
            .saturating_add(self.cache_read_tokens)
            .saturating_add(self.cache_write_tokens)
    }

    pub(crate) fn cache_hit_percent(&self) -> Option<u64> {
        let billed_input = self.billed_input_tokens();
        (billed_input > 0).then(|| {
            self.cache_read_tokens
                .saturating_mul(100)
                .saturating_add(billed_input / 2)
                / billed_input
        })
    }

    fn accumulate_usage(&mut self, usage: &TokenUsage) {
        self.input_tokens = self
            .input_tokens
            .saturating_add(u64::from(usage.input_tokens));
        self.output_tokens = self
            .output_tokens
            .saturating_add(u64::from(usage.output_tokens));
        self.cache_read_tokens = self
            .cache_read_tokens
            .saturating_add(u64::from(usage.cache_read_tokens));
        self.cache_write_tokens = self
            .cache_write_tokens
            .saturating_add(u64::from(usage.cache_write_tokens));
    }
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

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RequestedEditorTarget {
    File(String),
    Diff {
        project: PathBuf,
        path: String,
        content: String,
    },
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum WorkspacePage {
    #[default]
    Chat,
    Settings,
}

/// A session whose durable UI projections need to be computed off the UI thread.
pub(crate) struct SessionHydrationRequest {
    pub(crate) session_id: String,
    pub(crate) session_file: PathBuf,
}

/// The complete durable UI projection built from one JSONL store parse.
pub(crate) struct SessionProjectionResult {
    pub(crate) messages: Vec<ChatMessageInfo>,
    pub(crate) history_start: usize,
    pub(crate) history_has_older: bool,
    pub(crate) plan: SessionPlan,
    pub(crate) trajectory: Vec<TrajectoryEntry>,
    pub(crate) diagnostics: threadlane_session::harness::SessionDiagnostics,
    pub(crate) metrics: SessionMetricsInfo,
    pub(crate) token_usage: TokenUsage,
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
    diagnostics_by_session: HashMap<String, threadlane_session::harness::SessionDiagnostics>,
    session_metrics: HashMap<String, SessionMetricsInfo>,
    stashed_prompts: HashMap<String, String>,
    pub(crate) pending_permissions: HashMap<String, threadlane_session::PermissionRequest>,
    pub(crate) pending_hydrations: Vec<SessionHydrationRequest>,

    pub(crate) selected_model: String,
    pub(crate) model_roles: threadlane_session::ModelRoles,
    pub(crate) reasoning_effort: ReasoningEffort,
    pub(crate) workspace_page: WorkspacePage,
    pub(crate) openai_key: String,
    pub(crate) opencode_key: String,
    pub(crate) auth_status_msg: Option<String>,
    pub(crate) update_status: threadlane_updater::UpdateStatus,
    pub(crate) update_notice_dismissed: bool,
    pub(crate) requested_editor_target: Option<RequestedEditorTarget>,
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

fn extract_session_title(store: &impl SessionStore, fallback_id: &str) -> String {
    if let Some(name) = store.name() {
        if !name.trim().is_empty() {
            return name;
        }
    }
    let messages = {
        let active = store.active_branch_messages("main");
        if active.is_empty() {
            store.get_persisted_messages()
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
                let (title, health, updated_at) = match JsonlStore::open_read_only(&path) {
                    Ok(store) => (
                        extract_session_title(&store, &id),
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
    JsonlStore::open_read_only(session_file)
        .map(|store| store.plan())
        .unwrap_or_default()
}

pub fn load_session_messages(session_file: &Path) -> Vec<ChatMessageInfo> {
    load_session_message_page(session_file, usize::MAX).0
}

fn load_session_projection(
    session_file: &Path,
) -> (SessionPlan, Vec<ChatMessageInfo>, usize, bool) {
    let Ok(store) = JsonlStore::open_read_only(session_file) else {
        return (SessionPlan::default(), Vec::new(), 0, false);
    };
    // UI history is the durable chronological transcript projection, distinct
    // from the active model-context branch used for provider requests.
    let agent_messages = store.transcript("main").messages();
    let projected = project_agent_messages(agent_messages);
    let end = projected.len();
    let start = end.saturating_sub(CHAT_HISTORY_PAGE_SIZE);
    (
        store.plan(),
        projected[start..end].to_vec(),
        start,
        start > 0,
    )
}

fn load_session_message_page(
    session_file: &Path,
    end: usize,
) -> (Vec<ChatMessageInfo>, usize, bool) {
    let Ok(store) = JsonlStore::open_read_only(session_file) else {
        return (Vec::new(), 0, false);
    };
    project_message_page_from_store(&store, end)
}

fn project_message_page_from_store(
    store: &JsonlStore,
    end: usize,
) -> (Vec<ChatMessageInfo>, usize, bool) {
    let agent_messages = store.transcript("main").messages();
    let projected = project_agent_messages(agent_messages);
    let end = end.min(projected.len());
    let start = end.saturating_sub(CHAT_HISTORY_PAGE_SIZE);
    (projected[start..end].to_vec(), start, start > 0)
}

/// Computes an older transcript page from disk. Call this on GPUI's background executor.
pub(crate) fn compute_older_message_page(
    session_file: &Path,
    end: usize,
) -> (Vec<ChatMessageInfo>, usize, bool) {
    load_session_message_page(session_file, end)
}

/// Opens a session JSONL once and builds every UI projection required after hydration.
pub(crate) fn compute_full_session_projection(
    session_file: &Path,
) -> Result<SessionProjectionResult, String> {
    let store = JsonlStore::open_read_only(session_file).map_err(|error| error.to_string())?;
    let diagnostics = threadlane_session::harness::project_session_diagnostics(&store, "main")
        .map_err(|error| error.to_string())?;
    let (messages, history_start, history_has_older) =
        project_message_page_from_store(&store, usize::MAX);
    let (trajectory, metrics, token_usage) = AppState::project_trajectory_from_store(&store);
    Ok(SessionProjectionResult {
        messages,
        history_start,
        history_has_older,
        plan: store.plan(),
        trajectory,
        diagnostics,
        metrics,
        token_usage,
    })
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
    threadlane_session::harness::project_chat_messages(&agent_messages)
        .into_iter()
        .map(|msg| ChatMessageInfo {
            id: msg.id,
            role: match msg.role {
                threadlane_session::harness::UiMessageRole::User => MessageRole::User,
                threadlane_session::harness::UiMessageRole::Assistant => MessageRole::Assistant,
                threadlane_session::harness::UiMessageRole::System => MessageRole::System,
                threadlane_session::harness::UiMessageRole::Advisor(sev) => {
                    MessageRole::Advisor(sev)
                }
                threadlane_session::harness::UiMessageRole::Error => MessageRole::Error,
            },
            content: msg.content,
            tool_activities: msg
                .tool_activities
                .into_iter()
                .map(|act| ToolActivityInfo {
                    id: act.id,
                    category: act.category,
                    title: act.title,
                    summary: act.summary,
                    detail: act.detail,
                    is_expanded: false,
                })
                .collect(),
            streaming: false,
            reasoning_content: msg.reasoning_content,
            reasoning_expanded: false,
        })
        .collect()
}

pub(crate) fn runtime_status_text(status: SessionRuntimeStatus) -> Option<String> {
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
    model_roles: threadlane_session::ModelRoles,
) -> threadlane_session::CodingAgentOptions {
    let (api_key, account_id) = provider_credentials(&model);
    let mut agent_config = threadlane_session::AgentConfig::default();
    agent_config.model_roles = model_roles;

    threadlane_session::CodingAgentOptions {
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
                let project = AttachedProject::from_path(curr);
                registry_projects.push(project.clone());
                let _ = threadlane_session::save_project_registry(&registry_projects);
            }
        }

        let mut project_infos = Vec::new();
        let mut active_work_dir = None;
        let mut active_session_id = None;
        let mut active_session_file = None;
        let mut active_project_index = 0;
        for index in 1..registry_projects.len() {
            if registry_projects[index].last_opened_at
                > registry_projects[active_project_index].last_opened_at
            {
                active_project_index = index;
            }
        }

        for (i, p) in registry_projects.iter().enumerate() {
            let sessions = discover_sessions_in_project(&p.path);
            let is_active = i == active_project_index;

            if is_active {
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
                name: p.name.clone(),
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

        let model_roles = threadlane_session::ModelRoles::default();
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
                let runtime = SessionRuntime::new(
                    coding_agent_options(
                        work_dir.clone(),
                        session_file.clone(),
                        selected_model.clone(),
                        model_roles.clone(),
                    ),
                    ExecutionMode::Interactive,
                );
                let messages = initial_messages.clone();
                selected_model = runtime.selected_model.clone();
                session_status = runtime_status_text(runtime.status());
                session_runtimes.insert(session_file.clone(), runtime);
                messages
            }
            _ => Vec::new(),
        };

        let mut state = Self {
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
            diagnostics_by_session: HashMap::new(),
            session_metrics: HashMap::new(),
            stashed_prompts: HashMap::new(),
            selected_model,
            model_roles: threadlane_session::ModelRoles::default(),
            reasoning_effort: ReasoningEffort::default(),
            workspace_page: WorkspacePage::Chat,
            openai_key,
            opencode_key,
            auth_status_msg: None,
            update_status: threadlane_updater::UpdateStatus::Idle,
            update_notice_dismissed: false,
            requested_editor_target: None,
            stream_tx,
            stream_rx,
            pending_stream_event: Mutex::new(None),
            session_refresh_tx,
            session_refresh_rx,
            session_runtimes,
            deferred_stream_events: HashMap::new(),
            pending_permissions: HashMap::new(),
            pending_hydrations: Vec::new(),
        };
        if let (Some(session_id), Some(session_file)) = (
            state.active_session_id.clone(),
            active_session_file.as_deref(),
        ) {
            if let Err(error) = state.hydrate_session_projection(&session_id, session_file) {
                state.trajectory_by_session.insert(
                    session_id,
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
        state
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
        }
        self.invalidate_idle_runtimes();
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

    fn persist_project_selection(&self, work_dir: &Path, session_id: Option<&str>) {
        if let Err(error) = threadlane_session::select_project(work_dir, session_id) {
            tracing::warn!("Failed to persist selected project: {error}");
        }
    }

    pub(crate) fn select_draft_project(&mut self, work_dir: PathBuf) {
        if self
            .projects
            .iter()
            .any(|project| project.work_dir == work_dir)
        {
            self.active_work_dir = Some(work_dir.clone());
            self.active_session_id = None;
            self.is_new_task = true;
            self.messages.clear();
            self.history_session_file = None;
            self.history_start = 0;
            self.history_has_older = false;
            self.active_plan = SessionPlan::default();
            self.is_generating = false;
            self.session_status = None;
            self.persist_project_selection(&work_dir, None);
            self.request_session_refresh(&work_dir);
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

    pub(crate) fn history_page_request(&self) -> Option<(PathBuf, usize)> {
        self.history_has_older
            .then(|| self.history_session_file.clone().map(|file| (file, self.history_start)))
            .flatten()
    }

    pub(crate) fn request_open_file(&mut self, relative_path: String) {
        self.requested_editor_target = Some(RequestedEditorTarget::File(relative_path));
    }

    pub(crate) fn request_open_diff(
        &mut self,
        project: PathBuf,
        relative_path: String,
        content: String,
    ) {
        self.requested_editor_target = Some(RequestedEditorTarget::Diff {
            project,
            path: relative_path,
            content,
        });
    }

    pub(crate) fn select_session(
        &mut self,
        work_dir: PathBuf,
        session_id: String,
    ) -> SessionHydrationRequest {
        self.workspace_page = WorkspacePage::Chat;
        self.active_work_dir = Some(work_dir.clone());
        self.active_session_id = Some(session_id.clone());
        self.is_new_task = false;
        self.persist_project_selection(&work_dir, Some(&session_id));

        let session_file = self.session_file(&work_dir, &session_id);
        let completed_events = self
            .deferred_stream_events
            .remove(&session_id)
            .unwrap_or_default();
        let runtime = self.ensure_session_runtime(work_dir, session_file.clone());
        for event in completed_events {
            if let ChatStreamEvent::Agent { event, .. } = event {
                self.record_trajectory(&session_id, &event);
            }
        }
        self.messages.clear();
        self.history_session_file = Some(session_file.clone());
        self.history_start = 0;
        self.history_has_older = false;
        self.active_plan = SessionPlan::default();
        self.is_generating = runtime.is_generating();
        self.selected_model = runtime.selected_model.clone();
        self.session_status = Some("Loading session…".into());
        let request = SessionHydrationRequest {
            session_id,
            session_file,
        };
        self.pending_hydrations.push(SessionHydrationRequest {
            session_id: request.session_id.clone(),
            session_file: request.session_file.clone(),
        });
        request
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

    #[allow(dead_code)]
    pub(crate) fn update_model_roles(&mut self, roles: threadlane_session::ModelRoles) {
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
        let runtime = SessionRuntime::new(
            coding_agent_options(
                work_dir,
                session_file.clone(),
                self.selected_model.clone(),
                self.model_roles.clone(),
            ),
            ExecutionMode::Interactive,
        );
        self.session_runtimes.insert(session_file, runtime.clone());
        runtime
    }

    pub(crate) fn resolve_active_permission(
        &mut self,
        request_id: &str,
        decision: threadlane_session::PermissionDecision,
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
            let _ = self.select_session(next_work_dir, next_session_id);
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

        let record = threadlane_session::register_project(&canonical)?;

        if !self
            .projects
            .iter()
            .any(|project| project.work_dir == canonical)
        {
            self.projects.push(ProjectInfo {
                name: record.name,
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

        let _ = std::fs::File::create(&session_file);

        if let Some(project) = self
            .projects
            .iter_mut()
            .find(|project| project.work_dir == work_dir)
        {
            project.sessions = discover_sessions_in_project(&work_dir);
        }
        let _ = self.select_session(work_dir, session_id.clone());
        self.is_new_task = false;
        Ok(session_id)
    }

    /// Hydrates trajectory, token usage, and metrics projections from durable harness records.
    fn hydrate_session_projection(
        &mut self,
        session_id: &str,
        session_file: &Path,
    ) -> Result<(), String> {
        let result = compute_full_session_projection(session_file)?;
        self.diagnostics_by_session
            .insert(session_id.to_owned(), result.diagnostics);
        self.trajectory_by_session
            .insert(session_id.into(), result.trajectory);
        self.session_metrics.insert(session_id.into(), result.metrics);
        self.session_token_usage
            .insert(session_id.into(), result.token_usage);
        Ok(())
    }

    /// Projects trajectory entries, token usage, and metrics from an already-open store.
    fn project_trajectory_from_store(
        store: &JsonlStore,
    ) -> (Vec<TrajectoryEntry>, SessionMetricsInfo, TokenUsage) {
        let mut trajectory: Vec<TrajectoryEntry> = Vec::new();
        let mut metrics = SessionMetricsInfo::default();
        let mut durable_usage = TokenUsage::default();

        let mut tool_starts =
            HashMap::<(String, String), (String, String, String, serde_json::Value)>::new();
        let mut tool_finishes = HashMap::<String, (String, String, String)>::new();
        let provider_usage_keys = store
            .records()
            .iter()
            .filter_map(|record| match record {
                threadlane_session::harness::Record::Usage {
                    run_id: Some(run_id),
                    attempt: Some(attempt),
                    cause: threadlane_session::harness::UsageCause::Provider,
                    ..
                } => Some((run_id.clone(), *attempt)),
                _ => None,
            })
            .collect::<HashSet<_>>();
        for record in store.records() {
            use threadlane_session::harness::Record;
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
                        summary: format!("Step {attempt} started"),
                        detail: format!("lane {}", lane.as_str()),
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
                    lane,
                    run_id,
                    assistant_entry_id,
                    tool_call_id,
                    tool_name,
                    effective_args,
                    ..
                } => {
                    tool_starts.insert(
                        (assistant_entry_id.clone(), tool_call_id.clone()),
                        (
                            run_id.clone(),
                            lane.clone(),
                            tool_name.clone(),
                            effective_args.clone(),
                        ),
                    );
                    None
                }
                Record::ToolFinished {
                    lane,
                    run_id,
                    tool_call_id,
                    result_entry_id,
                    ..
                } => {
                    tool_finishes.insert(
                        result_entry_id.clone(),
                        (run_id.clone(), lane.clone(), tool_call_id.clone()),
                    );
                    None
                }
                Record::Usage { cause, usage, .. } => {
                    if matches!(cause, threadlane_session::harness::UsageCause::Provider) {
                        metrics.accumulate_usage(usage);
                        durable_usage.accumulate(usage);
                    }
                    None
                }
                Record::RunContextCaptured {
                    seq,
                    lane,
                    run_id,
                    model,
                    provider,
                    reasoning_effort,
                    prompt_cache_enabled,
                    work_dir,
                    system_prompt,
                    tool_schema_sha256,
                    enabled_tool_names,
                    ..
                } => {
                    let prompt_text = match system_prompt {
                        threadlane_session::harness::PromptSnapshot::Full { sha256, content } => {
                            format!("### System Prompt (SHA256 `{}`)\n\n```markdown\n{}\n```", sha256.as_str(), content.as_str())
                        }
                        threadlane_session::harness::PromptSnapshot::Redacted {
                            sha256,
                            byte_len,
                            reason,
                        } => format!(
                            "### System Prompt (Redacted)\n\n- Size: {byte_len} bytes\n- SHA256: `{}`\n- Reason: {}",
                            sha256.as_str(),
                            reason.as_str()
                        ),
                    };
                    let tools_list = if enabled_tool_names.is_empty() {
                        "None".to_string()
                    } else {
                        enabled_tool_names
                            .iter()
                            .map(|t| format!("`{}`", t.as_str()))
                            .collect::<Vec<_>>()
                            .join(", ")
                    };
                    let detail = format!(
                        "**Model**: `{}`\n\n**Provider**: `{}`\n\n**Reasoning Effort**: `{:?}`\n\n**Prompt Cache**: `{}`\n\n**Work Dir**: `{}`\n\n**Enabled Tools ({})**:\n{}\n\n**Tool Schema SHA256**: `{}`\n\n{}",
                        model.as_str(),
                        provider.as_str(),
                        reasoning_effort,
                        prompt_cache_enabled,
                        work_dir.as_str(),
                        enabled_tool_names.len(),
                        tools_list,
                        tool_schema_sha256.as_str(),
                        prompt_text
                    );
                    Some(TrajectoryEntry {
                        seq: Some(*seq),
                        run_id: Some(run_id.clone()),
                        turn: None,
                        category: "Context".into(),
                        summary: format!(
                            "{} via {} ({reasoning_effort:?})",
                            model.as_str(),
                            provider.as_str()
                        ),
                        detail,
                        lane: Some(lane.clone()),
                        correlation_id: None,
                    })
                }
                Record::ProviderRequestStarted {
                    seq,
                    lane,
                    run_id,
                    attempt,
                    provider,
                    model,
                    request_id,
                    ..
                } => Some(TrajectoryEntry {
                    seq: Some(*seq),
                    run_id: Some(run_id.clone()),
                    turn: Some(*attempt),
                    category: "Provider".into(),
                    summary: format!("{} request started", provider.as_str()),
                    detail: format!(
                        "**Provider**: `{}`\n\n**Model**: `{}`\n\n**Turn / Attempt**: `{}`\n\n**Request ID**: `{}`",
                        provider.as_str(),
                        model.as_str(),
                        attempt,
                        request_id.as_ref().map(|r| r.as_str()).unwrap_or("none")
                    ),
                    lane: Some(lane.clone()),
                    correlation_id: request_id.as_ref().map(|id| id.as_str().to_owned()),
                }),
                Record::ProviderRequestFinished {
                    seq,
                    lane,
                    run_id,
                    attempt,
                    request_id,
                    outcome,
                    error,
                    duration_ms,
                    usage,
                    ..
                } => {
                    if !provider_usage_keys.contains(&(run_id.clone(), *attempt)) {
                        if let Some(usage) = usage {
                            metrics.accumulate_usage(usage);
                            durable_usage.accumulate(usage);
                        }
                    }
                    let mut detail_lines = Vec::new();
                    detail_lines.push(format!("**Outcome**: `{:?}`", outcome));
                    if let Some(duration) = duration_ms {
                        detail_lines.push(format!("**Duration**: {duration} ms"));
                    }
                    if let Some(req_id) = request_id {
                        detail_lines.push(format!("**Request ID**: `{}`", req_id.as_str()));
                    }
                    if let Some(usage) = usage {
                        detail_lines.push(format!(
                            "**Tokens**: input={}, output={}, total={}",
                            usage.input_tokens, usage.output_tokens, usage.total_tokens
                        ));
                    }
                    if let Some(err) = error.as_ref() {
                        detail_lines.push(format!("**Category**: `{:?}`", err.category));
                        detail_lines.push(format!("**Retryable**: `{}`", err.retryable));
                        if let Some(code) = err.code.as_ref() {
                            detail_lines.push(format!("**Error Details**:\n```\n{}\n```", code.as_str()));
                        }
                    }
                    Some(TrajectoryEntry {
                        seq: Some(*seq),
                        run_id: Some(run_id.clone()),
                        turn: Some(*attempt),
                        category: "Provider".into(),
                        summary: format!("Provider request {outcome:?}"),
                        detail: detail_lines.join("\n\n"),
                        lane: Some(lane.clone()),
                        correlation_id: request_id.as_ref().map(|id| id.as_str().to_owned()),
                    })
                }
                Record::ProviderResponseAttached {
                    seq,
                    lane,
                    run_id,
                    attempt,
                    request_id,
                    entry_id,
                    reasoning_entry_id,
                    ..
                } => Some(TrajectoryEntry {
                    seq: Some(*seq),
                    run_id: Some(run_id.clone()),
                    turn: Some(*attempt),
                    category: "Provider".into(),
                    summary: "Provider response attached".into(),
                    detail: format!(
                        "entry {}{}",
                        entry_id,
                        reasoning_entry_id
                            .as_deref()
                            .map(|id| format!(", thinking {id}"))
                            .unwrap_or_default()
                    ),
                    lane: Some(lane.clone()),
                    correlation_id: request_id.as_ref().map(|id| id.as_str().to_owned()),
                }),
                Record::PermissionRequested {
                    seq,
                    lane,
                    run_id,
                    attempt,
                    request_id,
                    capability,
                    scopes,
                    detail_sha256,
                    ..
                } => Some(TrajectoryEntry {
                    seq: Some(*seq),
                    run_id: run_id.clone(),
                    turn: *attempt,
                    category: "Permission".into(),
                    summary: format!("{} permission requested", capability.as_str()),
                    detail: format!("scopes {scopes:?}; detail sha256 {}", detail_sha256.as_str()),
                    lane: Some(lane.clone()),
                    correlation_id: Some(request_id.as_str().to_owned()),
                }),
                Record::PermissionResolved {
                    seq,
                    lane,
                    run_id,
                    attempt,
                    request_id,
                    decision,
                    source,
                    remembered,
                    ..
                } => Some(TrajectoryEntry {
                    seq: Some(*seq),
                    run_id: run_id.clone(),
                    turn: *attempt,
                    category: "Permission".into(),
                    summary: format!("Permission {decision:?}"),
                    detail: format!("source {source:?}; remembered {remembered}"),
                    lane: Some(lane.clone()),
                    correlation_id: Some(request_id.as_str().to_owned()),
                }),
                Record::ToolExecutionObserved {
                    seq,
                    lane,
                    run_id,
                    attempt,
                    tool_call_id,
                    tool_name,
                    executor_kind,
                    phase,
                    duration_ms,
                    outcome,
                    cancelled,
                    ..
                } => Some(TrajectoryEntry {
                    seq: Some(*seq),
                    run_id: Some(run_id.clone()),
                    turn: *attempt,
                    category: "Tool runtime".into(),
                    summary: format!("{} {phase:?}", tool_name.as_str()),
                    detail: format!(
                        "executor {}; outcome {outcome:?}; duration {duration_ms:?} ms; cancelled {cancelled}",
                        executor_kind.as_str()
                    ),
                    lane: Some(lane.clone()),
                    correlation_id: Some(tool_call_id.as_str().to_owned()),
                }),
                Record::AbortObserved {
                    seq,
                    lane,
                    run_id,
                    attempt,
                    observation,
                    initiator,
                    target,
                    acknowledged,
                    ..
                } => Some(TrajectoryEntry {
                    seq: Some(*seq),
                    run_id: Some(run_id.clone()),
                    turn: *attempt,
                    category: "Cancellation".into(),
                    summary: format!("{observation:?} for {target:?}"),
                    detail: format!("initiator {initiator:?}; acknowledged {acknowledged}"),
                    lane: Some(lane.clone()),
                    correlation_id: None,
                }),
                Record::SubagentLifecycle {
                    seq,
                    lane,
                    run_id,
                    attempt,
                    child_run_id,
                    agent_id,
                    subagent_lane,
                    phase,
                    error,
                    ..
                } => Some(TrajectoryEntry {
                    seq: Some(*seq),
                    run_id: run_id.clone(),
                    turn: *attempt,
                    category: "Subagent".into(),
                    summary: format!("{} {phase:?}", agent_id.as_str()),
                    detail: format!(
                        "child {}; lane {}{}",
                        child_run_id.as_str(),
                        subagent_lane.as_str(),
                        error
                            .as_ref()
                            .map(|error| format!("; {}", error.as_str()))
                            .unwrap_or_default()
                    ),
                    lane: Some(lane.clone()),
                    correlation_id: Some(child_run_id.as_str().to_owned()),
                }),
                Record::StreamCheckpoint {
                    seq,
                    lane,
                    run_id,
                    attempt,
                    request_id,
                    text,
                    reasoning,
                    checkpoint_index,
                    byte_count,
                    fingerprint,
                    ..
                } => Some(TrajectoryEntry {
                    seq: Some(*seq),
                    run_id: Some(run_id.clone()),
                    turn: *attempt,
                    category: "Incomplete stream".into(),
                    summary: format!("Incomplete stream checkpoint {checkpoint_index}"),
                    detail: format!(
                        "{byte_count} bytes; text {} bytes; reasoning {} bytes; sha256 {}",
                        text.as_ref().map_or(0, |text| text.as_str().len()),
                        reasoning
                            .as_ref()
                            .map_or(0, |reasoning| reasoning.as_str().len()),
                        fingerprint.as_str()
                    ),
                    lane: Some(lane.clone()),
                    correlation_id: Some(request_id.as_str().to_owned()),
                }),
                _ => None,
            };
            if let Some(entry) = entry {
                trajectory.push(entry);
            }
        }
        for entry in store.entries() {
            if let AgentMessage::Assistant {
                tool_calls: Some(calls),
                ..
            } = &entry.message
            {
                for call in calls {
                    metrics.tool_calls = metrics.tool_calls.saturating_add(1);
                    let durable = tool_starts.get(&(entry.id.clone(), call.id.clone()));
                    let run_id = durable.map(|(run_id, _, _, _)| run_id.clone());
                    let lane = durable
                        .map(|(_, lane, _, _)| lane.clone())
                        .unwrap_or_else(|| entry.lane.clone());
                    let name = durable
                        .map(|(_, _, name, _)| name.as_str())
                        .unwrap_or(call.function.name.as_str());
                    let detail = durable
                        .map(|(_, _, _, args)| args.to_string())
                        .unwrap_or_else(|| call.function.arguments.clone());
                    trajectory.push(TrajectoryEntry {
                        seq: Some(entry.seq),
                        run_id,
                        turn: None,
                        category: "Tool".into(),
                        summary: format!("{name} running"),
                        detail,
                        lane: Some(lane),
                        correlation_id: Some(call.id.clone()),
                    });
                }
            }
            if let AgentMessage::Tool {
                tool_call_id,
                name,
                content,
                is_error,
                ..
            } = &entry.message
            {
                let durable = tool_finishes.get(&entry.id);
                trajectory.push(TrajectoryEntry {
                    seq: Some(entry.seq),
                    run_id: durable.map(|(run_id, _, _)| run_id.clone()),
                    turn: None,
                    category: "Tool".into(),
                    summary: format!("{name} {}", if *is_error { "failed" } else { "finished" }),
                    detail: content.clone(),
                    lane: Some(
                        durable
                            .map(|(_, lane, _)| lane.clone())
                            .unwrap_or_else(|| entry.lane.clone()),
                    ),
                    correlation_id: Some(
                        durable
                            .map(|(_, _, call_id)| call_id.clone())
                            .unwrap_or_else(|| tool_call_id.clone()),
                    ),
                });
                continue;
            }
            let projected = match &entry.message {
                AgentMessage::User { content } | AgentMessage::UserWithImages { content, .. } => {
                    Some((
                        "Input".to_string(),
                        "User input".to_string(),
                        content.clone(),
                    ))
                }
                AgentMessage::Assistant {
                    content: Some(content),
                    ..
                } if !content.trim().is_empty() => Some((
                    "Assistant".to_string(),
                    "Assistant response".to_string(),
                    content.clone(),
                )),
                AgentMessage::Custom {
                    custom_type,
                    payload,
                } if matches!(
                    custom_type.as_str(),
                    "thinking" | "goal_round" | "agent_error"
                ) =>
                {
                    let (category, summary, detail) = if custom_type == "agent_error" {
                        let err_msg = payload
                            .get("error")
                            .and_then(|v| v.as_str())
                            .unwrap_or("agent error");
                        (
                            "Error".to_string(),
                            "Agent Error".to_string(),
                            format!("### Error Details\n\n```\n{}\n```", err_msg),
                        )
                    } else {
                        (
                            "Context".to_string(),
                            custom_type.to_string(),
                            serde_json::to_string_pretty(payload)
                                .unwrap_or_else(|_| payload.to_string()),
                        )
                    };
                    Some((category, summary, detail))
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
        (trajectory, metrics, durable_usage)
    }

    /// Applies a completed background projection if its session remains active.
    pub(crate) fn session_status_for_file(&self, session_file: &Path) -> Option<String> {
        self.session_runtimes
            .get(session_file)
            .and_then(|runtime| runtime_status_text(runtime.status()))
    }

    pub(crate) fn apply_session_hydration(
        &mut self,
        session_id: &str,
        session_file: &Path,
        result: SessionProjectionResult,
    ) {
        if self.active_session_id.as_deref() != Some(session_id) {
            return;
        }
        self.messages = result.messages;
        self.history_session_file = Some(session_file.to_path_buf());
        self.history_start = result.history_start;
        self.history_has_older = result.history_has_older;
        self.active_plan = result.plan;
        self.trajectory_by_session
            .insert(session_id.to_owned(), result.trajectory);
        self.diagnostics_by_session
            .insert(session_id.to_owned(), result.diagnostics);
        self.session_metrics.insert(session_id.to_owned(), result.metrics);
        self.session_token_usage
            .insert(session_id.to_owned(), result.token_usage);
    }

    /// Applies an older page that was computed off the UI thread.
    pub(crate) fn apply_older_message_page(
        &mut self,
        session_file: &Path,
        older: Vec<ChatMessageInfo>,
        start: usize,
        has_older: bool,
    ) -> usize {
        if self.history_session_file.as_deref() != Some(session_file) {
            return 0;
        }
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

    fn record_trajectory(&mut self, session_id: &str, event: &AgentEvent) {
        let entry = match event {
            // Provider/tool-loop turn boundaries are ephemeral and have no
            // durable record, so they are intentionally excluded from the
            // canonical trajectory projection.
            AgentEvent::TurnStart { .. } | AgentEvent::TurnEnd { .. } => None,
            AgentEvent::ToolExecutionStart {
                name, arguments, ..
            } => Some(("Tool", format!("{name} running"), arguments.clone(), None)),
            AgentEvent::ToolExecutionEnd { name, result, .. } => Some((
                "Tool",
                format!(
                    "{name} {}",
                    if result.is_error {
                        "failed"
                    } else {
                        "finished"
                    }
                ),
                result.content.clone(),
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
                    turn: None,
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

    pub(crate) fn active_model_context_diagnostics(&self) -> Vec<TrajectoryEntry> {
        let Some(projection) = self
            .active_session_id
            .as_ref()
            .and_then(|id| self.diagnostics_by_session.get(id))
        else {
            return Vec::new();
        };
        projection
            .model_context
            .iter()
            .map(|entry| {
                let json_text = serde_json::to_string_pretty(&entry.message)
                    .unwrap_or_else(|_| format!("{:?}", entry.message));
                TrajectoryEntry {
                    seq: Some(entry.seq),
                    run_id: None,
                    turn: None,
                    category: "Model Context".into(),
                    summary: format!("{} · {}", entry.id, entry.message.role_str()),
                    detail: format!(
                        "**Entry ID**: `{}`\n**Role**: `{}`\n**Lane**: `{}`\n\n```json\n{}\n```",
                        entry.id,
                        entry.message.role_str(),
                        entry.lane,
                        json_text
                    ),
                    lane: Some(entry.lane.clone()),
                    correlation_id: Some(entry.id.clone()),
                }
            })
            .collect()
    }

    pub(crate) fn active_durable_event_diagnostics(&self) -> Vec<TrajectoryEntry> {
        let Some(projection) = self
            .active_session_id
            .as_ref()
            .and_then(|id| self.diagnostics_by_session.get(id))
        else {
            return Vec::new();
        };
        projection
            .durable_events
            .iter()
            .map(|event| {
                let (category, summary, detail) = match &event.kind {
                    threadlane_session::harness::DurableEventKind::Entry { role, parent_id } => (
                        "Entry",
                        format!("{} · {role}", event.id),
                        format!("parent={parent_id:?}"),
                    ),
                    threadlane_session::harness::DurableEventKind::Record => (
                        "Record",
                        format!("{} · durable record", event.id),
                        format!(
                            "seq={} lane={} run={}",
                            event.seq,
                            event.lane,
                            event.run_id.as_deref().unwrap_or("—")
                        ),
                    ),
                };
                TrajectoryEntry {
                    seq: Some(event.seq),
                    run_id: event.run_id.clone(),
                    turn: event.turn,
                    category: category.into(),
                    summary,
                    detail,
                    lane: Some(event.lane.clone()),
                    correlation_id: Some(event.id.clone()),
                }
            })
            .collect()
    }

    pub(crate) fn active_recovery_diagnostics(&self) -> Vec<TrajectoryEntry> {
        let Some(projection) = self
            .active_session_id
            .as_ref()
            .and_then(|id| self.diagnostics_by_session.get(id))
        else {
            return Vec::new();
        };
        project_recovery_diagnostics(&projection.recovery)
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
        const MAX_EVENTS_PER_DRAIN: usize = 128;
        let first = self
            .pending_stream_event
            .lock()
            .ok()
            .and_then(|mut pending| pending.take());
        let active_session_id = self.active_session_id.clone();
        let mut deferred = active_session_id
            .as_ref()
            .and_then(|session_id| self.deferred_stream_events.remove(session_id))
            .unwrap_or_default()
            .into_iter();
        let mut first = first;
        let mut processed = 0usize;
        let mut has_events = false;

        while processed < MAX_EVENTS_PER_DRAIN {
            let event = deferred
                .next()
                .or_else(|| first.take())
                .or_else(|| self.stream_rx.try_recv().ok());
            let Some(event) = event else {
                break;
            };
            has_events = true;
            processed = processed.saturating_add(1);
            match event {
                ChatStreamEvent::Agent { session_id, event }
                    if self.active_session_id.as_deref() == Some(&session_id) =>
                {
                    if matches!(&event, AgentEvent::TurnStart { .. }) {
                        if let Some(message) = self
                            .messages
                            .last_mut()
                            .filter(|message| message.role == MessageRole::Assistant)
                        {
                            message.streaming = false;
                        }
                    }
                    self.record_trajectory(&session_id, &event);
                    let metrics = self.session_metrics.entry(session_id.clone()).or_default();
                    match &event {
                        AgentEvent::AgentStart => metrics.turns = metrics.turns.saturating_add(1),
                        AgentEvent::ToolExecutionStart { .. } => {
                            metrics.tool_calls = metrics.tool_calls.saturating_add(1)
                        }
                        AgentEvent::AgentEnd { usage } => metrics.accumulate_usage(usage),
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
                        self.is_generating = false;
                        self.session_status = Some("Reconciling session…".into());
                        self.pending_hydrations.push(SessionHydrationRequest {
                            session_id: session_id.clone(),
                            session_file: session_file.clone(),
                        });
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
        if let Some(session_id) = active_session_id {
            let remaining = deferred.collect::<Vec<_>>();
            if !remaining.is_empty() {
                self.deferred_stream_events.insert(session_id, remaining);
            }
        }
        has_events
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
            crate::services::chat::maybe_generate_session_title(
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
        // persistence; writing it directly here would duplicate it.
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

fn project_recovery_diagnostics(
    lanes: &[threadlane_session::harness::LaneRecoveryDiagnostic],
) -> Vec<TrajectoryEntry> {
    let mut rows = Vec::new();
    for lane in lanes {
        let decision = match lane.decision {
            threadlane_session::harness::RecoveryDecision::None => "No recovery required",
            threadlane_session::harness::RecoveryDecision::ResumeFromLeaf => {
                "Resume interrupted operation from durable leaf"
            }
            threadlane_session::harness::RecoveryDecision::ReplaySafeToolsThenResume => {
                "Replay safe interrupted tools, then resume"
            }
            threadlane_session::harness::RecoveryDecision::AbortUnsafeTool => {
                "Abort interrupted run; unsafe tool cannot be replayed"
            }
            threadlane_session::harness::RecoveryDecision::WaitForDeferredResult => {
                "Wait for deferred provider result"
            }
            threadlane_session::harness::RecoveryDecision::ExplicitRetryRequired => {
                "Keep failed; require explicit retry"
            }
        };
        rows.push(TrajectoryEntry {
            seq: None,
            run_id: lane.open_operation.clone(),
            turn: None,
            category: "Decision".into(),
            summary: format!("{} · {decision}", lane.lane),
            detail: format!(
                "status={:?} attempts={} abort_requested={} leaf={}",
                lane.status,
                lane.attempts,
                lane.abort_requested,
                lane.leaf_id.as_deref().unwrap_or("—")
            ),
            lane: Some(lane.lane.clone()),
            correlation_id: lane.open_operation.clone(),
        });
        for tool in &lane.interrupted_tools {
            rows.push(TrajectoryEntry {
                seq: None,
                run_id: Some(tool.run_id.clone()),
                turn: None,
                category: "Interrupted Tool".into(),
                summary: format!("{} · replay {:?}", tool.name, tool.replay),
                detail: format!(
                    "call={} result_entry={}",
                    tool.call_id, tool.result_entry_id
                ),
                lane: Some(lane.lane.clone()),
                correlation_id: Some(tool.call_id.clone()),
            });
        }
        for queued in &lane.queued_work {
            rows.push(TrajectoryEntry {
                seq: None,
                run_id: lane.open_operation.clone(),
                turn: None,
                category: "Queued Work".into(),
                summary: format!("{:?} · {}", queued.queue, queued.entry_id),
                detail: String::new(),
                lane: Some(lane.lane.clone()),
                correlation_id: Some(queued.entry_id.clone()),
            });
        }
    }
    rows
}

#[cfg(test)]
mod tests {
    use super::*;
    use threadlane_session::harness::{
        OperationIntent, OperationOutcome, ProviderOutcome, Record, SessionStore, TraceString,
    };

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
    fn persisted_thinking_is_attached_to_the_following_assistant() {
        let messages = project_agent_messages(vec![
            AgentMessage::Custom {
                custom_type: "thinking".into(),
                payload: serde_json::json!({"text": "Planning"}),
            },
            AgentMessage::Assistant {
                content: Some("Answer".into()),
                tool_calls: None,
                stop_reason: Some("end_turn".into()),
                deferred_handle: None,
            },
        ]);

        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].content, "Answer");
        assert_eq!(messages[0].reasoning_content.as_deref(), Some("Planning"));
    }

    #[test]
    fn startup_restores_the_most_recent_project_and_its_sessions() {
        let dir = tempfile::tempdir().unwrap();
        let first_project = dir.path().join("first-project");
        let recent_project = dir.path().join("recent-project");
        std::fs::create_dir_all(&first_project).unwrap();
        let session_file = recent_project.join(".threadlane/sessions/recent-session.jsonl");
        std::fs::create_dir_all(session_file.parent().unwrap()).unwrap();
        let mut store = threadlane_session::harness::JsonlStore::open(&session_file).unwrap();
        store
            .append_entry(threadlane_session::harness::Entry {
                id: "node_1".into(),
                parent_id: None,
                lane: "main".into(),
                seq: 1,
                timestamp: 1,
                message: AgentMessage::user("recent prompt", Vec::new()),
                surface_op: threadlane_session::harness::SurfaceOperation::Append,
                terminate: false,
            })
            .unwrap();
        drop(store);

        let state = AppState::load_from_registry(vec![
            AttachedProject {
                id: "first".into(),
                path: first_project,
                name: "first".into(),
                last_selected_task_id: None,
                attached_at: 1,
                last_opened_at: 1,
                last_session_id: None,
            },
            AttachedProject {
                id: "recent".into(),
                path: recent_project.clone(),
                name: "recent".into(),
                last_selected_task_id: None,
                attached_at: 2,
                last_opened_at: 10,
                last_session_id: Some("recent-session".into()),
            },
        ]);

        assert_eq!(
            state.active_work_dir.as_deref(),
            Some(recent_project.as_path())
        );
        assert_eq!(state.active_session_id.as_deref(), Some("recent-session"));
        assert_eq!(
            state
                .projects
                .iter()
                .find(|project| project.work_dir == recent_project)
                .unwrap()
                .sessions
                .len(),
            1
        );
    }

    #[test]
    fn app_state_startup_hydrates_complete_initial_session_history() {
        use threadlane_provider::openai::{ToolCall, ToolCallFunction};
        use threadlane_session::harness::{
            CapabilitySnapshot, OperationIntent, PromptSnapshot, ProviderOutcome, Record,
            SessionStore, TraceString, UsageCause,
        };

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

        let usage = TokenUsage {
            input_tokens: 17,
            output_tokens: 9,
            cache_read_tokens: 4,
            cache_write_tokens: 2,
            total_tokens: 32,
        };
        let mut store = threadlane_session::harness::JsonlStore::open(&session_file).unwrap();
        store
            .append_entry(threadlane_session::harness::Entry {
                id: "node_1".into(),
                parent_id: None,
                lane: "main".into(),
                seq: 1,
                timestamp: 1,
                message: AgentMessage::User {
                    content: "Inspect the project".into(),
                },
                surface_op: threadlane_session::harness::SurfaceOperation::Append,
                terminate: false,
            })
            .unwrap();
        store
            .append_entry(threadlane_session::harness::Entry {
                id: "node_2".into(),
                parent_id: Some("node_1".into()),
                lane: "main".into(),
                seq: 2,
                timestamp: 2,
                message: AgentMessage::Custom {
                    custom_type: "thinking".into(),
                    payload: serde_json::json!({"text": "Reading the relevant files"}),
                },
                surface_op: threadlane_session::harness::SurfaceOperation::Append,
                terminate: false,
            })
            .unwrap();
        store
            .append_entry(threadlane_session::harness::Entry {
                id: "assistant-1".into(),
                parent_id: Some("node_2".into()),
                lane: "main".into(),
                seq: 3,
                timestamp: 3,
                message: AgentMessage::Assistant {
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
                },
                surface_op: threadlane_session::harness::SurfaceOperation::Append,
                terminate: false,
            })
            .unwrap();
        store
            .append_entry(threadlane_session::harness::Entry {
                id: "node_4".into(),
                parent_id: Some("assistant-1".into()),
                lane: "main".into(),
                seq: 4,
                timestamp: 4,
                message: AgentMessage::Tool {
                    tool_call_id: "call-read".into(),
                    name: "read_file".into(),
                    content: "file contents".into(),
                    is_error: false,
                    terminate: false,
                },
                surface_op: threadlane_session::harness::SurfaceOperation::Append,
                terminate: false,
            })
            .unwrap();
        store
            .append_record(Record::OperationStarted {
                id: "run-1".into(),
                seq: 99,
                lane: "main".into(),
                timestamp: 99,
                source_leaf_id: None,
                intent: OperationIntent::Run,
            })
            .unwrap();
        store
            .append_record(Record::StepAttempt {
                id: "attempt-1".into(),
                seq: 100,
                lane: "main".into(),
                timestamp: 100,
                run_id: "run-1".into(),
                attempt: 1,
                result_entry_id: "assistant-1".into(),
                compaction_reason: None,
            })
            .unwrap();
        store
            .append_record(Record::Usage {
                id: "usage-1".into(),
                seq: 101,
                lane: "main".into(),
                timestamp: 101,
                run_id: Some("run-1".into()),
                cause: UsageCause::Provider,
                entry_id: Some("assistant-1".into()),
                tool_call_id: None,
                attempt: Some(1),
                usage: usage.clone(),
            })
            .unwrap();
        store
            .append_record(Record::RunContextCaptured {
                id: "context-1".into(),
                context_window_limit: None,
                route_defaults: None,
                seq: 102,
                lane: "main".into(),
                timestamp: 102,
                run_id: "run-1".into(),
                attempt: None,
                model: TraceString::new("test-model").unwrap(),
                provider: TraceString::new("openai").unwrap(),
                reasoning_effort: ReasoningEffort::Medium,
                prompt_cache_enabled: false,
                work_dir: TraceString::new(work_dir.to_string_lossy()).unwrap(),
                system_prompt: PromptSnapshot::Redacted {
                    sha256: TraceString::new("prompt-sha").unwrap(),
                    byte_len: 128,
                    reason: TraceString::new("test-policy").unwrap(),
                },
                tool_schema_sha256: TraceString::new("tool-sha").unwrap(),
                enabled_tool_names: vec![TraceString::new("read_file").unwrap()],
                capabilities: CapabilitySnapshot {
                    capabilities: vec![TraceString::new("read_file").unwrap()],
                    fingerprint: Some(TraceString::new("capability-sha").unwrap()),
                },
                prompt_template_ids: Vec::new(),
                git_head: None,
            })
            .unwrap();
        store
            .append_record(Record::ProviderRequestStarted {
                id: "provider-start-1".into(),
                seq: 103,
                lane: "main".into(),
                timestamp: 103,
                run_id: "run-1".into(),
                attempt: 1,
                provider: TraceString::new("openai").unwrap(),
                model: TraceString::new("test-model").unwrap(),
                request_id: Some(TraceString::new("request-1").unwrap()),
            })
            .unwrap();
        store
            .append_record(Record::ProviderRequestFinished {
                id: "provider-finish-1".into(),
                seq: 104,
                lane: "main".into(),
                timestamp: 104,
                run_id: "run-1".into(),
                attempt: 1,
                request_id: Some(TraceString::new("request-1").unwrap()),
                outcome: ProviderOutcome::Completed,
                error: None,
                duration_ms: Some(25),
                usage: None,
            })
            .unwrap();
        drop(store);

        let state = AppState::load_from_registry(vec![AttachedProject {
            id: "hydration-test".into(),
            path: work_dir.clone(),
            name: "hydration-test".into(),
            last_selected_task_id: None,
            attached_at: 0,
            last_opened_at: 0,
            last_session_id: Some("hydration-test".into()),
        }]);

        assert_eq!(state.active_session_id.as_deref(), Some("hydration-test"));
        assert_eq!(state.active_work_dir.as_deref(), Some(work_dir.as_path()));
        assert!(!state.is_new_task);
        let messages = &state.messages;

        assert_eq!(messages.len(), 2);
        assert_eq!(
            messages[1].reasoning_content.as_deref(),
            Some("Reading the relevant files")
        );
        assert_eq!(messages[1].content, "The issue is fixed.");
        assert_eq!(messages[1].tool_activities.len(), 1);
        assert_eq!(messages[1].tool_activities[0].id, "call-read");
        assert_eq!(messages[1].tool_activities[0].detail, "file contents");
        assert!(state.trajectory_by_session["hydration-test"]
            .iter()
            .any(|entry| entry.summary == "User input"));
        assert!(state.trajectory_by_session["hydration-test"]
            .iter()
            .any(|entry| entry.summary == "read_file finished"));
        let trace = &state.trajectory_by_session["hydration-test"];
        let context_index = trace
            .iter()
            .position(|entry| entry.category == "Context")
            .unwrap();
        let provider_start_index = trace
            .iter()
            .position(|entry| entry.summary == "openai request started")
            .unwrap();
        let provider_finish_index = trace
            .iter()
            .position(|entry| entry.summary == "Provider request Completed")
            .unwrap();
        assert!(
            context_index < provider_start_index && provider_start_index < provider_finish_index
        );
        assert_eq!(state.session_metrics["hydration-test"].turns, 1);
        assert_eq!(state.session_metrics["hydration-test"].tool_calls, 1);
        let metrics = &state.session_metrics["hydration-test"];
        assert_eq!(metrics.input_tokens, 17);
        assert_eq!(metrics.output_tokens, 9);
        assert_eq!(metrics.cache_read_tokens, 4);
        assert_eq!(metrics.cache_write_tokens, 2);
        assert_eq!(metrics.billed_input_tokens(), 23);
        assert_eq!(metrics.cache_hit_percent(), Some(17));
        assert_eq!(state.current_session_token_usage(), usage);

        drop(state);
        let _ = std::fs::remove_dir_all(work_dir);
    }

    #[test]
    fn durable_projection_restores_ordered_tool_lifecycle_and_exact_usage() {
        use threadlane_provider::openai::{ToolCall, ToolCallFunction};
        use threadlane_session::harness::{
            Entry, OperationIntent, OperationOutcome, Record, SessionStore, ToolReplaySafety,
            UsageCause,
        };

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.jsonl");
        std::fs::write(&path, "").unwrap();
        let mut store = threadlane_session::harness::JsonlStore::open(&path).unwrap();
        store
            .append_record(Record::OperationStarted {
                id: "run-1".into(),
                seq: 1,
                lane: "main".into(),
                timestamp: 1,
                source_leaf_id: None,
                intent: OperationIntent::Run,
            })
            .unwrap();
        store
            .append_entry(Entry {
                id: "user-1".into(),
                parent_id: None,
                lane: "main".into(),
                seq: 2,
                timestamp: 2,
                message: AgentMessage::user("inspect", vec![]),
                surface_op: threadlane_session::harness::SurfaceOperation::Append,
                terminate: false,
            })
            .unwrap();
        store
            .append_record(Record::StepAttempt {
                id: "attempt-1".into(),
                seq: 3,
                lane: "main".into(),
                timestamp: 3,
                run_id: "run-1".into(),
                attempt: 1,
                result_entry_id: "assistant-1".into(),
                compaction_reason: None,
            })
            .unwrap();
        store
            .append_entry(Entry {
                id: "assistant-1".into(),
                parent_id: Some("user-1".into()),
                lane: "main".into(),
                seq: 4,
                timestamp: 4,
                message: AgentMessage::Assistant {
                    content: None,
                    tool_calls: Some(vec![ToolCall {
                        id: "call-1".into(),
                        r#type: "function".into(),
                        function: ToolCallFunction {
                            name: "read_file".into(),
                            arguments: r#"{"path":"src/lib.rs"}"#.into(),
                        },
                        thought_signature: None,
                    }]),
                    stop_reason: None,
                    deferred_handle: None,
                },
                surface_op: threadlane_session::harness::SurfaceOperation::Append,
                terminate: false,
            })
            .unwrap();
        store
            .append_record(Record::ToolStarted {
                id: "tool-start-1".into(),
                seq: 5,
                lane: "main".into(),
                timestamp: 5,
                run_id: "run-1".into(),
                assistant_entry_id: "assistant-1".into(),
                tool_index: 0,
                tool_call_id: "call-1".into(),
                tool_name: "read_file".into(),
                effective_args: serde_json::json!({"path": "src/lib.rs"}),
                result_entry_id: "tool-result-1".into(),
                replay: ToolReplaySafety::Safe,
            })
            .unwrap();
        store
            .append_entry(Entry {
                id: "tool-result-1".into(),
                parent_id: Some("assistant-1".into()),
                lane: "main".into(),
                seq: 6,
                timestamp: 6,
                message: AgentMessage::Tool {
                    tool_call_id: "call-1".into(),
                    name: "read_file".into(),
                    content: "result".into(),
                    is_error: false,
                    terminate: false,
                },
                surface_op: threadlane_session::harness::SurfaceOperation::Append,
                terminate: false,
            })
            .unwrap();
        store
            .append_record(Record::ToolFinished {
                id: "tool-finish-1".into(),
                seq: 7,
                lane: "main".into(),
                timestamp: 7,
                run_id: "run-1".into(),
                tool_call_id: "call-1".into(),
                result_entry_id: "tool-result-1".into(),
                terminate: false,
            })
            .unwrap();
        let usage = TokenUsage {
            input_tokens: 11,
            output_tokens: 7,
            cache_read_tokens: 5,
            cache_write_tokens: 3,
            total_tokens: 26,
        };
        store
            .append_record(Record::Usage {
                id: "usage-1".into(),
                seq: 8,
                lane: "main".into(),
                timestamp: 8,
                run_id: Some("run-1".into()),
                cause: UsageCause::Provider,
                entry_id: Some("assistant-1".into()),
                tool_call_id: None,
                attempt: Some(1),
                usage: usage.clone(),
            })
            .unwrap();
        store
            .append_record(Record::OperationFinished {
                id: "finish-1".into(),
                seq: 9,
                lane: "main".into(),
                timestamp: 9,
                run_id: "run-1".into(),
                outcome: OperationOutcome::Completed,
                error: None,
            })
            .unwrap();
        store
            .append_record(Record::OperationStarted {
                id: "run-2".into(),
                seq: 10,
                lane: "main".into(),
                timestamp: 10,
                source_leaf_id: Some("tool-result-1".into()),
                intent: OperationIntent::Run,
            })
            .unwrap();
        store
            .append_record(Record::StepAttempt {
                id: "attempt-2".into(),
                seq: 11,
                lane: "main".into(),
                timestamp: 11,
                run_id: "run-2".into(),
                attempt: 1,
                result_entry_id: "assistant-2".into(),
                compaction_reason: None,
            })
            .unwrap();
        store
            .append_entry(Entry {
                id: "assistant-2".into(),
                parent_id: Some("tool-result-1".into()),
                lane: "main".into(),
                seq: 12,
                timestamp: 12,
                message: AgentMessage::Assistant {
                    content: None,
                    tool_calls: Some(vec![ToolCall {
                        id: "call-1".into(),
                        r#type: "function".into(),
                        function: ToolCallFunction {
                            name: "write_file".into(),
                            arguments: r#"{"path":"src/new.rs"}"#.into(),
                        },
                        thought_signature: None,
                    }]),
                    stop_reason: None,
                    deferred_handle: None,
                },
                surface_op: threadlane_session::harness::SurfaceOperation::Append,
                terminate: false,
            })
            .unwrap();
        store
            .append_record(Record::ToolStarted {
                id: "tool-start-2".into(),
                seq: 13,
                lane: "main".into(),
                timestamp: 13,
                run_id: "run-2".into(),
                assistant_entry_id: "assistant-2".into(),
                tool_index: 0,
                tool_call_id: "call-1".into(),
                tool_name: "write_file".into(),
                effective_args: serde_json::json!({"path": "src/new.rs"}),
                result_entry_id: "tool-result-2".into(),
                replay: ToolReplaySafety::Never,
            })
            .unwrap();
        store
            .append_entry(Entry {
                id: "tool-result-2".into(),
                parent_id: Some("assistant-2".into()),
                lane: "main".into(),
                seq: 14,
                timestamp: 14,
                message: AgentMessage::Tool {
                    tool_call_id: "call-1".into(),
                    name: "write_file".into(),
                    content: "written".into(),
                    is_error: false,
                    terminate: false,
                },
                surface_op: threadlane_session::harness::SurfaceOperation::Append,
                terminate: false,
            })
            .unwrap();
        store
            .append_record(Record::ToolFinished {
                id: "tool-finish-2".into(),
                seq: 15,
                lane: "main".into(),
                timestamp: 15,
                run_id: "run-2".into(),
                tool_call_id: "call-1".into(),
                result_entry_id: "tool-result-2".into(),
                terminate: false,
            })
            .unwrap();
        store
            .append_record(Record::OperationFinished {
                id: "finish-2".into(),
                seq: 16,
                lane: "main".into(),
                timestamp: 16,
                run_id: "run-2".into(),
                outcome: OperationOutcome::Completed,
                error: None,
            })
            .unwrap();
        drop(store);

        let mut state = AppState::load_from_registry(Vec::new());
        state.hydrate_session_projection("session", &path).unwrap();

        assert_eq!(state.session_token_usage["session"], usage);
        let diagnostics = &state.diagnostics_by_session["session"];
        assert!(!diagnostics.model_context.is_empty());
        assert_eq!(
            diagnostics
                .durable_events
                .iter()
                .map(|event| event.seq)
                .collect::<Vec<_>>(),
            {
                let mut seqs = diagnostics
                    .durable_events
                    .iter()
                    .map(|event| event.seq)
                    .collect::<Vec<_>>();
                seqs.sort_unstable();
                seqs
            }
        );
        assert_eq!(diagnostics.recovery.len(), 1);
        let tool_rows = state.trajectory_by_session["session"]
            .iter()
            .filter(|entry| entry.correlation_id.as_deref() == Some("call-1"))
            .collect::<Vec<_>>();
        assert_eq!(tool_rows.len(), 4);
        assert_eq!(tool_rows[0].seq, Some(4));
        assert_eq!(tool_rows[0].run_id.as_deref(), Some("run-1"));
        assert_eq!(tool_rows[0].summary, "read_file running");
        assert_eq!(tool_rows[1].seq, Some(6));
        assert_eq!(tool_rows[1].run_id.as_deref(), Some("run-1"));
        assert_eq!(tool_rows[1].summary, "read_file finished");
        assert_eq!(tool_rows[2].seq, Some(12));
        assert_eq!(tool_rows[2].run_id.as_deref(), Some("run-2"));
        assert_eq!(tool_rows[2].summary, "write_file running");
        assert_eq!(tool_rows[3].seq, Some(14));
        assert_eq!(tool_rows[3].run_id.as_deref(), Some("run-2"));
        assert_eq!(tool_rows[3].summary, "write_file finished");
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
        let mut store = threadlane_session::harness::JsonlStore::open(&session_file).unwrap();
        store
            .append_entry(threadlane_session::harness::Entry {
                id: "call-1-assistant".into(),
                parent_id: None,
                lane: "main".into(),
                seq: 1,
                timestamp: 1,
                message: AgentMessage::Assistant {
                    content: None,
                    tool_calls: Some(vec![threadlane_provider::openai::ToolCall {
                        id: "call-1".into(),
                        r#type: "function".into(),
                        function: threadlane_provider::openai::ToolCallFunction {
                            name: "read_file".into(),
                            arguments: r#"{"path":"src/lib.rs"}"#.into(),
                        },
                        thought_signature: None,
                    }]),
                    stop_reason: None,
                    deferred_handle: None,
                },
                surface_op: threadlane_session::harness::SurfaceOperation::Append,
                terminate: false,
            })
            .unwrap();
        store
            .append_entry(threadlane_session::harness::Entry {
                id: "call-1-tool".into(),
                parent_id: Some("call-1-assistant".into()),
                lane: "main".into(),
                seq: 2,
                timestamp: 2,
                message: AgentMessage::Tool {
                    tool_call_id: "call-1".into(),
                    name: "read_file".into(),
                    content: "file contents".into(),
                    is_error: false,
                    terminate: false,
                },
                surface_op: threadlane_session::harness::SurfaceOperation::Append,
                terminate: false,
            })
            .unwrap();

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
            vec![
                TrajectoryEntry {
                    seq: None,
                    run_id: None,
                    turn: None,
                    category: "Tool".into(),
                    summary: "read_file running".into(),
                    detail: r#"{"path":"src/lib.rs"}"#.into(),
                    lane: Some("main".into()),
                    correlation_id: Some("call-1".into()),
                },
                TrajectoryEntry {
                    seq: None,
                    run_id: None,
                    turn: None,
                    category: "Tool".into(),
                    summary: "read_file finished".into(),
                    detail: "file contents".into(),
                    lane: Some("main".into()),
                    correlation_id: Some("call-1".into()),
                },
            ],
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
        assert_eq!(trajectory.len(), 2);
        assert_eq!(trajectory[0].summary, "read_file running");
        assert_eq!(trajectory[1].summary, "read_file finished");
        assert!(trajectory.iter().all(|entry| {
            entry.category == "Tool" && entry.correlation_id.as_deref() == Some("call-1")
        }));
    }

    #[test]
    fn durable_trajectory_hydrates_after_session_switch() {
        use threadlane_session::harness::SessionStore;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.jsonl");
        std::fs::write(&path, "").unwrap();
        let store = threadlane_session::harness::JsonlStore::open(&path).unwrap();
        let mut harness = threadlane_session::harness::AgentHarness::new(store);
        harness
            .accept_prompt("run-1", AgentMessage::user("old prompt", vec![]))
            .unwrap();
        harness.drive_to_completion().unwrap();
        let parent_id = harness.store().entries().last().unwrap().id.clone();
        let seq = harness.store().next_sequence();
        harness
            .store_mut()
            .append_entry(threadlane_session::harness::Entry {
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
                surface_op: threadlane_session::harness::SurfaceOperation::Append,
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
        assert_eq!(state.trajectory_by_session["session-a"].len(), 1);
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
    fn stream_drain_preserves_events_beyond_one_frame_budget() {
        let mut state = AppState::load_from_registry(Vec::new());
        state.messages.clear();
        state.active_session_id = Some("session".into());
        state.is_new_task = false;

        for index in 0..130 {
            state
                .stream_tx
                .send(ChatStreamEvent::Agent {
                    session_id: "session".into(),
                    event: AgentEvent::MessageUpdate {
                        text_delta: Some(format!("{index},")),
                        reasoning_delta: None,
                        tool_call_name: None,
                    },
                })
                .unwrap();
        }

        assert!(state.drain_chat_stream());
        assert_eq!(state.messages.len(), 1);
        assert_eq!(state.messages[0].content.matches(',').count(), 128);
        assert!(state.chat_stream_pending());
        assert!(state.drain_chat_stream());
        assert_eq!(state.messages[0].content.matches(',').count(), 130);
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
        let mut store = threadlane_session::harness::JsonlStore::open(&path).unwrap();
        let mut parent_id = None;
        for index in 0..45 {
            let id = format!("node_{index}");
            store
                .append_entry(threadlane_session::harness::Entry {
                    id: id.clone(),
                    parent_id,
                    lane: "main".into(),
                    seq: (index + 1) as u64,
                    timestamp: (index + 1) as u64,
                    message: AgentMessage::User {
                        content: format!("message-{index}"),
                    },
                    surface_op: threadlane_session::harness::SurfaceOperation::Append,
                    terminate: false,
                })
                .unwrap();
            parent_id = Some(id);
        }
        drop(store);

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

    #[test]
    fn startup_hydration_from_project_registry_populates_all_views() {
        let unique = std::time::SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let project_root = std::env::temp_dir().join(format!(
            "threadlane-gpui-startup-test-{}-{unique}",
            std::process::id()
        ));
        let sessions_dir = project_root.join(".threadlane").join("sessions");
        std::fs::create_dir_all(&sessions_dir).unwrap();

        let session_file = sessions_dir.join("session_1001.jsonl");
        let mut store = threadlane_session::harness::JsonlStore::open(&session_file).unwrap();
        store
            .append_entry(threadlane_session::harness::Entry {
                id: "node_1".into(),
                parent_id: None,
                lane: "main".into(),
                seq: 1,
                timestamp: 1,
                message: AgentMessage::User {
                    content: "Hello on startup".into(),
                },
                surface_op: threadlane_session::harness::SurfaceOperation::Append,
                terminate: false,
            })
            .unwrap();
        store
            .append_entry(threadlane_session::harness::Entry {
                id: "node_2".into(),
                parent_id: Some("node_1".into()),
                lane: "main".into(),
                seq: 2,
                timestamp: 2,
                message: AgentMessage::Assistant {
                    content: Some("I am ready".into()),
                    tool_calls: None,
                    stop_reason: None,
                    deferred_handle: None,
                },
                surface_op: threadlane_session::harness::SurfaceOperation::Append,
                terminate: false,
            })
            .unwrap();
        store
            .append_record(Record::OperationStarted {
                id: "run-start-1".into(),
                seq: 10,
                lane: "main".into(),
                timestamp: 10,
                source_leaf_id: None,
                intent: OperationIntent::Run,
            })
            .unwrap();
        store
            .append_record(Record::ProviderRequestFinished {
                id: "finish-1".into(),
                seq: 11,
                lane: "main".into(),
                timestamp: 11,
                run_id: "run-start-1".into(),
                attempt: 1,
                request_id: Some(TraceString::new("req-1").unwrap()),
                outcome: ProviderOutcome::Completed,
                error: None,
                duration_ms: Some(100),
                usage: Some(TokenUsage {
                    total_tokens: 50,
                    input_tokens: 20,
                    output_tokens: 12,
                    cache_read_tokens: 15,
                    cache_write_tokens: 3,
                }),
            })
            .unwrap();

        let mut attached_project = AttachedProject::from_path(project_root.clone());
        attached_project.last_opened_at = 1_000_000;

        let state = AppState::load_from_registry(vec![attached_project]);

        assert_eq!(state.active_session_id.as_deref(), Some("session_1001"));
        assert_eq!(state.messages.len(), 2);
        assert_eq!(state.messages[0].content, "Hello on startup");
        assert_eq!(state.messages[1].content, "I am ready");

        let trajectory = state
            .trajectory_by_session
            .get("session_1001")
            .expect("trajectory must be hydrated on startup");
        assert!(!trajectory.is_empty());
        assert!(trajectory.iter().any(|t| t.category == "Operation"));
        assert!(trajectory.iter().any(|t| t.category == "Provider"));

        let usage = state
            .session_token_usage
            .get("session_1001")
            .expect("token usage must be hydrated on startup");
        assert_eq!(usage.total_tokens, 50);

        let metrics = state
            .session_metrics
            .get("session_1001")
            .expect("session metrics must be hydrated on startup");
        assert_eq!(metrics.input_tokens, 20);
        assert_eq!(metrics.output_tokens, 12);
        assert_eq!(metrics.cache_read_tokens, 15);
        assert_eq!(metrics.cache_write_tokens, 3);
        assert_eq!(metrics.billed_input_tokens(), 38);
        assert_eq!(metrics.cache_hit_percent(), Some(39));

        let _ = std::fs::remove_dir_all(project_root);
    }

    #[test]
    fn branch_consistency_trajectory_is_session_wide_audit_log_while_chat_is_active_branch() {
        let unique = std::time::SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "threadlane-gpui-branch-test-{}-{unique}",
            std::process::id()
        ));
        let path = root.join("session.jsonl");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();

        let mut store = threadlane_session::harness::JsonlStore::open(&path).unwrap();
        store
            .append_entry(threadlane_session::harness::Entry {
                id: "msg-root".into(),
                parent_id: None,
                lane: "main".into(),
                seq: 1,
                timestamp: 1,
                message: AgentMessage::User {
                    content: "Root question".into(),
                },
                surface_op: threadlane_session::harness::SurfaceOperation::Append,
                terminate: false,
            })
            .unwrap();
        store
            .append_entry(threadlane_session::harness::Entry {
                id: "msg-branch-a".into(),
                parent_id: Some("msg-root".into()),
                lane: "main".into(),
                seq: 2,
                timestamp: 2,
                message: AgentMessage::Assistant {
                    content: Some("Branch A answer".into()),
                    tool_calls: None,
                    stop_reason: None,
                    deferred_handle: None,
                },
                surface_op: threadlane_session::harness::SurfaceOperation::Append,
                terminate: false,
            })
            .unwrap();
        store
            .append_entry(threadlane_session::harness::Entry {
                id: "msg-branch-b".into(),
                parent_id: Some("msg-root".into()),
                lane: "main".into(),
                seq: 3,
                timestamp: 3,
                message: AgentMessage::Assistant {
                    content: Some("Branch B alternative answer".into()),
                    tool_calls: None,
                    stop_reason: None,
                    deferred_handle: None,
                },
                surface_op: threadlane_session::harness::SurfaceOperation::Append,
                terminate: false,
            })
            .unwrap();
        store
            .append_record(Record::OperationStarted {
                id: "run-branch-a".into(),
                seq: 1,
                lane: "main".into(),
                timestamp: 1,
                source_leaf_id: None,
                intent: OperationIntent::Run,
            })
            .unwrap();
        store
            .append_record(Record::OperationFinished {
                id: "finish-branch-a".into(),
                seq: 2,
                lane: "main".into(),
                timestamp: 2,
                run_id: "run-branch-a".into(),
                outcome: OperationOutcome::Completed,
                error: None,
            })
            .unwrap();
        store
            .append_record(Record::OperationStarted {
                id: "run-branch-b".into(),
                seq: 3,
                lane: "main".into(),
                timestamp: 3,
                source_leaf_id: None,
                intent: OperationIntent::Run,
            })
            .unwrap();
        store
            .append_record(Record::OperationFinished {
                id: "finish-branch-b".into(),
                seq: 4,
                lane: "main".into(),
                timestamp: 4,
                run_id: "run-branch-b".into(),
                outcome: OperationOutcome::Completed,
                error: None,
            })
            .unwrap();

        let mut state = AppState::load_from_registry(Vec::new());
        state
            .hydrate_session_projection("branch-session", &path)
            .unwrap();

        let branch_messages = store.active_branch_messages("main");
        assert_eq!(branch_messages.len(), 2);
        assert!(matches!(
            &branch_messages[0],
            AgentMessage::User { content } if content == "Root question"
        ));
        assert!(matches!(
            &branch_messages[1],
            AgentMessage::Assistant { content, .. } if content.as_deref() == Some("Branch B alternative answer")
        ));

        let trajectory = state.trajectory_by_session.get("branch-session").unwrap();
        assert!(trajectory
            .iter()
            .any(|t| t.run_id.as_deref() == Some("run-branch-a")));
        assert!(trajectory
            .iter()
            .any(|t| t.run_id.as_deref() == Some("run-branch-b")));

        let _ = std::fs::remove_dir_all(root);
    }
}
