use threadlane_agent::harness::{
    AgentHarness, LaneHandle, MemoryStore, ProcedureError, ProvisionedEntry, QueueKind,
    ReduceError, SessionAgent,
};

use threadlane_agent::AgentMessage;
// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn user(content: &str) -> AgentMessage {
    AgentMessage::user(content, vec![])
}

fn assistant(text: &str) -> AgentMessage {
    AgentMessage::Assistant {
        content: Some(text.into()),
        tool_calls: None,
        stop_reason: Some("stop".into()),
        deferred_handle: None,
    }
}

// LaneHandle validation
// ---------------------------------------------------------------------------

#[test]
fn lane_handle_rejects_empty_name() {
    let err = LaneHandle::new("".into()).unwrap_err();
    assert!(matches!(err, ReduceError::InvalidLane(_)));
}

#[test]
fn lane_handle_rejects_whitespace_name() {
    let err = LaneHandle::new("   ".into()).unwrap_err();
    assert!(matches!(err, ReduceError::InvalidLane(_)));
}

#[test]
fn lane_handle_accepts_valid_name() {
    let handle = LaneHandle::new("tasks".into()).unwrap();
    assert_eq!(handle.name(), "tasks");
}

// ---------------------------------------------------------------------------
// main_lane() — the reducer always materialises a "main" lane.
// ---------------------------------------------------------------------------

#[test]
fn main_lane_available_on_fresh_session() {
    let harness = AgentHarness::new(MemoryStore::new("session-1"));
    let session = SessionAgent::new(harness);
    let main = session.main_lane().unwrap();
    assert_eq!(main.name(), "main");
}

#[test]
fn main_lane_persists_after_operations() {
    let harness = AgentHarness::new(MemoryStore::new("session-1"));
    let mut session = SessionAgent::new(harness);

    let main = session.main_lane().unwrap();
    session
        .accept_prompt(&main, "run-1", user("hello"))
        .unwrap();
    session.drive_to_completion(&main).unwrap();

    // The lane remains resolvable.
    let main = session.main_lane().unwrap();
    assert_eq!(main.name(), "main");
}

// ---------------------------------------------------------------------------
// Unknown lane rejected without a write
// ---------------------------------------------------------------------------

#[test]
fn unknown_lane_rejected_without_writes() {
    let harness = AgentHarness::new(MemoryStore::new("session-1"));
    let session = SessionAgent::new(harness);

    let result = session.lane("nonexistent");
    assert!(matches!(result, Err(ReduceError::InvalidLane(ref name)) if name == "nonexistent"));

    // Confirm the store is still empty — the rejection did not create a lane.
    let snap = session.snapshot().unwrap();
    assert!(snap.entries.is_empty());
    assert!(snap.records.is_empty());
}

// ---------------------------------------------------------------------------
// Stale / unknown handle — operations rejected without writes
// ---------------------------------------------------------------------------

#[test]
fn unknown_lane_accept_rejected_without_writes() {
    let harness = AgentHarness::new(MemoryStore::new("session-1"));
    let mut session = SessionAgent::new(harness);
    let ghost = LaneHandle::new("ghost".into()).unwrap();

    let result = session.accept_prompt(&ghost, "run-1", user("hello"));
    assert!(matches!(&result, Err(ProcedureError::Invalid(msg)) if msg.contains("ghost")));

    // Confirm no entries or records were created.
    let snap = session.snapshot().unwrap();
    assert!(snap.entries.is_empty(), "store must have no entries");
    assert!(snap.records.is_empty(), "store must have no records");
}

#[test]
fn unknown_lane_enqueue_rejected_without_writes() {
    let harness = AgentHarness::new(MemoryStore::new("session-1"));
    let mut session = SessionAgent::new(harness);
    let ghost = LaneHandle::new("ghost".into()).unwrap();
    let target = ProvisionedEntry {
        id: "entry-1".into(),
        parent_id: None,
        message: user("queued task"),
    };

    let result = session.enqueue(&ghost, Some("run-1"), QueueKind::Steer, target);
    assert!(matches!(&result, Err(ProcedureError::Invalid(msg)) if msg.contains("ghost")));

    let snap = session.snapshot().unwrap();
    assert!(snap.entries.is_empty());
    assert!(snap.records.is_empty());
}

