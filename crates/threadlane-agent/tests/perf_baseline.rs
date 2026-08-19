//! Measurement harness for session-tree hot paths.
//!
//! Ignored by default: reports timings rather than asserting behavior. Run with
//! `cargo test -p threadlane-agent --test perf_baseline -- --ignored --nocapture`.

use std::io::Write;
use std::time::Instant;
use tempfile::tempdir;
use threadlane_agent::harness::{JsonlStore, Record, SessionStore};
use threadlane_agent::session_tree::SessionTree;
use threadlane_agent::types::AgentMessage;

fn build_session(nodes: usize) -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("session.jsonl");
    let mut tree = SessionTree::new("bench".to_string());
    // `add_message` persists through the tree's own file path, which is the
    // same write path a live session uses.
    tree.file_path = Some(path.clone());

    for index in 0..nodes {
        let message = if index % 2 == 0 {
            AgentMessage::user(
                format!("user turn {index} with some representative length"),
                Vec::new(),
            )
        } else {
            AgentMessage::Assistant {
                content: Some(format!(
                    "assistant turn {index} with a longer body, roughly the size of a short \
                     reply that a model would stream back over a few seconds of generation"
                )),
                tool_calls: None,
                stop_reason: None,
                deferred_handle: None,
            }
        };
        tree.add_message(message);
    }
    (dir, path)
}

fn build_harness_session(nodes: usize) -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempdir().unwrap();
    let path = dir.path().join("session.jsonl");
    let (_tree_dir, tree_path) = build_session(nodes);
    std::fs::copy(tree_path, &path).unwrap();

    let harness_path = path.with_extension("harness.jsonl");
    let mut harness = std::fs::File::create(harness_path).unwrap();
    for index in 0..nodes {
        let seq = nodes as u64 + index as u64 + 1;
        serde_json::to_writer(
            &mut harness,
            &Record::FactSet {
                id: format!("fact-{index}"),
                seq,
                lane: "main".into(),
                timestamp: seq,
                run_id: None,
                key: format!("benchmark-{index}"),
                value: "value".into(),
            },
        )
        .unwrap();
        harness.write_all(b"\n").unwrap();
    }
    harness.sync_all().unwrap();
    (dir, path)
}

#[test]
#[ignore = "measurement harness, not an assertion"]
fn session_tree_load_and_branch_walk() {
    for nodes in [200usize, 1000, 4000] {
        let (_dir, path) = build_session(nodes);
        let bytes = std::fs::metadata(&path).unwrap().len();

        let load_start = Instant::now();
        let tree = SessionTree::load_from_file(&path).unwrap();
        let load = load_start.elapsed();

        let branch_start = Instant::now();
        let branch = tree.get_active_branch_messages();
        let branch_time = branch_start.elapsed();

        println!(
            "nodes={nodes:<5} file={:>7}KB  load={:>10?}  active_branch={:>10?}  branch_len={}",
            bytes / 1024,
            load,
            branch_time,
            branch.len()
        );
    }
}

#[test]
#[ignore = "measurement harness, not an assertion"]
fn jsonl_harness_append_latency_by_session_size() {
    const APPENDS: u32 = 3;

    for nodes in [200usize, 1000, 4000] {
        let (_dir, path) = build_harness_session(nodes);
        let bytes = std::fs::metadata(&path).unwrap().len()
            + std::fs::metadata(path.with_extension("harness.jsonl"))
                .unwrap()
                .len();
        let open_start = Instant::now();
        let mut store = JsonlStore::open(&path).unwrap();
        let open = open_start.elapsed();

        let append_start = Instant::now();
        for index in 0..APPENDS {
            let seq = store.next_sequence();
            store
                .append_record(Record::FactSet {
                    id: format!("measured-fact-{nodes}-{index}"),
                    seq,
                    lane: "main".into(),
                    timestamp: seq,
                    run_id: None,
                    key: format!("measured-{index}"),
                    value: "value".into(),
                })
                .unwrap();
        }
        let appends = append_start.elapsed();

        let reduce_start = Instant::now();
        let state = threadlane_agent::harness::Reducer::reduce(&store).unwrap();
        let reduce = reduce_start.elapsed();
        assert!(state.lane("main").is_some());

        println!(
            "nodes={nodes:<5} file={:>7}KB  open={:>10?}  {APPENDS} durable appends={:>10?}  per append={:>10?}  standalone reduce={:>10?}",
            bytes / 1024,
            open,
            appends,
            appends / APPENDS,
            reduce,
        );
    }
}
