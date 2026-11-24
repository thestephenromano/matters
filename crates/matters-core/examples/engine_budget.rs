//! Same-host cost of decoding honest engine bytes, from which the engine
//! checker's budgets are chosen (#45, `crdt::isolation`).
//!
//! ```text
//! cargo run --release -p matters-core --example engine_budget -- <scratch directory>
//! ```
//!
//! Prints one JSON object per workload: the blob's size, and what the
//! checker's own decoding of it (`isolation::decode`, which is the
//! receiver's decoding and more) took in a fresh process. That process
//! reads the blob first, as the checker's child does, then decodes it:
//! - `address_space_growth`: the peak address space (`VmPeak`) past the
//!   address space before decoding (`VmSize`): what `RLIMIT_AS` must allow;
//! - `resident_peak`: the peak resident set (`VmHWM`);
//! - `cpu_seconds`: processor time, user and system.
//!
//! Updates are at most 1 MiB (`MAX_PLAINTEXT_BYTES`); each workload fills
//! one commit as close to that as it can:
//! - `update-creates`: tasks created, titles only;
//! - `update-notes`: one task's notes of 1 MiB;
//! - `update-renames`: one task renamed again and again;
//! - `update-moves`: tasks created, then each moved to the top.
//!
//! Snapshots hold a collection of `tasks` tasks, each created, renamed
//! (every second) and completed (every tenth) in its own commit, as the app
//! writes them; their notes are 1 KiB of varied words (`snapshot-words`),
//! 1 KiB of one repeated letter, which compresses as far as LZ4 goes
//! (`snapshot-repeated`), or none (`snapshot-titles`).

use std::time::Instant;

use matters_core::crdt::isolation::{self, Blob};
use matters_core::crdt::{Anchor, Command, Document, Kind};
use matters_core::Status;

const MAX_UPDATE: usize = 1024 * 1024;

fn id(n: usize) -> String {
    uuid::Uuid::from_u128(n as u128 + 1).to_string()
}

fn create(n: usize, title: &str) -> Command {
    Command::Create {
        id: id(n),
        kind: Kind::Task,
        parent: None,
        at: Anchor::Last,
        title: title.into(),
        created_at: 0,
    }
}

/// The update of one commit of `commands`.
fn update(commands: &[Command]) -> Vec<u8> {
    let mut doc = Document::new(7).unwrap();
    let before = doc.version();
    doc.apply(commands, 1).unwrap();
    doc.updates_since(&before).unwrap()
}

/// The largest update, at most 1 MiB, of a commit of `make(n)` commands.
fn largest(make: impl Fn(usize) -> Vec<Command>) -> Vec<u8> {
    let (mut low, mut high) = (1, 1);
    while update(&make(high)).len() <= MAX_UPDATE {
        low = high;
        high *= 2;
    }
    while high - low > 1 {
        let mid = (low + high) / 2;
        if update(&make(mid)).len() <= MAX_UPDATE {
            low = mid;
        } else {
            high = mid;
        }
    }
    update(&make(low))
}

/// Words that compress as ordinary notes do.
fn words(n: usize, len: usize) -> String {
    const WORDS: [&str; 16] = [
        "call", "review", "draft", "plan", "meeting", "budget", "garden", "invoice", "travel",
        "report", "school", "doctor", "repair", "follow", "agenda", "update",
    ];
    let mut out = String::new();
    let mut state = n as u64 * 6364136223846793005 + 1442695040888963407;
    while out.len() < len {
        state = state
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        out.push_str(WORDS[(state >> 60) as usize]);
        out.push(' ');
    }
    out.truncate(len);
    out
}

fn snapshot(tasks: usize, notes: impl Fn(usize) -> String) -> Vec<u8> {
    let mut doc = Document::new(7).unwrap();
    for n in 0..tasks {
        doc.apply(&[create(n, &format!("Task {n}"))], n as i64)
            .unwrap();
        let notes = notes(n);
        if !notes.is_empty() {
            doc.apply(&[Command::SetNotes { id: id(n), notes }], n as i64)
                .unwrap();
        }
    }
    for n in (0..tasks).step_by(2) {
        doc.apply(
            &[Command::Rename {
                id: id(n),
                title: format!("Renamed {n}"),
            }],
            0,
        )
        .unwrap();
    }
    for n in (0..tasks).step_by(10) {
        doc.apply(
            &[Command::SetStatus {
                id: id(n),
                status: Status::Completed,
                completed_at: Some(0),
                closed_by_project: false,
            }],
            0,
        )
        .unwrap();
    }
    doc.snapshot().unwrap()
}

