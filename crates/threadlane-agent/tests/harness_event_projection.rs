use serde_json::json;
use threadlane_agent::events::AgentEvent;
use threadlane_agent::harness::{
    AgentHarness, Entry, EventPayload, HarnessEventHub, MemoryStore, OperationIntent,
    OperationOutcome, ProjectedAgentEvent, ProvisionedEntry, QueueKind, QueuedEntry, Record,
    SessionStore, StreamingState, ToolReplaySafety,
};
use threadlane_agent::AgentMessage;

// ── Payload mapping ────────────────────────────────────────────────────────

#[test]
fn agent_payload_is_not_projected() {
    let hub = HarnessEventHub::new(8);
    let event = hub.publish_agent_event(AgentEvent::AgentStart);
    // Ephemeral Agent payloads are raw TurnDriver streaming events — they
    // MUST NOT be presented as durable committed lifecycle events.
    assert!(event.project_agent_event().is_none());
    assert!(event.project().is_none());
}

#[test]
fn entry_committed_projects_to_message_end() {
    let hub = HarnessEventHub::new(8);
    let msg = AgentMessage::user("hello", vec![]);
    let entry = Entry {
        id: "e1".into(),
        parent_id: None,
        lane: "main".into(),
        seq: 1,
        timestamp: 1,
        message: msg.clone(),
        terminate: false,
    };
    let event = hub.publish(EventPayload::EntryCommitted(entry));
    let projected = event.project_agent_event();
    assert!(projected.is_some());
    assert!(matches!(&projected, Some(AgentEvent::MessageEnd { message }) if message == &msg));
}

#[test]
fn operation_started_run_projects_to_agent_start() {
    let hub = HarnessEventHub::new(8);
    let record = Record::OperationStarted {
        id: "op1".into(),
        seq: 1,
        lane: "main".into(),
        timestamp: 1,
        source_leaf_id: None,
        intent: OperationIntent::Run,
    };
    let event = hub.publish(EventPayload::RecordCommitted(record));
    let projected = event.project_agent_event();
    assert!(projected.is_some());
    assert!(matches!(projected.unwrap(), AgentEvent::AgentStart));
}

#[test]
fn operation_started_non_run_returns_none() {
    let hub = HarnessEventHub::new(8);
    for intent in [OperationIntent::Compaction, OperationIntent::Navigation] {
        let record = Record::OperationStarted {
            id: "op1".into(),
            seq: 1,
            lane: "main".into(),
            timestamp: 1,
            source_leaf_id: None,
            intent,
        };
        let event = hub.publish(EventPayload::RecordCommitted(record));
        assert!(event.project_agent_event().is_none());
    }
}

#[test]
fn operation_finished_completed_projects_to_agent_end() {
    let hub = HarnessEventHub::new(8);
    // The OperationFinished must correlate with a preceding OperationStarted
    // whose intent is Run for the projection to produce AgentEnd.
    hub.publish(EventPayload::RecordCommitted(Record::OperationStarted {
        id: "run-1".into(),
        seq: 1,
        lane: "main".into(),
        timestamp: 1,
        source_leaf_id: None,
        intent: OperationIntent::Run,
    }));
    let record = Record::OperationFinished {
        id: "op1".into(),
        seq: 2,
        lane: "main".into(),
        timestamp: 2,
        run_id: "run-1".into(),
        outcome: OperationOutcome::Completed,
        error: None,
    };
    let event = hub.publish(EventPayload::RecordCommitted(record));
    let projected = event.project_agent_event();
    assert!(projected.is_some());
    assert!(matches!(projected.unwrap(), AgentEvent::AgentEnd { .. }));
}

#[test]
fn operation_finished_failed_projects_to_agent_error() {
    let hub = HarnessEventHub::new(8);

    // Establish both Run intents before their matching finishes.
    hub.publish(EventPayload::RecordCommitted(Record::OperationStarted {
        id: "run-1".into(),
        seq: 1,
        lane: "main".into(),
        timestamp: 1,
        source_leaf_id: None,
        intent: OperationIntent::Run,
    }));
    hub.publish(EventPayload::RecordCommitted(Record::OperationStarted {
        id: "run-2".into(),
        seq: 2,
        lane: "main".into(),
        timestamp: 2,
        source_leaf_id: None,
        intent: OperationIntent::Run,
    }));

    // Failed with an error message uses that message.
    let record = Record::OperationFinished {
        id: "op1".into(),
        seq: 3,
        lane: "main".into(),
        timestamp: 3,
        run_id: "run-1".into(),
        outcome: OperationOutcome::Failed,
        error: Some("disk full".into()),
    };
    let event = hub.publish(EventPayload::RecordCommitted(record));
    let projected = event.project_agent_event();
    assert!(projected.is_some());
    match projected.unwrap() {
        AgentEvent::AgentError { error } => assert_eq!(error, "disk full"),
        other => panic!("expected AgentError, got {other:?}"),
    }

    // Failed without an error text uses a stable outcome message.
    let record_no_err = Record::OperationFinished {
        id: "op2".into(),
        seq: 4,
        lane: "main".into(),
        timestamp: 4,
        run_id: "run-2".into(),
        outcome: OperationOutcome::Failed,
        error: None,
    };
    let event2 = hub.publish(EventPayload::RecordCommitted(record_no_err));
    let projected2 = event2.project_agent_event();
    match projected2.unwrap() {
        AgentEvent::AgentError { error } => assert_eq!(error, "operation failed"),
        other => panic!("expected AgentError, got {other:?}"),
    }
}

