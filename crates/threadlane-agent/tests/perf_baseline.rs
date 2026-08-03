//! Measurement harness for session-tree hot paths.
//!
//! Ignored by default: reports timings rather than asserting behavior. Run with
//! `cargo test -p threadlane-agent --test perf_baseline -- --ignored --nocapture`.

use std::time::Instant;
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
