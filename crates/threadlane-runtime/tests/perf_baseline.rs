//! Ignored measurement harness for JSONL durability costs (AGENTS.md
//! performance convention). Run with:
//!
//! ```text
//! cargo test -p threadlane-runtime --release --test perf_baseline -- --ignored --nocapture
//! ```
//!
//! These are measurements, not assertions: numbers are compared before and
//! after optimization changes on the same machine.

use std::time::Instant;
use threadlane_runtime::harness::{JsonlStore, Record, SessionStore};

fn fact(id: String, seq: u64) -> Record {
    Record::FactSet {
        id,
        seq,
        lane: "main".into(),
        timestamp: 0,
        run_id: None,
        key: "bench".into(),
        value: "0123456789abcdef0123456789abcdef".into(),
    }
}

fn pad_to(store: &mut JsonlStore, n: usize) {
    while store.entries().len() + store.records().len() < n {
        let seq = store.next_sequence();
        let id = format!("fact-bench-{seq}");
        store.append_record(fact(id, seq)).unwrap();
    }
}

#[test]
#[ignore]
fn measure_append_scaling() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("append-scaling.jsonl");
    let mut store = JsonlStore::open(&path).unwrap();
    let batch = 200;
    for size in [500usize, 1000, 2000, 4000] {
        pad_to(&mut store, size);
        let start = Instant::now();
        for _ in 0..batch {
            let seq = store.next_sequence();
            let id = format!("fact-bench-{seq}");
            store.append_record(fact(id, seq)).unwrap();
        }
        println!(
            "append @n={size}: {:?}/record ({batch} records)",
            start.elapsed() / batch as u32
        );
    }
}

#[test]
#[ignore]
fn measure_open_scaling() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("open-scaling.jsonl");
    {
        let mut store = JsonlStore::open(&path).unwrap();
        pad_to(&mut store, 4000);
    }
    for _ in 0..3 {
        let start = Instant::now();
        let entries = JsonlStore::open(&path).unwrap().entries().len();
        println!("open n=4000 (entries={entries}): {:?}", start.elapsed());
    }
}
