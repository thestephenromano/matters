//! Crash safety of recorded writes (#35): a process killed just before or
//! just after a write's commit leaves the rows and the engine agreeing, and
//! keeps every write it acknowledged.

use std::process::Command;

use matters_core::{Database, Task};

fn id(n: u128) -> String {
    uuid::Uuid::from_u128(n).to_string()
}

/// Run in a child process: open the database named by `MATTERS_CRASH_DB`,
/// make one acknowledged write, then a second that `MATTERS_CRASH_AT`
/// interrupts. Ignored in ordinary runs; the parent test runs it.
#[test]
#[ignore]
fn crash_worker() {
    let Ok(path) = std::env::var("MATTERS_CRASH_DB") else {
        return;
    };
    let db = Database::open(&path).unwrap();
    if let Ok(update) = std::env::var("MATTERS_CRASH_UPDATE") {
        db.receive_update(&std::fs::read(update).unwrap()).unwrap();
        unreachable!("the crash point exits the process");
    }
    let point = std::env::var("MATTERS_CRASH_AT").unwrap();
    std::env::remove_var("MATTERS_CRASH_AT");
    db.rename_task(&id(1), "acknowledged").unwrap();
    std::env::set_var("MATTERS_CRASH_AT", point);
    db.rename_task(&id(1), "interrupted").unwrap();
    unreachable!("the crash point exits the process");
}

#[test]
fn a_crash_at_a_commit_boundary_keeps_rows_and_engine_together() {
    for (point, expected) in [
        ("before-commit", "acknowledged"),
        ("after-commit", "interrupted"),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("matters.db");
        {
            let db = Database::open(&path).unwrap();
            db.enable_sync().unwrap();
            db.add_task(&mut Task::new(id(1), "first", 0.0)).unwrap();
        }
        let status = Command::new(std::env::current_exe().unwrap())
            .args(["crash_worker", "--exact", "--ignored", "--nocapture"])
            .env("MATTERS_CRASH_DB", &path)
            .env("MATTERS_CRASH_AT", point)
            .status()
            .unwrap();
        assert_eq!(
            status.code(),
            Some(86),
            "{point}: exited at the crash point"
        );

        let db = Database::open(&path).unwrap();
        let row = db.get_task(&id(1)).unwrap().unwrap();
        assert_eq!(row.title, expected, "{point}");
        let engine = db.engine_snapshot().unwrap().unwrap();
        assert_eq!(
            engine.tasks[0].title, expected,
            "{point}: the engine agrees"
        );
        let journal: i64 = db
            .with_conn(|c| Ok(c.query_row("SELECT COUNT(*) FROM sync_journal", [], |r| r.get(0))?))
            .unwrap();
        let commits = if point == "before-commit" { 2 } else { 3 };
        assert_eq!(
            journal, commits,
            "{point}: one journal row per committed write"
        );
        db.rename_task(&id(1), "after restart").unwrap();
        assert_eq!(
            db.engine_snapshot().unwrap().unwrap().tasks[0].title,
            "after restart"
        );
    }
}

#[test]
fn a_crash_while_receiving_applies_the_update_whole_or_not_at_all() {
    let sender = Database::open_in_memory().unwrap();
    sender.enable_sync().unwrap();
    sender
        .add_task(&mut Task::new(id(2), "remote", 0.0))
        .unwrap();
    let update = sender.outgoing_updates(0).unwrap().remove(0).1;
    for (point, arrived) in [("before-commit", false), ("after-commit", true)] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("matters.db");
        let file = dir.path().join("update.bin");
        std::fs::write(&file, &update).unwrap();
        Database::open(&path).unwrap().enable_sync().unwrap();
        let status = Command::new(std::env::current_exe().unwrap())
            .args(["crash_worker", "--exact", "--ignored", "--nocapture"])
            .env("MATTERS_CRASH_DB", &path)
            .env("MATTERS_CRASH_AT", point)
            .env("MATTERS_CRASH_UPDATE", &file)
            .status()
            .unwrap();
        assert_eq!(status.code(), Some(86), "{point}");
        let db = Database::open(&path).unwrap();
        assert_eq!(db.get_task(&id(2)).unwrap().is_some(), arrived, "{point}");
        let engine = db.engine_snapshot().unwrap().unwrap();
        assert_eq!(engine.tasks.len(), usize::from(arrived), "{point}");
        // Receiving it again, as a retry does, lands it once.
        db.receive_update(&update).unwrap();
        assert_eq!(db.engine_snapshot().unwrap().unwrap().tasks.len(), 1);
        assert_eq!(db.get_task(&id(2)).unwrap().unwrap().title, "remote");
    }
}
