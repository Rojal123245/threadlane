use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicU64};
use std::sync::Arc;
use threadlane_agent::AgentEvent;
use threadlane_agent::harness::HarnessEventHub;
use threadlane_agent::SubagentRecoveryStatus;
use tokio::sync::broadcast;

use super::AgentWork;
use crate::wasi_extension::WasiExtensionManager;

pub(crate) static NEXT_SUBAGENT_UI_RUN_ID: AtomicU64 = AtomicU64::new(1);

pub(crate) type AgentRunner = Arc<
    dyn Fn(
            Vec<super::AgentRunTask>,
            bool,
            Option<String>,
        ) -> Pin<Box<dyn Future<Output = Result<serde_json::Value, String>> + Send>>
        + Send
        + Sync,
>;

#[cfg(test)]
pub(crate) type AgentWorkObserver = Arc<std::sync::Mutex<Vec<AgentWork>>>;
#[cfg(test)]
pub(crate) type SubagentObserverState = Arc<std::sync::Mutex<Option<AgentWorkObserver>>>;
#[cfg(test)]
pub(crate) type SubagentBoundaryObserver = Arc<dyn Fn() + Send + Sync>;

#[derive(Clone)]
pub(crate) struct SubagentRunContext {
    pub(crate) api_key: String,
    pub(crate) account_id: Option<String>,
    pub(crate) parent_model: String,
    pub(crate) parent_session_id: String,
    pub(crate) work_dir: PathBuf,
    pub(crate) extensions: Arc<WasiExtensionManager>,
    pub(crate) parent_event_tx: broadcast::Sender<AgentEvent>,
    pub(crate) parent_leaf_id: Option<String>,
    pub(crate) session_file: Option<PathBuf>,
    #[cfg(test)]
    pub(crate) scheduler_observer: Option<AgentWorkObserver>,
    #[cfg(test)]
    pub(crate) child_work_observer: Option<SubagentBoundaryObserver>,
    #[cfg(test)]
    pub(crate) child_tool_observer: Option<Arc<AtomicBool>>,
    pub(crate) semaphore: Arc<tokio::sync::Semaphore>,
}

#[derive(Debug, Clone)]
pub(crate) struct CompletedSubagentLane {
    pub(crate) lane: String,
    pub(crate) run_id: String,
    pub(crate) status: SubagentRecoveryStatus,
    pub(crate) summary: Option<String>,
}

#[derive(Default)]
pub(crate) struct InterruptedSubagentRecoveryState {
    pub(crate) recovered_lanes: Vec<CompletedSubagentLane>,
}

#[derive(Debug, Clone)]
pub(crate) struct SubagentLaneIdentity {
    pub(crate) lane_name: String,
    pub(crate) run_id: String,
    pub(crate) source_leaf_id: Option<String>,
    pub(crate) started_seq: u64,
}

#[derive(Debug, Clone)]
pub(crate) struct SubagentStartError {
    pub(crate) identity: Option<SubagentLaneIdentity>,
    pub(crate) error: String,
}

pub fn cancel_open_subagent_operations(session_file: &std::path::Path) -> Result<usize, String> {
    let mut journal = super::harness_journal::HarnessJournal::open(session_file)?;
    let snapshot = threadlane_agent::harness::Reducer::reduce(journal.store.store())
        .map_err(|error| error.to_string())?;

    let main_lane_name = "main";
    let open_subagents: Vec<(String, String)> = snapshot
        .lanes
        .iter()
        .filter(|lane| lane.name != main_lane_name)
        .filter_map(|lane| {
            lane.open_operation
                .as_ref()
                .map(|run_id| (lane.name.clone(), run_id.clone()))
        })
        .collect();

    let count = open_subagents.len();
    for (lane_name, run_id) in open_subagents {
        let is_already_requested = snapshot
            .lane(&lane_name)
            .is_some_and(|lane| lane.abort_requested);

        if !is_already_requested {
            let _ = journal.store.request_abort(&run_id);
            let _ = journal.store.drive_to_completion();
        }

        let _ = journal.store.finish_operation(
            &run_id,
            threadlane_agent::harness::OperationOutcome::Aborted,
            Some("Parent task finished".into()),
        );
        let _ = journal.store.drive_to_completion();
    }

    Ok(count)
}