#[test]
fn operation_finished_aborted_projects_to_agent_error() {
    let hub = HarnessEventHub::new(8);
    hub.publish(EventPayload::RecordCommitted(Record::OperationStarted {
        id: "run-1".into(),
        seq: 1,
        lane: "main".into(),
        timestamp: 1,
        source_leaf_id: None,
        intent: OperationIntent::Run,
    }));
    let record = Record::OperationFinished {
        id: "op1".into(),
        seq: 2,
        lane: "main".into(),
        timestamp: 2,
        run_id: "run-1".into(),
        outcome: OperationOutcome::Aborted,
        error: None,
    };
    let event = hub.publish(EventPayload::RecordCommitted(record));
    let projected = event.project_agent_event();
    assert!(projected.is_some());
    match projected.unwrap() {
        AgentEvent::AgentError { error } => assert_eq!(error, "operation aborted"),
        other => panic!("expected AgentError for Aborted, got {other:?}"),
    }
}

#[test]
fn operation_finished_declined_projects_to_agent_error() {
    let hub = HarnessEventHub::new(8);
    hub.publish(EventPayload::RecordCommitted(Record::OperationStarted {
        id: "run-1".into(),
        seq: 1,
        lane: "main".into(),
        timestamp: 1,
        source_leaf_id: None,
        intent: OperationIntent::Run,
    }));
    let record = Record::OperationFinished {
        id: "op1".into(),
        seq: 2,
        lane: "main".into(),
        timestamp: 2,
        run_id: "run-1".into(),
        outcome: OperationOutcome::Declined,
        error: None,
    };
    let event = hub.publish(EventPayload::RecordCommitted(record));
    let projected = event.project_agent_event();
    assert!(projected.is_some());
    match projected.unwrap() {
        AgentEvent::AgentError { error } => assert_eq!(error, "operation declined"),
        other => panic!("expected AgentError for Declined, got {other:?}"),
    }
}

#[test]
fn operation_finished_non_completed_never_produces_agent_end() {
    let hub = HarnessEventHub::new(8);
    // Each terminal outcome should have its own fresh OperationStarted(Run)
    // so that intent removal after the first finish doesn't starve the rest.
    for outcome in [
        OperationOutcome::Failed,
        OperationOutcome::Aborted,
        OperationOutcome::Declined,
    ] {
        let outcome_label = format!("{outcome:?}");
        let run_id = format!("run-{outcome_label}");
        hub.publish(EventPayload::RecordCommitted(Record::OperationStarted {
            id: run_id.clone(),
            seq: 1,
            lane: "main".into(),
            timestamp: 1,
            source_leaf_id: None,
            intent: OperationIntent::Run,
        }));
        let record = Record::OperationFinished {
            id: format!("op-{outcome_label}"),
            seq: 2,
            lane: "main".into(),
            timestamp: 2,
            run_id,
            outcome,
            error: None,
        };
        let event = hub.publish(EventPayload::RecordCommitted(record));
        let projected = event.project_agent_event().unwrap();
        assert!(
            !matches!(&projected, AgentEvent::AgentEnd { .. }),
            "outcome {outcome_label} must not project to AgentEnd"
        );
        assert!(
            matches!(&projected, AgentEvent::AgentError { .. }),
            "outcome {outcome_label} must project to AgentError"
        );
    }
}

#[test]
fn step_attempt_projects_to_turn_start() {
    let hub = HarnessEventHub::new(8);
    let record = Record::StepAttempt {
        id: "s1".into(),
        seq: 1,
        lane: "main".into(),
        timestamp: 1,
        run_id: "run-1".into(),
        attempt: 7,
        result_entry_id: "r1".into(),
        compaction_reason: None,
    };
    let event = hub.publish(EventPayload::RecordCommitted(record));
    let projected = event.project_agent_event();
    assert!(projected.is_some());
    assert!(matches!(
        projected.unwrap(),
        AgentEvent::TurnStart { turn_number: 7 }
    ));
}

#[test]
fn tool_started_projects_to_tool_execution_start() {
    let hub = HarnessEventHub::new(8);
    let args = json!({"path": "/tmp", "recursive": true});
    let record = Record::ToolStarted {
        id: "t1".into(),
        seq: 1,
        lane: "main".into(),
        timestamp: 1,
        run_id: "run-1".into(),
        assistant_entry_id: "a1".into(),
        tool_index: 0,
        tool_call_id: "call-1".into(),
        tool_name: "read_file".into(),
        effective_args: args.clone(),
        result_entry_id: "r1".into(),
        replay: ToolReplaySafety::Safe,
    };
    let event = hub.publish(EventPayload::RecordCommitted(record));
    let projected = event.project_agent_event();
    assert!(projected.is_some());
    match projected.unwrap() {
        AgentEvent::ToolExecutionStart {
            tool_call_id,
            name,
            arguments,
        } => {
            assert_eq!(tool_call_id, "call-1");
            assert_eq!(name, "read_file");
            let parsed: serde_json::Value =
                serde_json::from_str(&arguments).expect("arguments must be valid JSON");
            assert_eq!(parsed, args);
        }
        other => panic!("expected ToolExecutionStart, got {other:?}"),
    }
}

