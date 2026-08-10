use serde_json::Value;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use threadlane_agent::harness::{
    AgentHarness, DeferredResolution, Entry as HarnessEntry, EventError, HarnessEvent,
    HarnessEventHub, HookContext, HookKind, HookRegistry, JsonlStore, OperationIntent,
    OperationOutcome, Record as HarnessRecord, Reducer, RetryPolicy, SessionIdGenerator,
    SessionStore, Snapshot, StreamingState, Subscription,
    ToolReplaySafety as HarnessToolReplaySafety, ToolResult as HarnessToolResult, ToolSpec,
};
use threadlane_agent::{AgentMessage, AgentToolResult, TokenUsage};

pub struct HarnessWatch {
    pub(crate) hub: HarnessEventHub,
    pub(crate) subscription: Subscription,
}

impl HarnessWatch {
    pub fn snapshot(&self) -> &Snapshot {
        &self.subscription.snapshot
    }

    pub fn poll(&mut self) -> Result<Vec<HarnessEvent>, EventError> {
        self.hub.poll(&mut self.subscription)
    }
}

pub(crate) fn harness_event_hub(path: &Path) -> HarnessEventHub {
    static HUBS: std::sync::OnceLock<std::sync::Mutex<HashMap<PathBuf, HarnessEventHub>>> =
        std::sync::OnceLock::new();
    let hubs = HUBS.get_or_init(|| std::sync::Mutex::new(HashMap::new()));
    let mut hubs = hubs.lock().unwrap_or_else(|error| error.into_inner());
    hubs.entry(path.to_path_buf())
        .or_insert_with(|| HarnessEventHub::new(256))
        .clone()
}

pub(crate) fn harness_hook_registry(path: &Path) -> HookRegistry {
    static REGISTRIES: std::sync::OnceLock<std::sync::Mutex<HashMap<PathBuf, HookRegistry>>> =
        std::sync::OnceLock::new();
    let registries = REGISTRIES.get_or_init(|| std::sync::Mutex::new(HashMap::new()));
    let mut registries = registries.lock().unwrap_or_else(|error| error.into_inner());
    registries.entry(path.to_path_buf()).or_default().clone()
}

pub(crate) fn harness_cancellation_state(path: &Path) -> Arc<AtomicBool> {
    static STATES: std::sync::OnceLock<std::sync::Mutex<HashMap<PathBuf, Arc<AtomicBool>>>> =
        std::sync::OnceLock::new();
    let states = STATES.get_or_init(|| std::sync::Mutex::new(HashMap::new()));
    let mut states = states.lock().unwrap_or_else(|error| error.into_inner());
    states
        .entry(path.to_path_buf())
        .or_insert_with(|| Arc::new(AtomicBool::new(false)))
        .clone()
}

fn timestamp() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
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

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SubagentLaneIdentity {
    pub(crate) lane_name: String,
    pub(crate) run_id: String,
    pub(crate) source_leaf_id: Option<String>,
    pub(crate) started_seq: u64,
}

#[derive(Debug)]
pub(crate) struct SubagentStartError {
    pub(crate) identity: Option<SubagentLaneIdentity>,
    pub(crate) error: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum InterruptedSubagentRecoveryState {
    Pending,
    Complete,
}

pub(crate) struct HarnessJournal {
    pub(crate) store: AgentHarness<JsonlStore>,
    pub(crate) cancellation: Arc<AtomicBool>,
}

#[allow(dead_code)]
impl HarnessJournal {
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
        let executor = move |action: threadlane_agent::harness::EffectAction| {
            let mut store = JsonlStore::open(&persist_path).map_err(|error| {
                threadlane_agent::harness::ReduceError::Storage(error.to_string())
            })?;
            if let Err(error) = action.apply(&mut store) {
                persist_events.publish(threadlane_agent::harness::EventPayload::Fault(
                    error.to_string(),
                ));
                return Err(error);
            }
            let (payload, lane, run_id, turn) = match &action {
                threadlane_agent::harness::EffectAction::AppendEntry { entry } => (
                    threadlane_agent::harness::EventPayload::EntryCommitted(entry.clone()),
                    Some(entry.lane.clone()),
                    None,
                    None,
                ),
                threadlane_agent::harness::EffectAction::AppendRecord { record, .. } => (
                    threadlane_agent::harness::EventPayload::RecordCommitted(record.clone()),
                    Some(record.lane().to_owned()),
                    record.run_id().map(str::to_owned),
                    record.turn(),
                ),
            };
            persist_events.publish_identified_with_turn(payload, lane, run_id, turn, None);
            Ok(())
        };
        let cancellation = harness_cancellation_state(path);
        JsonlStore::open(path)
            .map(|store| Self {
                store: AgentHarness::with_executor_and_hooks(store, events, executor, hooks),
                cancellation,
            })
            .map_err(|error| error.to_string())
    }

