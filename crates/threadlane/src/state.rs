//! Shared GUI state & background task event types.
//!
//! Panel-specific state slices live in `crate::panels::<panel>::state`.

use std::collections::HashMap;
use std::path::PathBuf;
use threadlane_agent::AgentEvent;
use threadlane_coding_agent::{
    CapabilityCatalog, PackageScope, TaskAgentEvent,
};

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
    pub scope: PackageScope,
}

pub struct CapabilityExtensionRow {
    pub package_id: Option<String>,
    pub full_trust: bool,
    pub enabled: bool,
    pub revision: Option<String>,
    pub trusted: bool,
}

#[derive(Default)]
pub struct CapabilityState {
    pub packages: Vec<CapabilityPackageRow>,
    pub extensions: Vec<CapabilityExtensionRow>,
}

impl CapabilityState {
    pub fn refresh(&mut self, catalog: &CapabilityCatalog) {
        self.packages = catalog
            .packages()
            .iter()
            .map(|package| CapabilityPackageRow {
                id: package.id().to_owned(),
                name: package.name().to_owned(),
                scope: package.scope(),
            })
            .collect();
        self.extensions = catalog
            .extensions()
            .iter()
            .map(|extension| CapabilityExtensionRow {
                package_id: extension.package_id().map(str::to_owned),
                full_trust: extension.is_full_trust(),
                enabled: extension.is_enabled(),
                revision: extension.revision().map(str::to_owned),
                trusted: extension.is_trusted(),
            })
            .collect();
    }

    pub fn mark_revoked(&mut self, package_id: &str) {
        for extension in &mut self.extensions {
            if extension.full_trust && extension.package_id.as_deref() == Some(package_id) {
                extension.enabled = false;
                extension.trusted = false;
            }
        }
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

    #[test]
    fn capability_revoke_and_background_completion_update_state() {
        let mut capabilities = CapabilityState {
            extensions: vec![CapabilityExtensionRow {
                package_id: Some("example".into()),
                full_trust: true,
                enabled: true,
                revision: Some("revision".into()),
                trusted: true,
            }],
            ..Default::default()
        };
        capabilities.mark_revoked("example");
        assert!(!capabilities.extensions[0].enabled);
        assert!(!capabilities.extensions[0].trusted);

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
