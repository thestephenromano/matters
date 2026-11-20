//! Same-host cost of recording writes for sync (#35): save latency, reopen
//! time and journal size, with sync off (the app today) and on.
//!
//!     cargo run --release -p matters-core --features test-support \
//!         --example sync_cost -- <directory on the disk to measure> [tasks...]
//!
//! Prints one JSON object per workload. Each workload makes `tasks` tasks
//! with 1 KiB notes, then times 200 renames (each one durable save).

use std::time::Instant;

use matters_core::{Database, Task};

fn percentile(sorted: &[f64], p: f64) -> f64 {
    sorted[((sorted.len() - 1) as f64 * p).round() as usize]
}

fn run(dir: &std::path::Path, tasks: usize, sync: bool) -> serde_json::Value {
    let path = dir.join(format!("cost-{tasks}-{sync}.db"));
    for suffix in ["", "-wal", "-shm"] {
        let _ = std::fs::remove_file(format!("{}{suffix}", path.display()));
    }
    let db = Database::open(&path).unwrap();
    if sync {
        db.enable_sync().unwrap();
    }
    let notes = "x".repeat(1024);
    let ids: Vec<String> = (0..tasks)
        .map(|n| uuid::Uuid::from_u128(n as u128 + 1).to_string())
        .collect();
    let started = Instant::now();
    for id in &ids {
        let mut task = Task::new(id.clone(), "task", 0.0);
        task.notes = notes.clone();
        db.add_task(&mut task).unwrap();
    }
    let create_s = started.elapsed().as_secs_f64();
    let mut samples = Vec::new();
    for n in 0..200 {
        let id = &ids[(n * 7919) % ids.len()];
        let start = Instant::now();
        db.rename_task(id, &format!("renamed {n}")).unwrap();
        samples.push(start.elapsed().as_secs_f64() * 1000.0);
    }
    samples.sort_by(f64::total_cmp);
    let journal: (i64, i64) = db
        .with_conn(|c| {
            Ok(c.query_row(
                "SELECT COUNT(*), COALESCE(SUM(LENGTH(bytes)), 0) FROM sync_journal",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )?)
        })
        .unwrap();
    drop(db);
    let start = Instant::now();
    let reopened = Database::open(&path).unwrap();
    let reopen_ms = start.elapsed().as_secs_f64() * 1000.0;
    assert_eq!(reopened.sync_enabled(), sync);
    let file_bytes = std::fs::metadata(&path).unwrap().len();
    serde_json::json!({
        "tasks": tasks,
        "sync": sync,
        "create_all_s": create_s,
        "save_ms": {
            "p50": percentile(&samples, 0.50),
            "p95": percentile(&samples, 0.95),
            "p99": percentile(&samples, 0.99),
            "samples": samples.len(),
        },
        "reopen_ms": reopen_ms,
        "journal_rows": journal.0,
        "journal_bytes": journal.1,
        "database_file_bytes": file_bytes,
    })
}

fn main() {
    let mut args = std::env::args().skip(1);
    let dir = std::path::PathBuf::from(args.next().expect("a directory to measure in"));
    std::fs::create_dir_all(&dir).unwrap();
    let sizes: Vec<usize> = args.map(|a| a.parse().unwrap()).collect();
    let sizes = if sizes.is_empty() {
        vec![1_000, 10_000]
    } else {
        sizes
    };
    for tasks in sizes {
        for sync in [false, true] {
            println!("{}", run(&dir, tasks, sync));
        }
    }
}
