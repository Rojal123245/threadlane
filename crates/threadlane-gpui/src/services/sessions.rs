//! Harness-aware session runtimes for the GPUI frontend.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use threadlane_agent::AgentMessage;
use threadlane_coding_agent::{
    CodingAgent, CodingAgentCancellation, CodingAgentOptions, CodingAgentWorkHandle,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SessionRuntimeStatus {
    Ready,
    Working,
    Interrupted,
    Error(String),
}

pub struct SessionRuntime {
    pub agent: Arc<tokio::sync::Mutex<CodingAgent>>,
    pub cancellation: CodingAgentCancellation,
    pub work_handle: CodingAgentWorkHandle,
    pub session_file: PathBuf,
    pub selected_model: String,
    pub initial_messages: Vec<AgentMessage>,
    is_generating: AtomicBool,
    status: Mutex<SessionRuntimeStatus>,
}

impl SessionRuntime {
    pub fn new(options: CodingAgentOptions) -> Arc<Self> {
        let session_file = options
            .session_file
            .clone()
            .expect("GPUI session runtimes require a durable session file");
        let requested_model = options.model.clone();
        let agent = CodingAgent::new(options);
        let cancellation = agent.cancellation_handle();
        let work_handle = agent.work_handle();
        let initial_messages = {
            let branch = agent.session_tree.get_active_branch_messages();
            if branch.is_empty() {
                agent.session_tree.get_persisted_messages()
            } else {
                branch
            }
        };
        let status = if let Some(error) = agent.harness_error() {
            SessionRuntimeStatus::Error(error.to_owned())
        } else if agent.has_interrupted_work() {
            SessionRuntimeStatus::Interrupted
        } else {
            SessionRuntimeStatus::Ready
        };

        let selected_model = agent.session_tree.model.clone().unwrap_or(requested_model);

        Arc::new(Self {
            agent: Arc::new(tokio::sync::Mutex::new(agent)),
            cancellation,
            work_handle,
            session_file,
            selected_model,
            initial_messages,
            is_generating: AtomicBool::new(false),
            status: Mutex::new(status),
        })
    }

    pub fn is_generating(&self) -> bool {
        self.is_generating.load(Ordering::SeqCst)
    }

    pub fn status(&self) -> SessionRuntimeStatus {
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

    pub(crate) fn finish_generation(&self, error: Option<String>) {
        self.is_generating.store(false, Ordering::SeqCst);
        if let Ok(mut status) = self.status.lock() {
            *status = error
                .map(SessionRuntimeStatus::Error)
                .unwrap_or(SessionRuntimeStatus::Ready);
        }
    }

    pub fn cancel(&self) -> Result<(), String> {
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
        assert!(!runtime
            .session_file
            .with_file_name("session.harness.jsonl")
            .exists());

        drop(runtime);
        let _ = std::fs::remove_dir_all(work_dir);
    }
}