#[test]
fn unsupported_records_project_to_none() {
    let hub = HarnessEventHub::new(8);

    // FactSet — no AgentEvent equivalent
    let fact_event = hub.publish(EventPayload::RecordCommitted(Record::FactSet {
        id: "f1".into(),
        seq: 1,
        lane: "main".into(),
        timestamp: 1,
        run_id: None,
        key: "k".into(),
        value: "v".into(),
    }));
    assert!(fact_event.project_agent_event().is_none());

    // LaneMoved — no AgentEvent equivalent
    let lane_event = hub.publish(EventPayload::RecordCommitted(Record::LaneMoved {
        id: "l1".into(),
        seq: 1,
        lane: "main".into(),
        timestamp: 1,
        run_id: "run-1".into(),
        target_leaf_id: "t1".into(),
    }));
    assert!(lane_event.project_agent_event().is_none());

    // AbortRequested — no AgentEvent equivalent
    let abort_event = hub.publish(EventPayload::RecordCommitted(Record::AbortRequested {
        id: "a1".into(),
        seq: 1,
        lane: "main".into(),
        timestamp: 1,
        run_id: "run-1".into(),
    }));
    assert!(abort_event.project_agent_event().is_none());

    // QueueEnqueued — no AgentEvent equivalent
    let queue_event = hub.publish(EventPayload::RecordCommitted(Record::QueueEnqueued {
        id: "q1".into(),
        seq: 1,
        lane: "main".into(),
        timestamp: 1,
        run_id: None,
        queue: QueueKind::Steer,
        priority: None,
        target: ProvisionedEntry {
            id: "queued-1".into(),
            parent_id: None,
            message: AgentMessage::user("task", vec![]),
        },
    }));
    assert!(queue_event.project_agent_event().is_none());
}

#[test]
fn fault_and_streaming_payloads_project_to_none() {
    let hub = HarnessEventHub::new(8);

    let fault_event = hub.publish(EventPayload::Fault("oops".into()));
    assert!(fault_event.project_agent_event().is_none());

    let streaming_event = hub.publish_streaming(Some(StreamingState {
        lane: "main".into(),
        run_id: Some("r1".into()),
        ..Default::default()
    }));
    assert!(streaming_event.project_agent_event().is_none());

    let nil_stream_event = hub.publish_streaming(None);
    assert!(nil_stream_event.project_agent_event().is_none());
}

// ── Metadata retention ─────────────────────────────────────────────────────

#[test]
fn projected_struct_carries_cursor_and_identity() {
    let hub = HarnessEventHub::new(8);
    let record = Record::OperationStarted {
        id: "op1".into(),
        seq: 1,
        lane: "main".into(),
        timestamp: 1,
        source_leaf_id: None,
        intent: OperationIntent::Run,
    };
    let harness_event = hub.publish_identified_with_turn(
        EventPayload::RecordCommitted(record),
        Some("main".into()),
        Some("run-1".into()),
        Some(2),
        Some("recovery-a".into()),
    );

    let projected: ProjectedAgentEvent = harness_event.project().unwrap();
    assert_eq!(projected.cursor, harness_event.id);
    assert_eq!(projected.lane.as_deref(), Some("main"));
    assert_eq!(projected.run_id.as_deref(), Some("run-1"));
    assert_eq!(projected.turn, Some(2));
    assert_eq!(projected.recovery_id.as_deref(), Some("recovery-a"));
    assert!(matches!(&projected.event, AgentEvent::AgentStart));
}

#[test]
fn projection_cursor_is_monotonically_increasing() {
    let hub = HarnessEventHub::new(8);
    let e1 = hub.publish(EventPayload::EntryCommitted(Entry {
        id: "a".into(),
        parent_id: None,
        lane: "main".into(),
        seq: 1,
        timestamp: 1,
        message: AgentMessage::user("one", vec![]),
        terminate: false,
    }));
    let e2 = hub.publish(EventPayload::EntryCommitted(Entry {
        id: "b".into(),
        parent_id: None,
        lane: "main".into(),
        seq: 2,
        timestamp: 2,
        message: AgentMessage::user("two", vec![]),
        terminate: false,
    }));
    let e3 = hub.publish(EventPayload::EntryCommitted(Entry {
        id: "c".into(),
        parent_id: None,
        lane: "main".into(),
        seq: 3,
        timestamp: 3,
        message: AgentMessage::user("three", vec![]),
        terminate: false,
    }));

    let cursors: Vec<u64> = [&e1, &e2, &e3]
        .iter()
        .filter_map(|e| e.project().map(|p| p.cursor))
        .collect();
    assert_eq!(cursors.len(), 3);

    // cursors must be strictly increasing
    for window in cursors.windows(2) {
        assert!(window[0] < window[1]);
    }
}

