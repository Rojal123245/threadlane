//! Harness-aware session runtimes for the GPUI frontend.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use threadlane_session::{
    CodingAgent, CodingAgentCancellation, CodingAgentOptions, CodingAgentWorkHandle,
    PermissionDecision, PermissionHandle,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SessionRuntimeStatus {
    Ready,
    Working,
    Interrupted,
    Error(String),
}

pub struct SessionRuntime {
    pub(crate) agent: Arc<tokio::sync::Mutex<CodingAgent>>,
    pub(crate) cancellation: CodingAgentCancellation,
    pub(crate) work_handle: CodingAgentWorkHandle,
    permission_handle: PermissionHandle,
    pub(crate) session_file: PathBuf,
    pub(crate) selected_model: String,
    pub(crate) system_prompt: String,
    pub(crate) harness_error: Option<String>,
    is_generating: AtomicBool,
    status: Mutex<SessionRuntimeStatus>,
}

impl SessionRuntime {
    pub(crate) fn new(options: CodingAgentOptions) -> Arc<Self> {
        let session_file = options
            .session_file
            .clone()
            .expect("GPUI session runtimes require a durable session file");
        let agent = CodingAgent::new(options);
        let cancellation = agent.cancellation_handle();
        let work_handle = agent.work_handle();
        let permission_handle = agent.permission_handle();
        permission_handle.set_interactive(true);
        let system_prompt = agent.system_prompt_snapshot().unwrap_or_default();
        let harness_error = agent.harness_error().map(str::to_owned);
        let status = if let Some(error) = &harness_error {
            SessionRuntimeStatus::Error(error.clone())
        } else if agent.has_interrupted_work() {
            SessionRuntimeStatus::Interrupted
        } else {
            SessionRuntimeStatus::Ready
        };

        let selected_model = agent.model().to_string();

        Arc::new(Self {
            agent: Arc::new(tokio::sync::Mutex::new(agent)),
            cancellation,
            work_handle,
            permission_handle,
            session_file,
            selected_model,
            system_prompt,
            harness_error,
            is_generating: AtomicBool::new(false),
            status: Mutex::new(status),
        })
    }

    pub(crate) fn is_generating(&self) -> bool {
        self.is_generating.load(Ordering::SeqCst)
    }

    pub(crate) fn status(&self) -> SessionRuntimeStatus {
        self.status
            .lock()
            .map(|status| status.clone())
            .unwrap_or_else(|_| SessionRuntimeStatus::Error("Session runtime unavailable".into()))
    }

    pub(crate) fn begin_generation(&self) -> Result<(), String> {
        self.is_generating
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .map_err(|_| "A generation is already running for this session".to_string())?;
        if let Ok(mut status) = self.status.lock() {
            *status = SessionRuntimeStatus::Working;
        }
        Ok(())
    }

    pub(crate) async fn set_model_roles(&self, roles: threadlane_session::ModelRoles) {
        let mut agent = self.agent.lock().await;
        agent.set_model_roles(roles);
    }

    pub(crate) fn finish_generation(&self, error: Option<String>) {
        self.is_generating.store(false, Ordering::SeqCst);
        if let Ok(mut status) = self.status.lock() {
            *status = error
                .map(SessionRuntimeStatus::Error)
                .unwrap_or(SessionRuntimeStatus::Ready);
        }
    }

    pub(crate) fn resolve_permission(
        &self,
        request_id: &str,
        decision: PermissionDecision,
    ) -> bool {
        self.permission_handle.resolve(request_id, decision)
    }

    pub(crate) fn cancel(&self) -> Result<(), String> {
        self.cancellation.cancel()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn runtime_opens_harness_in_the_canonical_session_file() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let work_dir = std::env::temp_dir().join(format!("threadlane-gpui-runtime-{unique}"));
        let session_file = work_dir.join(".threadlane/sessions/session.jsonl");
        let runtime = SessionRuntime::new(CodingAgentOptions {
            api_key: "test-key".into(),
            account_id: None,
            model: "gpt-4o".into(),
            work_dir: work_dir.clone(),
            session_file: Some(session_file.clone()),
            system_prompt: Default::default(),
            agent_config: None,
            coding_config: None,
        });

        assert_eq!(runtime.session_file, session_file);
        assert!(runtime.session_file.exists());
        assert!(runtime.system_prompt.contains("Current working directory:"));
        assert!(!runtime
            .session_file
            .with_file_name("session.harness.jsonl")
            .exists());

        drop(runtime);
        let _ = std::fs::remove_dir_all(work_dir);
    }
}
