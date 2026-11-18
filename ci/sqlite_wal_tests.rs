//! Shared integration coverage for each independently linked SQLite graph.
//! This exercises concurrent API use; it does not reproduce SQLite's internal
//! WAL-reset race or replace upstream fix provenance.
use rusqlite::{params, Connection, TransactionBehavior};
use std::path::Path;
use std::sync::{Arc, Barrier};
use std::time::Duration;

type Result<T = ()> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;
// Finite fixture coverage: eight rows of payload and eight rounds per checkpoint
// mode. These are test workloads, not production limits or performance budgets.
const ROWS: i64 = 8;
const ROUNDS: i64 = 32;
fn open(path: &Path) -> Result<Connection> {
    let conn = Connection::open(path)?;
    // Bound lock waiting in this fixture, including on a loaded CI runner.
    conn.busy_timeout(Duration::from_secs(5))?;
    conn.execute_batch("PRAGMA synchronous=FULL; PRAGMA wal_autocheckpoint=0;")?;
    Ok(conn)
}
fn payload(generation: i64) -> String {
    format!("generation {generation}: {}", "x".repeat(2048))
}
fn initialize(path: &Path) -> Result<Connection> {
    let mut conn = open(path)?;
    let mode: String = conn.query_row("PRAGMA journal_mode=WAL", [], |r| r.get(0))?;
    assert_eq!(mode, "wal");
    conn.execute_batch("CREATE TABLE wal_probe(id INTEGER PRIMARY KEY,generation INTEGER NOT NULL,payload TEXT NOT NULL);")?;
    let tx = conn.transaction()?;
    for id in 0..ROWS {
        tx.execute(
            "INSERT INTO wal_probe VALUES (?1,0,?2)",
            params![id, payload(0)],
        )?;
    }
    tx.commit()?;
    Ok(conn)
}
fn verify(conn: &Connection, generation: i64) -> Result {
    let mut stmt = conn.prepare("SELECT id,generation,payload FROM wal_probe ORDER BY id")?;
    let rows = stmt.query_map([], |r| {
        Ok((
            r.get::<_, i64>(0)?,
            r.get::<_, i64>(1)?,
            r.get::<_, String>(2)?,
        ))
    })?;
    let rows = rows.collect::<rusqlite::Result<Vec<_>>>()?;
    assert_eq!(rows.len(), ROWS as usize);
    for (expected_id, (id, actual, bytes)) in rows.into_iter().enumerate() {
        assert_eq!(id, expected_id as i64);
        assert_eq!(actual, generation);
        assert_eq!(bytes, payload(generation));
    }
    assert_eq!(
        conn.query_row("PRAGMA integrity_check", [], |r| r.get::<_, String>(0))?,
        "ok"
    );
    Ok(())
}
fn checkpoint(conn: &Connection, mode: &str) -> Result<(i64, i64, i64)> {
    Ok(
        conn.query_row(&format!("PRAGMA wal_checkpoint({mode})"), [], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?))
        })?,
    )
}

#[test]
fn wal_commits_survive_concurrent_checkpoint_modes_and_reopen() -> Result {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("concurrent.db");
    let keeper = initialize(&path)?;
    let writer = open(&path)?;
    let checkpointer = open(&path)?;
    let barrier = Arc::new(Barrier::new(2));
    let writer_barrier = Arc::clone(&barrier);
    let handle = std::thread::spawn(move || -> Result {
        let mut writer = writer;
        // Keep both threads participating in the barriers even on SQL error,
        // so a test failure cannot strand the other thread indefinitely.
        let mut failure = None;
        for generation in 1..=ROUNDS {
            writer_barrier.wait();
            let result = (|| -> Result {
                let tx = writer.transaction_with_behavior(TransactionBehavior::Immediate)?;
                if tx.execute(
                    "UPDATE wal_probe SET generation=?1,payload=?2",
                    params![generation, payload(generation)],
                )? != ROWS as usize
                {
                    return Err("unexpected fixture row count".into());
                }
                tx.commit()?;
                Ok(())
            })();
            if let Err(error) = result {
                failure = Some(error);
            }
            writer_barrier.wait();
        }
        if let Some(error) = failure {
            return Err(error);
        }
        Ok(())
    });
    let mut results = Vec::new();
    for round in 0..ROUNDS {
        barrier.wait();
        results.push(checkpoint(
            &checkpointer,
            ["PASSIVE", "FULL", "RESTART", "TRUNCATE"][round as usize % 4],
        ));
        barrier.wait();
    }
    handle.join().expect("writer thread panicked")?;
    for result in results {
        let (busy, frames, copied) = result?;
        assert!([0, 1].contains(&busy));
        assert!(copied <= frames);
    }
    assert_eq!(checkpoint(&checkpointer, "TRUNCATE")?, (0, 0, 0));
    verify(&keeper, ROUNDS)?;
    drop(keeper);
    drop(checkpointer);
    verify(&open(&path)?, ROUNDS)
}

#[test]
fn wal_reader_snapshot_blocks_reset_until_released() -> Result {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("reader.db");
    let writer = initialize(&path)?;
    let reader = open(&path)?;
    let checkpointer = open(&path)?;
    checkpointer.busy_timeout(Duration::ZERO)?;
    reader.execute_batch("BEGIN;")?;
    verify(&reader, 0)?;
    writer.execute("UPDATE wal_probe SET generation=1,payload=?1", [payload(1)])?;
    verify(&reader, 0)?;
    assert_eq!(checkpoint(&checkpointer, "TRUNCATE")?.0, 1);
    reader.execute_batch("ROLLBACK;")?;
    assert_eq!(checkpoint(&checkpointer, "TRUNCATE")?, (0, 0, 0));
    verify(&reader, 1)
}

#[test]
#[ignore = "launched by wal_crash_and_consistent_backup_preserve_committed_state"]
fn wal_crash_worker() -> Result {
    if ["MATTERS_SQLITE_CRASH_DB", "MATTERS_SQLITE_CRASH_POINT"]
        .iter()
        .all(|k| std::env::var_os(k).is_none())
    {
        return Ok(());
    }
    let path = std::env::var("MATTERS_SQLITE_CRASH_DB")?;
    let point = std::env::var("MATTERS_SQLITE_CRASH_POINT")?;
    let mut conn = open(Path::new(&path))?;
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    tx.execute("UPDATE wal_probe SET generation=1,payload=?1", [payload(1)])?;
    if point == "before" {
        std::process::exit(151);
    }
    assert_eq!(point, "after");
    tx.commit()?;
    std::process::exit(152);
}

#[test]
fn wal_crash_and_consistent_backup_preserve_committed_state() -> Result {
    for (point, code, generation) in [("before", 151, 0), ("after", 152, 1)] {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("crash.db");
        drop(initialize(&path)?);
        let output = std::process::Command::new(std::env::current_exe()?)
            .args(["--exact", "wal::wal_crash_worker", "--ignored"])
            .env("MATTERS_SQLITE_CRASH_DB", &path)
            .env("MATTERS_SQLITE_CRASH_POINT", point)
            .output()?;
        assert_eq!(
            output.status.code(),
            Some(code),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        if point == "after" {
            assert!(std::fs::metadata(path.with_extension("db-wal"))?.len() > 32);
        }
        let recovered = open(&path)?;
        verify(&recovered, generation)?;
        let backup = dir.path().join("backup.db");
        recovered.execute(
            "VACUUM INTO ?1",
            [backup.to_str().ok_or("non-UTF8 fixture path")?],
        )?;
        verify(&open(&backup)?, generation)?;
    }
    Ok(())
}
