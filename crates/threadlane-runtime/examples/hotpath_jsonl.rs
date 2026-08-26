use threadlane_runtime::harness::{JsonlStore, MemoryStore, Record, Reducer, SessionStore};

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
        store.append_record(fact(format!("fact-bench-{seq}"), seq)).unwrap();
    }
}

#[hotpath::measure]
fn append_scaling() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = JsonlStore::open(dir.path().join("append-scaling.jsonl")).unwrap();
    pad_to(&mut store, 4_000);
    for _ in 0..200 {
        let seq = store.next_sequence();
        store.append_record(fact(format!("fact-bench-{seq}"), seq)).unwrap();
    }
}

#[hotpath::measure]
fn open_scaling() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("open-scaling.jsonl");
    let mut store = JsonlStore::open(&path).unwrap();
    pad_to(&mut store, 4_000);
    drop(store);
    for _ in 0..3 {
        std::hint::black_box(JsonlStore::open(&path).unwrap());
    }
}

#[hotpath::measure]
fn reducer_replay(store: &MemoryStore) {
    std::hint::black_box(Reducer::reduce(store).unwrap());
}

#[hotpath::main]
fn main() {
    append_scaling();
    open_scaling();

    let mut store = MemoryStore::new("reducer-replay");
    for seq in 1..=4_000 {
        store.append_record(fact(format!("fact-bench-{seq}"), seq));
    }
    reducer_replay(&store);
}
