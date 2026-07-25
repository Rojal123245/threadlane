use crate::coding_agent::{CodingAgent, CodingAgentOptions};
use threadlane_agent::AgentEvent;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
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
    project_id: String,
    pub session_file: PathBuf,
    status: TaskStatus,
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
            session_file: final_session_file.clone(),
            status: TaskStatus::Idle,
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
        tokio::spawn(async move {
            let mut sub_rx = rx;
            while let Ok(evt) = sub_rx.recv().await {
                let status = match &evt {
                    AgentEvent::AgentStart => Some(TaskStatus::Running),
                    AgentEvent::AgentEnd { .. } => Some(TaskStatus::Completed),
                    AgentEvent::AgentError { .. } => Some(TaskStatus::Failed),
                    _ => None,
                };
                if let Some(status) = status {
                    if let Some(task) = tasks.lock().unwrap().get_mut(&tid) {
                        task.status = status;
                    }
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

    pub fn submit_input(&self, task_id: &str, prompt: String) -> Result<(), String> {
        let (agent_arc, prompt_lock) = {
            let runtimes = self.runtimes.lock().unwrap();
            let rt = runtimes
                .get(task_id)
                .ok_or_else(|| format!("Task ID '{task_id}' not found"))?;
            (rt.agent.clone(), rt.prompt_lock.clone())
        };

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
                    tr.status = TaskStatus::Idle;
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
        if !self.tasks.lock().unwrap().contains_key(task_id) {
            return Err(format!("Task ID '{task_id}' not found"));
        }
        if let Some(handle) = self
            .runtimes
            .lock()
            .unwrap()
            .get_mut(task_id)
            .and_then(|runtime| runtime.run_handle.take())
        {
            handle.abort();
        }
        self.update_task_status(task_id, TaskStatus::Cancelled);
        Ok(())
    }

    fn update_task_status(&self, task_id: &str, status: TaskStatus) {
        let mut t_lock = self.tasks.lock().unwrap();
        if let Some(tr) = t_lock.get_mut(task_id) {
            tr.status = status;
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
        lock.values()
            .filter(|t| t.project_id == project_id)
            .cloned()
            .collect()
    }
}

fn md5_hash(input: &str) -> String {
    format!("{:x}", md5::compute(input))
}
