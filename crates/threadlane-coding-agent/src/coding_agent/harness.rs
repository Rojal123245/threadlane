use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::Value;

use threadlane_agent::harness::{
    AgentHarness, DeferredResolution, EffectAction, Entry as HarnessEntry,
    EventPayload, HarnessEventHub, HookContext, HookKind, HookRegistry,
    JsonlStore, OperationOutcome, ProvisionedEntry,
    QueueKind, Record as HarnessRecord, Reducer, ReduceError, RetryPolicy,
    SessionIdGenerator, SessionStore, Snapshot, Subscription,
    ToolReplaySafety as HarnessToolReplaySafety, ToolResult as HarnessToolResult,
    ToolSpec,
};
use threadlane_agent::{
    AgentMessage, AgentToolResult, TokenUsage,
};
use threadlane_agent::session_tree::SessionTree;

use super::harness_journal::{
    harness_cancellation_state, harness_event_hub, harness_hook_registry,
    HarnessWatch,
};
/// Owns the durable session store, the `main` lane handle, event hub, hook
/// registry, cancellation state, and a subscription for event projection.
/// Every foreground operation enters the harness through this adapter;
/// there is no second persistence path.
pub(crate) struct CodingSessionHarness {
    pub(crate) store: AgentHarness<JsonlStore>,
    pub(crate) session_path: PathBuf,
    pub(crate) main_lane_name: String,
    pub(crate) events: HarnessEventHub,
    pub(crate) hooks: HookRegistry,
    pub(crate) cancellation: Arc<AtomicBool>,
}

impl CodingSessionHarness {
    // ── Construction ──────────────────────────────────────────────────

    /// Open or create the JSONL session at `path` and build a canonical
    /// harness adapter.
    pub(crate) fn open(path: &Path) -> Result<Self, String> {
        if !path.exists() {
            fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
                .map_err(|error| error.to_string())?;
        }
        let events = harness_event_hub(path);
        let hooks = harness_hook_registry(path);
        let persist_path = path.to_path_buf();
        let persist_events = events.clone();
        let executor = move |action: EffectAction| {
            let mut store = JsonlStore::open(&persist_path)
                .map_err(|error| ReduceError::Storage(error.to_string()))?;
            if let Err(error) = action.apply(&mut store) {
                persist_events.publish(EventPayload::Fault(error.to_string()));
                return Err(error);
            }
            let (payload, lane, run_id, turn) = match &action {
                EffectAction::AppendEntry { entry } => (
                    EventPayload::EntryCommitted(entry.clone()),
                    Some(entry.lane.clone()),
                    None,
                    None,
                ),
                EffectAction::AppendRecord { record, .. } => (
                    EventPayload::RecordCommitted(record.clone()),
                    Some(record.lane().to_owned()),
                    record.run_id().map(str::to_owned),
                    record.turn(),
                ),
            };
            persist_events.publish_identified_with_turn(payload, lane, run_id, turn, None);
            Ok(())
        };
        let cancellation = harness_cancellation_state(path);
        let store = JsonlStore::open(path)
            .map(|store| {
                AgentHarness::with_executor_and_hooks(store, events.clone(), executor, hooks.clone())
            })
            .map_err(|error| error.to_string())?;
        Ok(Self {
            store,
            session_path: path.to_path_buf(),
            main_lane_name: "main".into(),
            events,
            hooks,
            cancellation,
        })
    }

    // ── Run lifecycle ─────────────────────────────────────────────────

    /// Start a foreground operation and accept the user prompt.
    ///
    /// Returns `Ok(())` after `accept_prompt` is driven to completion
    /// (committed to the JSONL store).
    pub(crate) fn begin_run(
        &mut self,
        run_id: &str,
        prompt: AgentMessage,
    ) -> Result<(), String> {
        self.refresh()?;
        self.store
            .accept_prompt(run_id, prompt)
            .map_err(|error| error.to_string())?;
        self.store
            .drive_to_completion()
            .map_err(|error| error.to_string())
    }

    /// Accept a prompt on the main lane without starting a new operation.
    pub(crate) fn accept_prompt(
        &mut self,
        run_id: &str,
        prompt: AgentMessage,
    ) -> Result<(), String> {
        self.refresh()?;
        self.store
            .accept_prompt(run_id, prompt)
            .map_err(|error| error.to_string())?;
        self.store
            .drive_to_completion()
            .map_err(|error| error.to_string())
    }

    /// Finish an operation with the given outcome and optional error.
    pub(crate) fn finish_run(
        &mut self,
        run_id: &str,
        outcome: OperationOutcome,
        error: Option<String>,
    ) -> Result<(), String> {
        self.store
            .finish_operation(run_id, outcome, error)
            .map_err(|error| error.to_string())?;
        self.store
            .drive_to_completion()
            .map_err(|error| error.to_string())
    }

    /// Generate a unique run identifier scoped to this session.
    pub(crate) fn unique_run_id(&mut self, prefix: &str) -> Result<String, String> {
        self.refresh()?;
        let used_ids = self
            .store
            .entries()
            .iter()
            .map(|entry| entry.id.clone())
            .chain(
                self.store
                    .records()
                    .iter()
                    .map(|record| record.id().to_owned()),
            )
            .collect::<Vec<_>>();
        Ok(SessionIdGenerator::new(self.store.session_id()).next(prefix, &used_ids))
    }

    // ── Cancellation ──────────────────────────────────────────────────

    /// Request abort for all open lanes and return the main lane's run id,
    /// if any.
    pub(crate) fn request_abort(&mut self) -> Result<Option<String>, String> {
        self.cancellation.store(true, Ordering::SeqCst);
        self.refresh()?;
        let state = Reducer::reduce(&self.store).map_err(|error| error.to_string())?;
        let open_lanes: Vec<(String, String)> = state
            .lanes
            .iter()
            .filter_map(|lane| {
                lane.open_operation
                    .as_ref()
                    .map(|run_id| (lane.name.clone(), run_id.clone()))
            })
            .collect();
        if open_lanes.is_empty() {
            return Ok(None);
        }
        let main_run_id = state
            .lane("main")
            .and_then(|lane| lane.open_operation.clone());
        for (lane_name, run_id) in open_lanes {
            let is_already_requested = state.lane(&lane_name).is_some_and(|l| l.abort_requested);
            if !is_already_requested {
                let _ = self.store.request_abort(&run_id);
                let _ = self.store.drive_to_completion();
            }
        }
        Ok(main_run_id)
    }