// ── Commit ordering ────────────────────────────────────────────────────────

#[test]
fn projection_respects_commit_order_in_subscription() {
    let store = MemoryStore::new("session-1");
    let hub = HarnessEventHub::new(16);

    // Subscribe before publishing to observe buffered delivery.
    let mut subscription = hub.subscribe(&store).unwrap();

    // Publish interleaved committed events — only projectable ones surface.
    hub.publish(EventPayload::RecordCommitted(Record::OperationStarted {
        id: "run-1".into(),
        seq: 1,
        lane: "main".into(),
        timestamp: 1,
        source_leaf_id: None,
        intent: OperationIntent::Run,
    }));
    hub.publish(EventPayload::Fault("between".into()));
    hub.publish(EventPayload::RecordCommitted(Record::StepAttempt {
        id: "s1".into(),
        seq: 1,
        lane: "main".into(),
        timestamp: 1,
        run_id: "run-1".into(),
        attempt: 1,
        result_entry_id: "r1".into(),
        compaction_reason: None,
    }));
    hub.publish(EventPayload::EntryCommitted(Entry {
        id: "e1".into(),
        parent_id: None,
        lane: "main".into(),
        seq: 1,
        timestamp: 1,
        message: AgentMessage::user("hi", vec![]),
        terminate: false,
    }));
    hub.publish(EventPayload::RecordCommitted(Record::OperationFinished {
        id: "op1".into(),
        seq: 1,
        lane: "main".into(),
        timestamp: 1,
        run_id: "run-1".into(),
        outcome: OperationOutcome::Completed,
        error: None,
    }));

    let events = hub.poll(&mut subscription).unwrap();

    let projected: Vec<ProjectedAgentEvent> = events.iter().filter_map(|e| e.project()).collect();

    // Only projectable committed payloads surface; Fault is skipped.
    assert_eq!(projected.len(), 4);

    // Cursors must be strictly increasing (commit ordering preserved).
    let cursors: Vec<u64> = projected.iter().map(|p| p.cursor).collect();
    for window in cursors.windows(2) {
        assert!(window[0] < window[1]);
    }

    // Verify the specific event types in order.
    assert!(matches!(&projected[0].event, AgentEvent::AgentStart));
    assert!(matches!(&projected[1].event, AgentEvent::TurnStart { .. }));
    assert!(matches!(&projected[2].event, AgentEvent::MessageEnd { .. }));
    assert!(matches!(&projected[3].event, AgentEvent::AgentEnd { .. }));
}

#[test]
fn agent_end_not_projected_before_operation_finished_commit() {
    let store = MemoryStore::new("session-1");
    let hub = HarnessEventHub::new(16);

    // Subscribe first so poll captures new events after the cursor.
    let mut subscription = hub.subscribe(&store).unwrap();

    // An AgentEnd only projects from OperationFinished, not from a raw Agent
    // event.  Publish a raw Agent event first, then the real committed records.
    hub.publish_agent_event(AgentEvent::AgentEnd {
        usage: Default::default(),
    });

    // The OperationStarted establishes the Run intent so that the matching
    // OperationFinished below can project to AgentEnd.
    hub.publish(EventPayload::RecordCommitted(Record::OperationStarted {
        id: "run-1".into(),
        seq: 1,
        lane: "main".into(),
        timestamp: 1,
        source_leaf_id: None,
        intent: OperationIntent::Run,
    }));

    hub.publish(EventPayload::RecordCommitted(Record::OperationFinished {
        id: "op1".into(),
        seq: 2,
        lane: "main".into(),
        timestamp: 2,
        run_id: "run-1".into(),
        outcome: OperationOutcome::Completed,
        error: None,
    }));

    let events = hub.poll(&mut subscription).unwrap();

    let projected: Vec<ProjectedAgentEvent> = events.iter().filter_map(|e| e.project()).collect();

    // The raw Agent payload is not projected; the OperationStarted projects
    // to AgentStart; only the correlated OperationFinished records yield
    // an AgentEnd.
    assert_eq!(projected.len(), 2);
    assert!(matches!(&projected[0].event, AgentEvent::AgentStart));
    assert!(matches!(&projected[1].event, AgentEvent::AgentEnd { .. }));

    // The projected cursor is strictly greater than the raw Agent event id.
    assert!(
        projected[1].cursor > events[0].id,
        "projected cursor {} must be after raw agent event cursor {}",
        projected[1].cursor,
        events[0].id
    );
}

