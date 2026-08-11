use super::store::SessionStore;
use super::types::{Entry, OperationIntent, OperationOutcome, Record, ReduceError, ReducedState};

use crate::types::TokenUsage;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct StreamingState {
    pub lane: String,
    pub run_id: Option<String>,
    pub assistant_text: String,
    pub reasoning: String,
    pub tool_call_ids: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Snapshot {
    pub session_id: String,
    pub state: ReducedState,
    pub entries: Vec<Entry>,
    pub records: Vec<Record>,
    #[serde(default)]
    pub streaming: Option<StreamingState>,
}

impl Snapshot {
    pub fn from_store<S: SessionStore>(store: &S) -> Result<Self, ReduceError> {
        Ok(Self {
            session_id: store.session_id().into(),
            state: super::Reducer::reduce(store)?,
            entries: store.entries().to_vec(),
            records: store.records().to_vec(),
            streaming: None,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum EventPayload {
    EntryCommitted(Entry),
    RecordCommitted(Record),
    Fault(String),
    Streaming(Option<StreamingState>),
    Agent(crate::events::AgentEvent),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HarnessEvent {
    pub id: u64,
    pub payload: EventPayload,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lane: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recovery_id: Option<String>,
    /// The correlated [`OperationIntent`] for an [`EventPayload::RecordCommitted`]
    /// that wraps a [`Record::OperationFinished`]; resolved by the event hub from
    /// the matching [`Record::OperationStarted`].  `None` for other payloads and
    /// for finished operations whose start was not observed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operation_intent: Option<OperationIntent>,
}

/// A compatibility projection of a [`HarnessEvent`] into an [`AgentEvent`],
/// carrying the commit cursor and identity fields for downstream consumers
/// that need lane/run/turn context without depending on the full harness event.
#[derive(Debug, Clone, PartialEq)]
pub struct ProjectedAgentEvent {
    /// The durable commit cursor (`HarnessEvent::id`).
    pub cursor: u64,
    pub lane: Option<String>,
    pub run_id: Option<String>,
    pub turn: Option<u32>,
    /// The recovery identifier from the originating [`HarnessEvent`].
    pub recovery_id: Option<String>,
    pub event: crate::events::AgentEvent,
}

impl HarnessEvent {
    /// Project this harness event into an [`AgentEvent`] from committed
    /// [`EventPayload::EntryCommitted`] and [`EventPayload::RecordCommitted`]
    /// payloads.  Ephemeral [`EventPayload::Agent`] payloads (raw TurnDriver
    /// streaming events) are **never** projected — only durable commit records
    /// yield lifecycle-compatible events.
    ///
    /// | Committed payload | AgentEvent |
    /// |---|---|
    /// | `EntryCommitted` | `MessageEnd` with the entry's `message` |
    /// | `RecordCommitted::OperationStarted` (`Run`) | `AgentStart` |
    /// | `RecordCommitted::OperationFinished` (when `operation_intent` is `Run`) | `AgentEnd` / `AgentError` |
    /// | `RecordCommitted::StepAttempt` | `TurnStart` with `attempt` |
    /// | `RecordCommitted::ToolStarted` | `ToolExecutionStart` with JSON-serialized `effective_args` |
    /// | Everything else | `None` |
    pub fn project_agent_event(&self) -> Option<crate::events::AgentEvent> {
        match &self.payload {
            EventPayload::Agent(_) => None,
            EventPayload::EntryCommitted(entry) => Some(crate::events::AgentEvent::MessageEnd {
                message: entry.message.clone(),
            }),
            EventPayload::RecordCommitted(record) => match record {
                Record::OperationStarted { intent, .. } if *intent == OperationIntent::Run => {
                    Some(crate::events::AgentEvent::AgentStart)
                }
                Record::OperationFinished { outcome, error, .. }
                    if self.operation_intent == Some(OperationIntent::Run) =>
                {
                    match outcome {
                        OperationOutcome::Completed => Some(crate::events::AgentEvent::AgentEnd {
                            usage: TokenUsage::default(),
                        }),
                        _ => Some(crate::events::AgentEvent::AgentError {
                            error: error.clone().unwrap_or_else(|| match outcome {
                                OperationOutcome::Failed => "operation failed".to_string(),
                                OperationOutcome::Aborted => "operation aborted".to_string(),
                                OperationOutcome::Declined => "operation declined".to_string(),
                                _ => unreachable!(),
                            }),
                        }),
                    }
                }
                Record::StepAttempt { attempt, .. } => Some(crate::events::AgentEvent::TurnStart {
                    turn_number: *attempt as usize,
                }),
                Record::ToolStarted {
                    tool_call_id,
                    tool_name,
                    effective_args,
                    ..
                } => Some(crate::events::AgentEvent::ToolExecutionStart {
                    tool_call_id: tool_call_id.clone(),
                    name: tool_name.clone(),
                    arguments: serde_json::to_string(effective_args).unwrap_or_default(),
                }),
                _ => None,
            },
            _ => None,
        }
    }

    /// Project into a [`ProjectedAgentEvent`] carrying the commit cursor and
    /// identity fields alongside the inner agent event.
    pub fn project(&self) -> Option<ProjectedAgentEvent> {
        self.project_agent_event().map(|event| ProjectedAgentEvent {
            cursor: self.id,
            lane: self.lane.clone(),
            run_id: self.run_id.clone(),
            turn: self.turn,
            recovery_id: self.recovery_id.clone(),
            event,
        })
    }

    /// Short label for logging the payload variant.
    pub fn payload_variant(&self) -> &'static str {
        match &self.payload {
            EventPayload::EntryCommitted(_) => "EntryCommitted",
            EventPayload::RecordCommitted(r) => match r {
                Record::OperationStarted { .. } => "OperationStarted",
                Record::OperationFinished { .. } => "OperationFinished",
                Record::StepAttempt { .. } => "StepAttempt",
                Record::ToolStarted { .. } => "ToolStarted",
                Record::AbortRequested { .. } => "AbortRequested",
                Record::LaneMoved { .. } => "LaneMoved",
                _ => "Record(…)",
            },
            EventPayload::Fault(_) => "Fault",
            EventPayload::Streaming(_) => "Streaming",
            EventPayload::Agent(_) => "Agent",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EventError {
    Gap { requested: u64, oldest: u64 },
}

#[derive(Debug, Clone)]
pub struct Subscription {
    pub snapshot: Snapshot,
    next_id: u64,
    lane: Option<String>,
}

#[derive(Debug, Clone)]
pub struct HarnessEventHub {
    inner: Arc<Mutex<HarnessEventHubState>>,
}

#[derive(Debug)]
struct HarnessEventHubState {
    capacity: usize,
    next_id: u64,
    events: VecDeque<HarnessEvent>,
    streaming: Option<StreamingState>,
    /// Tracks the intent of every observed `OperationStarted` record keyed by
    /// `(lane, run_id)` so that the matching `OperationFinished` can carry it.
    operation_intents: HashMap<(String, String), OperationIntent>,
}

/// Hydrate `operation_intents` from store records so a fresh hub after restart
/// can still correlate a later `OperationFinished` with its original intent.
///
/// Only *currently open* operations are hydrated — when the store contains
/// both an `OperationStarted` and a corresponding `OperationFinished` the
/// intent is cleared and a duplicate finish will not project.
fn hydrate_intents_from_store<S: SessionStore>(
    intents: &mut HashMap<(String, String), OperationIntent>,
    store: &S,
) {
    // First pass: collect every OperationStarted intent.
    for record in store.records() {
        if let Record::OperationStarted {
            intent, lane, id, ..
        } = record
        {
            intents.insert((lane.clone(), id.clone()), intent.clone());
        }
    }
    // Second pass: remove closed operations — those with a persisted
    // OperationFinished.
    for record in store.records() {
        if let Record::OperationFinished { lane, run_id, .. } = record {
            intents.remove(&(lane.clone(), run_id.clone()));
        }
    }
}

impl HarnessEventHub {
    pub fn new(capacity: usize) -> Self {
        Self {
            inner: Arc::new(Mutex::new(HarnessEventHubState {
                capacity: capacity.max(1),
                next_id: 1,
                events: VecDeque::new(),
                streaming: None,
                operation_intents: HashMap::new(),
            })),
        }
    }

    pub fn publish(&self, payload: EventPayload) -> HarnessEvent {
        self.publish_identified(payload, None, None, None)
    }

    pub fn publish_agent_event(&self, event: crate::events::AgentEvent) -> HarnessEvent {
        self.publish(EventPayload::Agent(event))
    }

    pub fn publish_identified(
        &self,
        payload: EventPayload,
        lane: Option<String>,
        run_id: Option<String>,
        recovery_id: Option<String>,
    ) -> HarnessEvent {
        self.publish_identified_with_turn(payload, lane, run_id, None, recovery_id)
    }

    pub fn publish_identified_with_turn(
        &self,
        payload: EventPayload,
        lane: Option<String>,
        run_id: Option<String>,
        turn: Option<u32>,
        recovery_id: Option<String>,
    ) -> HarnessEvent {
        let mut state = self.inner.lock().unwrap_or_else(|error| error.into_inner());

        // Resolve the operation intent: store for OperationStarted, look up for
        // OperationFinished.  Use the provided identity fields when they are set,
        // falling back to the record's own lane / run_id so that both the
        // harness-integrated path (publish_identified) and direct test publication
        // (publish) produce correct correlation.
        let mut operation_intent = None;
        if let EventPayload::RecordCommitted(record) = &payload {
            let effective_lane = lane.clone().unwrap_or_else(|| record.lane().to_owned());
            let effective_run: Option<String> = run_id
                .clone()
                .or_else(|| record.run_id().map(str::to_owned));
            match record {
                Record::OperationStarted { intent, .. } => {
                    if let Some(r) = &effective_run {
                        state
                            .operation_intents
                            .insert((effective_lane, r.clone()), intent.clone());
                    }
                }
                Record::OperationFinished { .. } => {
                    if let Some(r) = &effective_run {
                        operation_intent = state
                            .operation_intents
                            .get(&(effective_lane.clone(), r.clone()))
                            .cloned();
                        // Remove the spent intent so per-run memory stays bounded.
                        state.operation_intents.remove(&(effective_lane, r.clone()));
                    }
                }
                _ => {}
            }
        }

        let event = HarnessEvent {
            id: state.next_id,
            payload,
            lane,
            run_id,
            turn,
            recovery_id,
            operation_intent,
        };
        state.next_id += 1;
        if state.events.len() == state.capacity {
            state.events.pop_front();
        }
        state.events.push_back(event.clone());
        event
    }

    pub fn publish_streaming(&self, state: Option<StreamingState>) -> HarnessEvent {
        let (lane, run_id) = state
            .as_ref()
            .map(|state| (Some(state.lane.clone()), state.run_id.clone()))
            .unwrap_or((None, None));
        let mut hub = self.inner.lock().unwrap_or_else(|error| error.into_inner());
        hub.streaming = state.clone();
        let event = HarnessEvent {
            id: hub.next_id,
            payload: EventPayload::Streaming(state),
            lane,
            run_id,
            turn: None,
            recovery_id: None,
            operation_intent: None,
        };
        hub.next_id += 1;
        if hub.events.len() == hub.capacity {
            hub.events.pop_front();
        }
        hub.events.push_back(event.clone());
        event
    }

    pub fn streaming_state(&self) -> Option<StreamingState> {
        self.inner
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .streaming
            .clone()
    }

    pub fn subscribe<S: SessionStore>(&self, store: &S) -> Result<Subscription, ReduceError> {
        self.subscribe_for_lane(store, None)
    }

    pub fn subscribe_for_lane<S: SessionStore>(
        &self,
        store: &S,
        lane: Option<&str>,
    ) -> Result<Subscription, ReduceError> {
        // Keep the cursor paired with the snapshot. Commits cannot publish
        // between these two observations, so polling starts without a gap.
        let mut state = self.inner.lock().unwrap_or_else(|error| error.into_inner());
        let mut snapshot = Snapshot::from_store(store)?;
        snapshot.streaming = state
            .streaming
            .clone()
            .filter(|stream| lane.is_none_or(|lane| stream.lane == lane));

        // Hydrate operation_intents from existing store records so a fresh
        // hub after restart can correlate a later OperationFinished with its
        // original OperationStarted intent.
        hydrate_intents_from_store(&mut state.operation_intents, store);

        Ok(Subscription {
            snapshot,
            next_id: state.next_id,
            lane: lane.map(str::to_owned),
        })
    }

    pub fn poll(&self, subscription: &mut Subscription) -> Result<Vec<HarnessEvent>, EventError> {
        let state = self.inner.lock().unwrap_or_else(|error| error.into_inner());
        let Some(oldest) = state.events.front().map(|event| event.id) else {
            return Ok(Vec::new());
        };
        if subscription.next_id < oldest {
            return Err(EventError::Gap {
                requested: subscription.next_id,
                oldest,
            });
        }
        let events: Vec<_> = state
            .events
            .iter()
            .filter(|event| {
                event.id >= subscription.next_id
                    && subscription
                        .lane
                        .as_deref()
                        .is_none_or(|lane| event.lane.as_deref() == Some(lane))
            })
            .cloned()
            .collect();
        if let Some(last_seen) = state
            .events
            .iter()
            .filter(|event| event.id >= subscription.next_id)
            .map(|event| event.id)
            .next_back()
        {
            subscription.next_id = last_seen + 1;
        }
        Ok(events)
    }

    pub fn unsubscribe(self) {}
}