    /// Reconcile an aborted operation: insert abort entry, record, and
    /// finish with `Aborted` outcome.  Returns `true` if recovery produced
    /// a terminal state.
    pub(crate) fn recover_abort(&mut self) -> Result<bool, String> {
        self.refresh()?;
        let state = Reducer::reduce(&self.store).map_err(|error| error.to_string())?;
        let Some(lane) = state.lane("main") else {
            return Ok(false);
        };
        let Some(run_id) = lane.open_operation.clone() else {
            return Ok(false);
        };
        if !lane.abort_requested {
            return Err(format!("suspended harness operation {run_id}"));
        }
        let start_seq = self
            .store
            .records()
            .iter()
            .find_map(|record| match record {
                HarnessRecord::OperationStarted { id, seq, .. } if id == &run_id => Some(*seq),
                _ => None,
            })
            .ok_or_else(|| format!("missing harness operation {run_id}"))?;
        if let Some(assistant_entry_id) = self
            .store
            .entries()
            .iter()
            .filter(|entry| entry.seq > start_seq)
            .find_map(|entry| {
                matches!(&entry.message, AgentMessage::Assistant { .. })
                    .then_some(entry.id.clone())
            })
        {
            self.store
                .reconcile_abort(&run_id, &assistant_entry_id)
                .map_err(|error| error.to_string())?;
            self.store
                .drive_to_completion()
                .map_err(|error| error.to_string())?;
            return Ok(true);
        }
        let result_entry_id = self.store.records().iter().rev().find_map(|record| {
            matches!(record, HarnessRecord::StepAttempt { run_id: record_run_id, .. } if record_run_id == &run_id)
                .then(|| match record {
                    HarnessRecord::StepAttempt { result_entry_id, .. } => result_entry_id.clone(),
                    _ => unreachable!(),
                })
        });
        let had_result_entry = result_entry_id.is_some();
        let entry_id = result_entry_id.unwrap_or_else(|| format!("abort-entry-{run_id}"));
        let has_abort_entry = self.store.entries().iter().any(|entry| {
            entry.id == entry_id
                && matches!(
                    &entry.message,
                    AgentMessage::Assistant {
                        stop_reason: Some(reason),
                        ..
                    } if reason == "aborted"
                )
        });
        if !had_result_entry && !has_abort_entry {
            self.store
                .append_record_gated(HarnessRecord::StepAttempt {
                    id: format!("abort-attempt-{run_id}"),
                    seq: self.next_seq(),
                    lane: "main".into(),
                    timestamp: timestamp(),
                    run_id: run_id.clone(),
                    attempt: lane.attempts.saturating_add(1),
                    result_entry_id: entry_id.clone(),
                    compaction_reason: None,
                })
                .map_err(|error| error.to_string())?;
            self.store
                .drive_to_completion()
                .map_err(|error| error.to_string())?;
        }
        if !has_abort_entry {
            let seq = self.next_seq();
            self.store
                .append_entry_gated(HarnessEntry {
                    id: entry_id.clone(),
                    parent_id: lane.leaf_id.clone(),
                    lane: "main".into(),
                    seq,
                    timestamp: timestamp(),
                    message: AgentMessage::Assistant {
                        content: Some("Run aborted before completion.".into()),
                        tool_calls: None,
                        stop_reason: Some("aborted".into()),
                        deferred_handle: None,
                    },
                    terminate: false,
                })
                .map_err(|error| error.to_string())?;
            self.store
                .drive_to_completion()
                .map_err(|error| error.to_string())?;
        }
        self.finish_run(
            &run_id,
            OperationOutcome::Aborted,
            Some("Generation cancelled".into()),
        )?;
        Ok(true)
    }

    // ── Assistant attempt & messages ──────────────────────────────────

    /// Append a user/assistant/tool message as a harness entry on the main
    /// lane.
    pub(crate) fn append_message(&mut self, message: AgentMessage) -> Result<(), String> {
        self.refresh()?;
        if self
            .store
            .entries()
            .last()
            .is_some_and(|entry| entry.message == message)
        {
            return Ok(());
        }
        let parent_id = Reducer::reduce(&self.store)
            .ok()
            .and_then(|state| state.lane("main").and_then(|lane| lane.leaf_id.clone()))
            .or_else(|| {
                self.store
                    .entries()
                    .iter()
                    .rev()
                    .find(|entry| entry.lane == "main")
                    .map(|entry| entry.id.clone())
            });
        let seq = self.next_seq();
        let terminate = matches!(
            &message,
            AgentMessage::Tool {
                terminate: true,
                ..
            }
        );
        let id = match &message {
            AgentMessage::Assistant { .. } => Reducer::reduce(&self.store)
                .ok()
                .and_then(|state| {
                    state
                        .lane("main")
                        .and_then(|lane| lane.open_operation.clone())
                })
                .and_then(|run_id| {
                    self.store
                        .records()
                        .iter()
                        .rev()
                        .find_map(|record| match record {
                            HarnessRecord::StepAttempt {
                                run_id: record_run_id,
                                result_entry_id,
                                ..
                            } if record_run_id == &run_id
                                && !self
                                    .store
                                    .entries()
                                    .iter()
                                    .any(|entry| entry.id == result_entry_id.as_str()) =>
                            {
                                Some(result_entry_id.clone())
                            }
                            _ => None,
                        })
                })
                .unwrap_or_else(|| format!("v2-entry-{seq}")),
            AgentMessage::Tool { tool_call_id, .. } => format!("v2-tool-result-{tool_call_id}"),
            _ => format!("v2-entry-{seq}"),
        };
        self.store
            .append_entry_gated(HarnessEntry {
                id,
                parent_id,
                lane: "main".into(),
                seq,
                timestamp: timestamp(),
                message,
                terminate,
            })
            .map_err(|error| error.to_string())?;
        self.store
            .drive_to_completion()
            .map_err(|error| error.to_string())
    }