#[test]
fn agent_start_and_end_are_separated_by_commit_events_in_projection() {
    let store = MemoryStore::new("session-1");
    let hub = HarnessEventHub::new(16);

    // Subscribe before publishing to observe all events via poll.
    let mut subscription = hub.subscribe(&store).unwrap();

    hub.publish(EventPayload::RecordCommitted(Record::OperationStarted {
        id: "run-1".into(),
        seq: 1,
        lane: "main".into(),
        timestamp: 1,
        source_leaf_id: None,
        intent: OperationIntent::Run,
    }));
    hub.publish(EventPayload::EntryCommitted(Entry {
        id: "e1".into(),
        parent_id: None,
        lane: "main".into(),
        seq: 1,
        timestamp: 1,
        message: AgentMessage::user("mid", vec![]),
        terminate: false,
    }));
    hub.publish(EventPayload::RecordCommitted(Record::OperationFinished {
        id: "op1".into(),
        seq: 1,
        lane: "main".into(),
        timestamp: 1,
        run_id: "run-1".into(),
        outcome: OperationOutcome::Completed,
        error: None,
    }));

    let events = hub.poll(&mut subscription).unwrap();

    let projected: Vec<ProjectedAgentEvent> = events.iter().filter_map(|e| e.project()).collect();

    assert_eq!(projected.len(), 3);
    assert!(matches!(&projected[0].event, AgentEvent::AgentStart));
    assert!(matches!(&projected[1].event, AgentEvent::MessageEnd { .. }));
    assert!(matches!(&projected[2].event, AgentEvent::AgentEnd { .. }));

    // The intermediate commit (EntryCommitted) sits between them in the raw
    // stream, and its cursor is between the boundary events.
    assert!(projected[0].cursor < events[1].id);
    assert!(events[1].id < projected[2].cursor);
}

// ── Reconnect cursor behavior ──────────────────────────────────────────────

#[test]
fn reconnecting_with_snapshot_plus_cursor_avoids_gaps() {
    let mut store = MemoryStore::new("session-1");
    let hub = HarnessEventHub::new(16);

    // Persist entries and records to the store so the subscription snapshot
    // carries real committed state, not just an empty baseline.
    store.append_message(None, AgentMessage::user("prior", vec![]));
    store.append_record(Record::OperationStarted {
        id: "prior-op".into(),
        seq: 2,
        lane: "main".into(),
        timestamp: 2,
        source_leaf_id: None,
        intent: OperationIntent::Navigation,
    });

    // Subscribe before publishing, so the poll observes new events after
    // the subscription's snapshot cursor.
    let mut sub1 = hub.subscribe(&store).unwrap();
    assert_eq!(sub1.snapshot.entries.len(), 1);
    assert_eq!(sub1.snapshot.records.len(), 1);

    hub.publish(EventPayload::RecordCommitted(Record::OperationStarted {
        id: "op1".into(),
        seq: 1,
        lane: "main".into(),
        timestamp: 1,
        source_leaf_id: None,
        intent: OperationIntent::Run,
    }));

    let batch1 = hub.poll(&mut sub1).unwrap();
    let projected1: Vec<ProjectedAgentEvent> = batch1.iter().filter_map(|e| e.project()).collect();
    assert_eq!(projected1.len(), 1);
    let last_cursor = projected1.last().unwrap().cursor;

    // Publish events *after* sub1 polled but *before* the fresh subscription.
    // These sit in the ring buffer and are beyond sub1's cursor.
    hub.publish(EventPayload::Fault("ignored".into()));
    hub.publish(EventPayload::RecordCommitted(Record::StepAttempt {
        id: "s1".into(),
        seq: 1,
        lane: "main".into(),
        timestamp: 1,
        run_id: "run-1".into(),
        attempt: 1,
        result_entry_id: "r1".into(),
        compaction_reason: None,
    }));

    // A fresh subscription starts at the current next_id. Its snapshot
    // includes the persisted store state but not the buffered hub events
    // published after sub1 polled — those are only visible via poll.
    let mut sub2 = hub.subscribe(&store).unwrap();
    assert_eq!(sub2.snapshot.entries.len(), 1);
    assert_eq!(sub2.snapshot.records.len(), 1);

    // Publish one more event; the fresh subscriber MUST see exactly this
    // new event and no duplicates of previously published events.
    hub.publish(EventPayload::RecordCommitted(Record::StepAttempt {
        id: "s2".into(),
        seq: 1,
        lane: "main".into(),
        timestamp: 1,
        run_id: "run-1".into(),
        attempt: 2,
        result_entry_id: "r2".into(),
        compaction_reason: None,
    }));
    let batch2 = hub.poll(&mut sub2).unwrap();
    let projected2: Vec<ProjectedAgentEvent> = batch2.iter().filter_map(|e| e.project()).collect();
    assert_eq!(projected2.len(), 1);
    assert!(projected2[0].cursor > last_cursor);
}
#[test]
fn cursor_tracks_last_projected_event_across_poll_batches() {
    let store = MemoryStore::new("session-1");
    let hub = HarnessEventHub::new(16);

    // Subscribe before publishing so the subscription sees buffered events.
    let mut subscription = hub.subscribe(&store).unwrap();
    hub.publish(EventPayload::RecordCommitted(Record::OperationStarted {
        id: "run-1".into(),
        seq: 1,
        lane: "main".into(),
        timestamp: 1,
        source_leaf_id: None,
        intent: OperationIntent::Run,
    }));
    hub.publish(EventPayload::RecordCommitted(Record::StepAttempt {
        id: "s1".into(),
        seq: 1,
        lane: "main".into(),
        timestamp: 1,
        run_id: "run-1".into(),
        attempt: 1,
        result_entry_id: "r1".into(),
        compaction_reason: None,
    }));

    let batch1 = hub.poll(&mut subscription).unwrap();
    let projected1: Vec<ProjectedAgentEvent> = batch1.iter().filter_map(|e| e.project()).collect();
    assert_eq!(projected1.len(), 2);
    let cursor_after_batch1 = projected1.last().unwrap().cursor;

    hub.publish(EventPayload::RecordCommitted(Record::OperationFinished {
        id: "op1".into(),
        seq: 1,
        lane: "main".into(),
        timestamp: 1,
        run_id: "run-1".into(),
        outcome: OperationOutcome::Completed,
        error: None,
    }));

    let batch2 = hub.poll(&mut subscription).unwrap();
    let projected2: Vec<ProjectedAgentEvent> = batch2.iter().filter_map(|e| e.project()).collect();
    assert_eq!(projected2.len(), 1);
    assert!(projected2[0].cursor > cursor_after_batch1);
}

