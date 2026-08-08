use async_trait::async_trait;
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
    SessionStore, Snapshot, StreamingState, Subscription, ToolRecovery,
    ToolReplaySafety as HarnessToolReplaySafety, ToolResult as HarnessToolResult, ToolSpec,
};
use threadlane_agent::{
    AgentMessage, AgentToolResult, SubagentRecoveryStatus, TokenUsage,
};

pub(crate) struct SubagentJournalAdapter {
    pub(crate) session_file: PathBuf,
    pub(crate) lane: String,
    pub(crate) run_id: String,
}

#[async_trait]
impl threadlane_agent::journal::AgentJournal for SubagentJournalAdapter {
    async fn record_assistant_message(&self, message: AgentMessage) -> Result<(), String> {
        let mut journal = HarnessJournal::open(&self.session_file)?;
        journal
            .append_message_to_lane(&self.lane, &self.run_id, message)
            .map(|_| ())
    }

    async fn record_tool_message(&self, message: AgentMessage) -> Result<(), String> {
        let mut journal = HarnessJournal::open(&self.session_file)?;
        journal.append_message_to_lane(&self.lane, &self.run_id, message.clone())?;
        journal.finish_tool_message(&self.run_id, &message)
    }

    async fn record_tool_intent(
        &self,
        tool_call_id: &str,
        tool_name: &str,
        arguments: &str,
    ) -> Result<(), String> {
        let effective_args =
            serde_json::from_str(arguments).map_err(|e| format!("invalid tool arguments: {e}"))?;
        let mut journal = HarnessJournal::open(&self.session_file)?;
        journal.tool_started_on_lane(
            &self.lane,
            &self.run_id,
            tool_call_id,
            tool_name,
            effective_args,
        )
    }

    async fn record_tool_completion(
        &self,
        _tool_call_id: &str,
        _terminate: bool,
    ) -> Result<(), String> {
        Ok(())
    }

    async fn record_provider_usage(&self, usage: TokenUsage) -> Result<(), String> {
        let mut journal = HarnessJournal::open(&self.session_file)?;
        journal.record_provider_usage(&self.run_id, usage)
    }

    async fn record_discarded_usage(&self, usage: TokenUsage) -> Result<(), String> {
        let mut journal = HarnessJournal::open(&self.session_file)?;
        journal.record_discarded_usage(&self.run_id, usage)
    }

    async fn record_streaming_state(&self, _state: StreamingState) -> Result<(), String> {
        Ok(())
    }

    async fn run_provider_hook(&self, _kind: HookKind) -> Result<(), String> {
        Ok(())
    }
}

pub(crate) struct HarnessJournalAdapter {
    pub(crate) session_file: PathBuf,
    pub(crate) active_run: Arc<std::sync::Mutex<Option<String>>>,
}

#[async_trait]
impl threadlane_agent::journal::AgentJournal for HarnessJournalAdapter {
    async fn record_assistant_message(&self, message: AgentMessage) -> Result<(), String> {
        let active = self.active_run.lock().ok().is_some_and(|r| r.is_some());
        if active {
            HarnessJournal::append_message_to_path(&self.session_file, message)
        } else {
            Ok(())
        }
    }

    async fn record_tool_message(&self, message: AgentMessage) -> Result<(), String> {
        if let Some(run_id) = self.active_run.lock().ok().and_then(|r| r.clone()) {
            let mut journal = HarnessJournal::open(&self.session_file)?;
            journal.append_message(message.clone())?;
            journal.finish_tool_message(&run_id, &message)
        } else {
            Ok(())
        }
    }

    async fn record_tool_intent(
        &self,
        tool_call_id: &str,
        tool_name: &str,
        arguments: &str,
    ) -> Result<(), String> {
        if let Some(run_id) = self.active_run.lock().ok().and_then(|r| r.clone()) {
            let effective_args = serde_json::from_str(arguments)
                .map_err(|e| format!("invalid tool arguments: {e}"))?;
            HarnessJournal::append_tool_intent_to_path(
                &self.session_file,
                &run_id,
                tool_call_id,
                tool_name,
                effective_args,
            )
            .await
        } else {
            Ok(())
        }
    }

    async fn record_tool_completion(
        &self,
        _tool_call_id: &str,
        _terminate: bool,
    ) -> Result<(), String> {
        Ok(())
    }

    async fn record_provider_usage(&self, usage: TokenUsage) -> Result<(), String> {
        if let Some(run_id) = self.active_run.lock().ok().and_then(|r| r.clone()) {
            let mut journal = HarnessJournal::open(&self.session_file)?;
            journal.record_provider_usage(&run_id, usage)
        } else {
            Ok(())
        }
    }

    async fn record_discarded_usage(&self, usage: TokenUsage) -> Result<(), String> {
        if let Some(run_id) = self.active_run.lock().ok().and_then(|r| r.clone()) {
            let mut journal = HarnessJournal::open(&self.session_file)?;
            journal.record_discarded_usage(&run_id, usage)
        } else {
            Ok(())
        }
    }

    async fn record_streaming_state(&self, mut state: StreamingState) -> Result<(), String> {
        let run_id = self.active_run.lock().ok().and_then(|r| r.clone());
        let empty = state.assistant_text.is_empty()
            && state.reasoning.is_empty()
            && state.tool_call_ids.is_empty();
        if empty {
            harness_event_hub(&self.session_file).publish_streaming(None);
        } else {
            state.lane = "main".into();
            state.run_id = run_id;
            harness_event_hub(&self.session_file).publish_streaming(Some(state));
        }
        Ok(())
    }

    async fn run_provider_hook(&self, kind: HookKind) -> Result<(), String> {
        if let Some(run_id) = self.active_run.lock().ok().and_then(|r| r.clone()) {
            let mut journal = HarnessJournal::open(&self.session_file)?;
            if kind == HookKind::BeforeRequest {
                journal.prepare_assistant_attempt(&run_id)?;
            }
            let context = HookContext {
                session_id: String::new(),
                lane: "main".into(),
                run_id: Some(run_id),
                resume_data: None,
                tool_call_id: None,
                tool_name: None,
                tool_arguments: None,
                tool_result_content: None,
                tool_result_is_error: None,
            };
            let failures = journal.store.hooks().run(kind, &context).await;
            for failure in &failures {
                eprintln!("provider hook {} failed: {}", failure.id, failure.message);
            }
            Ok(())
        } else {
            Ok(())
        }
    }
}

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

pub(crate) struct HarnessJournal {
    pub(crate) store: AgentHarness<JsonlStore>,
    pub(crate) cancellation: Arc<AtomicBool>,
}

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

    pub(crate) fn start_with_prompt(&mut self, run_id: &str, prompt: AgentMessage) -> Result<(), String> {
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

    pub(crate) fn finish_tool_message(&mut self, run_id: &str, message: &AgentMessage) -> Result<(), String> {
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
    pub(crate) fn record_provider_usage(&mut self, run_id: &str, usage: TokenUsage) -> Result<(), String> {
        self.refresh()?;
        self.store
            .record_provider_usage(run_id, usage)
            .map_err(|error| error.to_string())?;
        self.store
            .drive_to_completion()
            .map_err(|error| error.to_string())
    }

    pub(crate) fn record_discarded_usage(&mut self, run_id: &str, usage: TokenUsage) -> Result<(), String> {
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
}