    /// Append a message to a named lane (used for subagent results).
    pub(crate) fn append_message_to_lane(
        &mut self,
        lane: &str,
        run_id: &str,
        message: AgentMessage,
    ) -> Result<String, String> {
        self.refresh()?;
        let prefix = format!("subagent-entry-{run_id}-");
        if matches!(
            message,
            AgentMessage::User { .. } | AgentMessage::Assistant { .. }
        ) {
            if let Some(entry) = self.store.entries().iter().rev().find(|entry| {
                entry.lane == lane && entry.id.starts_with(&prefix) && entry.message == message
            }) {
                return Ok(entry.id.clone());
            }
        }
        let ordinal = self
            .store
            .entries()
            .iter()
            .filter(|entry| entry.id.starts_with(&prefix))
            .count();
        let id = match &message {
            AgentMessage::Tool { tool_call_id, .. } => {
                format!("subagent-result-{run_id}-{tool_call_id}")
            }
            AgentMessage::Assistant { .. } => self
                .store
                .records()
                .iter()
                .filter_map(|record| match record {
                    HarnessRecord::StepAttempt {
                        run_id: record_run,
                        result_entry_id,
                        ..
                    } if record_run == run_id => Some(result_entry_id.clone()),
                    _ => None,
                })
                .next()
                .unwrap_or_else(|| format!("{prefix}{ordinal}")),
            _ => format!("{prefix}{ordinal}"),
        };
        if let Some(entry) = self
            .store
            .entries()
            .iter()
            .find(|entry| entry.lane == lane && entry.id == id)
        {
            return Ok(entry.id.clone());
        }
        let parent_id = match &message {
            AgentMessage::Tool { tool_call_id, .. } => self
                .store
                .records()
                .iter()
                .rev()
                .find_map(|record| match record {
                    HarnessRecord::ToolStarted {
                        tool_call_id: id,
                        assistant_entry_id,
                        ..
                    } if id == tool_call_id => Some(assistant_entry_id.clone()),
                    _ => None,
                })
                .or_else(|| {
                    Reducer::reduce(self.store.store())
                        .ok()
                        .and_then(|state| state.lane(lane).and_then(|l| l.leaf_id.clone()))
                }),
            _ => Reducer::reduce(self.store.store())
                .ok()
                .and_then(|state| state.lane(lane).and_then(|l| l.leaf_id.clone()))
                .or_else(|| {
                    self.store
                        .entries()
                        .iter()
                        .rev()
                        .find(|e| e.lane == lane)
                        .map(|e| e.id.clone())
                }),
        };
        let seq = self.next_seq();
        let terminate = matches!(
            &message,
            AgentMessage::Tool {
                terminate: true,
                ..
            }
        );
        let entry = HarnessEntry {
            id: id.clone(),
            seq,
            lane: lane.into(),
            parent_id,
            timestamp: timestamp(),
            message,
            terminate,
        };
        self.store
            .append_entry_gated(entry)
            .map_err(|error| error.to_string())?;
        self.store
            .drive_to_completion()
            .map_err(|error| error.to_string())?;
        Ok(id)
    }

    /// Prepare an assistant attempt record for the given run.  Returns
    /// the result entry id that the assistant message should carry.
    pub(crate) fn prepare_assistant_attempt(
        &mut self,
        run_id: &str,
    ) -> Result<String, String> {
        self.refresh()?;
        let state = Reducer::reduce(&self.store).map_err(|error| error.to_string())?;
        let lane = state
            .lane("main")
            .filter(|lane| lane.open_operation.as_deref() == Some(run_id))
            .ok_or_else(|| format!("harness operation {run_id} is not open"))?;

        if let Some(result_entry_id) = self.store.records().iter().find_map(|record| {
            let HarnessRecord::StepAttempt {
                run_id: record_run_id,
                result_entry_id,
                ..
            } = record
            else {
                return None;
            };
            (record_run_id == run_id
                && !self
                    .store
                    .entries()
                    .iter()
                    .any(|entry| entry.id == *result_entry_id))
            .then(|| result_entry_id.clone())
        }) {
            return Ok(result_entry_id);
        }

        let attempt = lane.attempts.saturating_add(1);
        let result_entry_id = format!("entry-{run_id}-assistant-{attempt}");
        let seq = harness_next_seq(self.store.store());
        self.store
            .append_record_gated(HarnessRecord::StepAttempt {
                id: format!("attempt-{run_id}-{attempt}"),
                seq,
                lane: "main".into(),
                timestamp: timestamp(),
                run_id: run_id.into(),
                attempt,
                result_entry_id: result_entry_id.clone(),
                compaction_reason: None,
            })
            .map_err(|error| error.to_string())?;
        self.store
            .drive_to_completion()
            .map_err(|error| error.to_string())?;
        Ok(result_entry_id)
    }

    /// Record a completed assistant attempt after the assistant message
    /// has been appended.
    pub(crate) fn record_assistant_attempt(
        &mut self,
        run_id: &str,
        usage: TokenUsage,
    ) -> Result<(), String> {
        self.refresh()?;
        let start_seq = self
            .store
            .records()
            .iter()
            .find_map(|record| match record {
                HarnessRecord::OperationStarted { id, seq, .. } if id == run_id => Some(*seq),
                _ => None,
            })
            .ok_or_else(|| format!("missing harness operation {run_id}"))?;
        let result_entry_id = self
            .store
            .entries()
            .iter()
            .filter(|entry| {
                entry.seq > start_seq
                    && matches!(&entry.message, AgentMessage::Assistant { .. })
            })
            .max_by_key(|entry| entry.seq)
            .map(|entry| entry.id.clone())
            .ok_or_else(|| format!("run {run_id} has no assistant result"))?;
        self.store
            .finish_assistant_attempt(run_id, &result_entry_id, usage)
            .map_err(|error| error.to_string())?;
        self.store
            .drive_to_completion()
            .map_err(|error| error.to_string())
    }

