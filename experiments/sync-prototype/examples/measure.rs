//! Small same-host comparison against the unmodified Matters SQLite core.
//! This is not the full ADR workload or a production performance gate.
use matters_core::{domain::Task, storage::Database};
use matters_sync_prototype::{Command, Kind, Replica, Result};
use serde_json::json;
use std::{path::Path, time::Instant};

fn id(n: usize) -> String {
    uuid::Uuid::from_u128(n as u128 + 1).to_string()
}
fn ms(start: Instant) -> f64 {
    start.elapsed().as_secs_f64() * 1000.0
}
fn summary(mut samples: Vec<f64>) -> serde_json::Value {
    samples.sort_by(f64::total_cmp);
    let rank = |p: usize| samples[(samples.len() * p).div_ceil(100) - 1];
    json!({"samples":samples,"p50_ms":rank(50),"p95_ms":rank(95),"p99_ms":rank(99)})
}

fn main() -> Result<()> {
    let parent = std::env::args()
        .nth(1)
        .ok_or("provide a directory on the measured filesystem (not tmpfs)")?;
    let count: usize = std::env::args()
        .nth(2)
        .unwrap_or_else(|| "1000".into())
        .parse()?;
    if count == 0 {
        return Err("task count must be positive".into());
    }
    let dir = tempfile::tempdir_in(Path::new(&parent))?;
    let baseline_path = dir.path().join("baseline.db");
    let prototype_path = dir.path().join("prototype.db");
    let mut prototype = Replica::open(&prototype_path)?;
    let baseline = Database::open(&baseline_path)?;
    // Equal durability settings, on the same filesystem. The current app uses WAL already.
    baseline.with_conn(|c| Ok(c.execute_batch("PRAGMA synchronous=FULL;")?))?;
    let notes = "x".repeat(1024);
    let mut seed = Vec::new();
    for n in 0..count {
        let mut task = Task::new(id(n), format!("task {n}"), n as f64);
        task.notes = notes.clone();
        baseline.create_task(&task)?;
        seed.push(Command::Create {
            id: id(n),
            kind: Kind::Task,
            parent: None,
            title: format!("task {n}"),
        });
        seed.push(Command::InsertNotes {
            id: id(n),
            index: 0,
            text: notes.clone(),
        });
    }
    prototype.execute_batch(seed)?;
    let mut baseline_samples = Vec::new();
    let mut prototype_samples = Vec::new();
    // Five warmups, then 100 alternating edits: an explicit small exploratory workload.
    for i in 0..105 {
        let task_id = id(i % count);
        let title = format!("edit {i}");
        let run_baseline = || -> Result<f64> {
            let start = Instant::now();
            let mut task = baseline
                .get_task(&task_id)?
                .ok_or("missing baseline task")?;
            task.title = title.clone();
            baseline.update_task(&task)?;
            Ok(ms(start))
        };
        let (b, p) = if i % 2 == 0 {
            let b = run_baseline()?;
            let start = Instant::now();
            prototype.execute(Command::Rename {
                id: task_id.clone(),
                title: title.clone(),
            })?;
            (b, ms(start))
        } else {
            let start = Instant::now();
            prototype.execute(Command::Rename {
                id: task_id.clone(),
                title: title.clone(),
            })?;
            let p = ms(start);
            (run_baseline()?, p)
        };
        if i >= 5 {
            baseline_samples.push(b);
            prototype_samples.push(p);
        }
    }
    let start = Instant::now();
    prototype.rebuild()?;
    let rebuild = ms(start);
    let projection = prototype.projection()?;
    let records = prototype.journal_len()?;
    let bytes: usize = prototype.outbox()?.iter().map(|r| r.bytes.len()).sum();
    drop(prototype);
    let start = Instant::now();
    let restored = Replica::open(&prototype_path)?;
    let reopen = ms(start);
    assert_eq!(restored.projection()?, projection);
    drop(restored);
    drop(baseline);
    println!(
        "{}",
        serde_json::to_string_pretty(&json!({
            "workload":{"tasks":count,"notes_bytes_per_task":1024,"measured_renames":100,"warmup_renames":5,"writers":1,"checklist_items":0},
            "durability":"SQLite WAL, synchronous=FULL; one process, same filesystem",
            "baseline":summary(baseline_samples),"prototype":summary(prototype_samples),
            "prototype_rebuild_ms":rebuild,"prototype_reopen_ms":reopen,
            "prototype_journal_records":records,"prototype_journal_payload_bytes":bytes,
            "baseline_db_bytes":std::fs::metadata(&baseline_path)?.len(),"prototype_db_bytes":std::fs::metadata(&prototype_path)?.len(),
            "scope":"Exploratory current-core comparison; full-history fork and full projection per command. Not an isolated engine benchmark or ADR acceptance workload."
        }))?
    );
    Ok(())
}