// ── Duplicate resistance ───────────────────────────────────────────────────

#[test]
fn poll_without_new_events_returns_empty() {
    let store = MemoryStore::new("session-1");
    let hub = HarnessEventHub::new(8);

    // Subscribe first to capture the starting cursor, then publish.
    let mut subscription = hub.subscribe(&store).unwrap();

    hub.publish(EventPayload::RecordCommitted(Record::OperationStarted {
        id: "op1".into(),
        seq: 1,
        lane: "main".into(),
        timestamp: 1,
        source_leaf_id: None,
        intent: OperationIntent::Run,
    }));

    let batch1 = hub.poll(&mut subscription).unwrap();
    assert_eq!(batch1.len(), 1);

    // Second poll with no new publishes yields nothing.
    let batch2 = hub.poll(&mut subscription).unwrap();
    assert!(batch2.is_empty());
}

#[test]
fn re_poll_does_not_re_emit_projected_events() {
    let store = MemoryStore::new("session-1");
    let hub = HarnessEventHub::new(8);

    // Subscribe before publishing so the poll sees the event.
    let mut subscription = hub.subscribe(&store).unwrap();

    hub.publish(EventPayload::RecordCommitted(Record::OperationStarted {
        id: "op1".into(),
        seq: 1,
        lane: "main".into(),
        timestamp: 1,
        source_leaf_id: None,
        intent: OperationIntent::Run,
    }));

    let first_poll = hub.poll(&mut subscription).unwrap();
    let first_projected: Vec<ProjectedAgentEvent> =
        first_poll.iter().filter_map(|e| e.project()).collect();
    assert_eq!(first_projected.len(), 1);

    let second_poll = hub.poll(&mut subscription).unwrap();
    let second_projected: Vec<ProjectedAgentEvent> =
        second_poll.iter().filter_map(|e| e.project()).collect();
    assert!(second_projected.is_empty());
}

#[test]
fn gap_error_stops_projection_but_preserves_cursor_for_reconnect() {
    let store = MemoryStore::new("session-1");
    let hub = HarnessEventHub::new(2);

    let mut subscription = hub.subscribe(&store).unwrap();

    // Overflow the ring buffer to induce a gap.
    hub.publish(EventPayload::RecordCommitted(Record::OperationStarted {
        id: "run-1".into(),
        seq: 1,
        lane: "main".into(),
        timestamp: 1,
        source_leaf_id: None,
        intent: OperationIntent::Run,
    }));
    hub.publish(EventPayload::RecordCommitted(Record::StepAttempt {
        id: "s1".into(),
        seq: 1,
        lane: "main".into(),
        timestamp: 1,
        run_id: "run-1".into(),
        attempt: 1,
        result_entry_id: "r1".into(),
        compaction_reason: None,
    }));
    hub.publish(EventPayload::RecordCommitted(Record::OperationFinished {
        id: "op1".into(),
        seq: 1,
        lane: "main".into(),
        timestamp: 1,
        run_id: "run-1".into(),
        outcome: OperationOutcome::Completed,
        error: None,
    }));

    let result = hub.poll(&mut subscription);
    assert!(result.is_err());

    // A fresh subscription after the gap starts clean with cursor past
    // evicted events. No gap error, but also no catch-up events since
    // all published events are before the fresh cursor.
    let mut new_sub = hub.subscribe(&store).unwrap();
    let batch = hub.poll(&mut new_sub).unwrap();
    assert!(batch.is_empty());

    // New events published after the fresh subscription are visible.
    hub.publish(EventPayload::RecordCommitted(Record::StepAttempt {
        id: "s2".into(),
        seq: 1,
        lane: "main".into(),
        timestamp: 1,
        run_id: "run-1".into(),
        attempt: 2,
        result_entry_id: "r2".into(),
        compaction_reason: None,
    }));
    let catch_up = hub.poll(&mut new_sub).unwrap();
    let projected: Vec<ProjectedAgentEvent> = catch_up.iter().filter_map(|e| e.project()).collect();
    assert_eq!(projected.len(), 1);
}