    // ── Tools ─────────────────────────────────────────────────────────

    /// Record a tool intent (after hooks have run).
    pub(crate) async fn append_tool_intent_after_hook(
        &mut self,
        run_id: &str,
        tool_call_id: &str,
        tool_name: &str,
        effective_args: Value,
    ) -> Result<(), String> {
        self.refresh()?;
        if self.store.records().iter().any(|record| {
            matches!(record, HarnessRecord::ToolStarted {
                run_id: record_run_id,
                tool_call_id: record_call_id,
                ..
            } if record_run_id == run_id && record_call_id == tool_call_id)
        }) {
            return Ok(());
        }
        let assistant = self
            .store
            .entries()
            .iter()
            .rev()
            .find(|entry| {
                matches!(
                    &entry.message,
                    AgentMessage::Assistant { tool_calls: Some(calls), .. }
                        if calls.iter().any(|call| call.id == tool_call_id)
                )
            })
            .ok_or_else(|| format!("missing assistant entry for tool {tool_call_id}"))?;
        let assistant_id = assistant.id.clone();
        let tool_index = match &assistant.message {
            AgentMessage::Assistant {
                tool_calls: Some(calls),
                ..
            } => calls
                .iter()
                .position(|call| call.id == tool_call_id)
                .ok_or_else(|| format!("tool {tool_call_id} is absent from assistant entry"))?,
            _ => return Err("assistant entry has no tool calls".into()),
        };
        self.store
            .start_tool_batch(
                run_id,
                &assistant_id,
                &[ToolSpec {
                    index: tool_index,
                    call_id: tool_call_id.into(),
                    name: tool_name.into(),
                    effective_args,
                    result_entry_id: format!("v2-tool-result-{tool_call_id}"),
                    replay: match threadlane_agent::classify_tool_replay_safety(tool_name) {
                        threadlane_agent::ToolReplaySafety::Safe => {
                            HarnessToolReplaySafety::Safe
                        }
                        threadlane_agent::ToolReplaySafety::Never => {
                            HarnessToolReplaySafety::Never
                        }
                    },
                }],
            )
            .map_err(|error| error.to_string())?;
        self.store
            .drive_to_completion()
            .map_err(|error| error.to_string())
    }

    /// Record tool-started on a specific lane (subagent support).
    pub(crate) fn tool_started_on_lane(
        &mut self,
        lane: &str,
        run_id: &str,
        tool_call_id: &str,
        tool_name: &str,
        effective_args: Value,
    ) -> Result<(), String> {
        self.refresh()?;
        if self.store.records().iter().any(|record| {
            matches!(record, HarnessRecord::ToolStarted {
                run_id: record_run_id,
                tool_call_id: record_call_id,
                ..
            } if record_run_id == run_id && record_call_id == tool_call_id)
        }) {
            return Ok(());
        }
        let assistant = self
            .store
            .entries()
            .iter()
            .rev()
            .find(|entry| {
                entry.lane == lane
                    && matches!(
                        &entry.message,
                        AgentMessage::Assistant { tool_calls: Some(calls), .. }
                            if calls.iter().any(|call| call.id == tool_call_id)
                    )
            })
            .ok_or_else(|| {
                format!("missing assistant entry for tool {tool_call_id} on lane {lane}")
            })?;
        let assistant_id = assistant.id.clone();
        let tool_index = match &assistant.message {
            AgentMessage::Assistant {
                tool_calls: Some(calls),
                ..
            } => calls
                .iter()
                .position(|call| call.id == tool_call_id)
                .ok_or_else(|| format!("tool {tool_call_id} is absent from assistant entry"))?,
            _ => return Err("assistant entry has no tool calls".into()),
        };
        self.store
            .start_tool_batch(
                run_id,
                &assistant_id,
                &[ToolSpec {
                    index: tool_index,
                    call_id: tool_call_id.into(),
                    name: tool_name.into(),
                    effective_args,
                    result_entry_id: format!("v2-tool-result-{tool_call_id}"),
                    replay: match threadlane_agent::classify_tool_replay_safety(tool_name) {
                        threadlane_agent::ToolReplaySafety::Safe => {
                            HarnessToolReplaySafety::Safe
                        }
                        threadlane_agent::ToolReplaySafety::Never => {
                            HarnessToolReplaySafety::Never
                        }
                    },
                }],
            )
            .map_err(|error| error.to_string())?;
        self.store
            .drive_to_completion()
            .map_err(|error| error.to_string())
    }

    /// Finish a tool message: record ToolFinished and drive effects.
    pub(crate) fn finish_tool_message(
        &mut self,
        run_id: &str,
        message: &AgentMessage,
    ) -> Result<(), String> {
        let AgentMessage::Tool {
            tool_call_id,
            name,
            content,
            is_error,
            terminate,
        } = message
        else {
            return Ok(());
        };
        self.refresh()?;
        self.store
            .finish_existing_tool(
                run_id,
                threadlane_agent::harness::ToolResult {
                    call_id: tool_call_id.clone(),
                    name: name.clone(),
                    content: content.clone(),
                    is_error: *is_error,
                    terminate: *terminate,
                },
            )
            .map_err(|error| error.to_string())?;
        self.store
            .drive_to_completion()
            .map_err(|error| error.to_string())
    }

    /// Finish a replayed tool result.
    pub(crate) fn finish_replayed_tool(
        &mut self,
        run_id: &str,
        result: &AgentToolResult,
    ) -> Result<(), String> {
        self.refresh()?;
        self.store
            .finish_existing_tool(
                run_id,
                HarnessToolResult {
                    call_id: result.tool_call_id.clone(),
                    name: result.name.clone(),
                    content: result.content.clone(),
                    is_error: result.is_error,
                    terminate: result.terminates(),
                },
            )
            .map_err(|error| error.to_string())?;
        self.store
            .drive_to_completion()
            .map_err(|error| error.to_string())
    }