#[test]
fn unknown_lane_cancel_rejected_without_writes() {
    let harness = AgentHarness::new(MemoryStore::new("session-1"));
    let mut session = SessionAgent::new(harness);
    let ghost = LaneHandle::new("ghost".into()).unwrap();

    let result = session.cancel_queued(&ghost, Some("run-1"), "entry-1");
    assert!(matches!(&result, Err(ProcedureError::Invalid(msg)) if msg.contains("ghost")));

    let snap = session.snapshot().unwrap();
    assert!(snap.entries.is_empty());
    assert!(snap.records.is_empty());
}

#[test]
fn unknown_lane_drive_rejected_without_writes() {
    let harness = AgentHarness::new(MemoryStore::new("session-1"));
    let mut session = SessionAgent::new(harness);
    let ghost = LaneHandle::new("ghost".into()).unwrap();

    // drive_one
    let result = session.drive_one(&ghost);
    assert!(result.is_err());

    // drive_to_completion
    let result = session.drive_to_completion(&ghost);
    assert!(result.is_err());

    let snap = session.snapshot().unwrap();
    assert!(snap.entries.is_empty());
    assert!(snap.records.is_empty());
}

// ---------------------------------------------------------------------------
// Wrong-lane abort — handle cannot abort another lane's operation
// ---------------------------------------------------------------------------

#[test]
fn wrong_lane_abort_rejected() {
    let harness = AgentHarness::new(MemoryStore::new("session-1"));
    let mut session = SessionAgent::new(harness);
    let main = session.main_lane().unwrap();

    // Bootstrap a child lane with an open operation through the facade.
    // bootstrap_child_lane parks the prompt and drives the first effect
    // (OperationStarted), leaving the operation open.
    let _child = session
        .bootstrap_child_lane("child", "run-child", user("child task"))
        .unwrap();

    // main lane handle must NOT be able to abort child's operation.
    let result = session.request_abort(&main, "run-child");
    assert!(
        matches!(&result, Err(ProcedureError::Invalid(msg)) if msg.contains("does not own operation"))
    );

    // Child lane and its open operation are still intact.
    let snap = session.snapshot().unwrap();
    let child_lane = snap.state.lane("child").expect("child lane must exist");
    assert_eq!(
        child_lane.open_operation.as_deref(),
        Some("run-child"),
        "child lane should still have its open operation"
    );
    assert!(
        !child_lane.abort_requested,
        "abort must not have been requested"
    );
}

#[test]
fn unknown_lane_abort_rejected() {
    let harness = AgentHarness::new(MemoryStore::new("session-1"));
    let mut session = SessionAgent::new(harness);
    let ghost = LaneHandle::new("ghost".into()).unwrap();

    let result = session.request_abort(&ghost, "run-1");
    assert!(matches!(&result, Err(ProcedureError::Invalid(msg)) if msg.contains("unknown lane")));

    let snap = session.snapshot().unwrap();
    assert!(snap.entries.is_empty());
    assert!(snap.records.is_empty());
}

// ---------------------------------------------------------------------------
// accept_prompt + drive_to_completion = one durable run
// ---------------------------------------------------------------------------

