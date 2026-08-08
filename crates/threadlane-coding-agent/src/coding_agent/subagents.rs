use std::fmt;
use std::path::PathBuf;

use async_trait::async_trait;

use threadlane_agent::harness::{HookKind, StreamingState};
use threadlane_agent::{AgentMessage, TokenUsage};

use super::harness::CodingSessionHarness;

// ── Child-lane identity ────────────────────────────────────────────

/// Deterministic identity for a child subagent lane, derived from the
/// parent session ID and the tool-call ID that spawned the subagent.
///
/// Every child lane uses a canonical lane name and run identifier
/// anchored to the parent session, ensuring consistent recovery and
/// event correlation.
#[derive(Debug, Clone)]
pub(crate) struct SubagentLaneIdentity {
    /// The lane name within the parent session (e.g. `subagent-task-1`).
    pub(crate) lane_name: String,
    /// The run identifier for this subagent's operation.
    pub(crate) run_id: String,
    /// The entry id in the parent lane that the subagent result will
    /// attach to, if known at construction time.
    pub(crate) source_leaf_id: Option<String>,
    /// The sequence number at which the subagent lane was first recorded.
    pub(crate) started_seq: u64,
}

impl SubagentLaneIdentity {
    /// Build a deterministic lane identity from a parent session id and a
    /// tool-call id.  The lane name is `subagent-<tool_call_id>` and the
    /// run id is `subagent-<parent_session_id>-<tool_call_id>`.
    pub(crate) fn from_parent_tool_call(
        parent_session_id: &str,
        tool_call_id: &str,
    ) -> Self {
        let lane_name = format!("subagent-{tool_call_id}");
        let run_id = format!("subagent-{parent_session_id}-{tool_call_id}");
        Self {
            lane_name,
            run_id,
            source_leaf_id: None,
            started_seq: 0,
        }
    }

    /// Attach the parent-lane leaf id for event correlation.
    pub(crate) fn with_source_leaf(mut self, leaf_id: String) -> Self {
        self.source_leaf_id = Some(leaf_id);
        self
    }

    /// Record the sequence number at which this subagent lane was started.
    pub(crate) fn with_started_seq(mut self, seq: u64) -> Self {
        self.started_seq = seq;
        self
    }
}

// ── Journal adapter ────────────────────────────────────────────────

/// Lane-aware journal adapter for subagent child processes.
///
/// Every write is routed through [`CodingSessionHarness`], which is the
/// canonical session store path for the parent session.  There is no
/// separate sidecar or direct-append path.
pub(crate) struct SubagentJournalAdapter {
    session_file: PathBuf,
    lane: String,
    run_id: String,
}

impl SubagentJournalAdapter {
    /// Create a journal adapter that routes all writes for `lane` / `run_id`
    /// through the canonical [`CodingSessionHarness`] for `session_file`.
    pub(crate) fn new(session_file: PathBuf, lane: String, run_id: String) -> Self {
        Self {
            session_file,
            lane,
            run_id,
        }
    }

    /// Open the canonical harness and route a message to the child lane.
    fn with_harness<F, T>(&self, f: F) -> Result<T, String>
    where
        F: FnOnce(&mut CodingSessionHarness) -> Result<T, String>,
    {
        let mut harness = CodingSessionHarness::open(&self.session_file)?;
        f(&mut harness)
    }
}

#[async_trait]
impl threadlane_agent::journal::AgentJournal for SubagentJournalAdapter {
    async fn record_assistant_message(&self, message: AgentMessage) -> Result<(), String> {
        self.with_harness(|harness| {
            harness
                .append_message_to_lane(&self.lane, &self.run_id, message)
                .map(|_| ())
        })
    }

    async fn record_tool_message(&self, message: AgentMessage) -> Result<(), String> {
        self.with_harness(|harness| {
            harness.append_message_to_lane(&self.lane, &self.run_id, message.clone())?;
            harness.finish_tool_message(&self.run_id, &message)
        })
    }

    async fn record_tool_intent(
        &self,
        tool_call_id: &str,
        tool_name: &str,
        arguments: &str,
    ) -> Result<(), String> {
        let effective_args = serde_json::from_str(arguments)
            .map_err(|e| format!("invalid tool arguments: {e}"))?;
        self.with_harness(|harness| {
            harness.tool_started_on_lane(
                &self.lane,
                &self.run_id,
                tool_call_id,
                tool_name,
                effective_args,
            )
        })
    }

    async fn record_tool_completion(
        &self,
        _tool_call_id: &str,
        _terminate: bool,
    ) -> Result<(), String> {
        Ok(())
    }

    async fn record_provider_usage(&self, usage: TokenUsage) -> Result<(), String> {
        self.with_harness(|harness| {
            harness.record_provider_usage(&self.run_id, usage)
        })
    }