    pub(crate) fn append_message_to_path(path: &Path, message: AgentMessage) -> Result<(), String> {
        let mut journal = Self::open(path)?;
        journal.append_message(message)
    }

    pub(crate) async fn append_tool_intent_to_path(
        path: &Path,
        run_id: &str,
        tool_call_id: &str,
        tool_name: &str,
        effective_args: Value,
    ) -> Result<(), String> {
        let mut journal = Self::open(path)?;
        journal
            .append_tool_intent_after_hook(run_id, tool_call_id, tool_name, effective_args)
            .await
    }

    pub(crate) fn start_with_prompt(
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

    pub(crate) fn finish(
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

    pub(crate) fn schedule_retry(&mut self, run_id: &str, reason: &str) -> Result<u32, String> {
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
                            } if record_run_id.as_str() == run_id
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
        let seq = harness_next_seq(self.store.store());
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
    pub(crate) fn prepare_assistant_attempt(&mut self, run_id: &str) -> Result<String, String> {
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
                        threadlane_agent::ToolReplaySafety::Safe => HarnessToolReplaySafety::Safe,
                        threadlane_agent::ToolReplaySafety::Never => HarnessToolReplaySafety::Never,
                    },
                }],
            )
            .map_err(|error| error.to_string())?;
        self.store
            .drive_to_completion()
            .map_err(|error| error.to_string())
    }

    pub(crate) fn tool_started_on_lane(
        &mut self,
        lane: &str,
        run_id: &str,
        tool_call_id: &str,
        tool_name: &str,
        effective_args: Value,
    ) -> Result<(), String> {
        self.refresh()?;
        let result_entry_id = format!("subagent-result-{run_id}-{tool_call_id}");
        let assistant_entry_id = match self
            .store
            .entries()
            .iter()
            .rev()
            .find(|entry| {
                entry.lane == lane && matches!(entry.message, AgentMessage::Assistant { .. })
            })
            .map(|entry| entry.id.clone())
        {
            Some(id) => id,
            None => {
                let assistant_msg = AgentMessage::Assistant {
                    content: None,
                    tool_calls: None,
                    stop_reason: None,
                    deferred_handle: None,
                };
                self.append_message_to_lane(lane, run_id, assistant_msg)?
            }
        };
        let tool_index = self
            .store
            .records()
            .iter()
            .filter(|record| match record {
                HarnessRecord::ToolStarted {
                    run_id: r_id,
                    lane: r_lane,
                    ..
                } => r_id == run_id && r_lane == lane,
                _ => false,
            })
            .count();
        let record = HarnessRecord::ToolStarted {
            id: format!("tool-started-{run_id}-{tool_call_id}"),
            seq: harness_next_seq(self.store.store()),
            lane: lane.into(),
            timestamp: timestamp(),
            run_id: run_id.into(),
            assistant_entry_id,
            tool_index,
            tool_call_id: tool_call_id.into(),
            tool_name: tool_name.into(),
            effective_args,
            result_entry_id,
            replay: match threadlane_agent::classify_tool_replay_safety(tool_name) {
                threadlane_agent::ToolReplaySafety::Safe => HarnessToolReplaySafety::Safe,
                threadlane_agent::ToolReplaySafety::Never => HarnessToolReplaySafety::Never,
            },
        };
        self.store
            .append_record_gated(record)
            .map_err(|error| error.to_string())?;
        self.store
            .drive_to_completion()
            .map_err(|error| error.to_string())
    }
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

    pub(crate) fn next_seq(&self) -> u64 {
        self.store
            .entries()
            .iter()
            .map(|entry| entry.seq)
            .chain(self.store.records().iter().map(HarnessRecord::seq))
            .max()
            .unwrap_or(0)
            + 1
    }

    pub(crate) fn refresh(&mut self) -> Result<(), String> {
        let path = self.store.store().path().to_path_buf();
        let hooks = std::mem::take(self.store.hooks_mut());
        match Self::open(&path) {
            Ok(mut refreshed) => {
                *refreshed.store.hooks_mut() = hooks;
                self.store = refreshed.store;
                Ok(())
            }
            Err(error) => {
                *self.store.hooks_mut() = hooks;
                Err(error)
            }
        }
    }

    #[cfg(test)]
    pub(crate) fn start(
        &mut self,
        run_id: &str,
        source_leaf_id: Option<String>,
    ) -> Result<(), String> {
        self.refresh()?;
        self.store
            .start_operation(run_id, source_leaf_id, OperationIntent::Run)
            .map_err(|error| error.to_string())?;
        self.store
            .drive_to_completion()
            .map_err(|error| error.to_string())
    }

    #[cfg(test)]
    pub(crate) async fn run_before_tool_hook(
        &self,
        run_id: &str,
        tool_call_id: &str,
        tool_name: &str,
    ) -> Result<(), String> {
        let context = HookContext {
            session_id: self.store.session_id().to_owned(),
            lane: "main".into(),
            run_id: Some(run_id.into()),
            resume_data: None,
            tool_call_id: Some(tool_call_id.into()),
            tool_name: Some(tool_name.into()),
            tool_arguments: None,
            tool_result_content: None,
            tool_result_is_error: None,
        };
        self.store
            .hooks()
            .run_before_tool(&context)
            .await
            .map_err(|failures| {
                failures
                    .into_iter()
                    .map(|failure| {
                        format!(
                            "{} ({tool_call_id}/{tool_name}): {}",
                            failure.id, failure.message
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("; ")
            })
    }

    pub(crate) fn finish_operation(
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

    pub(crate) fn append_replayed_tool_entry(
        &mut self,
        run_id: &str,
        assistant_entry_id: &str,
        spec: &ToolSpec,
        result: &AgentToolResult,
    ) -> Result<(), String> {
        let state = Reducer::reduce(self.store.store()).map_err(|error| error.to_string())?;
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

    #[cfg(test)]
    pub(crate) async fn append_tool_intent(
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
        self.run_before_tool_hook(run_id, tool_call_id, tool_name)
            .await?;
        self.append_tool_intent_after_hook(run_id, tool_call_id, tool_name, effective_args)
            .await
    }

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
                    && matches!(
                        &entry.message,
                        threadlane_agent::AgentMessage::Assistant { .. }
                    )
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
            .filter(|entry| {
                matches!(&entry.message,
                AgentMessage::Assistant {
                    tool_calls: Some(tool_calls),
                    ..
                } if !tool_calls.is_empty())
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
        let mut pending_results = Vec::new();
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
            let persisted_termination = self
                .store
                .entries()
                .iter()
                .find(|entry| entry.id == *result_entry)
                .is_some_and(|entry| entry.terminate);
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
                .unwrap_or_else(|_| serde_json::Value::String(call.function.arguments.clone()));
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
            let finished = self.store.records().iter().any(|record| {
                matches!(record, HarnessRecord::ToolFinished {
                    run_id: record_run_id,
                    tool_call_id,
                    ..
                } if record_run_id == run_id && tool_call_id == &call.id)
            });
            if finished {
                continue;
            }
            pending_results.push(threadlane_agent::harness::ToolResult {
                call_id: call.id.clone(),
                name: name.to_string(),
                content: persisted_result.0,
                is_error: persisted_result.1,
                terminate: termination
                    .get(&call.id)
                    .copied()
                    .unwrap_or(persisted_termination),
            });
        }
        if !pending_results.is_empty() {
            self.store
                .finish_existing_tool_batch(run_id, &pending_results, TokenUsage::default())
                .map_err(|error| error.to_string())?;
            self.store
                .drive_to_completion()
                .map_err(|error| error.to_string())?;
        }
        Ok(())
    }

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
                matches!(&entry.message, AgentMessage::Assistant { .. }).then_some(entry.id.clone())
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
                    threadlane_agent::AgentMessage::Assistant {
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
                .append_entry_gated(threadlane_agent::harness::Entry {
                    id: entry_id.clone(),
                    parent_id: lane.leaf_id.clone(),
                    lane: "main".into(),
                    seq,
                    timestamp: timestamp(),
                    message: threadlane_agent::AgentMessage::Assistant {
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
        self.finish(
            &run_id,
            OperationOutcome::Aborted,
            Some("Generation cancelled".into()),
        )?;
        Ok(true)
    }

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
            self.finish(run_id, OperationOutcome::Completed, None)?;
        }
        Ok(terminal)
    }

    pub(crate) fn start_subagent_lane(
        &mut self,
        lane_hint: &str,
        task: &str,
        source_leaf_id: Option<&str>,
    ) -> Result<SubagentLaneIdentity, SubagentStartError> {
        if self.cancellation.load(Ordering::SeqCst) {
            return Err(SubagentStartError {
                identity: None,
                error: "Subagent start rejected because the parent is cancelling".into(),
            });
        }
        static START_LOCK: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
        let _start_lock = START_LOCK
            .get_or_init(|| std::sync::Mutex::new(()))
            .lock()
            .map_err(|error| SubagentStartError {
                identity: None,
                error: error.to_string(),
            })?;
        let mut attempt_idx = 0;
        let identity = loop {
            self.refresh().map_err(|error| SubagentStartError {
                identity: None,
                error: error.to_string(),
            })?;
            let used_ids = self
                .store
                .entries()
                .iter()
                .map(|entry| entry.id.clone())
                .chain(
                    self.store
                        .records()
                        .iter()
                        .flat_map(|record| [record.id().to_owned(), record.lane().to_owned()]),
                )
                .collect::<Vec<_>>();
            let generator = SessionIdGenerator::new(self.store.session_id());
            let base_run_id = generator.next("subagent-run", &used_ids);
            let run_id = if attempt_idx == 0 {
                base_run_id
            } else {
                format!("{base_run_id}-{attempt_idx}")
            };
            let mut lane_ids = used_ids.clone();
            lane_ids.push(run_id.clone());
            let base_lane = generator.next(lane_hint, &lane_ids);
            let lane_name = if attempt_idx == 0 {
                base_lane
            } else {
                format!("{base_lane}-{attempt_idx}")
            };
            let identity = SubagentLaneIdentity {
                lane_name: lane_name.clone(),
                run_id: run_id.clone(),
                source_leaf_id: source_leaf_id.map(str::to_owned),
                started_seq: 0,
            };
            if let Err(error) = self.store.start_operation_on_lane(
                &lane_name,
                &run_id,
                source_leaf_id.map(str::to_owned),
                OperationIntent::Run,
            ) {
                let err_str = error.to_string();
                if err_str.contains("DuplicateId") {
                    attempt_idx += 1;
                    continue;
                }
                if source_leaf_id.is_some()
                    && (err_str.contains("source leaf does not exist")
                        || err_str.contains("MissingParent"))
                {
                    if let Err(retry_err) = self.store.start_operation_on_lane(
                        &lane_name,
                        &run_id,
                        None,
                        OperationIntent::Run,
                    ) {
                        if retry_err.to_string().contains("DuplicateId") {
                            attempt_idx += 1;
                            continue;
                        }
                        return Err(SubagentStartError {
                            identity: None,
                            error: retry_err.to_string(),
                        });
                    }
                } else {
                    return Err(SubagentStartError {
                        identity: None,
                        error: err_str,
                    });
                }
            }
            break identity;
        };
        self.store
            .drive_to_completion()
            .map_err(|error| SubagentStartError {
                identity: Some(identity.clone()),
                error: error.to_string(),
            })?;
        let prompt_message = AgentMessage::user(task.to_owned(), Vec::new());
        let prompt_entry_id = format!("subagent-entry-{}-0", identity.run_id);
        let effective_parent_id = source_leaf_id
            .filter(|id| self.store.entries().iter().any(|e| e.id == *id))
            .map(str::to_owned);
        self.store
            .append_entry_gated(threadlane_agent::harness::Entry {
                id: prompt_entry_id,
                parent_id: effective_parent_id,
                lane: identity.lane_name.clone(),
                seq: harness_next_seq(self.store.store()),
                timestamp: timestamp(),
                message: prompt_message,
                terminate: false,
            })
            .map_err(|error| SubagentStartError {
                identity: Some(identity.clone()),
                error: error.to_string(),
            })?;
        self.store
            .drive_to_completion()
            .map_err(|error| SubagentStartError {
                identity: Some(identity.clone()),
                error: error.to_string(),
            })?;
        self.store
            .append_record_gated(HarnessRecord::StepAttempt {
                id: format!("assistant-attempt-action-{}-1", identity.run_id),
                seq: harness_next_seq(self.store.store()),
                lane: identity.lane_name.clone(),
                timestamp: timestamp(),
                run_id: identity.run_id.clone(),
                attempt: 1,
                result_entry_id: format!("entry-{}-assistant-1", identity.run_id),
                compaction_reason: None,
            })
            .map_err(|error| SubagentStartError {
                identity: Some(identity.clone()),
                error: error.to_string(),
            })?;
        self.store
            .drive_to_completion()
            .map_err(|error| SubagentStartError {
                identity: Some(identity.clone()),
                error: error.to_string(),
            })?;
        Ok(identity)
    }

    pub(crate) fn finish_subagent_lane(
        &mut self,
        _lane: &str,
        run_id: &str,
        outcome: OperationOutcome,
        error: Option<String>,
    ) -> Result<(), String> {
        self.refresh()?;
        let is_open = Reducer::reduce(self.store.store()).ok().map(|state| {
            state
                .lanes
                .iter()
                .any(|l| l.open_operation.as_deref() == Some(run_id))
        }) == Some(true);
        if !is_open {
            return Ok(());
        }

        if outcome == OperationOutcome::Aborted {
            let mut any_provisioned = false;
            if let Ok(state) = Reducer::reduce(self.store.store()) {
                if let Some(l) = state
                    .lanes
                    .iter()
                    .find(|l| l.open_operation.as_deref() == Some(run_id))
                {
                    for tool in &l.tools {
                        if !tool.completed
                            && tool.run_id == run_id
                            && !self
                                .store
                                .entries()
                                .iter()
                                .any(|entry| entry.id == tool.result_entry_id)
                        {
                            self.append_message_to_lane(
                                &l.name,
                                run_id,
                                AgentMessage::Tool {
                                    tool_call_id: tool.tool_call_id.clone(),
                                    name: tool.tool_name.clone(),
                                    content: error
                                        .clone()
                                        .unwrap_or_else(|| "Tool execution cancelled.".into()),
                                    is_error: true,
                                    terminate: false,
                                },
                            )?;
                            any_provisioned = true;
                        }
                    }
                }
            }
            if any_provisioned {
                let _ = self.refresh();
            }
            let _ = self.store.request_abort(run_id);
            let _ = self.store.drive_to_completion();
            let _ = self.refresh();
            if self.store.reconcile_abort_run(run_id).is_ok() {
                let _ = self.store.drive_to_completion();
                return Ok(());
            }
        }

        self.store
            .finish_operation(run_id, outcome, error)
            .map_err(|error| error.to_string())?;
        self.store
            .drive_to_completion()
            .map_err(|error| error.to_string())
    }

    pub(crate) fn checkpoint(
        &mut self,
        lane: &str,
        run_id: &str,
        messages: &[AgentMessage],
    ) -> Result<(), String> {
        if messages.is_empty() {
            return Ok(());
        }
        self.refresh()?;
        for message in messages {
            self.append_message_to_lane(lane, run_id, message.clone())?;
        }
        Ok(())
    }

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
                replay: threadlane_agent::ToolReplaySafety::Safe,
                ..
            } = tool
            else {
                continue;
            };
            let already_completed =
                records.iter().any(|record| {
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
}
