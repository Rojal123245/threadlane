use crate::coding_agent::{CodingAgent, CodingAgentOptions};
use crate::packages::ExtensionScope;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use threadlane_agent::AgentEvent;
use tokio::sync::broadcast;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TaskStatus {
    Idle,
    Running,
    Waiting,
    Completed,
    Failed,
    Cancelled,
    Interrupted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TaskKind {
    Background,
    Subagent,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectRecord {
    pub id: String,
    path: PathBuf,
    name: String,
    last_selected_task_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskRecord {
    pub id: String,
    pub project_id: String,
    pub session_id: String,
    pub session_file: Option<PathBuf>,
    pub parent_task_id: Option<String>,
    pub kind: TaskKind,
    pub agent: String,
    pub summary: String,
    pub current_activity: Option<String>,
    pub status: TaskStatus,
    pub started_at_ms: u128,
    pub finished_at_ms: Option<u128>,
}

impl TaskRecord {
    pub fn cancellable(&self) -> bool {
        self.kind == TaskKind::Background && self.active()
    }

    fn active(&self) -> bool {
        matches!(
            self.status,
            TaskStatus::Idle | TaskStatus::Running | TaskStatus::Waiting
        )
    }
}

#[derive(Debug, Clone)]
pub struct TaskAgentEvent {
    task_id: String,
    project_id: String,
    event: AgentEvent,
}

impl TaskAgentEvent {
    pub fn task_id(&self) -> &str {
        &self.task_id
    }

    pub fn project_id(&self) -> &str {
        &self.project_id
    }

    pub fn into_parts(self) -> (String, String, AgentEvent) {
        (self.task_id, self.project_id, self.event)
    }
}

struct TaskRuntime {
    agent: Arc<tokio::sync::Mutex<CodingAgent>>,
    status: TaskStatus,
    prompt_lock: Arc<tokio::sync::Mutex<()>>,
    run_handle: Option<tokio::task::AbortHandle>,
}

pub struct HarnessSupervisor {
    global_dir: PathBuf,
    projects: Arc<Mutex<HashMap<String, ProjectRecord>>>,
    tasks: Arc<Mutex<HashMap<String, TaskRecord>>>,
    runtimes: Arc<Mutex<HashMap<String, TaskRuntime>>>,
    event_tx: broadcast::Sender<TaskAgentEvent>,
}

impl HarnessSupervisor {
    pub fn new(global_dir: PathBuf) -> Self {
        let (event_tx, _) = broadcast::channel(1000);
        let _ = fs::create_dir_all(&global_dir);
        let supervisor = Self {
            global_dir,
            projects: Arc::new(Mutex::new(HashMap::new())),
            tasks: Arc::new(Mutex::new(HashMap::new())),
            runtimes: Arc::new(Mutex::new(HashMap::new())),
            event_tx,
        };
        supervisor.load_registry();
        supervisor
    }

    pub fn subscribe(&self) -> broadcast::Receiver<TaskAgentEvent> {
        self.event_tx.subscribe()
    }

    fn registry_file(&self) -> PathBuf {
        self.global_dir.join("projects.json")
    }

    fn load_registry(&self) {
        let file = self.registry_file();
        if file.exists() {
            if let Ok(contents) = fs::read_to_string(&file) {
                if let Ok(records) = serde_json::from_str::<Vec<ProjectRecord>>(&contents) {
                    let mut lock = self.projects.lock().unwrap();
                    for rec in records {
                        lock.insert(rec.id.clone(), rec);
                    }
                }
            }
        }
    }

    fn save_registry(&self) {
        let records: Vec<ProjectRecord> = self.projects.lock().unwrap().values().cloned().collect();
        let file = self.registry_file();
        if let Ok(json) = serde_json::to_string_pretty(&records) {
            let tmp = file.with_extension("json.tmp");
            if fs::write(&tmp, json).is_ok() {
                let _ = fs::rename(tmp, file);
            }
        }
    }

    pub fn register_project(&self, raw_path: &Path) -> Result<ProjectRecord, String> {
        let canonical = raw_path.canonicalize().map_err(|e| {
            format!(
                "Failed to canonicalize project path '{}': {e}",
                raw_path.display()
            )
        })?;

        let id = md5_hash(&canonical.to_string_lossy());
        let name = canonical
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| "project".into());

        let record = ProjectRecord {
            id: id.clone(),
            path: canonical,
            name,
            last_selected_task_id: None,
        };

        {
            let mut lock = self.projects.lock().unwrap();
            lock.insert(id.clone(), record.clone());
        }
        self.save_registry();
        Ok(record)
    }

    pub fn create_task(
        &self,
        project_id: &str,
        session_file: Option<PathBuf>,
        options: CodingAgentOptions,
    ) -> Result<String, String> {
        let project = {
            let lock = self.projects.lock().unwrap();
            lock.get(project_id)
                .cloned()
                .ok_or_else(|| format!("Project ID '{project_id}' not found"))?
        };

        static TASK_COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        let count = TASK_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let task_id = format!(
            "task-{}-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis(),
            count
        );

        let final_session_file = session_file.unwrap_or_else(|| {
            project
                .path
                .join(format!(".threadlane/sessions/{}.jsonl", task_id))
        });

        let mut opts = options;
        opts.work_dir = project.path.clone();
        opts.session_file = Some(final_session_file.clone());

        let coding_agent = CodingAgent::new(opts);
        let rx = coding_agent.subscribe();

        let agent_arc = Arc::new(tokio::sync::Mutex::new(coding_agent));
        let task_record = TaskRecord {
            id: task_id.clone(),
            project_id: project_id.to_string(),
            session_id: final_session_file
                .file_stem()
                .map(|stem| stem.to_string_lossy().into_owned())
                .unwrap_or_else(|| task_id.clone()),
            session_file: Some(final_session_file.clone()),
            parent_task_id: None,
            kind: TaskKind::Background,
            agent: "task".to_owned(),
            summary: String::new(),
            current_activity: None,
            status: TaskStatus::Idle,
            started_at_ms: now_ms(),
            finished_at_ms: None,
        };

        let runtime = TaskRuntime {
            agent: agent_arc,
            status: TaskStatus::Idle,
            prompt_lock: Arc::new(tokio::sync::Mutex::new(())),
            run_handle: None,
        };

        {
            let mut t_lock = self.tasks.lock().unwrap();
            t_lock.insert(task_id.clone(), task_record);

            let mut r_lock = self.runtimes.lock().unwrap();
            r_lock.insert(task_id.clone(), runtime);

            let mut p_lock = self.projects.lock().unwrap();
            if let Some(p) = p_lock.get_mut(project_id) {
                p.last_selected_task_id = Some(task_id.clone());
            }
        }
        self.save_registry();

        let event_tx = self.event_tx.clone();
        let tasks = self.tasks.clone();
        let runtimes = self.runtimes.clone();
        let tid = task_id.clone();
        let pid = project_id.to_string();
        let session_id = final_session_file
            .file_stem()
            .map(|stem| stem.to_string_lossy().into_owned())
            .unwrap_or_else(|| tid.clone());
        let event_session_file = final_session_file.clone();
        tokio::spawn(async move {
            let mut sub_rx = rx;
            while let Ok(evt) = sub_rx.recv().await {
                let status = match &evt {
                    AgentEvent::AgentStart => Some(TaskStatus::Running),
                    AgentEvent::AgentEnd { .. } => Some(TaskStatus::Completed),
                    AgentEvent::AgentError { .. } => Some(TaskStatus::Failed),
                    _ => None,
                };
                {
                    let mut task_records = tasks.lock().unwrap();
                    if let Some(task) = task_records.get_mut(&tid) {
                        apply_background_event(task, &evt);
                    }
                    apply_subagent_event(
                        &mut task_records,
                        &pid,
                        &session_id,
                        Some(&event_session_file),
                        Some(&tid),
                        &evt,
                    );
                }
                if let Some(status) = status {
                    if let Some(runtime) = runtimes.lock().unwrap().get_mut(&tid) {
                        runtime.status = status;
                    }
                }
                let _ = event_tx.send(TaskAgentEvent {
                    task_id: tid.clone(),
                    project_id: pid.clone(),
                    event: evt,
                });
            }
        });

        Ok(task_id)
    }

    pub async fn reload_extensions(
        &self,
        scope: ExtensionScope,
        project_root: Option<&Path>,
    ) -> Result<usize, String> {
        let target_project_id = if scope == ExtensionScope::Project {
            let project_root = project_root
                .ok_or_else(|| "Project extension reload requires a project".to_owned())?;
            let canonical = project_root
                .canonicalize()
                .unwrap_or_else(|_| project_root.to_path_buf());
            self.projects
                .lock()
                .unwrap()
                .values()
                .find(|project| project.path == canonical)
                .map(|project| project.id.clone())
        } else {
            None
        };

        let task_projects: HashMap<String, String> = self
            .tasks
            .lock()
            .unwrap()
            .iter()
            .map(|(task_id, task)| (task_id.clone(), task.project_id.clone()))
            .collect();
        let targets: Vec<_> = self
            .runtimes
            .lock()
            .unwrap()
            .iter()
            .filter(|(task_id, _)| {
                scope == ExtensionScope::Global
                    || target_project_id
                        .as_ref()
                        .is_some_and(|project_id| task_projects.get(*task_id) == Some(project_id))
            })
            .map(|(task_id, runtime)| {
                (
                    task_id.clone(),
                    runtime.prompt_lock.clone(),
                    runtime.agent.clone(),
                )
            })
            .collect();

        let mut reloaded = 0;
        let mut failures = Vec::new();
        for (task_id, prompt_lock, agent) in targets {
            let _prompt_guard = prompt_lock.lock().await;
            let mut agent = agent.lock().await;
            match agent.reload_extensions().await {
                Ok(_) => reloaded += 1,
                Err(error) => failures.push(format!("{task_id}: {error}")),
            }
        }

        if failures.is_empty() {
            Ok(reloaded)
        } else {
            Err(failures.join("; "))
        }
    }

    pub fn submit_input(&self, task_id: &str, prompt: String) -> Result<(), String> {
        let (agent_arc, prompt_lock) = {
            let runtimes = self.runtimes.lock().unwrap();
            let rt = runtimes
                .get(task_id)
                .ok_or_else(|| format!("Task ID '{task_id}' not found"))?;
            (rt.agent.clone(), rt.prompt_lock.clone())
        };

        if let Some(task) = self.tasks.lock().unwrap().get_mut(task_id) {
            task.summary = prompt.clone();
        }
        self.update_task_status(task_id, TaskStatus::Running);

        let tid = task_id.to_string();
        let tasks_map = self.tasks.clone();
        let runtimes_map = self.runtimes.clone();

        let handle = tokio::spawn(async move {
            let _guard = prompt_lock.lock().await;
            let mut agent = agent_arc.lock().await;
            let _ = agent.handle_input(&prompt).await;

            let mut t_lock = tasks_map.lock().unwrap();
            if let Some(tr) = t_lock.get_mut(&tid) {
                if tr.status == TaskStatus::Running {
                    tr.status = TaskStatus::Completed;
                    tr.current_activity = None;
                    tr.finished_at_ms = Some(now_ms());
                }
            }
            let mut r_lock = runtimes_map.lock().unwrap();
            if let Some(rt) = r_lock.get_mut(&tid) {
                if rt.status == TaskStatus::Running {
                    rt.status = TaskStatus::Completed;
                }
                rt.run_handle = None;
            }
        });
        if let Some(runtime) = self.runtimes.lock().unwrap().get_mut(task_id) {
            runtime.run_handle = Some(handle.abort_handle());
        }

        Ok(())
    }

    pub fn cancel_task(&self, task_id: &str) -> Result<(), String> {
        if let Some(handle) = self
            .runtimes
            .lock()
            .unwrap()
            .get_mut(task_id)
            .and_then(|runtime| runtime.run_handle.take())
        {
            handle.abort();
        }
        let finished_at_ms = now_ms();
        let mut tasks = self.tasks.lock().unwrap();
        let Some(task) = tasks.get_mut(task_id) else {
            return Err(format!("Task ID '{task_id}' not found"));
        };
        task.status = TaskStatus::Cancelled;
        task.current_activity = None;
        task.finished_at_ms = Some(finished_at_ms);
        for child in tasks.values_mut() {
            if child.parent_task_id.as_deref() == Some(task_id) && child.active() {
                child.status = TaskStatus::Cancelled;
                child.current_activity = None;
                child.finished_at_ms = Some(finished_at_ms);
            }
        }
        drop(tasks);
        if let Some(runtime) = self.runtimes.lock().unwrap().get_mut(task_id) {
            runtime.status = TaskStatus::Cancelled;
        }
        Ok(())
    }

    fn update_task_status(&self, task_id: &str, status: TaskStatus) {
        let mut t_lock = self.tasks.lock().unwrap();
        if let Some(tr) = t_lock.get_mut(task_id) {
            tr.status = status;
            if matches!(
                status,
                TaskStatus::Completed
                    | TaskStatus::Failed
                    | TaskStatus::Cancelled
                    | TaskStatus::Interrupted
            ) {
                tr.finished_at_ms = Some(now_ms());
            }
        }
        let mut r_lock = self.runtimes.lock().unwrap();
        if let Some(rt) = r_lock.get_mut(task_id) {
            rt.status = status;
        }
    }

    pub fn get_task_status(&self, task_id: &str) -> Option<TaskStatus> {
        let lock = self.tasks.lock().unwrap();
        lock.get(task_id).map(|t| t.status)
    }

    pub fn list_tasks_for_project(&self, project_id: &str) -> Vec<TaskRecord> {
        let lock = self.tasks.lock().unwrap();
        let mut tasks = lock
            .values()
            .filter(|t| t.project_id == project_id)
            .cloned()
            .collect::<Vec<_>>();
        tasks.sort_by(|left, right| {
            right
                .started_at_ms
                .cmp(&left.started_at_ms)
                .then_with(|| left.id.cmp(&right.id))
        });
        tasks
    }

    pub fn observe_session_event(
        &self,
        project_id: &str,
        session_id: &str,
        session_file: Option<&Path>,
        event: &AgentEvent,
    ) -> bool {
        apply_subagent_event(
            &mut self.tasks.lock().unwrap(),
            project_id,
            session_id,
            session_file,
            None,
            event,
        )
    }

    pub fn finish_session_tasks(&self, project_id: &str, session_id: &str) -> bool {
        let mut changed = false;
        let finished_at_ms = now_ms();
        for task in self.tasks.lock().unwrap().values_mut() {
            if task.project_id == project_id
                && task.session_id == session_id
                && task.kind == TaskKind::Subagent
                && task.active()
            {
                task.status = TaskStatus::Cancelled;
                task.current_activity = None;
                task.finished_at_ms = Some(finished_at_ms);
                changed = true;
            }
        }
        changed
    }
}

fn now_ms() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}

fn child_task_id(tool_call_id: &str) -> Option<String> {
    let tagged = tool_call_id.strip_prefix("subagent-")?;
    let mut parts = tagged.splitn(3, ':');
    let run_id = parts.next()?.parse::<u64>().ok()?;
    let task_index = parts.next()?.parse::<usize>().ok()?;
    parts.next()?;
    Some(format!("subagent-{run_id}:{task_index}"))
}

fn apply_background_event(task: &mut TaskRecord, event: &AgentEvent) -> bool {
    match event {
        AgentEvent::AgentStart => {
            task.status = TaskStatus::Running;
            true
        }
        AgentEvent::AgentEnd { .. } => {
            task.status = TaskStatus::Completed;
            task.current_activity = None;
            task.finished_at_ms = Some(now_ms());
            true
        }
        AgentEvent::AgentError { error } => {
            task.status = TaskStatus::Failed;
            task.current_activity = Some(error.clone());
            task.finished_at_ms = Some(now_ms());
            true
        }
        AgentEvent::ToolExecutionStart {
            tool_call_id, name, ..
        } if child_task_id(tool_call_id).is_none() => {
            task.current_activity = Some(name.clone());
            true
        }
        AgentEvent::ToolExecutionEnd { tool_call_id, .. }
            if child_task_id(tool_call_id).is_none() =>
        {
            task.current_activity = None;
            true
        }
        _ => false,
    }
}

fn apply_subagent_event(
    tasks: &mut HashMap<String, TaskRecord>,
    project_id: &str,
    session_id: &str,
    session_file: Option<&Path>,
    parent_task_id: Option<&str>,
    event: &AgentEvent,
) -> bool {
    match event {
        AgentEvent::SubagentQueued {
            run_id,
            task_index,
            agent,
            task,
        } => {
            let id = format!("subagent-{run_id}:{task_index}");
            tasks.insert(
                id.clone(),
                TaskRecord {
                    id,
                    project_id: project_id.to_owned(),
                    session_id: session_id.to_owned(),
                    session_file: session_file.map(Path::to_path_buf),
                    parent_task_id: parent_task_id.map(str::to_owned),
                    kind: TaskKind::Subagent,
                    agent: agent.clone(),
                    summary: task.clone(),
                    current_activity: None,
                    status: TaskStatus::Idle,
                    started_at_ms: now_ms(),
                    finished_at_ms: None,
                },
            );
            true
        }
        AgentEvent::SubagentStarted {
            run_id,
            task_index,
        } => {
            let id = format!("subagent-{run_id}:{task_index}");
            let Some(task) = tasks.get_mut(&id) else {
                return false;
            };
            task.status = TaskStatus::Running;
            true
        }
        AgentEvent::ToolExecutionStart {
            tool_call_id, name, ..
        } => {
            let Some(id) = child_task_id(tool_call_id) else {
                return false;
            };
            let Some(task) = tasks.get_mut(&id) else {
                return false;
            };
            task.current_activity = Some(name.clone());
            true
        }
        AgentEvent::ToolExecutionEnd { tool_call_id, .. } => {
            let Some(id) = child_task_id(tool_call_id) else {
                return false;
            };
            let Some(task) = tasks.get_mut(&id) else {
                return false;
            };
            task.current_activity = None;
            true
        }
        AgentEvent::SubagentFinished {
            run_id,
            task_index,
            succeeded,
            error,
        } => {
            let id = format!("subagent-{run_id}:{task_index}");
            let Some(task) = tasks.get_mut(&id) else {
                return false;
            };
            task.status = if *succeeded {
                TaskStatus::Completed
            } else {
                TaskStatus::Failed
            };
            task.current_activity = error.clone();
            task.finished_at_ms = Some(now_ms());
            true
        }
        _ => false,
    }
}

fn md5_hash(input: &str) -> String {
    format!("{:x}", md5::compute(input))
}

#[cfg(test)]
mod tests {
    use super::*;
    use threadlane_agent::TokenUsage;

    #[test]
    fn subagent_lifecycle_is_tracked_under_its_session() {
        let mut tasks = HashMap::new();
        let session_file = PathBuf::from("/repo/.threadlane/sessions/chat.jsonl");
        assert!(apply_subagent_event(
            &mut tasks,
            "project-1",
            "chat",
            Some(&session_file),
            None,
            &AgentEvent::SubagentQueued {
                run_id: 7,
                task_index: 1,
                agent: "reviewer".into(),
                task: "Review the patch".into(),
            },
        ));
        let task = &tasks["subagent-7:1"];
        assert_eq!(task.kind, TaskKind::Subagent);
        assert_eq!(task.project_id, "project-1");
        assert_eq!(task.session_id, "chat");
        assert_eq!(task.agent, "reviewer");
        assert_eq!(task.summary, "Review the patch");
        assert_eq!(task.status, TaskStatus::Idle);
        assert!(!task.cancellable());

        assert!(apply_subagent_event(
            &mut tasks,
            "project-1",
            "chat",
            Some(&session_file),
            None,
            &AgentEvent::SubagentStarted {
                run_id: 7,
                task_index: 1,
            },
        ));
        assert_eq!(tasks["subagent-7:1"].status, TaskStatus::Running);
    }

    #[test]
    fn child_tool_events_update_only_the_matching_subagent() {
        let mut tasks = HashMap::new();
        let session_file = PathBuf::from("/repo/.threadlane/sessions/chat.jsonl");
        for task_index in 0..2 {
            apply_subagent_event(
                &mut tasks,
                "project-1",
                "chat",
                Some(&session_file),
                None,
                &AgentEvent::SubagentQueued {
                    run_id: 8,
                    task_index,
                    agent: format!("worker-{task_index}"),
                    task: format!("Task {task_index}"),
                },
            );
        }
        apply_subagent_event(
            &mut tasks,
            "project-1",
            "chat",
            Some(&session_file),
            None,
            &AgentEvent::ToolExecutionStart {
                tool_call_id: "subagent-8:1:read-1".into(),
                name: "read_file".into(),
                arguments: r#"{"path":"src/lib.rs"}"#.into(),
            },
        );
        assert_eq!(tasks["subagent-8:0"].current_activity, None);
        assert_eq!(
            tasks["subagent-8:1"].current_activity.as_deref(),
            Some("read_file")
        );
    }

    #[test]
    fn subagent_finish_records_failure() {
        let mut tasks = HashMap::new();
        apply_subagent_event(
            &mut tasks,
            "project-1",
            "chat",
            None,
            Some("task-parent"),
            &AgentEvent::SubagentQueued {
                run_id: 9,
                task_index: 0,
                agent: "worker".into(),
                task: "Implement".into(),
            },
        );
        apply_subagent_event(
            &mut tasks,
            "project-1",
            "chat",
            None,
            Some("task-parent"),
            &AgentEvent::SubagentFinished {
                run_id: 9,
                task_index: 0,
                succeeded: false,
                error: Some("provider failed".into()),
            },
        );
        let task = &tasks["subagent-9:0"];
        assert_eq!(task.parent_task_id.as_deref(), Some("task-parent"));
        assert_eq!(task.status, TaskStatus::Failed);
        assert_eq!(task.current_activity.as_deref(), Some("provider failed"));
        assert!(task.finished_at_ms.is_some());
    }

    #[test]
    fn background_task_tracks_current_tool_and_completion() {
        let mut task = TaskRecord {
            id: "task-1".into(),
            project_id: "project-1".into(),
            session_id: "task-1".into(),
            session_file: None,
            parent_task_id: None,
            kind: TaskKind::Background,
            agent: "task".into(),
            summary: "Run checks".into(),
            current_activity: None,
            status: TaskStatus::Running,
            started_at_ms: 1,
            finished_at_ms: None,
        };
        assert!(apply_background_event(
            &mut task,
            &AgentEvent::ToolExecutionStart {
                tool_call_id: "command-1".into(),
                name: "run_command".into(),
                arguments: r#"{"command":"cargo check"}"#.into(),
            },
        ));
        assert_eq!(task.current_activity.as_deref(), Some("run_command"));
        assert!(apply_background_event(
            &mut task,
            &AgentEvent::AgentEnd {
                usage: TokenUsage::default(),
            },
        ));
        assert_eq!(task.status, TaskStatus::Completed);
        assert!(task.finished_at_ms.is_some());
    }

    #[test]
    fn unfinished_session_subagents_are_cancelled_when_parent_run_ends() {
        let dir = tempfile::tempdir().unwrap();
        let supervisor = HarnessSupervisor::new(dir.path().to_path_buf());
        supervisor.observe_session_event(
            "project-1",
            "chat",
            None,
            &AgentEvent::SubagentQueued {
                run_id: 10,
                task_index: 0,
                agent: "worker".into(),
                task: "Inspect".into(),
            },
        );
        assert!(supervisor.finish_session_tasks("project-1", "chat"));
        let task = supervisor
            .list_tasks_for_project("project-1")
            .into_iter()
            .find(|task| task.id == "subagent-10:0")
            .unwrap();
        assert_eq!(task.status, TaskStatus::Cancelled);
        assert!(task.finished_at_ms.is_some());
    }
}