    /// Record tool completions with termination flags.
    pub(crate) fn record_completed_tools_with_termination(
        &mut self,
        run_id: &str,
        termination: &HashMap<String, bool>,
    ) -> Result<(), String> {
        self.refresh()?;
        let start_seq = self
            .store
            .records()
            .iter()
            .find_map(|record| match record {
                HarnessRecord::OperationStarted { id, seq, .. } if id == run_id => Some(*seq),
                _ => None,
            })
            .ok_or_else(|| format!("missing harness operation {run_id}"))?;
        let Some(assistant) = self
            .store
            .entries()
            .iter()
            .filter(|entry| entry.seq > start_seq)
            .filter_map(|entry| match &entry.message {
                AgentMessage::Assistant {
                    tool_calls: Some(tool_calls),
                    ..
                } if !tool_calls.is_empty() => Some(entry),
                _ => None,
            })
            .max_by_key(|entry| entry.seq)
        else {
            return Ok(());
        };
        let assistant_id = assistant.id.clone();
        let tool_entries = self
            .store
            .entries()
            .iter()
            .filter(|entry| entry.parent_id.as_deref() == Some(assistant_id.as_str()))
            .filter_map(|entry| match &entry.message {
                AgentMessage::Tool {
                    tool_call_id, name, ..
                } => Some((tool_call_id.clone(), name.clone(), entry.id.clone())),
                _ => None,
            })
            .collect::<Vec<_>>();
        let AgentMessage::Assistant {
            tool_calls: Some(tool_calls),
            ..
        } = &assistant.message
        else {
            return Ok(());
        };
        let tool_calls = tool_calls.clone();
        if tool_calls
            .iter()
            .any(|call| !tool_entries.iter().any(|(id, _, _)| id == &call.id))
        {
            return Err(format!("run {run_id} has an incomplete tool batch"));
        }
        for (index, call) in tool_calls.iter().enumerate() {
            let (_, name, result_entry) = tool_entries
                .iter()
                .find(|(id, _, _)| id == &call.id)
                .expect("tool batch completeness was checked");
            let persisted_result = self
                .store
                .entries()
                .iter()
                .find(|entry| entry.id == *result_entry)
                .and_then(|entry| match &entry.message {
                    AgentMessage::Tool {
                        content, is_error, ..
                    } => Some((content.clone(), *is_error)),
                    _ => None,
                })
                .ok_or_else(|| format!("run {run_id} has an invalid tool result"))?;
            let args = serde_json::from_str(&call.function.arguments)
                .unwrap_or_else(|_| Value::String(call.function.arguments.clone()));
            let replay = match threadlane_agent::classify_tool_replay_safety(name) {
                threadlane_agent::ToolReplaySafety::Safe => HarnessToolReplaySafety::Safe,
                threadlane_agent::ToolReplaySafety::Never => HarnessToolReplaySafety::Never,
            };
            let started = self.store.records().iter().any(|record| {
                matches!(record, HarnessRecord::ToolStarted {
                    run_id: record_run_id,
                    tool_call_id,
                    ..
                } if record_run_id == run_id && tool_call_id == &call.id)
            });
            if !started {
                self.store
                    .start_tool_batch(
                        run_id,
                        &assistant_id,
                        &[ToolSpec {
                            index,
                            call_id: call.id.clone(),
                            name: name.to_string(),
                            effective_args: args,
                            result_entry_id: result_entry.clone(),
                            replay,
                        }],
                    )
                    .map_err(|error| error.to_string())?;
                self.store
                    .drive_to_completion()
                    .map_err(|error| error.to_string())?;
            }
            let terminate = termination.get(&call.id).copied().unwrap_or(false);
            self.store
                .finish_existing_tool(
                    run_id,
                    threadlane_agent::harness::ToolResult {
                        call_id: call.id.clone(),
                        name: name.clone(),
                        content: persisted_result.0,
                        is_error: persisted_result.1,
                        terminate,
                    },
                )
                .map_err(|error| error.to_string())?;
            self.store
                .drive_to_completion()
                .map_err(|error| error.to_string())?;
        }
        Ok(())
    }

    // ── Usage ─────────────────────────────────────────────────────────

    /// Record provider token usage for a run.
    pub(crate) fn record_provider_usage(
        &mut self,
        run_id: &str,
        usage: TokenUsage,
    ) -> Result<(), String> {
        self.refresh()?;
        self.store
            .record_provider_usage(run_id, usage)
            .map_err(|error| error.to_string())?;
        self.store
            .drive_to_completion()
            .map_err(|error| error.to_string())
    }

    /// Record discarded (non-terminal) token usage.
    pub(crate) fn record_discarded_usage(
        &mut self,
        run_id: &str,
        usage: TokenUsage,
    ) -> Result<(), String> {
        self.refresh()?;
        self.store
            .record_discarded_usage(run_id, usage)
            .map_err(|error| error.to_string())?;
        self.store
            .drive_to_completion()
            .map_err(|error| error.to_string())
    }

    // ── Retry ─────────────────────────────────────────────────────────

    /// Schedule a retry for a failed run.
    pub(crate) fn schedule_retry(
        &mut self,
        run_id: &str,
        reason: &str,
    ) -> Result<u32, String> {
        self.refresh()?;
        let attempt = self
            .store
            .schedule_retry(
                run_id,
                reason,
                RetryPolicy {
                    max_attempts: 3,
                    base_delay: 1_000,
                    max_delay: 8_000,
                },
            )
            .map_err(|error| error.to_string())?;
        self.store
            .drive_to_completion()
            .map_err(|error| error.to_string())?;
        Ok(attempt)
    }

    /// Begin a previously scheduled retry attempt.
    pub(crate) fn begin_retry(&mut self, run_id: &str) -> Result<u32, String> {
        self.refresh()?;
        let attempt = self
            .store
            .begin_retry(run_id)
            .map_err(|error| error.to_string())?;
        self.store
            .drive_to_completion()
            .map_err(|error| error.to_string())?;
        Ok(attempt)
    }