#[test]
fn prompt_driven_to_completion_is_durable() {
    let harness = AgentHarness::new(MemoryStore::new("session-1"));
    let mut session = SessionAgent::new(harness);
    let main = session.main_lane().unwrap();

    let before_entries = session.harness().store().entries().len();
    assert_eq!(before_entries, 0, "fresh store should be empty");

    // Accept a prompt through the facade.
    let _result_entry_id = session
        .accept_prompt(&main, "run-1", user("do the thing"))
        .unwrap();

    // Parked — not yet durable.
    let snap_before = session.snapshot().unwrap();
    assert_eq!(snap_before.entries.len(), before_entries);

    // Drive to completion on the lane.
    session.drive_to_completion(&main).unwrap();

    let snap_after = session.snapshot().unwrap();
    assert!(
        snap_after.entries.len() > before_entries,
        "expected entries after drive"
    );

    // The user entry was created.
    let user_entry_id = "entry-run-1-user";
    assert!(
        snap_after.entries.iter().any(|e| e.id == user_entry_id),
        "user entry {user_entry_id} not found in store"
    );

    // Operation started record was committed.
    assert!(
        snap_after.records.iter().any(|r| r.id() == "run-1"),
        "operation start record missing"
    );
}

// ---------------------------------------------------------------------------
// Two lanes — unique monotonically increasing sequences
// ---------------------------------------------------------------------------

#[test]
fn two_lanes_receive_increasing_sequences() {
    let harness = AgentHarness::new(MemoryStore::new("session-1"));
    let mut session = SessionAgent::new(harness);

    let main = session.main_lane().unwrap();

    // Accept a prompt on the main lane.
    session
        .accept_prompt(&main, "run-main", user("main task"))
        .unwrap();
    session.drive_to_completion(&main).unwrap();

    // Bootstrap a child lane through the canonical facade method.
    let child = session
        .bootstrap_child_lane("child", "run-child", user("child task"))
        .unwrap();
    session.drive_to_completion(&child).unwrap();

    // All entries must have monotonically increasing seq values.
    let entries = session.harness().store().entries();
    assert!(
        entries.len() >= 2,
        "expected at least 2 entries, got {}",
        entries.len()
    );
    for window in entries.windows(2) {
        assert!(
            window[1].seq > window[0].seq,
            "sequence not monotonic: {} followed by {}",
            window[0].seq,
            window[1].seq,
        );
    }

    // All sequences are unique.
    let mut seqs: Vec<u64> = entries.iter().map(|e| e.seq).collect();
    seqs.sort();
    let mut deduped = seqs.clone();
    deduped.dedup();
    assert_eq!(seqs.len(), deduped.len(), "duplicate sequences found");
}

// ---------------------------------------------------------------------------
// Snapshot and subscription agreement
// ---------------------------------------------------------------------------

#[test]
fn snapshot_and_subscription_agree_on_lane_state() {
    let harness = AgentHarness::new(MemoryStore::new("session-1"));
    let mut session = SessionAgent::new(harness);
    let main = session.main_lane().unwrap();

    // Accept a prompt and drive it.
    session
        .accept_prompt(&main, "run-1", user("first prompt"))
        .unwrap();
    session.drive_to_completion(&main).unwrap();

    // Snapshot now.
    let snapshot = session.snapshot().unwrap();

    // Subscribe and compare baseline snapshot.
    let subscription = session.watch_session().unwrap();

    assert_eq!(
        snapshot.session_id, subscription.snapshot.session_id,
        "session ids should match"
    );
    assert_eq!(
        snapshot.entries.len(),
        subscription.snapshot.entries.len(),
        "entry counts should match"
    );

    // Lane state in both should agree.
    let snap_main = snapshot.state.lane("main").unwrap();
    let sub_main = subscription.snapshot.state.lane("main").unwrap();
    assert_eq!(snap_main.status, sub_main.status);
    assert_eq!(snap_main.attempts, sub_main.attempts);
    assert_eq!(snap_main.open_operation, sub_main.open_operation);
}