fn status(field: &str) -> u64 {
    let status = std::fs::read_to_string("/proc/self/status").unwrap();
    status
        .lines()
        .find_map(|l| l.strip_prefix(field))
        .and_then(|v| v.trim().strip_suffix("kB"))
        .and_then(|v| v.trim().parse::<u64>().ok())
        .unwrap()
        * 1024
}

fn cpu_seconds() -> f64 {
    // utime and stime, fields 14 and 15 of /proc/self/stat, in clock ticks
    // (100 a second on Linux).
    let stat = std::fs::read_to_string("/proc/self/stat").unwrap();
    let fields: Vec<&str> = stat[stat.rfind(')').unwrap() + 2..].split(' ').collect();
    let ticks: u64 = fields[11].parse::<u64>().unwrap() + fields[12].parse::<u64>().unwrap();
    ticks as f64 / 100.0
}

/// In a fresh process: read the blob, then decode it as the checker does.
fn measure(kind: &str, path: &str) {
    let blob = std::fs::read(path).unwrap();
    let kind = if kind == "update" {
        Blob::Update
    } else {
        Blob::Snapshot
    };
    let size = status("VmSize:");
    let cpu = cpu_seconds();
    let started = Instant::now();
    isolation::decode(kind, &blob);
    println!(
        "{}",
        serde_json::json!({
            "address_space_growth": status("VmPeak:") - size,
            "resident_peak": status("VmHWM:"),
            "cpu_seconds": cpu_seconds() - cpu,
            "wall_seconds": started.elapsed().as_secs_f64(),
        })
    );
}

fn run(dir: &std::path::Path, name: &str, kind: &str, blob: &[u8]) {
    let path = dir.join(format!("{name}.bin"));
    std::fs::write(&path, blob).unwrap();
    let out = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["measure", kind, path.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(out.status.success(), "{name}: {out:?}");
    let mut measured: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    measured["workload"] = name.into();
    measured["bytes"] = blob.len().into();
    println!("{measured}");
    std::fs::remove_file(path).unwrap();
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args[0] == "measure" {
        return measure(&args[1], &args[2]);
    }
    let dir = std::path::PathBuf::from(&args[0]);
    std::fs::create_dir_all(&dir).unwrap();
    let creates = largest(|n| (0..n).map(|i| create(i, &format!("Task {i}"))).collect());
    run(&dir, "update-creates", "update", &creates);
    let notes = largest(|n| {
        vec![
            create(0, "Notes"),
            Command::SetNotes {
                id: id(0),
                notes: words(0, n * 1024),
            },
        ]
    });
    run(&dir, "update-notes", "update", &notes);
    let renames = largest(|n| {
        let mut commands = vec![create(0, "Renamed")];
        commands.extend((0..n).map(|i| Command::Rename {
            id: id(0),
            title: ["a", "b"][i % 2].into(),
        }));
        commands
    });
    run(&dir, "update-renames", "update", &renames);
    let moves = largest(|n| {
        let mut commands: Vec<Command> = (0..n).map(|i| create(i, "Moved")).collect();
        commands.extend((0..n).map(|i| Command::Place {
            id: id(i),
            at: Anchor::First,
        }));
        commands
    });
    run(&dir, "update-moves", "update", &moves);
    for tasks in [1_000, 10_000, 50_000, 100_000] {
        let s = snapshot(tasks, |n| words(n, 1024));
        run(&dir, &format!("snapshot-words-{tasks}"), "snapshot", &s);
        let s = snapshot(tasks, |_| "x".repeat(1024));
        run(&dir, &format!("snapshot-repeated-{tasks}"), "snapshot", &s);
        let s = snapshot(tasks, |_| String::new());
        run(&dir, &format!("snapshot-titles-{tasks}"), "snapshot", &s);
    }
}
