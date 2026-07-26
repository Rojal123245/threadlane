//! Shared GUI state & background task event types.
//!
//! Panel-specific state slices live in `crate::panels::<panel>::state`.

use std::collections::HashMap;
use std::path::PathBuf;
use threadlane_agent::AgentEvent;
use threadlane_coding_agent::{
    CapabilityCatalog, ExtensionRecord, ExtensionScope, TaskAgentEvent,
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

#[derive(Clone)]
pub struct CapabilityExtensionRow {
    pub id: String,
    pub name: String,
    pub version: String,
    pub module_path: PathBuf,
    pub scope: ExtensionScope,
    pub enabled: bool,
    pub effective: bool,
}

impl CapabilityExtensionRow {
    pub fn scope_status(&self) -> String {
        let scope = match self.scope {
            ExtensionScope::Global => "Global",
            ExtensionScope::Project => "Project",
        };
        let status = if !self.enabled {
            "Disabled"
        } else if self.effective {
            "Active"
        } else {
            "Overridden"
        };
        format!("{scope} · {status}")
    }

    pub fn matches_record(&self, record: &ExtensionRecord) -> bool {
        record.id() == self.id
            && record.name() == self.name
            && record.version() == self.version
            && record.scope() == self.scope
            && record.module_path() == self.module_path
    }
}

#[derive(Default)]
pub struct CapabilityState {
    pub extensions: Vec<CapabilityExtensionRow>,
}

impl CapabilityState {
    pub fn refresh(&mut self, catalog: &CapabilityCatalog) {
        self.refresh_records(catalog.extensions());
    }

    fn refresh_records(&mut self, extensions: &[ExtensionRecord]) {
        self.extensions = extensions
            .iter()
            .map(|extension| CapabilityExtensionRow {
                id: extension.id().to_owned(),
                name: extension.name().to_owned(),
                version: extension.version().to_owned(),
                module_path: extension.module_path().to_path_buf(),
                scope: extension.scope(),
                enabled: extension.is_enabled(),
                effective: extension.is_effective(),
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
    ExtensionFilePicked {
        path: Option<PathBuf>,
        scope: ExtensionScope,
    },
    ExtensionReloadCompleted {
        reload_id: u64,
        reloaded: usize,
        failures: Vec<String>,
    },
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
    use threadlane_coding_agent::ExtensionManager;

    fn push_unsigned_leb(mut value: u32, bytes: &mut Vec<u8>) {
        loop {
            let mut byte = (value & 0x7f) as u8;
            value >>= 7;
            if value != 0 {
                byte |= 0x80;
            }
            bytes.push(byte);
            if value == 0 {
                break;
            }
        }
    }

    fn push_signed_leb(mut value: i64, bytes: &mut Vec<u8>) {
        loop {
            let byte = (value as u8) & 0x7f;
            value >>= 7;
            let done = (value == 0 && byte & 0x40 == 0)
                || (value == -1 && byte & 0x40 != 0);
            bytes.push(if done { byte } else { byte | 0x80 });
            if done {
                break;
            }
        }
    }

    fn push_section(wasm: &mut Vec<u8>, id: u8, payload: &[u8]) {
        wasm.push(id);
        push_unsigned_leb(payload.len() as u32, wasm);
        wasm.extend_from_slice(payload);
    }

    fn manifest_wasm(name: &str, version: &str) -> Vec<u8> {
        let manifest = format!(
            r#"{{"api_version":1,"name":"{name}","version":"{version}","description":"test fixture"}}"#
        );
        let mut wasm = b"\0asm\x01\0\0\0".to_vec();
        push_section(&mut wasm, 1, &[1, 0x60, 0, 1, 0x7e]);
        push_section(&mut wasm, 3, &[1, 0]);
        push_section(&mut wasm, 5, &[1, 0, 1]);

        let mut exports = vec![2];
        push_unsigned_leb("extension_info".len() as u32, &mut exports);
        exports.extend_from_slice(b"extension_info");
        exports.extend_from_slice(&[0, 0]);
        push_unsigned_leb("memory".len() as u32, &mut exports);
        exports.extend_from_slice(b"memory");
        exports.extend_from_slice(&[2, 0]);
        push_section(&mut wasm, 7, &exports);

        let mut body = vec![0, 0x42];
        push_signed_leb(manifest.len() as i64, &mut body);
        body.push(0x0b);
        let mut code = vec![1];
        push_unsigned_leb(body.len() as u32, &mut code);
        code.extend_from_slice(&body);
        push_section(&mut wasm, 10, &code);

        let mut data = vec![1, 0, 0x41, 0, 0x0b];
        push_unsigned_leb(manifest.len() as u32, &mut data);
        data.extend_from_slice(manifest.as_bytes());
        push_section(&mut wasm, 11, &data);
        wasm
    }

    #[test]
    fn capability_refresh_projects_both_scopes_and_runtime_state() {
        let root = std::env::temp_dir().join(format!(
            "threadlane-capability-state-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let global_threadlane = root.join("global-threadlane");
        let project = root.join("project");
        fs::create_dir_all(&project).unwrap();
        let global_source = root.join("global.wasm");
        let project_source = root.join("project.wasm");
        fs::write(&global_source, manifest_wasm("shared_ext", "1.0.0")).unwrap();
        fs::write(&project_source, manifest_wasm("shared_ext", "2.0.0")).unwrap();

        let manager =
            ExtensionManager::new(Some(global_threadlane.clone()), Some(project.clone()));
        manager
            .install_from_wasm(&global_source, ExtensionScope::Global)
            .unwrap();
        manager
            .install_from_wasm(&project_source, ExtensionScope::Project)
            .unwrap();
        let global_threadlane = global_threadlane.canonicalize().unwrap();
        let project = project.canonicalize().unwrap();

        let mut capabilities = CapabilityState::default();
        let records = manager.discover();
        capabilities.refresh_records(&records);

        let global = capabilities
            .extensions
            .iter()
            .find(|row| row.scope == ExtensionScope::Global)
            .unwrap();
        assert_eq!(global.id, "shared_ext");
        assert_eq!(global.name, "shared_ext");
        assert_eq!(global.version, "1.0.0");
        assert_eq!(
            global.module_path,
            global_threadlane.join("extensions/shared_ext.wasm")
        );
        assert!(global.enabled);
        assert!(!global.effective);
        assert_eq!(global.scope_status(), "Global · Overridden");

        let project_row = capabilities
            .extensions
            .iter()
            .find(|row| row.scope == ExtensionScope::Project)
            .unwrap();
        assert_eq!(
            project_row.module_path,
            project.join(".threadlane/extensions/shared_ext.wasm")
        );
        assert_eq!(project_row.version, "2.0.0");
        assert!(project_row.enabled);
        assert!(project_row.effective);
        assert_eq!(project_row.scope_status(), "Project · Active");

        let project_record = records
            .iter()
            .find(|record| record.scope() == ExtensionScope::Project)
            .unwrap();
        manager.set_enabled(project_record, false).unwrap();
        capabilities.refresh_records(&manager.discover());
        let project_row = capabilities
            .extensions
            .iter()
            .find(|row| row.scope == ExtensionScope::Project)
            .unwrap();
        assert_eq!(project_row.scope_status(), "Project · Disabled");
        let global = capabilities
            .extensions
            .iter()
            .find(|row| row.scope == ExtensionScope::Global)
            .unwrap();
        assert_eq!(global.scope_status(), "Global · Active");

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn visible_extension_identity_rejects_replaced_manifest() {
        let root = std::env::temp_dir().join(format!(
            "threadlane-extension-identity-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let project = root.join("project");
        fs::create_dir_all(&project).unwrap();
        let source = root.join("identity.wasm");
        fs::write(&source, manifest_wasm("identity_ext", "1.0.0")).unwrap();
        let manager = ExtensionManager::new(None, Some(project));
        manager
            .install_from_wasm(&source, ExtensionScope::Project)
            .unwrap();
        let record = manager.discover().into_iter().next().unwrap();
        let visible = CapabilityExtensionRow {
            id: record.id().to_owned(),
            name: record.name().to_owned(),
            version: record.version().to_owned(),
            module_path: record.module_path().to_path_buf(),
            scope: record.scope(),
            enabled: record.is_enabled(),
            effective: record.is_effective(),
        };

        assert!(visible.matches_record(&record));

        let mut replaced_name = visible.clone();
        replaced_name.name = "replacement_ext".to_owned();
        assert!(!replaced_name.matches_record(&record));

        let mut replaced_version = visible;
        replaced_version.version = "2.0.0".to_owned();
        assert!(!replaced_version.matches_record(&record));

        fs::remove_dir_all(root).unwrap();
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