#[test]
fn lane_scoped_watch_filters_events() {
    let harness = AgentHarness::new(MemoryStore::new("session-1"));
    let mut session = SessionAgent::new(harness);
    let main = session.main_lane().unwrap();

    // Create subscriptions BEFORE publishing any events so that poll()
    // captures every committed event from this point forward.
    // Use the event hub directly because SessionAgent::watch validates
    // lane existence, and the child lane does not exist yet.
    let harness_ref = session.harness();
    let events = harness_ref.events();
    let mut main_sub = events
        .subscribe_for_lane(harness_ref, Some("main"))
        .unwrap();
    let mut child_sub = events
        .subscribe_for_lane(harness_ref, Some("child"))
        .unwrap();

    // Publish committed events on main (entries + records).
    session
        .accept_prompt(&main, "run-main", user("main task"))
        .unwrap();
    session.drive_to_completion(&main).unwrap();

    // Bootstrap a child lane — this publishes entries + records with
    // lane = "child" through the harness event hub.
    let child = session
        .bootstrap_child_lane("child", "run-child", user("child task"))
        .unwrap();
    session.drive_to_completion(&child).unwrap();

    // Poll the main subscription — must be non-empty and contain only
    // main-lane events.
    let main_events = session.harness().events().poll(&mut main_sub).unwrap();
    assert!(
        !main_events.is_empty(),
        "main subscription must receive at least one event"
    );
    for event in &main_events {
        assert_eq!(
            event.lane.as_deref(),
            Some("main"),
            "main subscription leaked event from lane '{:?}'",
            event.lane,
        );
    }

    // Poll the child subscription — must be non-empty and contain at
    // least one child-lane event, with no main-lane events leaked.
    let child_events = session.harness().events().poll(&mut child_sub).unwrap();
    assert!(
        !child_events.is_empty(),
        "child subscription must receive at least one event"
    );
    let has_child = child_events
        .iter()
        .any(|e| e.lane.as_deref() == Some("child"));
    assert!(
        has_child,
        "child subscription must contain at least one child-lane event"
    );
    for event in &child_events {
        assert_ne!(
            event.lane.as_deref(),
            Some("main"),
            "child subscription leaked main-lane event: {:?}",
            event.lane,
        );
    }
}

// ---------------------------------------------------------------------------
// Bound queue dispatch — enqueue, drive, cancel through the facade
// ---------------------------------------------------------------------------

#[test]
fn bound_queue_enqueue_and_cancel() {
    let harness = AgentHarness::new(MemoryStore::new("session-1"));
    let mut session = SessionAgent::new(harness);
    let main = session.main_lane().unwrap();

    // Accept and drive an operation to completion.
    session
        .accept_prompt(&main, "run-1", user("base task"))
        .unwrap();
    session.drive_to_completion(&main).unwrap();

    // Enqueue a bound entry through the facade.
    let target = ProvisionedEntry {
        id: "queued-1".into(),
        parent_id: None,
        message: user("queued task"),
    };
    session
        .enqueue(&main, Some("run-1"), QueueKind::Steer, target)
        .unwrap();

    // Drive to commit the QueueEnqueued record.
    session.drive_to_completion(&main).unwrap();

    // Assert QueueEnqueued record with bound run_id.
    let snap = session.snapshot().unwrap();
    let queue_record = snap
        .records
        .iter()
        .find_map(|r| {
            if let threadlane_agent::harness::Record::QueueEnqueued { run_id, id, .. } = r {
                Some((id.clone(), run_id.clone()))
            } else {
                None
            }
        })
        .expect("QueueEnqueued record not found");
    assert_eq!(
        queue_record.1.as_deref(),
        Some("run-1"),
        "bound queue entry must carry the bound run_id"
    );

    // Cancel the queued entry through the facade (match by target.id).
    session
        .cancel_queued(&main, Some("run-1"), "queued-1")
        .unwrap();

    // Drive to commit the QueueCancelled record.
    session.drive_to_completion(&main).unwrap();

    // Assert QueueCancelled record exists.
    let snap = session.snapshot().unwrap();
    assert!(
        snap.records
            .iter()
            .any(|r| { matches!(r, threadlane_agent::harness::Record::QueueCancelled { .. }) }),
        "QueueCancelled record missing"
    );
}

// ---------------------------------------------------------------------------
// Unbound queue dispatch — enqueue, drive, consume through the facade
// ---------------------------------------------------------------------------