// ── Restart hydration ─────────────────────────────────────────────────────

#[test]
fn restart_hydrates_operation_intents_from_store() {
    // Simulate a restart: the store contains an OperationStarted(Run) that
    // was committed before the process went down.  A fresh hub and
    // subscription must pick up the intent so that a later
    // OperationFinished can project to AgentEnd.
    let mut store = MemoryStore::new("session-1");
    store.append_record(Record::OperationStarted {
        id: "run-1".into(),
        seq: 1,
        lane: "main".into(),
        timestamp: 1,
        source_leaf_id: None,
        intent: OperationIntent::Run,
    });

    // Fresh hub — no prior OperationStarted was published through it.
    let hub = HarnessEventHub::new(8);
    let mut subscription = hub.subscribe(&store).unwrap();

    // Publish the matching OperationFinished.  If hydration worked, the hub
    // knows this run's intent is Run and the finished record projects to
    // AgentEnd.
    hub.publish(EventPayload::RecordCommitted(Record::OperationFinished {
        id: "fin-1".into(),
        seq: 2,
        lane: "main".into(),
        timestamp: 2,
        run_id: "run-1".into(),
        outcome: OperationOutcome::Completed,
        error: None,
    }));

    let events = hub.poll(&mut subscription).unwrap();
    let projected: Vec<ProjectedAgentEvent> = events.iter().filter_map(|e| e.project()).collect();
    assert_eq!(projected.len(), 1);
    assert!(
        matches!(&projected[0].event, AgentEvent::AgentEnd { .. }),
        "restart hydration must project AgentEnd from stored OperationStarted intent"
    );
}

#[test]
fn restart_hydration_excludes_completed_historical_runs() {
    let mut store = MemoryStore::new("session-1");

    // Completed historical run: both start and finish are persisted.
    store.append_record(Record::OperationStarted {
        id: "run-completed".into(),
        seq: 1,
        lane: "main".into(),
        timestamp: 1,
        source_leaf_id: None,
        intent: OperationIntent::Run,
    });
    store.append_record(Record::OperationFinished {
        id: "fin-completed".into(),
        seq: 2,
        lane: "main".into(),
        timestamp: 2,
        run_id: "run-completed".into(),
        outcome: OperationOutcome::Completed,
        error: None,
    });

    // Open run: only the start is persisted (process went down before finish).
    store.append_record(Record::OperationStarted {
        id: "run-open".into(),
        seq: 3,
        lane: "main".into(),
        timestamp: 3,
        source_leaf_id: None,
        intent: OperationIntent::Run,
    });

    // Fresh hub — hydration must only keep the open run's intent.
    let hub = HarnessEventHub::new(8);
    let mut subscription = hub.subscribe(&store).unwrap();

    // Duplicate finish for the completed run: must NOT project because
    // hydration cleared its intent.
    hub.publish(EventPayload::RecordCommitted(Record::OperationFinished {
        id: "dup-fin-completed".into(),
        seq: 100,
        lane: "main".into(),
        timestamp: 100,
        run_id: "run-completed".into(),
        outcome: OperationOutcome::Completed,
        error: None,
    }));

    // Finish for the open run: MUST project to AgentEnd.
    hub.publish(EventPayload::RecordCommitted(Record::OperationFinished {
        id: "fin-open".into(),
        seq: 101,
        lane: "main".into(),
        timestamp: 101,
        run_id: "run-open".into(),
        outcome: OperationOutcome::Completed,
        error: None,
    }));

    let events = hub.poll(&mut subscription).unwrap();
    let projected: Vec<ProjectedAgentEvent> = events.iter().filter_map(|e| e.project()).collect();

    // Only the open-run finish projects; the duplicate for the completed
    // run is dropped because its intent was cleared by the store's
    // persisted finish.
    assert_eq!(
        projected.len(),
        1,
        "only the open-run finish must project; duplicate completed-run finish must not"
    );
    assert!(
        matches!(&projected[0].event, AgentEvent::AgentEnd { .. }),
        "the open-run finish must project to AgentEnd"
    );
}

// ── Non-Run operation finishes ────────────────────────────────────────────

#[test]
fn non_run_operation_finished_produces_no_terminal_event() {
    let hub = HarnessEventHub::new(8);

    // Navigation and Compaction intents are not Run — their finishes must
    // never produce AgentEnd or AgentError.
    for intent in [OperationIntent::Navigation, OperationIntent::Compaction] {
        let intent_label = format!("{intent:?}");
        // Establish the non-Run intent.
        hub.publish(EventPayload::RecordCommitted(Record::OperationStarted {
            id: "run-1".into(),
            seq: 1,
            lane: "main".into(),
            timestamp: 1,
            source_leaf_id: None,
            intent,
        }));

        // Completing a non-Run operation must not project.
        let finished = hub.publish(EventPayload::RecordCommitted(Record::OperationFinished {
            id: format!("fin-{intent_label}"),
            seq: 2,
            lane: "main".into(),
            timestamp: 2,
            run_id: "run-1".into(),
            outcome: OperationOutcome::Completed,
            error: None,
        }));
        assert!(
            finished.project_agent_event().is_none(),
            "{intent_label} Completed must not project to a terminal event"
        );

        // Failing a non-Run operation must not project either.
        let failed = hub.publish(EventPayload::RecordCommitted(Record::OperationFinished {
            id: format!("fail-{intent_label}"),
            seq: 3,
            lane: "main".into(),
            timestamp: 3,
            run_id: "run-1".into(),
            outcome: OperationOutcome::Failed,
            error: Some("err".into()),
        }));
        assert!(
            failed.project_agent_event().is_none(),
            "{intent_label} Failed must not project to a terminal event"
        );
    }
}

