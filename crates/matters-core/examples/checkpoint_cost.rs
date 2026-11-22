//! Same-host cost of checkpoints (#40).
//!
//! For each workload it reports:
//! - the history a collection retains;
//! - how long making a checkpoint takes, and its size;
//! - how long a new device takes to start from it, against receiving every
//!   record from the first.
//!
//! ```text
//! cargo run --release -p matters-core --features test-support \
//!     --example checkpoint_cost -- <directory on the disk to measure> [tasks...]
//! ```
//!
//! Prints one JSON object per workload. A workload of `tasks` tasks has:
//! - 1 KiB notes;
//! - a rename of every second task;
//! - a completion of every tenth;
//! - each edit its own sealed record, as the app writes them.
//!
//! Nothing is pruned: the checkpoint is added to the retained history.

use std::time::Instant;

use matters_core::storage::membership::{MemoryKeyStore, SyncStanding};
use matters_core::{Database, Task};
use matters_sync::authority::{parse, Body, Route};
use matters_sync::checkpoint::Cursor;

fn route() -> Route {
    Route {
        application: [1; 16],
        tenant: [2; 16],
        collection: [3; 16],
    }
}

const GENERATION: [u8; 16] = [9; 16];

fn open_fresh(path: &std::path::Path) -> Database {
    for suffix in ["", "-wal", "-shm"] {
        let _ = std::fs::remove_file(format!("{}{suffix}", path.display()));
    }
    Database::open(path).unwrap()
}

fn join(admin: &Database, path: &std::path::Path) -> Database {
    let joiner = open_fresh(path);
    let keys = MemoryKeyStore::default();
    let shown = admin.invite().unwrap();
    let request = joiner.start_enrollment(&keys, &shown).unwrap();
    let id = parse(&request).unwrap().message.signer;
    admin.admit(&request, &shown).unwrap();
    let envelopes: Vec<Vec<u8>> = admin
        .outgoing_controls()
        .unwrap()
        .into_iter()
        .filter(|o| {
            matches!(parse(&o.bytes).unwrap().message.body,
                Body::KeyEnvelope(ref k) if k.recipient == id)
        })
        .map(|o| o.bytes)
        .collect();
    assert_eq!(
        joiner
            .finish_enrollment(&keys, &admin.sync_controls().unwrap(), &envelopes)
            .unwrap(),
        SyncStanding::Active
    );
    joiner
}

fn ms(started: Instant) -> f64 {
    started.elapsed().as_secs_f64() * 1000.0
}

fn run(dir: &std::path::Path, tasks: usize) -> serde_json::Value {
    let a = open_fresh(&dir.join(format!("checkpoint-{tasks}-a.db")));
    a.create_collection(&MemoryKeyStore::default(), route())
        .unwrap();
    let notes = "x".repeat(1024);
    let ids: Vec<String> = (0..tasks)
        .map(|n| uuid::Uuid::from_u128(n as u128 + 1).to_string())
        .collect();
    for id in &ids {
        let mut task = Task::new(id.clone(), "task", 0.0);
        task.notes = notes.clone();
        a.add_task(&mut task).unwrap();
    }
    for id in ids.iter().step_by(2) {
        a.rename_task(id, "renamed").unwrap();
    }
    for id in ids.iter().step_by(10) {
        a.complete_task(id).unwrap();
    }
    let records: Vec<Vec<u8>> = a
        .outgoing_records()
        .unwrap()
        .into_iter()
        .map(|r| r.bytes)
        .collect();
    let record_bytes: usize = records.iter().map(Vec::len).sum();
    for r in a.outgoing_records().unwrap() {
        a.acknowledge_record(&r.record_id).unwrap();
    }
    let started = Instant::now();
    a.create_checkpoint(Some(Cursor {
        generation: GENERATION,
        sequence: records.len() as u64,
    }))
    .unwrap()
    .unwrap();
    let create_ms = ms(started);
    let parts: Vec<Vec<u8>> = a
        .outgoing_checkpoint_parts()
        .unwrap()
        .into_iter()
        .map(|(_, b)| b)
        .collect();
    let checkpoint_bytes: usize = parts.iter().map(Vec::len).sum();
    let coverage = a.latest_checkpoint().unwrap().unwrap();

    // A new device from the checkpoint.
    let c = join(&a, &dir.join(format!("checkpoint-{tasks}-c.db")));
    let started = Instant::now();
    assert!(c.start_from_checkpoint(&parts, GENERATION).unwrap());
    let start_ms = ms(started);
    // A new device from every record.
    let d = join(&a, &dir.join(format!("checkpoint-{tasks}-d.db")));
    let started = Instant::now();
    for r in &records {
        d.receive_record(r).unwrap();
    }
    let replay_ms = ms(started);
    for (n, id) in ids.iter().enumerate().step_by(97) {
        let want = a.get_task(id).unwrap().unwrap();
        for db in [&c, &d] {
            let got = db.get_task(id).unwrap().unwrap();
            assert_eq!(
                (&got.title, &got.notes, got.status),
                (&want.title, &want.notes, want.status),
                "task {n}"
            );
        }
    }
    serde_json::json!({
        "tasks": tasks,
        "records": records.len(),
        "retained_record_bytes": record_bytes,
        "bytes_per_record": record_bytes / records.len(),
        "checkpoint_bytes": checkpoint_bytes,
        "checkpoint_payload_bytes": coverage.length,
        "checkpoint_parts": parts.len(),
        "checkpoint_to_history": checkpoint_bytes as f64 / record_bytes as f64,
        "create_ms": create_ms,
        "start_from_checkpoint_ms": start_ms,
        "replay_every_record_ms": replay_ms,
    })
}

fn main() {
    let mut args = std::env::args().skip(1);
    let dir = std::path::PathBuf::from(args.next().expect("a directory to measure on"));
    std::fs::create_dir_all(&dir).unwrap();
    let sizes: Vec<usize> = args.map(|a| a.parse().unwrap()).collect();
    for tasks in if sizes.is_empty() { vec![1000] } else { sizes } {
        println!("{}", run(&dir, tasks));
    }
}