#[test]
fn unbound_queue_enqueue_and_consume() {
    let harness = AgentHarness::new(MemoryStore::new("session-1"));
    let mut session = SessionAgent::new(harness);
    let main = session.main_lane().unwrap();

    // Accept and drive an operation to completion.
    session
        .accept_prompt(&main, "run-1", user("base task"))
        .unwrap();
    session.drive_to_completion(&main).unwrap();

    // Enqueue an unbound entry through the facade.
    let target = ProvisionedEntry {
        id: "queued-2".into(),
        parent_id: None,
        message: user("unbound task"),
    };
    session
        .enqueue(&main, None, QueueKind::FollowUp, target)
        .unwrap();

    // Drive to commit the QueueEnqueued record.
    session.drive_to_completion(&main).unwrap();

    // Assert QueueEnqueued record with no run_id.
    let snap = session.snapshot().unwrap();
    let queue_record = snap
        .records
        .iter()
        .find_map(|r| {
            if let threadlane_agent::harness::Record::QueueEnqueued { run_id, id, .. } = r {
                Some((id.clone(), run_id.clone()))
            } else {
                None
            }
        })
        .expect("QueueEnqueued record not found");
    assert!(
        queue_record.1.is_none(),
        "unbound queue entry must have no run_id"
    );
    // Cancel the unbound entry through the facade (match by target.id).
    session.cancel_queued(&main, None, "queued-2").unwrap();

    // Drive to commit the QueueCancelled record.
    session.drive_to_completion(&main).unwrap();

    // Assert QueueCancelled record exists.
    let snap = session.snapshot().unwrap();
    assert!(
        snap.records
            .iter()
            .any(|r| { matches!(r, threadlane_agent::harness::Record::QueueCancelled { .. }) }),
        "QueueCancelled record missing after unbound cancel"
    );
}

// ---------------------------------------------------------------------------
// Consume queued entry through the facade
// ---------------------------------------------------------------------------

#[test]
fn consume_queued_entry_through_facade() {
    let harness = AgentHarness::new(MemoryStore::new("session-1"));
    let mut session = SessionAgent::new(harness);
    let main = session.main_lane().unwrap();

    // Accept and drive an operation.
    session
        .accept_prompt(&main, "run-1", user("base task"))
        .unwrap();
    session.drive_to_completion(&main).unwrap();

    // Enqueue a bound entry.
    let target = ProvisionedEntry {
        id: "queued-consume".into(),
        parent_id: None,
        message: user("to be consumed"),
    };
    session
        .enqueue(&main, Some("run-1"), QueueKind::NextRun, target)
        .unwrap();
    session.drive_to_completion(&main).unwrap();

    // Consume the queued entry through the facade (match by target.id).
    session
        .consume_queued(&main, Some("run-1"), "queued-consume")
        .unwrap();
    // Drive to commit the QueueConsumed record.
    session.drive_to_completion(&main).unwrap();

    // Assert QueueConsumed record exists.
    let snap = session.snapshot().unwrap();
    assert!(
        snap.records
            .iter()
            .any(|r| { matches!(r, threadlane_agent::harness::Record::QueueConsumed { .. }) }),
        "QueueConsumed record missing"
    );
}

// ---------------------------------------------------------------------------
// Multi-lane sequence uniqueness — queued work before prompt is driven
// ---------------------------------------------------------------------------

