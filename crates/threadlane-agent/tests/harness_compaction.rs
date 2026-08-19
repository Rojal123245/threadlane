use threadlane_agent::harness::{
    CompactionProcedure, Entry, GatedEffects, LaneStatus, MemoryStore, Record, Reducer,
    SessionStore,
};
use threadlane_agent::AgentMessage;

#[test]
fn compaction_appends_a_summary_without_rewriting_history() {
    let mut store = MemoryStore::new("session-1");
    store
        .try_append_entry(Entry {
            id: "entry-1".into(),
            parent_id: None,
            lane: "main".into(),
            seq: 1,
            timestamp: 1,
            surface_op: threadlane_agent::harness::SurfaceOperation::Append,
            message: AgentMessage::user("old context", vec![]),
            terminate: false,
        })
        .unwrap();
    let mut effects = GatedEffects::new();
    CompactionProcedure::accept(
        &store,
        "run-compaction-1",
        "architecture and verification notes",
        &mut effects,
    )
    .unwrap();
    assert_eq!(store.entries().len(), 1);
    effects.run_to_completion(&mut store).unwrap();

    let reduced = Reducer::reduce(&store).unwrap();
    assert_eq!(reduced.lane("main").unwrap().status, LaneStatus::Completed);
    assert_eq!(store.entries().len(), 2);
    assert_eq!(
        reduced.lane("main").unwrap().leaf_id.as_deref(),
        Some("compaction-run-compaction-1-summary")
    );
    assert!(store.entries()[1].parent_id.is_none());
    assert!(matches!(
        &store.entries()[1].message,
        AgentMessage::Custom { custom_type, payload }
            if custom_type == "compaction_summary"
                && payload.get("schema_version").and_then(|value| value.as_u64()) == Some(1)
                && payload.get("checkpoint_kind").and_then(|value| value.as_str()) == Some("manual")
                && payload.get("source_leaf_id").and_then(|value| value.as_str()) == Some("entry-1")
    ));
    assert!(store.records().iter().any(|record| matches!(
        record,
        Record::StepAttempt { compaction_reason: Some(reason), .. } if reason == "manual"
    )));
}

#[test]
fn model_context_is_a_branch_projection_not_a_log_replay() {
    let mut store = MemoryStore::new("session-1");
    store
        .try_append_entry(Entry {
            id: "old-user".into(),
            parent_id: None,
            lane: "main".into(),
            seq: 1,
            timestamp: 1,
            surface_op: threadlane_agent::harness::SurfaceOperation::Append,
            message: AgentMessage::user("old context", vec![]),
            terminate: false,
        })
        .unwrap();
    let mut effects = GatedEffects::new();
    CompactionProcedure::accept(&store, "run-compaction-1", "retained facts", &mut effects)
        .unwrap();
    effects.run_to_completion(&mut store).unwrap();

    let projection = store.model_context("main").unwrap();
    assert_eq!(
        projection.leaf_id.as_deref(),
        Some("compaction-run-compaction-1-summary")
    );
    assert_eq!(projection.entries.len(), 1);
    assert_eq!(
        projection.checkpoint.as_ref().map(|checkpoint| checkpoint.entry_id.as_str()),
        Some("compaction-run-compaction-1-summary")
    );
    assert!(matches!(
        &projection.entries[0].message,
        AgentMessage::Custom { custom_type, .. } if custom_type == "compaction_summary"
    ));
    assert_eq!(store.entries().len(), 2, "history remains append-only");
    assert!(!projection.messages().is_empty());
}

#[test]
fn transcript_projection_retains_compacted_history_outside_model_context() {
    let mut store = MemoryStore::new("session-1");
    store
        .try_append_entry(Entry {
            id: "old-user".into(),
            parent_id: None,
            lane: "main".into(),
            seq: 1,
            timestamp: 1,
            surface_op: threadlane_agent::harness::SurfaceOperation::Append,
            message: AgentMessage::user("old context", vec![]),
            terminate: false,
        })
        .unwrap();
    let mut effects = GatedEffects::new();
    CompactionProcedure::accept(&store, "run-compaction-1", "retained facts", &mut effects)
        .unwrap();
    effects.run_to_completion(&mut store).unwrap();

    assert_eq!(store.model_context("main").unwrap().entries.len(), 1);
    let transcript = store.transcript("main");
    assert_eq!(transcript.entries.len(), 2);
    assert_eq!(transcript.entries[0].id, "old-user");
    assert_eq!(
        transcript.entries[1].id,
        "compaction-run-compaction-1-summary"
    );
}

#[test]
fn model_context_and_transcript_diverge_after_compaction_but_remain_ordered() {
    let mut store = MemoryStore::new("session-1");
    store
        .try_append_entry(Entry {
            id: "user-1".into(),
            parent_id: None,
            lane: "main".into(),
            seq: 1,
            timestamp: 1,
            surface_op: threadlane_agent::harness::SurfaceOperation::Append,
            message: AgentMessage::user("first request", vec![]),
            terminate: false,
        })
        .unwrap();
    let mut effects = GatedEffects::new();
    CompactionProcedure::accept(&store, "run-compaction-1", "first request retained", &mut effects)
        .unwrap();
    effects.run_to_completion(&mut store).unwrap();
    store
        .try_append_entry(Entry {
            id: "user-2".into(),
            parent_id: Some("compaction-run-compaction-1-summary".into()),
            lane: "main".into(),
            seq: 7,
            timestamp: 7,
            surface_op: threadlane_agent::harness::SurfaceOperation::Append,
            message: AgentMessage::user("second request", vec![]),
            terminate: false,
        })
        .unwrap();

    let model = store.model_context("main").unwrap();
    assert_eq!(
        model.entries.iter().map(|entry| entry.id.as_str()).collect::<Vec<_>>(),
        vec!["compaction-run-compaction-1-summary", "user-2"]
    );
    let transcript = store.transcript("main");
    assert_eq!(
        transcript.entries.iter().map(|entry| entry.id.as_str()).collect::<Vec<_>>(),
        vec!["user-1", "compaction-run-compaction-1-summary", "user-2"]
    );
}
