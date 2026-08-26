use std::path::Path;
use threadlane_runtime::harness::{JsonlStore, MemoryStore, Record, Reducer, SessionStore};

const SAMPLES: usize = 10;

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
        store
            .append_record(fact(format!("fact-bench-{seq}"), seq))
            .unwrap();
    }
}

#[hotpath::measure]
fn append_scaling(store: &mut JsonlStore) {
    for _ in 0..200 {
        let seq = store.next_sequence();
        store
            .append_record(fact(format!("fact-bench-{seq}"), seq))
            .unwrap();
    }
}

#[hotpath::measure]
fn open_scaling(path: &Path) {
    for _ in 0..3 {
        std::hint::black_box(JsonlStore::open(path).unwrap());
    }
}

#[hotpath::measure]
fn reducer_replay(store: &MemoryStore) {
    std::hint::black_box(Reducer::reduce(store).unwrap());
}

#[hotpath::main]
fn main() {
    let append_dir = tempfile::tempdir().unwrap();
    let mut append_store =
        JsonlStore::open(append_dir.path().join("append-scaling.jsonl")).unwrap();
    pad_to(&mut append_store, 4_000);

    let open_dir = tempfile::tempdir().unwrap();
    let open_path = open_dir.path().join("open-scaling.jsonl");
    let mut open_store = JsonlStore::open(&open_path).unwrap();
    pad_to(&mut open_store, 4_000);
    drop(open_store);

    let mut store = MemoryStore::new("reducer-replay");
    for seq in 1..=4_000 {
        store.append_record(fact(format!("fact-bench-{seq}"), seq));
    }

    for _ in 0..SAMPLES {
        append_scaling(&mut append_store);
        open_scaling(&open_path);
        reducer_replay(&store);
    }
}