    // ── Deferred ──────────────────────────────────────────────────────

    /// Redeem a deferred operation and optionally finish the run.
    pub(crate) fn redeem_deferred(
        &mut self,
        run_id: &str,
        resolution: DeferredResolution,
    ) -> Result<bool, String> {
        self.refresh()?;
        let terminal = self
            .store
            .redeem_deferred(run_id, resolution)
            .map_err(|error| error.to_string())?;
        self.store
            .drive_to_completion()
            .map_err(|error| error.to_string())?;
        if terminal {
            self.finish_run(run_id, OperationOutcome::Completed, None)?;
        }
        Ok(terminal)
    }

    // ── Compaction ────────────────────────────────────────────────────

    /// Accept a compaction summary.
    pub(crate) fn accept_compaction(
        &mut self,
        run_id: &str,
        summary: &str,
    ) -> Result<(), String> {
        self.refresh()?;
        self.store
            .accept_compaction(run_id, summary)
            .map_err(|error| error.to_string())?;
        self.store
            .drive_to_completion()
            .map_err(|error| error.to_string())
    }

    // ── Facts ─────────────────────────────────────────────────────────

    /// Set a session-level fact.
    pub(crate) fn set_fact(
        &mut self,
        lane: &str,
        key: &str,
        value: String,
    ) -> Result<(), String> {
        self.refresh()?;
        self.store
            .set_fact(lane, key, value, None)
            .map_err(|error| error.to_string())?;
        self.store
            .drive_to_completion()
            .map_err(|error| error.to_string())
    }

    // ── Replay & navigation ───────────────────────────────────────────

    /// Append a replayed tool entry to the store.
    pub(crate) fn append_replayed_tool_entry(
        &mut self,
        run_id: &str,
        assistant_entry_id: &str,
        spec: &ToolSpec,
        result: &AgentToolResult,
    ) -> Result<(), String> {
        let state =
            Reducer::reduce(self.store.store()).map_err(|error| error.to_string())?;
        let lane = state
            .lanes
            .iter()
            .find(|lane| lane.open_operation.as_deref() == Some(run_id))
            .ok_or_else(|| format!("harness operation {run_id} is not open"))?;
        let seq = self.next_seq();
        self.store
            .append_entry_gated(HarnessEntry {
                id: spec.result_entry_id.clone(),
                parent_id: Some(assistant_entry_id.into()),
                lane: lane.name.clone(),
                seq,
                timestamp: timestamp(),
                message: AgentMessage::Tool {
                    tool_call_id: result.tool_call_id.clone(),
                    name: result.name.clone(),
                    content: result.content.clone(),
                    is_error: result.is_error,
                    terminate: result.terminates(),
                },
                terminate: result.terminates(),
            })
            .map_err(|error| error.to_string())?;
        self.store
            .drive_to_completion()
            .map_err(|error| error.to_string())
    }

    /// Claim safe tool replays for recovery.
    pub(crate) fn claim_safe_replays(
        &mut self,
        tools: &[HarnessRecord],
    ) -> Result<Vec<HarnessRecord>, String> {
        let records = self.store.records().to_vec();
        let entries = self.store.entries().to_vec();
        let mut claimed = Vec::new();
        for tool in tools {
            let HarnessRecord::ToolStarted {
                lane,
                run_id,
                assistant_entry_id,
                tool_index,
                tool_call_id,
                tool_name,
                effective_args,
                result_entry_id,
                replay: HarnessToolReplaySafety::Safe,
                ..
            } = tool
            else {
                continue;
            };
            let already_completed = records.iter().any(|record| {
                matches!(
                    record,
                    HarnessRecord::ToolFinished {
                        tool_call_id: finished_call,
                        ..
                    } if finished_call == tool_call_id
                )
            }) || entries.iter().any(|entry| entry.id.contains(tool_call_id));
            if already_completed {
                continue;
            }
            let seq = self.next_seq();
            self.store
                .append_record_gated(HarnessRecord::ToolStarted {
                    id: format!("replay-claim-{run_id}-{tool_call_id}-{seq}"),
                    seq,
                    lane: lane.clone(),
                    timestamp: timestamp(),
                    run_id: run_id.clone(),
                    assistant_entry_id: assistant_entry_id.clone(),
                    tool_index: *tool_index,
                    tool_call_id: tool_call_id.clone(),
                    tool_name: tool_name.clone(),
                    effective_args: effective_args.clone(),
                    result_entry_id: result_entry_id.clone(),
                    replay: HarnessToolReplaySafety::Never,
                })
                .map_err(|error| error.to_string())?;
            self.store
                .drive_to_completion()
                .map_err(|error| error.to_string())?;
            claimed.push(tool.clone());
        }
        Ok(claimed)
    }

    /// Materialize a session tree branch path as harness entries.
    pub(crate) fn navigate_branch(
        &mut self,
        branch_ids: &[String],
        session_tree: &SessionTree,
    ) -> Result<Option<String>, String> {
        self.refresh()?;
        let mut harness_target_id = None;
        let mut parent_id: Option<String> = None;
        for legacy_id in branch_ids {
            let node = session_tree
                .nodes
                .get(legacy_id)
                .ok_or_else(|| format!("Node ID not found in session tree: {legacy_id}"))?;
            if matches!(node.message, AgentMessage::System { .. }) {
                continue;
            }
            let entry_id = if self
                .store
                .entries()
                .iter()
                .any(|entry| entry.id == *legacy_id)
            {
                legacy_id.clone()
            } else {
                format!("v2-navigation-{legacy_id}")
            };
            if !self
                .store
                .entries()
                .iter()
                .any(|entry| entry.id == entry_id)
            {
                self.store
                    .append_entry_gated(HarnessEntry {
                        id: entry_id.clone(),
                        parent_id: parent_id.clone(),
                        lane: "main".into(),
                        seq: harness_next_seq(self.store.store()),
                        timestamp: timestamp(),
                        message: node.message.clone(),
                        terminate: matches!(
                            node.message,
                            AgentMessage::Tool {
                                terminate: true,
                                ..
                            }
                        ),
                    })
                    .map_err(|error| error.to_string())?;
                self.store
                    .drive_to_completion()
                    .map_err(|error| error.to_string())?;
            }
            parent_id = Some(entry_id.clone());
            if *legacy_id == branch_ids[branch_ids.len() - 1] {
                harness_target_id = Some(entry_id);
            }
        }
        Ok(harness_target_id)
    }

