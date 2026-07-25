//! Shared GUI state & background task event types.
//!
//! Panel-specific state slices live in `crate::panels::<panel>::state`.

use std::collections::HashMap;
use std::path::PathBuf;
use threadlane_agent::AgentEvent;
use threadlane_coding_agent::{CapabilityCatalog, TaskAgentEvent};

pub use crate::panels::chat::*;
pub use crate::panels::command_palette::*;

pub use crate::panels::sessions::*;
pub use crate::path_utils::truncate_chars;

#[derive(Default)]
pub struct BackgroundTaskState {
    by_project: HashMap<PathBuf, HashMap<String, BackgroundTaskStatus>>,
}

#[derive(Clone, Copy)]
enum BackgroundTaskStatus {
    Running,
    Completed,
    Failed,
}

impl BackgroundTaskState {
    pub fn started(&mut self, task_id: String, work_dir: PathBuf) {
        self.by_project
            .entry(work_dir)
            .or_default()
            .insert(task_id, BackgroundTaskStatus::Running);
    }

    pub fn apply_agent_event(&mut self, task_id: &str, event: &AgentEvent) {
        let status = match event {
            AgentEvent::AgentStart => BackgroundTaskStatus::Running,
            AgentEvent::AgentEnd { .. } => BackgroundTaskStatus::Completed,
            AgentEvent::AgentError { .. } => BackgroundTaskStatus::Failed,
            _ => return,
        };
        for tasks in self.by_project.values_mut() {
            if let Some(current) = tasks.get_mut(task_id) {
                *current = status;
                return;
            }
        }
    }

    pub fn summary(&self) -> String {
        let mut total = 0;
        let mut running = 0;
        for tasks in self.by_project.values() {
            total += tasks.len();
            running += tasks
                .values()
                .filter(|status| matches!(status, BackgroundTaskStatus::Running))
                .count();
        }
        format!("{total} tasks · {running} running")
    }
}

pub struct CapabilityPackageRow {
    pub id: String,
    pub name: String,
    #[allow(dead_code)]
    pub module_path: PathBuf,
    #[allow(dead_code)]
    pub enabled: bool,
}

#[derive(Default)]
pub struct CapabilityState {
    pub packages: Vec<CapabilityPackageRow>,
}

impl CapabilityState {
    pub fn refresh(&mut self, catalog: &CapabilityCatalog) {
        self.packages = catalog
            .packages()
            .iter()
            .map(|package| CapabilityPackageRow {
                id: package.id().to_owned(),
                name: package.name().to_owned(),
                module_path: package.module_path().to_path_buf(),
                enabled: package.is_enabled(),
            })
            .collect();
    }
}

/// Events sent from background tokio tasks to the UI thread.
pub enum GuiAgentEvent {
    GenerationAgent {
        generation_id: u64,
        work_dir: PathBuf,
        session_id: String,
        event: AgentEvent,
    },
    DeviceCodePrompt {
        user_code: String,
        url: String,
    },
    DeviceLoginSuccess,
    DeviceLoginError(String),
    SessionTitleGenerated,
    AvailableModelsLoaded(Vec<String>),
    ProjectFolderPicked(Result<Option<PathBuf>, String>),
    PackageFolderPicked(Option<PathBuf>),
    BackgroundTask(TaskAgentEvent),
    CommandOutput {
        generation_id: u64,
        work_dir: PathBuf,
        session_id: String,
        output: String,
    },
    GenerationFinished {
        generation_id: u64,
        work_dir: PathBuf,
        session_id: String,
    },
    AntigravityLoginSuccess {
        email: Option<String>,
    },
    AntigravityLoginError(String),
    AntigravityDoctorReport(String),
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn capability_refresh_copies_project_wasi_package_state() {
        let project = std::env::temp_dir().join(format!(
            "threadlane-capability-state-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let package = project.join(".threadlane/extensions/test-extension");
        fs::create_dir_all(&package).unwrap();
        fs::write(
            package.join("threadlane-package.json"),
            r#"{
                "id": "test-extension",
                "name": "Test Extension",
                "version": "1.0.0",
                "description": "test fixture",
                "extension": "extension.wasm"
            }"#,
        )
        .unwrap();
        let module_path = package.join("extension.wasm");
        fs::write(&module_path, b"test wasm").unwrap();

        let catalog = CapabilityCatalog::discover(Some(&project));
        let mut capabilities = CapabilityState::default();
        capabilities.refresh(&catalog);
        assert_eq!(capabilities.packages.len(), 1);
        assert_eq!(capabilities.packages[0].id, "test-extension");
        assert_eq!(capabilities.packages[0].name, "Test Extension");
        assert_eq!(capabilities.packages[0].module_path, module_path);
        assert!(capabilities.packages[0].enabled);
        fs::remove_dir_all(project).unwrap();
    }

    #[test]
    fn background_completion_updates_state() {
        let mut tasks = BackgroundTaskState::default();
        tasks.started("task-1".into(), PathBuf::from("/project"));
        assert_eq!(tasks.summary(), "1 tasks · 1 running");
        tasks.apply_agent_event(
            "task-1",
            &AgentEvent::AgentEnd {
                usage: Default::default(),
            },
        );
        assert_eq!(tasks.summary(), "1 tasks · 0 running");
    }
}