    async fn record_discarded_usage(&self, usage: TokenUsage) -> Result<(), String> {
        self.with_harness(|harness| {
            harness.record_discarded_usage(&self.run_id, usage)
        })
    }

    async fn record_streaming_state(&self, _state: StreamingState) -> Result<(), String> {
        Ok(())
    }

    async fn run_provider_hook(&self, _kind: HookKind) -> Result<(), String> {
        Ok(())
    }
}

impl fmt::Debug for SubagentJournalAdapter {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SubagentJournalAdapter")
            .field("lane", &self.lane)
            .field("run_id", &self.run_id)
            .finish_non_exhaustive()
    }
}

// ── Child-lane lifecycle helpers ───────────────────────────────────

/// Accept a child prompt on a subagent lane through the canonical harness.
pub(crate) fn accept_child_prompt(
    session_file: &std::path::Path,
    lane: &str,
    run_id: &str,
    prompt: AgentMessage,
) -> Result<(), String> {
    let mut harness = CodingSessionHarness::open(session_file)?;
    harness.append_message_to_lane(lane, run_id, prompt).map(|_| ())
}

/// Checkpoint a subagent lane by persisting the last-turn messages through
/// the canonical harness.  Returns the leaf entry id after the checkpoint.
pub(crate) fn checkpoint_child_lane(
    session_file: &std::path::Path,
    lane: &str,
    run_id: &str,
    messages: &[AgentMessage],
) -> Result<String, String> {
    let mut harness = CodingSessionHarness::open(session_file)?;
    let mut last_id = String::new();
    for message in messages {
        last_id = harness.append_message_to_lane(lane, run_id, message.clone())?;
    }
    Ok(last_id)
}

/// Claim safe tool replays for a subagent lane through the canonical harness.
pub(crate) fn claim_safe_replays_for_lane(
    session_file: &std::path::Path,
    tools: &[threadlane_agent::harness::Record],
) -> Result<Vec<threadlane_agent::harness::Record>, String> {
    let mut harness = CodingSessionHarness::open(session_file)?;
    harness.claim_safe_replays(tools)
}

/// Finish a subagent lane with the given outcome through the canonical harness.
pub(crate) fn finish_subagent_lane(
    session_file: &std::path::Path,
    run_id: &str,
    outcome: threadlane_agent::harness::OperationOutcome,
    error: Option<String>,
) -> Result<(), String> {
    let mut harness = CodingSessionHarness::open(session_file)?;
    harness.finish_run(run_id, outcome, error)
}

/// Abort open subagent operations through the canonical harness.
/// Returns the count of aborted subagent lanes.
pub(crate) fn abort_open_subagent_operations(
    session_file: &std::path::Path,
) -> Result<usize, String> {
    let mut harness = CodingSessionHarness::open(session_file)?;
    // request_abort marks all open lanes, then we count non-main lanes that were open.
    harness.request_abort()?;
    let snapshot = harness.snapshot()?;
    let count = snapshot
        .state
        .lanes
        .iter()
        .filter(|lane| lane.name != "main" && lane.abort_requested)
        .count();
    Ok(count)
}

/// Recover subagent lane records from the canonical store into a form
/// suitable for resumption.  Returns the set of non-main-lane records
/// that represent recoverable child-lane state.
pub(crate) fn recover_subagent_records(
    session_file: &std::path::Path,
) -> Result<Vec<threadlane_agent::harness::Record>, String> {
    let store = threadlane_agent::harness::JsonlStore::open(session_file)
        .map_err(|e| e.to_string())?;
    let records: Vec<threadlane_agent::harness::Record> = store
        .records()
        .iter()
        .filter(|r| r.lane() != "main")
        .cloned()
        .collect();
    Ok(records)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// SubagentLaneIdentity produces deterministic lane/run names.
    #[test]
    fn lane_identity_from_parent_tool_call() {
        let id = SubagentLaneIdentity::from_parent_tool_call("sess-001", "call-42");
        assert_eq!(id.lane_name, "subagent-call-42");
        assert_eq!(id.run_id, "subagent-sess-001-call-42");
        assert!(id.source_leaf_id.is_none());
        assert_eq!(id.started_seq, 0);
    }

    #[test]
    fn lane_identity_builder_methods() {
        let id = SubagentLaneIdentity::from_parent_tool_call("sess-001", "call-42")
            .with_source_leaf("entry-abc".into())
            .with_started_seq(5);
        assert_eq!(id.lane_name, "subagent-call-42");
        assert_eq!(id.source_leaf_id, Some("entry-abc".into()));
        assert_eq!(id.started_seq, 5);
    }
}