    // ── Observation ───────────────────────────────────────────────────

    /// Take a point-in-time snapshot of the session.
    pub(crate) fn snapshot(&mut self) -> Result<Snapshot, String> {
        self.refresh()?;
        self.store.snapshot().map_err(|error| error.to_string())
    }

    /// Subscribe to session-scoped events.
    pub(crate) fn watch(&mut self) -> Result<HarnessWatch, String> {
        self.refresh()?;
        let subscription = self
            .store
            .watch_session()
            .map_err(|error| error.to_string())?;
        Ok(HarnessWatch {
            hub: self.events.clone(),
            subscription,
        })
    }

    /// Drive all pending effects to completion.
    pub(crate) fn drive_to_completion(&mut self) -> Result<(), String> {
        self.store
            .drive_to_completion()
            .map_err(|error| error.to_string())
    }

    // ── Internal ──────────────────────────────────────────────────────

    /// Re-read the store from disk to pick up external writes.
    /// During the migration window, some callers still write directly.
    pub(crate) fn refresh(&mut self) -> Result<(), String> {
        let path = self.store.store().path().to_path_buf();
        let hooks = std::mem::take(self.store.hooks_mut());
        let events = self.events.clone();
        let cancellation = self.cancellation.clone();
        match JsonlStore::open(&path) {
            Ok(store) => {
                let persist_path = path.clone();
                let persist_events = events.clone();
                let executor = move |action: EffectAction| {
                    let mut store = JsonlStore::open(&persist_path)
                        .map_err(|error| ReduceError::Storage(error.to_string()))?;
                    if let Err(error) = action.apply(&mut store) {
                        persist_events
                            .publish(EventPayload::Fault(error.to_string()));
                        return Err(error);
                    }
                    let (payload, lane, run_id, turn) = match &action {
                        EffectAction::AppendEntry { entry } => (
                            EventPayload::EntryCommitted(entry.clone()),
                            Some(entry.lane.clone()),
                            None,
                            None,
                        ),
                        EffectAction::AppendRecord { record, .. } => (
                            EventPayload::RecordCommitted(record.clone()),
                            Some(record.lane().to_owned()),
                            record.run_id().map(str::to_owned),
                            record.turn(),
                        ),
                    };
                    persist_events.publish_identified_with_turn(
                        payload, lane, run_id, turn, None,
                    );
                    Ok(())
                };
                *self.store.hooks_mut() = hooks;
                self.store = AgentHarness::with_executor_and_hooks(
                    store,
                    events,
                    executor,
                    self.store.hooks().clone(),
                );
                let _ = cancellation;
                Ok(())
            }
            Err(error) => {
                *self.store.hooks_mut() = hooks;
                Err(error.to_string())
            }
        }
    }

    fn next_seq(&self) -> u64 {
        self.store
            .entries()
            .iter()
            .map(|entry| entry.seq)
            .chain(self.store.records().iter().map(HarnessRecord::seq))
            .max()
            .unwrap_or(0)
            + 1
    }

    /// Persist any messages from the provider that are not yet recorded
    /// in the harness store.  Called after each turn to ensure the
    /// canonical session path captures all assistant/tool entries.
    pub(crate) fn sync_messages(&mut self, messages: &[AgentMessage]) -> Result<(), String> {
        // Collect the set of message hashes already in the store to avoid
        // duplicates.
        let existing: std::collections::HashSet<String> = self
            .store
            .entries()
            .iter()
            .filter(|e| e.lane == "main")
            .map(|e| format!("{:?}", e.message))
            .collect();
        for msg in messages {
            if matches!(msg, AgentMessage::System { .. }) {
                continue;
            }
            let key = format!("{:?}", msg);
            if existing.contains(&key) {
                continue;
            }
            // Only persist non-user messages; user prompts are handled by
            // begin_run/accept_prompt.
            if msg.is_user() {
                continue;
            }
            self.append_message(msg.clone())?;
        }
        Ok(())
    }
    /// Run hooks of the given kind for the main lane.
    pub(crate) async fn run_hooks(
        &self,
        kind: HookKind,
        context: &HookContext,
    ) {
        for failure in self.store.hooks().run(kind, context).await {
            eprintln!(
                "hook {} ({:?}) failed: {}",
                failure.id, kind,
                failure.message
            );
        }
    }
}

// ── Helpers ───────────────────────────────────────────────────────────

fn timestamp() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