#[test]
fn multi_lane_sequence_uniqueness_with_queued_work() {
    let harness = AgentHarness::new(MemoryStore::new("session-ml"));
    let mut session = SessionAgent::new(harness);
    let main = session.main_lane().unwrap();

    // Accept a prompt on the main lane (parked, not driven yet).
    session
        .accept_prompt(&main, "run-main", user("main task"))
        .unwrap();

    // Enqueue an unbound entry on the main lane before driving.
    let queued_target = ProvisionedEntry {
        id: "queued-pre-drive".into(),
        parent_id: None,
        message: user("queued before drive"),
    };
    session
        .enqueue(&main, None, QueueKind::FollowUp, queued_target)
        .unwrap();

    // Bootstrap a child lane before driving the main lane.
    let child = session
        .bootstrap_child_lane("child", "run-child", user("child task"))
        .unwrap();

    // Enqueue a bound entry on the child lane.
    let child_target = ProvisionedEntry {
        id: "queued-child".into(),
        parent_id: None,
        message: user("child queued task"),
    };
    session
        .enqueue(&child, Some("run-child"), QueueKind::Steer, child_target)
        .unwrap();

    // Now drive both lanes to completion.
    session.drive_to_completion(&main).unwrap();
    session.drive_to_completion(&child).unwrap();
    let snap = session.snapshot().unwrap();

    // Collect all sequence numbers from entries.
    let mut seqs: Vec<u64> = snap.entries.iter().map(|e| e.seq).collect();
    // Collect all sequence numbers from records.
    for record in &snap.records {
        seqs.push(record.seq());
    }

    assert!(
        seqs.len() >= 4,
        "expected at least 4 sequenced items, got {}",
        seqs.len()
    );

    // All sequences must be strictly greater than 0.
    assert!(
        seqs.iter().all(|&s| s > 0),
        "all sequences must be positive"
    );

    // No duplicates.
    let mut sorted = seqs.clone();
    sorted.sort();
    let mut deduped = sorted.clone();
    deduped.dedup();
    assert_eq!(
        sorted.len(),
        deduped.len(),
        "duplicate sequence numbers found: {:?}",
        seqs
    );

    // --- Global commit order: records must be strictly increasing in
    //     append (commit) order, not merely after sorting. ---
    let record_seqs: Vec<u64> = snap.records.iter().map(|r| r.seq()).collect();
    for window in record_seqs.windows(2) {
        assert!(
            window[1] > window[0],
            "records not strictly increasing in append order: {} -> {}",
            window[0],
            window[1],
        );
    }

    // --- Per-lane monotonicity: each lane's records must be strictly
    //     increasing in the order they appear in snap.records. ---
    for lane_name in &["main", "child"] {
        let lane_seqs: Vec<u64> = snap
            .records
            .iter()
            .filter(|r| r.lane() == *lane_name)
            .map(|r| r.seq())
            .collect();
        assert!(
            !lane_seqs.is_empty(),
            "lane '{}' must have at least one record",
            lane_name,
        );
        for window in lane_seqs.windows(2) {
            assert!(
                window[1] > window[0],
                "lane '{}' records not strictly increasing: {} -> {}",
                lane_name,
                window[0],
                window[1],
            );
        }
    }

    // --- Bootstrap ordering: the first committed record must be a
    //     main-lane record, proving main committed before the child
    //     bootstrap. ---
    let first_record = snap.records.first().expect("at least one record");
    assert_eq!(
        first_record.lane(),
        "main",
        "first committed record must be main-lane, got '{}'; child-first commit detected",
        first_record.lane(),
    );
}

// ---------------------------------------------------------------------------
// Unbound queue dispatch — reject bound entry with None run_id
// ---------------------------------------------------------------------------

#[test]
fn unbound_cancel_rejects_bound_entry() {
    let harness = AgentHarness::new(MemoryStore::new("session-ucr"));
    let mut session = SessionAgent::new(harness);
    let main = session.main_lane().unwrap();

    // Accept and drive an operation.
    session
        .accept_prompt(&main, "run-1", user("base task"))
        .unwrap();
    session.drive_to_completion(&main).unwrap();

    // Enqueue a bound entry.
    let target = ProvisionedEntry {
        id: "bound-entry".into(),
        parent_id: None,
        message: user("bound task"),
    };
    session
        .enqueue(&main, Some("run-1"), QueueKind::Steer, target)
        .unwrap();
    session.drive_to_completion(&main).unwrap();

    // Attempt to cancel with None run_id against a bound entry must fail.
    let err = session
        .cancel_queued(&main, None, "bound-entry")
        .unwrap_err();
    assert!(
        matches!(err, ProcedureError::Invalid(_)),
        "expected Invalid, got {:?}",
        err
    );
    assert!(
        err.to_string().contains("bound"),
        "error should mention 'bound': {}",
        err
    );

    // Verify no QueueCancelled record was parked — snapshot unchanged.
    let snap = session.snapshot().unwrap();
    assert!(
        !snap
            .records
            .iter()
            .any(|r| { matches!(r, threadlane_agent::harness::Record::QueueCancelled { .. }) }),
        "QueueCancelled record must not exist after rejected unbound cancel"
    );
}