#[test]
fn agent_harness_events_projectable_after_drive_to_completion() {
    let hub = HarnessEventHub::new(16);
    let mut harness = AgentHarness::with_events(MemoryStore::new("session-1"), hub.clone());

    // Start a Run operation on the main lane. This parks the OperationStarted
    // effect but does not commit it yet.
    harness
        .start_operation("run-1", None, OperationIntent::Run)
        .unwrap();

    // Subscribe before driving so poll observes events published during driving.
    let mut subscription = hub.subscribe(harness.store()).unwrap();

    // Drive the parked OperationStarted effect. This commits the record and
    // publishes it, producing an AgentStart through the correlated intent.
    harness.drive_to_completion().unwrap();

    // Explicitly finish the operation and drive that effect to commit the
    // OperationFinished record.  The hub resolves the Run intent from the
    // earlier OperationStarted, so the finished record projects to AgentEnd.
    harness
        .finish_operation("run-1", OperationOutcome::Completed, None)
        .unwrap();
    harness.drive_to_completion().unwrap();

    let events = hub.poll(&mut subscription).unwrap();

    // The harness publishes committed Record events. Projection converts
    // those into lifecycle AgentEvents.
    let projected: Vec<_> = events.iter().filter_map(|e| e.project()).collect();

    // A complete Run lifecycle must produce at least two projectable events
    // (AgentStart + AgentEnd).
    assert!(
        projected.len() >= 2,
        "drive_to_completion must produce AgentStart and AgentEnd, got {projected:?}"
    );

    // Every projected event must have a valid cursor and a known variant.
    for p in &projected {
        assert!(p.cursor > 0);
        assert!(matches!(
            &p.event,
            AgentEvent::AgentStart
                | AgentEvent::TurnStart { .. }
                | AgentEvent::MessageStart { .. }
                | AgentEvent::MessageUpdate { .. }
                | AgentEvent::MessageEnd { .. }
                | AgentEvent::TurnEnd { .. }
                | AgentEvent::AgentEnd { .. }
                | AgentEvent::AgentError { .. }
                | AgentEvent::ToolExecutionStart { .. }
                | AgentEvent::ToolExecutionUpdate { .. }
                | AgentEvent::ToolExecutionEnd { .. }
                | AgentEvent::SubagentQueued { .. }
                | AgentEvent::SubagentStarted { .. }
                | AgentEvent::SubagentFinished { .. }
                | AgentEvent::SubagentRecovery { .. }
                | AgentEvent::PlanUpdated { .. }
                | AgentEvent::StreamRuleTriggered { .. }
        ));
    }

    // Non-projectable events in the stream should be safely skipped.
    let total_events = events.len();
    let agent_events = projected.len();
    assert!(agent_events <= total_events);

    // A successful drive_to_completion must produce exactly one correlated
    // AgentEnd (not AgentError).
    assert!(
        projected
            .iter()
            .any(|p| matches!(&p.event, AgentEvent::AgentEnd { .. })),
        "successful drive_to_completion must produce AgentEnd"
    );
    assert!(
        !projected
            .iter()
            .any(|p| matches!(&p.event, AgentEvent::AgentError { .. })),
        "successful drive_to_completion must not produce AgentError"
    );
}

#[test]
fn projection_round_trips_through_harness_event_hub() {
    let hub = HarnessEventHub::new(16);
    let store = MemoryStore::new("session-1");

    // Subscribe before publishing so the poll observes the committed entry.
    let mut subscription = hub.subscribe(&store).unwrap();

    // Publish a committed entry, then project, and verify round-trip.
    let msg = AgentMessage::user("round-trip", vec![]);
    let entry = Entry {
        id: "e1".into(),
        parent_id: None,
        lane: "main".into(),
        seq: 1,
        timestamp: 1,
        message: msg.clone(),
        terminate: false,
    };
    let harness_event = hub.publish(EventPayload::EntryCommitted(entry));

    let batch = hub.poll(&mut subscription).unwrap();

    let projected: Vec<ProjectedAgentEvent> = batch.iter().filter_map(|e| e.project()).collect();
    assert_eq!(projected.len(), 1);
    assert_eq!(projected[0].cursor, harness_event.id);
    assert!(matches!(&projected[0].event, AgentEvent::MessageEnd { message } if message == &msg));
}