fn harness_next_seq(store: &JsonlStore) -> u64 {
    store
        .entries()
        .iter()
        .map(|entry| entry.seq)
        .chain(store.records().iter().map(HarnessRecord::seq))
        .max()
        .unwrap_or(0)
        + 1
}
#[cfg(test)]
mod tests {
    use super::*;
    fn temp_session() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.jsonl");
        (dir, path)
    }

    // ── No-tool prompt: one OperationStarted + one StepAttempt + one
    //    OperationFinished ──────────────────────────────────────────────
    #[test]
    fn no_tool_prompt_produces_one_operation() {
        let (_dir, path) = temp_session();
        let mut harness = CodingSessionHarness::open(&path).unwrap();

        let run_id = harness.unique_run_id("test").unwrap();
        harness
            .begin_run(&run_id, AgentMessage::user("hello", vec![]))
            .unwrap();

        // Prepare an assistant attempt
        let result_entry_id = harness.prepare_assistant_attempt(&run_id).unwrap();

        // Append the assistant message
        harness
            .append_message(AgentMessage::Assistant {
                content: Some("Hello!".into()),
                tool_calls: None,
                stop_reason: Some("end_turn".into()),
                deferred_handle: None,
            })
            .unwrap();

        // Record the attempt
        harness
            .record_assistant_attempt(
                &run_id,
                TokenUsage {
                    input_tokens: 10,
                    output_tokens: 5,
                    cache_read_tokens: 0,
                    cache_write_tokens: 0,
                    total_tokens: 0,
                },
            )
            .unwrap();

        // Finish
        harness
            .finish_run(&run_id, OperationOutcome::Completed, None)
            .unwrap();

        // Verify records
        let records = harness.store.records();
        let started = records
            .iter()
            .filter(|r| matches!(r, HarnessRecord::OperationStarted { .. }))
            .count();
        let attempts = records
            .iter()
            .filter(|r| matches!(r, HarnessRecord::StepAttempt { .. }))
            .count();
        let finished = records
            .iter()
            .filter(|r| matches!(r, HarnessRecord::OperationFinished { .. }))
            .count();

        assert_eq!(started, 1, "expected exactly one OperationStarted");
        assert_eq!(attempts, 1, "expected exactly one StepAttempt");
        assert_eq!(finished, 1, "expected exactly one OperationFinished");

        // Verify sequences are monotonically increasing
        let seqs: Vec<u64> = records.iter().map(|r| r.seq()).collect();
        for window in seqs.windows(2) {
            assert!(window[0] < window[1], "sequences must increase");
        }
    }

    // ── Reopening produces same reduced main-lane state ──────────────
    #[test]
    fn reopening_produces_same_main_lane_state() {
        let (_dir, path) = temp_session();

        let run_id = {
            let mut harness = CodingSessionHarness::open(&path).unwrap();
            let id = harness.unique_run_id("test").unwrap();
            harness
                .begin_run(&id, AgentMessage::user("hello", vec![]))
                .unwrap();
            harness
                .prepare_assistant_attempt(&id)
                .unwrap();
            harness
                .append_message(AgentMessage::Assistant {
                    content: Some("Hi there".into()),
                    tool_calls: None,
                    stop_reason: Some("end_turn".into()),
                    deferred_handle: None,
                })
                .unwrap();
            harness
                .record_assistant_attempt(
                    &id,
                    TokenUsage {
                        input_tokens: 10,
                        output_tokens: 3,
                        cache_read_tokens: 0,
                        cache_write_tokens: 0,
                        total_tokens: 0,
                    },
                )
                .unwrap();
            harness
                .finish_run(&id, OperationOutcome::Completed, None)
                .unwrap();
            id
        };

        // Reopen and verify
        let mut reopened = CodingSessionHarness::open(&path).unwrap();
        let state = Reducer::reduce(&reopened.store).unwrap();
        let main_lane = state.lane("main").expect("main lane should exist");

        // The operation should be completed (not open)
        assert!(
            main_lane.open_operation.is_none(),
            "main lane should not have an open operation after finish"
        );

        // Verify the snapshot is consistent
        let snapshot = reopened.snapshot().unwrap();
        let main_snapshot = snapshot
            .state
            .lanes
            .iter()
            .find(|l| l.name == "main")
            .expect("main lane in snapshot");
        assert_eq!(main_snapshot.attempts, 1);
        assert_eq!(main_snapshot.open_operation, None);

        // Verify entries exist
        let entries = reopened.store.entries();
        assert!(
            entries.iter().any(|e| matches!(
                &e.message,
                AgentMessage::User { .. }
            )),
            "user prompt entry should be present"
        );
        assert!(
            entries.iter().any(|e| matches!(
                &e.message,
                AgentMessage::Assistant { .. }
            )),
            "assistant entry should be present"
        );
    }

    // ── Error during finish_run propagates correctly ──────────────────
    #[test]
    fn error_during_run_terminates_operation() {
        let (_dir, path) = temp_session();
        let mut harness = CodingSessionHarness::open(&path).unwrap();

        let run_id = harness.unique_run_id("test").unwrap();
        harness
            .begin_run(&run_id, AgentMessage::user("hello", vec![]))
            .unwrap();
        harness
            .prepare_assistant_attempt(&run_id)
            .unwrap();
        harness
            .append_message(AgentMessage::Assistant {
                content: Some("error occurred".into()),
                tool_calls: None,
                stop_reason: Some("error".into()),
                deferred_handle: None,
            })
            .unwrap();

        let result = harness.finish_run(
            &run_id,
            OperationOutcome::Failed,
            Some("provider error".into()),
        );
        assert!(result.is_ok(), "finish with error should succeed");

        // Verify the operation is marked as failed
        let state = Reducer::reduce(&harness.store).unwrap();
        let main_lane = state.lane("main").unwrap();
        assert!(main_lane.open_operation.is_none());

        // Verify records show the failure
        let records = harness.store.records();
        let finished_record = records
            .iter()
            .find(|r| matches!(r, HarnessRecord::OperationFinished { .. }));
        assert!(finished_record.is_some(), "should have OperationFinished");
    }

    // ── Sync messages deduplicates correctly ──────────────────────────
    #[test]
    fn sync_messages_deduplicates_existing_entries() {
        let (_dir, path) = temp_session();
        let mut harness = CodingSessionHarness::open(&path).unwrap();

        let run_id = harness.unique_run_id("test").unwrap();
        harness
            .begin_run(&run_id, AgentMessage::user("hello", vec![]))
            .unwrap();
        harness
            .prepare_assistant_attempt(&run_id)
            .unwrap();
        harness
            .append_message(AgentMessage::Assistant {
                content: Some("response".into()),
                tool_calls: None,
                stop_reason: Some("end_turn".into()),
                deferred_handle: None,
            })
            .unwrap();

        let entry_count_before = harness.store.entries().len();

        // Syncing the same messages again should not create duplicates
        harness
            .sync_messages(&[
                AgentMessage::Assistant {
                    content: Some("response".into()),
                    tool_calls: None,
                    stop_reason: Some("end_turn".into()),
                    deferred_handle: None,
                },
            ])
            .unwrap();

        assert_eq!(
            harness.store.entries().len(),
            entry_count_before,
            "sync_messages should not create duplicate entries"
        );
    }
}