#[test]
fn unbound_consume_rejects_bound_entry() {
    let harness = AgentHarness::new(MemoryStore::new("session-uco"));
    let mut session = SessionAgent::new(harness);
    let main = session.main_lane().unwrap();

    // Accept and drive an operation.
    session
        .accept_prompt(&main, "run-1", user("base task"))
        .unwrap();
    session.drive_to_completion(&main).unwrap();

    // Enqueue a bound entry.
    let target = ProvisionedEntry {
        id: "bound-consume-entry".into(),
        parent_id: None,
        message: user("bound task"),
    };
    session
        .enqueue(&main, Some("run-1"), QueueKind::NextRun, target)
        .unwrap();
    session.drive_to_completion(&main).unwrap();

    // Attempt to consume with None run_id against a bound entry must fail.
    let err = session
        .consume_queued(&main, None, "bound-consume-entry")
        .unwrap_err();
    assert!(
        matches!(err, ProcedureError::Invalid(_)),
        "expected Invalid, got {:?}",
        err
    );
    assert!(
        err.to_string().contains("bound"),
        "error should mention 'bound': {}",
        err
    );

    // Verify no QueueConsumed record was parked.
    let snap = session.snapshot().unwrap();
    assert!(
        !snap
            .records
            .iter()
            .any(|r| { matches!(r, threadlane_agent::harness::Record::QueueConsumed { .. }) }),
        "QueueConsumed record must not exist after rejected unbound consume"
    );
}

#[test]
fn unbound_cancel_on_truly_unbound_entry_succeeds() {
    let harness = AgentHarness::new(MemoryStore::new("session-ucs"));
    let mut session = SessionAgent::new(harness);
    let main = session.main_lane().unwrap();

    // Accept and drive an operation.
    session
        .accept_prompt(&main, "run-1", user("base task"))
        .unwrap();
    session.drive_to_completion(&main).unwrap();

    // Enqueue an unbound entry.
    let target = ProvisionedEntry {
        id: "unbound-entry".into(),
        parent_id: None,
        message: user("unbound task"),
    };
    session
        .enqueue(&main, None, QueueKind::FollowUp, target)
        .unwrap();
    session.drive_to_completion(&main).unwrap();

    // Cancel with None run_id against a truly unbound entry must succeed.
    session.cancel_queued(&main, None, "unbound-entry").unwrap();
    session.drive_to_completion(&main).unwrap();

    // Verify QueueCancelled record exists.
    let snap = session.snapshot().unwrap();
    assert!(
        snap.records
            .iter()
            .any(|r| { matches!(r, threadlane_agent::harness::Record::QueueCancelled { .. }) }),
        "QueueCancelled record missing after valid unbound cancel"
    );
}

#[test]
fn unbound_consume_on_truly_unbound_entry_succeeds() {
    let harness = AgentHarness::new(MemoryStore::new("session-uco-ok"));
    let mut session = SessionAgent::new(harness);
    let main = session.main_lane().unwrap();

    // Accept and drive an operation.
    session
        .accept_prompt(&main, "run-1", user("base task"))
        .unwrap();
    session.drive_to_completion(&main).unwrap();

    // Enqueue an unbound entry.
    let target = ProvisionedEntry {
        id: "unbound-consume-entry".into(),
        parent_id: None,
        message: user("unbound task"),
    };
    session
        .enqueue(&main, None, QueueKind::NextRun, target)
        .unwrap();
    session.drive_to_completion(&main).unwrap();

    // Consume with None run_id against a truly unbound entry must succeed.
    session
        .consume_queued(&main, None, "unbound-consume-entry")
        .unwrap();
    session.drive_to_completion(&main).unwrap();

    // Verify QueueConsumed record exists.
    let snap = session.snapshot().unwrap();
    assert!(
        snap.records
            .iter()
            .any(|r| { matches!(r, threadlane_agent::harness::Record::QueueConsumed { .. }) }),
        "QueueConsumed record missing after valid unbound consume"
    );
}

