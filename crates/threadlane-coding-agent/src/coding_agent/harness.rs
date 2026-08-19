use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::permission::PermissionTraceEvent;
use threadlane_agent::harness::{
    AbortInitiator, AbortObservation, AbortTarget, AgentHarness, BoundedText, CapabilitySnapshot,
    DeferredResolution, EffectAction, Entry as HarnessEntry, ErrorCategory, EventPayload,
    HarnessEventHub, HookContext, HookKind, HookRegistry, JsonlStore, OperationOutcome,
    PromptSnapshot, ProviderErrorSummary, ProviderOutcome, Record as HarnessRecord, ReduceError,
    Reducer, RetryPolicy, SessionIdGenerator, SessionStore, Snapshot, SubagentLifecyclePhase,
    ToolExecutionOutcome, ToolExecutionPhase, ToolReplaySafety as HarnessToolReplaySafety,
    ToolResult as HarnessToolResult, ToolSpec, TraceString,
};
use threadlane_agent::session_tree::SessionTree;
use threadlane_agent::{
    AgentMessage, AgentToolResult, ProviderTraceEvent, ReasoningEffort, TokenUsage,
    ToolExecutionTraceEvent,
};

use threadlane_agent::harness::{EventError, HarnessEvent, OperationIntent, Subscription};

pub struct HarnessWatch {
    pub(crate) hub: HarnessEventHub,
    pub(crate) subscription: Subscription,
}

impl HarnessWatch {
    pub fn snapshot(&self) -> &Snapshot {
        &self.subscription.snapshot
    }

    pub(crate) fn poll(&mut self) -> Result<Vec<HarnessEvent>, EventError> {
        self.hub.poll(&mut self.subscription)
    }
}

#[derive(Clone)]
struct HarnessSessionEntry {
    hub: HarnessEventHub,
    hooks: HookRegistry,
    cancellation: Arc<AtomicBool>,
}

fn harness_session_entry(path: &Path) -> HarnessSessionEntry {
    static SESSIONS: std::sync::OnceLock<std::sync::Mutex<HashMap<PathBuf, HarnessSessionEntry>>> =
        std::sync::OnceLock::new();
    let sessions = SESSIONS.get_or_init(|| std::sync::Mutex::new(HashMap::new()));
    let mut sessions = sessions.lock().unwrap_or_else(|error| error.into_inner());
    sessions
        .entry(path.to_path_buf())
        .or_insert_with(|| HarnessSessionEntry {
            hub: HarnessEventHub::new(256),
            hooks: HookRegistry::default(),
            cancellation: Arc::new(AtomicBool::new(false)),
        })
        .clone()
}

fn harness_event_hub(path: &Path) -> HarnessEventHub {
    harness_session_entry(path).hub
}

fn harness_hook_registry(path: &Path) -> HookRegistry {
    harness_session_entry(path).hooks
}