// ---------------------------------------------------------------------------
// Pending effects + no-tool acceptance — next_seq_with_effects prevents
// sequence collisions when effects from other lanes are parked.
// ---------------------------------------------------------------------------

#[test]
fn pending_effects_preserve_no_tool_sequences() {
    use threadlane_agent::harness::{GatedEffects, NoToolRun};

    let mut store = MemoryStore::new("session-pending");

    // --- setup: complete a no-tool run on "main" so it exists and is idle ---
    let mut setup = GatedEffects::new();
    NoToolRun::accept(&store, "nt-setup", "setup", assistant("ready"), &mut setup).unwrap();
    setup.run_to_completion(&mut store).unwrap();

    // --- park a prompt on a child lane (creates pending effects) ---
    let mut effects = GatedEffects::new();
    threadlane_agent::harness::PromptProcedure::accept_on_lane(
        &store,
        "child",
        "run-child",
        user("child task"),
        &mut effects,
    )
    .unwrap();

    // --- accept a no-tool run on the (idle) main lane ---
    // NoToolRun::accept_on_lane MUST use next_seq_with_effects so the
    // sequences allocated to this run are strictly greater than the parked
    // child-lane sequences.
    NoToolRun::accept(&store, "nt-main", "hello", assistant("hi"), &mut effects).unwrap();

    effects.run_to_completion(&mut store).unwrap();

    // Every entry and record must have a unique sequence number.
    let mut seqs: Vec<u64> = store.entries().iter().map(|e| e.seq).collect();
    for record in store.records() {
        seqs.push(record.seq());
    }
    assert!(
        seqs.len() >= 12,
        "expected at least 12 sequenced items, got {}",
        seqs.len()
    );

    let mut sorted = seqs.clone();
    sorted.sort();
    sorted.dedup();
    assert_eq!(
        seqs.len(),
        sorted.len(),
        "duplicate sequence numbers found: {:?}",
        seqs
    );

    // Sequences must be strictly monotonic.
    for window in sorted.windows(2) {
        assert!(
            window[1] > window[0],
            "sequence not monotonic: {} followed by {}",
            window[0],
            window[1],
        );
    }
}

// ---------------------------------------------------------------------------
// Executor-backed child bootstrap — the executor commits synchronously
// on park(), so no pending actions remain and the lane is resolved directly.
// ---------------------------------------------------------------------------

#[test]
fn executor_backed_child_bootstrap_succeeds() {
    let shared = std::sync::Arc::new(std::sync::Mutex::new(MemoryStore::new("session-exec")));
    let target = shared.clone();
    let events = threadlane_agent::harness::HarnessEventHub::new(256);
    let harness =
        AgentHarness::with_executor(MemoryStore::new("session-exec"), events, move |action| {
            action.apply(&mut *target.lock().unwrap())
        });
    let mut session = SessionAgent::new(harness);

    // Bootstrap must succeed — the executor committed during park().
    let child = session
        .bootstrap_child_lane("child", "exec-run", user("exec child"))
        .unwrap();
    assert_eq!(child.name(), "child");

    // Verify the returned handle is recognized (known_external_lanes accepts it).
    assert!(session.lane("child").is_ok());

    // Verify the lane was committed to the executor's target store.
    let store = shared.lock().unwrap();
    let snap = threadlane_agent::harness::Reducer::reduce(&*store).unwrap();
    let lane = snap.lane("child").expect("child lane must exist");
    assert_eq!(lane.open_operation.as_deref(), Some("exec-run"));
}