pub(crate) fn harness_cancellation_state(path: &Path) -> Arc<AtomicBool> {
    harness_session_entry(path).cancellation
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

/// Proof that a run prompt has been committed to the canonical session log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AcceptedRun {
    pub(crate) run_id: String,
    pub(crate) lane: String,
    pub(crate) prompt_entry_id: String,
    pub(crate) assistant_entry_id: String,
    pub(crate) accepted_through_seq: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum InterruptedSubagentRecoveryState {
    Pending,
    Complete,
}

/// Owns the durable session store, the `main` lane handle, event hub, hook
/// registry, cancellation state, and a subscription for event projection.
/// Every foreground operation enters the harness through this adapter;
/// there is no second persistence path.
#[allow(dead_code)]
pub(crate) struct CodingSessionHarness {
    pub(crate) store: AgentHarness<JsonlStore>,
    pub(crate) session_path: PathBuf,
    pub(crate) main_lane_name: String,
    pub(crate) events: HarnessEventHub,
    pub(crate) hooks: HookRegistry,
    pub(crate) cancellation: Arc<AtomicBool>,
}

#[allow(dead_code)]
pub(crate) type HarnessJournal = CodingSessionHarness;

#[allow(dead_code)]
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
                AgentHarness::with_executor_and_hooks(
                    store,
                    events.clone(),
                    executor,
                    hooks.clone(),
                )
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

    pub(crate) fn append_message_to_path(path: &Path, message: AgentMessage) -> Result<(), String> {
        let mut journal = Self::open(path)?;
        journal.append_message(message).map(|_| ())
    }

    pub(crate) fn capture_run_context(
        &mut self,
        run_id: &str,
        lane: &str,
        model: String,
        provider: String,
        reasoning_effort: ReasoningEffort,
        prompt_cache_enabled: bool,
        work_dir: String,
        system_prompt: PromptSnapshot,
        tool_schema_sha256: String,
        enabled_tool_names: Vec<String>,
        capabilities: Vec<String>,
        capability_sha256: Option<String>,
        prompt_template_ids: Vec<String>,
        git_head: Option<String>,
    ) -> Result<(), String> {
        self.refresh()?;
        let trace = |value: String| TraceString::new(value);
        let record = HarnessRecord::RunContextCaptured {
            id: format!("run-context-{run_id}"),
            seq: harness_next_seq(self.store.store()),
            lane: lane.into(),
            timestamp: timestamp(),
            run_id: run_id.into(),
            attempt: None,
            model: trace(model)?,
            provider: trace(provider)?,
            reasoning_effort,
            prompt_cache_enabled,
            work_dir: trace(work_dir)?,
            system_prompt,
            tool_schema_sha256: trace(tool_schema_sha256)?,
            enabled_tool_names: enabled_tool_names
                .into_iter()
                .take(256)
                .map(TraceString::new)
                .collect::<Result<Vec<_>, _>>()?,
            capabilities: CapabilitySnapshot {
                capabilities: capabilities
                    .into_iter()
                    .take(256)
                    .map(TraceString::new)
                    .collect::<Result<Vec<_>, _>>()?,
                fingerprint: capability_sha256.map(TraceString::new).transpose()?,
            },
            prompt_template_ids: prompt_template_ids
                .into_iter()
                .take(256)
                .map(TraceString::new)
                .collect::<Result<Vec<_>, _>>()?,
            git_head: git_head.map(TraceString::new).transpose()?,
        };
        self.store
            .append_record_gated(record)
            .map_err(|error| error.to_string())?;
        self.store
            .drive_to_completion()
            .map_err(|error| error.to_string())
    }

    pub(crate) fn record_provider_trace_to_path(
        path: &Path,
        run_id: &str,
        event: ProviderTraceEvent,
    ) -> Result<(), String> {
        let mut journal = Self::open(path)?;
        journal.refresh()?;
        let event = match event {
            ProviderTraceEvent::AssistantReady {
                attempt,
                request_id,
                reasoning,
                message,
            } => {
                let reasoning_entry_id = if let Some(reasoning) =
                    reasoning.filter(|reasoning| !reasoning.trim().is_empty())
                {
                    Some(journal.append_message(AgentMessage::Custom {
                        custom_type: "thinking".into(),
                        payload: serde_json::json!({ "text": reasoning }),
                    })?)
                } else {
                    None
                };
                let entry_id = journal.append_message(message)?;
                let seq = harness_next_seq(journal.store.store());
                let record = HarnessRecord::ProviderResponseAttached {
                    id: format!("provider-response-{run_id}-{request_id}"),
                    seq,
                    lane: "main".into(),
                    timestamp: timestamp(),
                    run_id: run_id.into(),
                    attempt,
                    request_id: Some(TraceString::new(request_id)?),
                    entry_id,
                    reasoning_entry_id,
                };
                journal
                    .store
                    .append_record_gated(record)
                    .map_err(|error| error.to_string())?;
                journal
                    .store
                    .drive_to_completion()
                    .map_err(|error| error.to_string())?;
                return Ok(());
            }
            event => event,
        };
        let seq = harness_next_seq(journal.store.store());
        let record = match event {
            ProviderTraceEvent::AssistantReady { .. } => unreachable!(),
            ProviderTraceEvent::Started {
                attempt,
                request_id,
                model,
                provider,
            } => HarnessRecord::ProviderRequestStarted {
                id: format!("provider-start-{run_id}-{request_id}"),
                seq,
                lane: "main".into(),
                timestamp: timestamp(),
                run_id: run_id.into(),
                attempt,
                provider: TraceString::new(provider)?,
                model: TraceString::new(model)?,
                request_id: Some(TraceString::new(request_id)?),
            },
            ProviderTraceEvent::Checkpoint {
                attempt,
                request_id,
                checkpoint_index,
                text,
                reasoning,
            } => {
                let mut digest = Sha256::new();
                digest.update(text.as_bytes());
                if let Some(reasoning) = reasoning.as_deref() {
                    digest.update(reasoning.as_bytes());
                }
                HarnessRecord::StreamCheckpoint {
                    id: format!("stream-checkpoint-{run_id}-{request_id}-{checkpoint_index}"),
                    seq,
                    lane: "main".into(),
                    timestamp: timestamp(),
                    run_id: run_id.into(),
                    attempt: Some(attempt),
                    request_id: TraceString::new(request_id)?,
                    assistant_entry_id: None,
                    text: (!text.is_empty()).then(|| BoundedText::truncated(&text)),
                    reasoning: reasoning
                        .as_deref()
                        .filter(|reasoning| !reasoning.is_empty())
                        .map(BoundedText::truncated),
                    checkpoint_index,
                    byte_count: text.len() as u64
                        + reasoning.as_ref().map_or(0, String::len) as u64,
                    fingerprint: TraceString::new(format!("{:x}", digest.finalize()))?,
                }
            }
            ProviderTraceEvent::Finished {
                attempt,
                request_id,
                outcome,
                error,
                duration_ms,
                usage,
            } => HarnessRecord::ProviderRequestFinished {
                id: format!("provider-finish-{run_id}-{request_id}"),
                seq,
                lane: "main".into(),
                timestamp: timestamp(),
                run_id: run_id.into(),
                attempt,
                request_id: Some(TraceString::new(request_id)?),
                outcome,
                error,
                duration_ms: Some(duration_ms),
                usage,
            },
        };
        journal
            .store
            .append_record_gated(record)
            .map_err(|error| error.to_string())?;
        journal
            .store
            .drive_to_completion()
            .map_err(|error| error.to_string())
    }

    pub(crate) fn record_permission_trace_to_path(
        path: &Path,
        run_id: Option<&str>,
        event: PermissionTraceEvent,
    ) -> Result<(), String> {
        let mut journal = Self::open(path)?;
        journal.refresh()?;
        let state = Reducer::reduce(journal.store.store()).map_err(|error| error.to_string())?;
        let attempt = run_id.and_then(|_| state.lane("main").map(|lane| lane.attempts));
        let seq = harness_next_seq(journal.store.store());
        let record = match event {
            PermissionTraceEvent::Requested {
                request_id,
                capability,
                scopes,
                detail_sha256,
                source,
            } => HarnessRecord::PermissionRequested {
                id: format!("permission-request-{request_id}-{seq}"),
                seq,
                lane: "main".into(),
                timestamp: timestamp(),
                run_id: run_id.map(str::to_owned),
                attempt,
                request_id: TraceString::new(request_id)?,
                capability: TraceString::new(capability)?,
                scopes,
                detail_sha256: TraceString::new(detail_sha256)?,
                source,
            },
            PermissionTraceEvent::Resolved {
                request_id,
                decision,
                scope,
                source,
                remembered,
            } => HarnessRecord::PermissionResolved {
                id: format!("permission-resolved-{request_id}-{seq}"),
                seq,
                lane: "main".into(),
                timestamp: timestamp(),
                run_id: run_id.map(str::to_owned),
                attempt,
                request_id: TraceString::new(request_id)?,
                decision,
                scope,
                source,
                remembered,
            },
        };
        journal
            .store
            .append_record_gated(record)
            .map_err(|error| error.to_string())?;
        journal
            .store
            .drive_to_completion()
            .map_err(|error| error.to_string())
    }

    pub(crate) async fn record_tool_execution_to_path(
        path: &Path,
        run_id: &str,
        event: ToolExecutionTraceEvent,
    ) -> Result<(), String> {
        let mut journal = Self::open(path)?;
        journal.refresh()?;
        if let ToolExecutionTraceEvent::Started {
            tool_call_id,
            tool_name,
            effective_arguments,
            ..
        } = &event
        {
            let has_intent = journal.store.records().iter().any(|record| {
                matches!(
                    record,
                    HarnessRecord::ToolStarted {
                        run_id: intent_run_id,
                        tool_call_id: intent_call_id,
                        ..
                    } if intent_run_id == run_id && intent_call_id == tool_call_id
                )
            });
            if !has_intent {
                let effective_args = serde_json::from_str(effective_arguments)
                    .unwrap_or_else(|_| Value::String(effective_arguments.clone()));
                journal
                    .append_tool_intent_after_hook(run_id, tool_call_id, tool_name, effective_args)
                    .await?;
                journal.refresh()?;
            }
        }
        let state = Reducer::reduce(journal.store.store()).map_err(|error| error.to_string())?;
        let attempt = state.lane("main").map(|lane| lane.attempts);
        let seq = harness_next_seq(journal.store.store());
        let record = match event {
            ToolExecutionTraceEvent::Started {
                tool_call_id,
                tool_name,
                executor_kind,
                effective_arguments: _,
                started_at_ms,
            } => HarnessRecord::ToolExecutionObserved {
                id: format!("tool-execution-start-{run_id}-{tool_call_id}-{seq}"),
                seq,
                lane: "main".into(),
                timestamp: timestamp(),
                run_id: run_id.into(),
                attempt,
                tool_call_id: TraceString::new(tool_call_id)?,
                tool_name: TraceString::new(tool_name)?,
                executor_kind: TraceString::new(executor_kind)?,
                phase: ToolExecutionPhase::Started,
                started_at_ms: Some(started_at_ms),
                duration_ms: None,
                outcome: None,
                exit_code: None,
                cancelled: false,
                is_error: None,
                terminate: None,
                output_sha256: None,
                output_bytes: None,
            },
            ToolExecutionTraceEvent::Finished {
                tool_call_id,
                tool_name,
                executor_kind,
                started_at_ms,
                duration_ms,
                is_error,
                terminate,
                output_sha256,
                output_bytes,
            } => HarnessRecord::ToolExecutionObserved {
                id: format!("tool-execution-finish-{run_id}-{tool_call_id}-{seq}"),
                seq,
                lane: "main".into(),
                timestamp: timestamp(),
                run_id: run_id.into(),
                attempt,
                tool_call_id: TraceString::new(tool_call_id)?,
                tool_name: TraceString::new(tool_name)?,
                executor_kind: TraceString::new(executor_kind)?,
                phase: ToolExecutionPhase::Finished,
                started_at_ms: Some(started_at_ms),
                duration_ms: Some(duration_ms),
                outcome: Some(if is_error {
                    ToolExecutionOutcome::Failed
                } else {
                    ToolExecutionOutcome::Succeeded
                }),
                exit_code: None,
                cancelled: false,
                is_error: Some(is_error),
                terminate: Some(terminate),
                output_sha256: Some(TraceString::new(output_sha256)?),
                output_bytes: Some(output_bytes),
            },
        };
        journal
            .store
            .append_record_gated(record)
            .map_err(|error| error.to_string())?;
        journal
            .store
            .drive_to_completion()
            .map_err(|error| error.to_string())
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

    pub(crate) async fn record_tool_result_to_path(
        path: &Path,
        run_id: &str,
        result: &AgentToolResult,
    ) -> Result<(), String> {
        let mut journal = Self::open(path)?;
        journal.finish_tool_result(run_id, result)
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
            let mut identity = SubagentLaneIdentity {
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
                    identity.source_leaf_id = None;
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
            .append_entry_gated(HarnessEntry {
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
        let state = Reducer::reduce(self.store.store()).map_err(|error| SubagentStartError {
            identity: Some(identity.clone()),
            error: error.to_string(),
        })?;
        let parent_run_id = state
            .lane("main")
            .and_then(|lane| lane.open_operation.clone());
        let parent_attempt = state.lane("main").map(|lane| lane.attempts);
        let seq = harness_next_seq(self.store.store());
        self.store
            .append_record_gated(HarnessRecord::SubagentLifecycle {
                id: format!("subagent-started-{}-{seq}", identity.run_id),
                seq,
                lane: "main".into(),
                timestamp: timestamp(),
                run_id: parent_run_id,
                attempt: parent_attempt,
                child_run_id: TraceString::new(identity.run_id.clone()).map_err(|error| {
                    SubagentStartError {
                        identity: Some(identity.clone()),
                        error,
                    }
                })?,
                parent_tool_call_id: None,
                task_index: None,
                agent_id: TraceString::new(lane_hint).map_err(|error| SubagentStartError {
                    identity: Some(identity.clone()),
                    error,
                })?,
                subagent_lane: TraceString::new(identity.lane_name.clone()).map_err(|error| {
                    SubagentStartError {
                        identity: Some(identity.clone()),
                        error,
                    }
                })?,
                phase: SubagentLifecyclePhase::Started,
                result_entry_id: None,
                error: None,
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

    // ── Run lifecycle ─────────────────────────────────────────────────


    /// Start a foreground operation and accept the user prompt.
    ///
    /// Returns `Ok(())` after `accept_prompt` is driven to completion
    /// (committed to the JSONL store).
    pub(crate) fn begin_run(
        &mut self,
        run_id: &str,
        prompt: AgentMessage,
    ) -> Result<AcceptedRun, String> {
        self.refresh()?;
        let assistant_entry_id = self
            .store
            .accept_prompt(run_id, prompt)
            .map_err(|error| error.to_string())?;
        self.store
            .drive_to_completion()
            .map_err(|error| error.to_string())?;
        let accepted_through_seq = self
            .store
            .store()
            .entries()
            .iter()
            .chain(std::iter::empty())
            .map(|entry| entry.seq)
            .chain(self.store.store().records().iter().map(|record| record.seq()))
            .max()
            .unwrap_or(0);
        Ok(AcceptedRun {
            run_id: run_id.to_owned(),
            lane: self.main_lane_name.clone(),
            prompt_entry_id: format!("entry-{run_id}-user"),
            assistant_entry_id,
            accepted_through_seq,
        })
    }

    /// Append a tool intent.
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

    /// Start a foreground operation with an optional prompt.
    pub(crate) fn start(
        &mut self,
        run_id: &str,
        prompt: Option<AgentMessage>,
    ) -> Result<(), String> {
        self.refresh()?;
        self.store
            .start_operation(run_id, None, OperationIntent::Run)
            .map_err(|error| error.to_string())?;
        if let Some(msg) = prompt {
            self.store
                .accept_prompt(run_id, msg)
                .map_err(|error| error.to_string())?;
        }
        self.store
            .drive_to_completion()
            .map_err(|error| error.to_string())
    }

    /// Finish an operation with the given outcome and optional error.
    pub(crate) fn finish(
        &mut self,
        run_id: &str,
        outcome: OperationOutcome,
        error: Option<String>,
    ) -> Result<(), String> {
        self.finish_run(run_id, outcome, error)
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

    pub(crate) fn observe_abort_signal(
        &mut self,
        run_id: &str,
        acknowledged: bool,
    ) -> Result<(), String> {
        self.refresh()?;
        let state = Reducer::reduce(&self.store).map_err(|error| error.to_string())?;
        let attempt = state.lane("main").map(|lane| lane.attempts);
        let unfinished_requests = self
            .store
            .records()
            .iter()
            .filter_map(|record| match record {
                HarnessRecord::ProviderRequestStarted {
                    run_id: provider_run_id,
                    attempt,
                    request_id: Some(request_id),
                    ..
                } if provider_run_id == run_id
                    && !self.store.records().iter().any(|candidate| {
                        matches!(
                            candidate,
                            HarnessRecord::ProviderRequestFinished {
                                run_id: finished_run_id,
                                request_id: Some(finished_request_id),
                                ..
                            } if finished_run_id == run_id && finished_request_id == request_id
                        )
                    }) =>
                {
                    Some((*attempt, request_id.clone()))
                }
                _ => None,
            })
            .collect::<Vec<_>>();
        for (provider_attempt, request_id) in unfinished_requests {
            let seq = harness_next_seq(self.store.store());
            self.store
                .append_record_gated(HarnessRecord::ProviderRequestFinished {
                    id: format!("provider-finish-{run_id}-{}", request_id.as_str()),
                    seq,
                    lane: "main".into(),
                    timestamp: timestamp(),
                    run_id: run_id.into(),
                    attempt: provider_attempt,
                    request_id: Some(request_id),
                    outcome: ProviderOutcome::Aborted,
                    error: Some(ProviderErrorSummary {
                        category: ErrorCategory::Cancelled,
                        code: TraceString::new("runtime_abort").ok(),
                        retryable: false,
                    }),
                    duration_ms: None,
                    usage: None,
                })
                .map_err(|error| error.to_string())?;
            self.store
                .drive_to_completion()
                .map_err(|error| error.to_string())?;
        }
        let seq = harness_next_seq(self.store.store());
        self.store
            .append_record_gated(HarnessRecord::AbortObserved {
                id: format!("abort-observed-{run_id}-{seq}"),
                seq,
                lane: "main".into(),
                timestamp: timestamp(),
                run_id: run_id.into(),
                attempt,
                observation: AbortObservation::SignalSent,
                initiator: AbortInitiator::User,
                target: AbortTarget::ActiveRun,
                acknowledged,
                detail: None,
            })
            .map_err(|error| error.to_string())?;
        self.store
            .drive_to_completion()
            .map_err(|error| error.to_string())
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
    pub(crate) fn append_message(&mut self, message: AgentMessage) -> Result<String, String> {
        self.append_message_inner(message, true)
    }

    /// Append a message discovered while reconciling the provider transcript.
    ///
    /// Transcript synchronization already determines whether this is a new
    /// occurrence.  It must not apply the legacy last-entry content check,
    /// because two consecutive provider messages can legitimately have the
    /// same serialized value.
    fn append_synced_message(&mut self, message: AgentMessage) -> Result<String, String> {
        self.append_message_inner(message, false)
    }

    fn append_message_inner(
        &mut self,
        message: AgentMessage,
        deduplicate_last_entry: bool,
    ) -> Result<String, String> {
        self.refresh()?;
        if deduplicate_last_entry {
            if let Some(entry) = self.store.entries().last() {
                if entry.message == message {
                    return Ok(entry.id.clone());
                }
            }
        }
        let state = Reducer::reduce(&self.store).ok();
        let latest_main_entry = || {
            self.store
                .entries()
                .iter()
                .rev()
                .find(|entry| entry.lane == "main")
                .map(|entry| entry.id.clone())
        };
        let parent_id = state
            .as_ref()
            .and_then(|state| state.lane("main"))
            .and_then(|lane| {
                if lane.open_operation.is_some() {
                    latest_main_entry().or_else(|| lane.leaf_id.clone())
                } else {
                    lane.leaf_id.clone()
                }
            })
            .or_else(|| latest_main_entry());
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
                id: id.clone(),
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
            .map_err(|error| error.to_string())?;
        Ok(id)
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
                entry.seq > start_seq && matches!(&entry.message, AgentMessage::Assistant { .. })
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

    /// Finish a freshly executed tool result: record the tool result Entry, ToolFinished, and drive effects.
    pub(crate) fn finish_tool_result(
        &mut self,
        run_id: &str,
        result: &AgentToolResult,
    ) -> Result<(), String> {
        self.refresh()?;
        self.store
            .finish_tool(
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
            .filter(|entry| entry.seq > assistant.seq)
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
    pub(crate) fn accept_compaction(&mut self, run_id: &str, summary: &str) -> Result<(), String> {
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
    pub(crate) fn set_fact(&mut self, lane: &str, key: &str, value: String) -> Result<(), String> {
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
        let state = Reducer::reduce(self.store.store()).map_err(|error| error.to_string())?;
        let lane = state
            .lanes
            .iter()
            .find(|lane| lane.open_operation.as_deref() == Some(run_id))
            .ok_or_else(|| format!("harness operation {run_id} is not open"))?;
        let parent_id = if spec.index == 0 {
            assistant_entry_id.to_string()
        } else {
            state
                .lanes
                .iter()
                .flat_map(|lane| lane.tools.iter())
                .find(|tool| {
                    tool.run_id == run_id
                        && tool.assistant_entry_id == assistant_entry_id
                        && tool.tool_index + 1 == spec.index
                })
                .filter(|tool| {
                    self.store
                        .entries()
                        .iter()
                        .any(|entry| entry.id == tool.result_entry_id)
                })
                .map(|tool| tool.result_entry_id.clone())
                .unwrap_or_else(|| assistant_entry_id.to_string())
        };
        let seq = self.next_seq();
        self.store
            .append_entry_gated(HarnessEntry {
                id: spec.result_entry_id.clone(),
                parent_id: Some(parent_id),
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
        self.refresh()?;
        // The provider gives us the complete conversation, not stable entry
        // IDs.  Track occurrences rather than using a set: two turns can
        // legitimately produce byte-for-byte identical assistant messages,
        // including an empty assistant result.
        let mut existing: HashMap<String, usize> = HashMap::new();
        for entry in self
            .store
            .model_context("main")
            .map_err(|error| error.to_string())?
            .entries
        {
            *existing.entry(format!("{:?}", entry.message)).or_default() += 1;
        }

        for msg in messages {
            if matches!(msg, AgentMessage::System { .. }) {
                continue;
            }
            // Initial prompts are already present through begin_run, while
            // queued/steered/generated user messages may exist only in the
            // provider transcript. Occurrence matching handles both cases.
            let key = format!("{:?}", msg);
            if let Some(count) = existing.get_mut(&key) {
                if *count > 0 {
                    *count -= 1;
                    continue;
                }
            }
            if let AgentMessage::Tool { tool_call_id, .. } = msg {
                self.refresh()?;
                let unfinished_tool =
                    Reducer::reduce(self.store.store()).ok().and_then(|state| {
                        let lane = state.lane("main")?;
                        let run_id = lane.open_operation.as_deref()?;
                        lane.tools.iter().find_map(|tool| {
                            (tool.run_id == run_id
                                && tool.tool_call_id == *tool_call_id
                                && !tool.completed)
                                .then(|| (run_id.to_owned(), tool.result_entry_id.clone()))
                        })
                    });
                if let Some((run_id, result_entry_id)) = unfinished_tool {
                    // ToolStarted may be durable while its result entry is
                    // not, if the process was interrupted between those
                    // writes. Recreate the entry before closing the intent.
                    if !self
                        .store
                        .entries()
                        .iter()
                        .any(|entry| entry.id == result_entry_id)
                    {
                        self.append_synced_message(msg.clone())?;
                    }
                    self.finish_tool_message(&run_id, msg)?;
                    continue;
                }
            }
            self.append_synced_message(msg.clone())?;
        }
        Ok(())
    }
    pub(crate) fn assert_model_visible(&mut self, messages: &[AgentMessage]) -> Result<(), String> {
        self.refresh()?;
        let logged = self
            .store
            .model_context("main")
            .map_err(|error| error.to_string())?
            .messages();
        let expected = messages
            .iter()
            .filter(|message| !matches!(message, AgentMessage::System { .. }))
            .cloned()
            .collect::<Vec<_>>();
        if logged == expected {
            return Ok(());
        }
        let mismatch = logged
            .iter()
            .zip(expected.iter())
            .position(|(logged, expected)| logged != expected)
            .unwrap_or_else(|| logged.len().min(expected.len()));
        Err(format!(
            "model-visible history diverges at index {mismatch}: durable_count={}, provider_count={}",
            logged.len(),
            expected.len()
        ))
    }

    /// Run hooks of the given kind for the main lane.
    pub(crate) async fn run_hooks(&self, kind: HookKind, context: &HookContext) {
        for failure in self.store.hooks().run(kind, context).await {
            eprintln!(
                "hook {} ({:?}) failed: {}",
                failure.id, kind, failure.message
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

    #[tokio::test]
    async fn tool_intent_precedes_physical_execution_observation() {
        let (_dir, path) = temp_session();
        let mut harness = CodingSessionHarness::open(&path).unwrap();
        harness
            .begin_run("run-1", AgentMessage::user("prompt", vec![]))
            .unwrap();
        harness.prepare_assistant_attempt("run-1").unwrap();
        CodingSessionHarness::record_provider_trace_to_path(
            &path,
            "run-1",
            ProviderTraceEvent::AssistantReady {
                attempt: 1,
                request_id: "request-1".into(),
                reasoning: None,
                message: AgentMessage::Assistant {
                    content: None,
                    tool_calls: Some(vec![threadlane_provider::openai::ToolCall {
                        id: "call-1".into(),
                        r#type: "function".into(),
                        function: threadlane_provider::openai::ToolCallFunction {
                            name: "read_file".into(),
                            arguments: r#"{"path":"README.md"}"#.into(),
                        },
                        thought_signature: None,
                    }]),
                    stop_reason: None,
                    deferred_handle: None,
                },
            },
        )
        .unwrap();

        CodingSessionHarness::record_tool_execution_to_path(
            &path,
            "run-1",
            ToolExecutionTraceEvent::Started {
                tool_call_id: "call-1".into(),
                tool_name: "read_file".into(),
                executor_kind: "builtin".into(),
                effective_arguments: r#"{"path":"README.md"}"#.into(),
                started_at_ms: 10,
            },
        )
        .await
        .unwrap();

        let store = JsonlStore::open(&path).unwrap();
        let intent_seq = store.records().iter().find_map(|record| match record {
            HarnessRecord::ToolStarted { seq, .. } => Some(*seq),
            _ => None,
        });
        let observed_seq = store.records().iter().find_map(|record| match record {
            HarnessRecord::ToolExecutionObserved { seq, .. } => Some(*seq),
            _ => None,
        });
        assert!(matches!(
            (intent_seq, observed_seq),
            (Some(intent), Some(observed)) if intent < observed
        ));
    }

    #[test]
    fn provider_attempt_trace_has_one_ordered_terminal_record() {
        let (_dir, path) = temp_session();
        let mut harness = CodingSessionHarness::open(&path).unwrap();
        harness
            .begin_run("run-1", AgentMessage::user("prompt", vec![]))
            .unwrap();

        CodingSessionHarness::record_provider_trace_to_path(
            &path,
            "run-1",
            ProviderTraceEvent::Started {
                attempt: 1,
                request_id: "request-1".into(),
                model: "test-model".into(),
                provider: "openai".into(),
            },
        )
        .unwrap();
        CodingSessionHarness::record_provider_trace_to_path(
            &path,
            "run-1",
            ProviderTraceEvent::Finished {
                attempt: 1,
                request_id: "request-1".into(),
                outcome: threadlane_agent::harness::ProviderOutcome::Completed,
                error: None,
                duration_ms: 12,
                usage: Some(TokenUsage {
                    input_tokens: 3,
                    output_tokens: 2,
                    total_tokens: 5,
                    ..Default::default()
                }),
            },
        )
        .unwrap();

        let store = JsonlStore::open(&path).unwrap();
        let start_seq = store.records().iter().find_map(|record| match record {
            HarnessRecord::ProviderRequestStarted {
                seq, request_id, ..
            } if request_id.as_ref().map(TraceString::as_str) == Some("request-1") => Some(*seq),
            _ => None,
        });
        let finishes = store
            .records()
            .iter()
            .filter_map(|record| match record {
                HarnessRecord::ProviderRequestFinished {
                    seq,
                    request_id,
                    usage,
                    ..
                } if request_id.as_ref().map(TraceString::as_str) == Some("request-1") => {
                    assert_eq!(usage.as_ref().map(|usage| usage.total_tokens), Some(5));
                    Some(*seq)
                }
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(finishes.len(), 1);
        assert!(start_seq.is_some_and(|seq| seq < finishes[0]));
    }

    #[test]
    fn cancellation_closes_an_unfinished_provider_attempt_before_abort_observation() {
        let (_dir, path) = temp_session();
        let mut harness = CodingSessionHarness::open(&path).unwrap();
        harness
            .begin_run("run-1", AgentMessage::user("prompt", vec![]))
            .unwrap();
        CodingSessionHarness::record_provider_trace_to_path(
            &path,
            "run-1",
            ProviderTraceEvent::Started {
                attempt: 1,
                request_id: "request-1".into(),
                model: "test-model".into(),
                provider: "openai".into(),
            },
        )
        .unwrap();
        let run_id = harness.request_abort().unwrap().unwrap();
        harness.observe_abort_signal(&run_id, true).unwrap();

        let store = JsonlStore::open(&path).unwrap();
        let provider_finish_seq = store.records().iter().find_map(|record| match record {
            HarnessRecord::ProviderRequestFinished {
                seq,
                outcome: ProviderOutcome::Aborted,
                ..
            } => Some(*seq),
            _ => None,
        });
        let abort_observed_seq = store.records().iter().find_map(|record| match record {
            HarnessRecord::AbortObserved { seq, .. } => Some(*seq),
            _ => None,
        });
        assert!(matches!(
            (provider_finish_seq, abort_observed_seq),
            (Some(provider), Some(abort)) if provider < abort
        ));
    }

    #[test]
    fn invalid_subagent_source_is_not_retained_for_passive_commit() {
        let (_dir, path) = temp_session();
        let mut harness = CodingSessionHarness::open(&path).unwrap();

        let identity = harness
            .start_subagent_lane("worker", "inspect", Some("node_69"))
            .unwrap();

        assert!(identity.source_leaf_id.is_none());
        assert!(harness.store.records().iter().any(|record| matches!(
            record,
            HarnessRecord::OperationStarted {
                lane,
                source_leaf_id: None,
                ..
            } if lane == &identity.lane_name
        )));
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
        let _result_entry_id = harness.prepare_assistant_attempt(&run_id).unwrap();

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

        let _run_id = {
            let mut harness = CodingSessionHarness::open(&path).unwrap();
            let id = harness.unique_run_id("test").unwrap();
            harness
                .begin_run(&id, AgentMessage::user("hello", vec![]))
                .unwrap();
            harness.prepare_assistant_attempt(&id).unwrap();
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
            entries
                .iter()
                .any(|e| matches!(&e.message, AgentMessage::User { .. })),
            "user prompt entry should be present"
        );
        assert!(
            entries
                .iter()
                .any(|e| matches!(&e.message, AgentMessage::Assistant { .. })),
            "assistant entry should be present"
        );
    }

    #[test]
    fn main_tool_result_stays_on_the_active_branch() {
        let (_dir, path) = temp_session();
        let mut harness = CodingSessionHarness::open(&path).unwrap();
        let run_id = harness.unique_run_id("test").unwrap();
        harness
            .begin_run(&run_id, AgentMessage::user("inspect", vec![]))
            .unwrap();
        harness.prepare_assistant_attempt(&run_id).unwrap();
        harness
            .append_message(AgentMessage::Assistant {
                content: None,
                tool_calls: Some(vec![threadlane_provider::openai::ToolCall {
                    id: "call-1".into(),
                    r#type: "function".into(),
                    function: threadlane_provider::openai::ToolCallFunction {
                        name: "read_file".into(),
                        arguments: "{}".into(),
                    },
                    thought_signature: None,
                }]),
                stop_reason: None,
                deferred_handle: None,
            })
            .unwrap();
        harness
            .append_message(AgentMessage::Tool {
                tool_call_id: "call-1".into(),
                name: "read_file".into(),
                content: "done".into(),
                is_error: false,
                terminate: false,
            })
            .unwrap();
        harness
            .append_message(AgentMessage::Assistant {
                content: Some("finished".into()),
                tool_calls: None,
                stop_reason: Some("end_turn".into()),
                deferred_handle: None,
            })
            .unwrap();

        let store = threadlane_agent::harness::JsonlStore::open_read_only(&path).unwrap();
        let branch = store.model_context("main").unwrap().messages();
        assert!(matches!(
            branch.get(1),
            Some(AgentMessage::Assistant {
                tool_calls: Some(_),
                ..
            })
        ));
        assert!(
            matches!(branch.get(2), Some(AgentMessage::Tool { tool_call_id, .. }) if tool_call_id == "call-1")
        );
        assert!(
            matches!(branch.get(3), Some(AgentMessage::Assistant { content: Some(content), .. }) if content == "finished")
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
        harness.prepare_assistant_attempt(&run_id).unwrap();
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
        harness.prepare_assistant_attempt(&run_id).unwrap();
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
            .sync_messages(&[AgentMessage::Assistant {
                content: Some("response".into()),
                tool_calls: None,
                stop_reason: Some("end_turn".into()),
                deferred_handle: None,
            }])
            .unwrap();

        assert_eq!(
            harness.store.entries().len(),
            entry_count_before,
            "sync_messages should not create duplicate entries"
        );
    }

    #[tokio::test]
    async fn sync_messages_repairs_a_tool_intent_without_its_result_entry() {
        let (_dir, path) = temp_session();
        let mut harness = CodingSessionHarness::open(&path).unwrap();
        let run_id = harness.unique_run_id("test").unwrap();
        harness
            .begin_run(&run_id, AgentMessage::user("inspect", vec![]))
            .unwrap();

        harness
            .append_message(AgentMessage::Assistant {
                content: None,
                tool_calls: Some(vec![threadlane_provider::openai::ToolCall {
                    id: "call-1".into(),
                    r#type: "function".into(),
                    function: threadlane_provider::openai::ToolCallFunction {
                        name: "read_file".into(),
                        arguments: "{\"path\":\"README.md\"}".into(),
                    },
                    thought_signature: None,
                }]),
                stop_reason: None,
                deferred_handle: None,
            })
            .unwrap();
        harness
            .append_tool_intent(
                &run_id,
                "call-1",
                "read_file",
                serde_json::json!({"path": "README.md"}),
            )
            .await
            .unwrap();

        let result = AgentMessage::Tool {
            tool_call_id: "call-1".into(),
            name: "read_file".into(),
            content: "contents".into(),
            is_error: false,
            terminate: false,
        };
        harness.sync_messages(&[result.clone()]).unwrap();

        let state = Reducer::reduce(&harness.store).unwrap();
        assert!(state
            .lane("main")
            .unwrap()
            .tools
            .iter()
            .find(|tool| tool.tool_call_id == "call-1")
            .unwrap()
            .completed);
    }

    #[test]
    fn sync_messages_persists_provider_visible_queued_user_messages() {
        let (_dir, path) = temp_session();
        let mut harness = CodingSessionHarness::open(&path).unwrap();
        let run_id = harness.unique_run_id("test").unwrap();
        let initial = AgentMessage::user("initial", vec![]);
        let queued = AgentMessage::user("queued follow-up", vec![]);

        harness.begin_run(&run_id, initial.clone()).unwrap();
        harness
            .sync_messages(&[initial.clone(), queued.clone()])
            .unwrap();
        harness
            .assert_model_visible(&[initial, queued.clone()])
            .unwrap();

        assert!(harness
            .store
            .model_context("main")
            .unwrap()
            .entries
            .iter()
            .any(|entry| entry.message == queued));
    }

    #[test]
    fn model_visibility_rejects_extra_durable_messages() {
        let (_dir, path) = temp_session();
        let mut harness = CodingSessionHarness::open(&path).unwrap();
        let run_id = harness.unique_run_id("test").unwrap();
        let prompt = AgentMessage::user("inspect", vec![]);
        let extra = AgentMessage::Assistant {
            content: Some("stale response".into()),
            tool_calls: None,
            stop_reason: Some("end_turn".into()),
            deferred_handle: None,
        };

        harness.begin_run(&run_id, prompt.clone()).unwrap();
        harness.sync_messages(&[prompt.clone(), extra]).unwrap();

        let error = harness.assert_model_visible(&[prompt]).unwrap_err();
        assert!(error.contains("durable_count=2, provider_count=1"));
    }

    #[test]
    fn sync_messages_persists_reasoning_before_model_visibility_check() {
        let (_dir, path) = temp_session();
        let mut harness = CodingSessionHarness::open(&path).unwrap();
        let run_id = harness.unique_run_id("test").unwrap();
        let prompt = AgentMessage::user("inspect", vec![]);
        let thinking = AgentMessage::Custom {
            custom_type: "thinking".into(),
            payload: serde_json::json!({"text": "reasoning"}),
        };

        harness.begin_run(&run_id, prompt.clone()).unwrap();
        harness
            .sync_messages(&[prompt.clone(), thinking.clone()])
            .unwrap();
        harness
            .assert_model_visible(&[prompt, thinking.clone()])
            .unwrap();

        assert!(harness
            .store
            .entries()
            .iter()
            .any(|entry| entry.message == thinking));
    }

    #[test]
    fn stale_metadata_and_chained_tool_results_remain_model_visible_and_get_lifecycle_records() {
        let (_dir, path) = temp_session();
        let mut harness = CodingSessionHarness::open(&path).unwrap();
        let run_id = harness.unique_run_id("test").unwrap();
        let prompt = AgentMessage::user("inspect", vec![]);
        harness.begin_run(&run_id, prompt.clone()).unwrap();
        harness.prepare_assistant_attempt(&run_id).unwrap();

        let mut stale_tree = SessionTree::load_from_file(&path).unwrap();
        stale_tree.set_name("stale metadata".into()).unwrap();

        let assistant = AgentMessage::Assistant {
            content: None,
            tool_calls: Some(vec![
                threadlane_provider::openai::ToolCall {
                    id: "call-1".into(),
                    r#type: "function".into(),
                    function: threadlane_provider::openai::ToolCallFunction {
                        name: "read_file".into(),
                        arguments: "{}".into(),
                    },
                    thought_signature: None,
                },
                threadlane_provider::openai::ToolCall {
                    id: "call-2".into(),
                    r#type: "function".into(),
                    function: threadlane_provider::openai::ToolCallFunction {
                        name: "grep".into(),
                        arguments: "{}".into(),
                    },
                    thought_signature: None,
                },
            ]),
            stop_reason: None,
            deferred_handle: None,
        };
        let first_tool = AgentMessage::Tool {
            tool_call_id: "call-1".into(),
            name: "read_file".into(),
            content: "first".into(),
            is_error: false,
            terminate: false,
        };
        let second_tool = AgentMessage::Tool {
            tool_call_id: "call-2".into(),
            name: "grep".into(),
            content: "second".into(),
            is_error: false,
            terminate: false,
        };
        let final_assistant = AgentMessage::Assistant {
            content: Some("done".into()),
            tool_calls: None,
            stop_reason: Some("end_turn".into()),
            deferred_handle: None,
        };
        let messages = vec![prompt, assistant, first_tool, second_tool, final_assistant];

        harness.sync_messages(&messages).unwrap();
        harness.assert_model_visible(&messages).unwrap();
        harness
            .record_completed_tools_with_termination(&run_id, &HashMap::new())
            .unwrap();

        assert_eq!(
            harness
                .store
                .records()
                .iter()
                .filter(|record| matches!(record, HarnessRecord::ToolStarted { .. }))
                .count(),
            2
        );
        assert_eq!(
            harness
                .store
                .records()
                .iter()
                .filter(|record| matches!(record, HarnessRecord::ToolFinished { .. }))
                .count(),
            2
        );
    }

    #[test]
    fn sync_messages_persists_identical_empty_assistant_results_for_each_run() {
        let (_dir, path) = temp_session();
        let mut harness = CodingSessionHarness::open(&path).unwrap();
        let empty_assistant = AgentMessage::Assistant {
            content: None,
            tool_calls: None,
            stop_reason: None,
            deferred_handle: None,
        };
        let mut provider_messages = Vec::new();

        for prompt_text in ["first prompt", "second prompt"] {
            let run_id = harness.unique_run_id("test").unwrap();
            let prompt = AgentMessage::user(prompt_text, vec![]);
            harness.begin_run(&run_id, prompt.clone()).unwrap();
            provider_messages.push(prompt);
            provider_messages.push(empty_assistant.clone());

            // This mirrors CodingAgent's full provider-state synchronization
            // after each prompt. The second empty assistant must be a new
            // durable entry even though its content matches the first one.
            harness.sync_messages(&provider_messages).unwrap();
            harness
                .record_assistant_attempt(&run_id, TokenUsage::default())
                .unwrap();
            harness
                .finish_run(&run_id, OperationOutcome::Completed, None)
                .unwrap();
        }

        let assistant_entries: Vec<_> = harness
            .store
            .entries()
            .iter()
            .filter(|entry| matches!(entry.message, AgentMessage::Assistant { .. }))
            .collect();
        assert_eq!(assistant_entries.len(), 2);
        assert_ne!(assistant_entries[0].id, assistant_entries[1].id);
        assert_eq!(
            harness
                .store
                .records()
                .iter()
                .filter(|record| matches!(record, HarnessRecord::OperationFinished { .. }))
                .count(),
            2
        );
    }

    #[test]
    fn assistant_ready_persists_provider_response_attached_record() {
        let (_dir, path) = temp_session();
        let mut harness = CodingSessionHarness::open(&path).unwrap();
        let run_id = harness.unique_run_id("test").unwrap();
        harness
            .begin_run(&run_id, AgentMessage::user("test prompt", vec![]))
            .unwrap();

        CodingSessionHarness::record_provider_trace_to_path(
            &path,
            &run_id,
            ProviderTraceEvent::AssistantReady {
                attempt: 1,
                request_id: "req-123".into(),
                reasoning: Some("deep thinking".into()),
                message: AgentMessage::Assistant {
                    content: Some("final answer".into()),
                    tool_calls: None,
                    stop_reason: None,
                    deferred_handle: None,
                },
            },
        )
        .unwrap();

        let updated_harness = CodingSessionHarness::open(&path).unwrap();
        let response_record = updated_harness
            .store
            .records()
            .iter()
            .find(|record| matches!(record, HarnessRecord::ProviderResponseAttached { .. }))
            .expect("must record ProviderResponseAttached");

        if let HarnessRecord::ProviderResponseAttached {
            run_id: rec_run_id,
            attempt,
            request_id,
            entry_id,
            reasoning_entry_id,
            ..
        } = response_record
        {
            assert_eq!(rec_run_id, &run_id);
            assert_eq!(*attempt, 1);
            assert_eq!(request_id.as_ref().map(|r| r.as_str()), Some("req-123"));
            assert!(!entry_id.is_empty());
            assert!(reasoning_entry_id.is_some());
        } else {
            panic!("expected ProviderResponseAttached");
        }
    }
}
