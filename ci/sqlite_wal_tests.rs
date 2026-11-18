//! Shared integration coverage, not a reproduction of SQLite's internal race.
use rusqlite::{params, Connection, TransactionBehavior};
use std::path::Path;
use std::sync::mpsc;
use std::time::Duration;

type Result<T = ()> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;
// Eight rows per generation and eight rounds per checkpoint mode are a finite
// fixture workload. Payloads span four 4096-byte pages each, so the crash worker's
// four-page cache must spill. Actual WAL frames are checked, not inferred.
const ROWS: i64 = 8;
const ROUNDS: i64 = 32;
const PAGE_SIZE: usize = 4096;
const PAYLOAD_BYTES: usize = 4 * PAGE_SIZE;
// Fail a stalled test instead of deadlocking if either worker exits unexpectedly.
const WAIT: Duration = Duration::from_secs(10);
fn open(path: &Path) -> Result<Connection> {
    let conn = Connection::open(path)?;
    conn.busy_timeout(Duration::from_secs(5))?;
    conn.execute_batch("PRAGMA synchronous=FULL; PRAGMA wal_autocheckpoint=0;")?;
    Ok(conn)
}
fn payload(generation: i64) -> String {
    format!("generation {generation}: {}", "x".repeat(PAYLOAD_BYTES))
}
fn append(conn: &Connection, generation: i64) -> Result {
    for id in 0..ROWS {
        conn.execute(
            "INSERT INTO wal_probe VALUES (?1,?2,?3)",
            params![generation, id, payload(generation)],
        )?;
    }
    Ok(())
}
fn initialize(path: &Path) -> Result<Connection> {
    let mut conn = open(path)?;
    conn.pragma_update(None, "page_size", PAGE_SIZE as i64)?;
    let mode: String = conn.query_row("PRAGMA journal_mode=WAL", [], |r| r.get(0))?;
    assert_eq!(mode, "wal");
    conn.execute_batch("CREATE TABLE wal_probe(generation INTEGER NOT NULL,id INTEGER NOT NULL,payload TEXT NOT NULL,PRIMARY KEY(generation,id));")?;
    let tx = conn.transaction()?;
    append(&tx, 0)?;
    tx.commit()?;
    Ok(conn)
}
fn verify(conn: &Connection, generation: i64) -> Result {
    let mut stmt =
        conn.prepare("SELECT generation,id,payload FROM wal_probe ORDER BY generation,id")?;
    let rows = stmt.query_map([], |r| {
        Ok((
            r.get::<_, i64>(0)?,
            r.get::<_, i64>(1)?,
            r.get::<_, String>(2)?,
        ))
    })?;
    let rows = rows.collect::<rusqlite::Result<Vec<_>>>()?;
    assert_eq!(
        rows.len(),
        ((generation + 1) * ROWS) as usize,
        "every committed generation must remain"
    );
    for (index, (actual_generation, id, bytes)) in rows.into_iter().enumerate() {
        let expected_generation = index as i64 / ROWS;
        assert_eq!(id, index as i64 % ROWS);
        assert_eq!(actual_generation, expected_generation);
        assert_eq!(bytes, payload(expected_generation));
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
fn completed(result: (i64, i64, i64)) -> i64 {
    let (busy, frames, copied) = result;
    assert_eq!(busy, 0, "checkpoint must complete");
    assert!(frames >= 0 && copied >= 0, "checkpoint must actually run");
    assert_eq!(frames, copied);
    copied
}

#[test]
fn wal_commits_survive_concurrent_checkpoint_modes_and_reopen() -> Result {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("concurrent.db");
    let keeper = initialize(&path)?;
    let mut writer = open(&path)?;
    let checkpointer = open(&path)?;
    let (ready_tx, ready_rx) = mpsc::channel();
    let (go_tx, go_rx) = mpsc::channel();
    let handle = std::thread::spawn(move || -> Result {
        for generation in 1..=ROUNDS {
            let tx = writer.transaction_with_behavior(TransactionBehavior::Immediate)?;
            append(&tx, generation)?;
            ready_tx.send(())?; // transaction remains open while parent checkpoints
            go_rx.recv_timeout(WAIT)?;
            tx.commit()?;
            ready_tx.send(())?;
            go_rx.recv_timeout(WAIT)?; // don't begin next transaction prematurely
        }
        Ok(())
    });
    let checked = (|| -> Result {
        let mut progress_while_writer_active = 0;
        let mut successful_modes = [0; 4];
        for round in 0..ROUNDS {
            ready_rx.recv_timeout(WAIT)?;
            // PASSIVE can copy prior committed frames while a writer transaction
            // is held open. The channel prevents the writer from committing yet.
            progress_while_writer_active += completed(checkpoint(&checkpointer, "PASSIVE")?);
            go_tx.send(())?;
            let index = round as usize % 4;
            let mode = ["PASSIVE", "FULL", "RESTART", "TRUNCATE"][index];
            let concurrent = checkpoint(&checkpointer, mode)?;
            assert!([0, 1].contains(&concurrent.0));
            assert!(concurrent.1 >= 0 && concurrent.2 >= 0 && concurrent.2 <= concurrent.1);
            ready_rx.recv_timeout(WAIT)?;
            // Completion after the commit is required for every mode, even if
            // the concurrent attempt encountered the writer lock.
            completed(checkpoint(&checkpointer, mode)?);
            successful_modes[index] += 1;
            go_tx.send(())?;
        }
        assert!(
            progress_while_writer_active > 0,
            "checkpoint must copy committed frames during an open writer transaction"
        );
        assert_eq!(successful_modes, [8; 4]);
        Ok(())
    })();
    drop(go_tx); // release a waiting writer on an error before joining
    let written = handle.join().expect("writer thread panicked");
    checked?;
    written?;
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
    let mut writer = initialize(&path)?;
    let reader = open(&path)?;
    let checkpointer = open(&path)?;
    checkpointer.busy_timeout(Duration::ZERO)?;
    reader.execute_batch("BEGIN;")?;
    verify(&reader, 0)?;
    let tx = writer.transaction()?;
    append(&tx, 1)?;
    tx.commit()?;
    verify(&reader, 0)?;
    let (busy, frames, copied) = checkpoint(&checkpointer, "TRUNCATE")?;
    assert_eq!(busy, 1);
    assert!(frames > 0 && copied >= 0 && copied < frames);
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
    conn.execute_batch("PRAGMA cache_size=4; PRAGMA cache_spill=ON;")?;
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    append(&tx, 1)?;
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
        let wal_path = path.with_extension("db-wal");
        assert!(!wal_path.exists(), "start with no prior WAL frames");
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
        // Inspect before opening/recovering. SQLite WAL has a 32-byte header,
        // then 24-byte frame headers + one page; frame offset 4 is the commit
        // database size (zero for uncommitted frames). This is fixture evidence,
        // not an independent WAL validator: https://sqlite.org/fileformat2.html#walformat
        let bytes = std::fs::read(&wal_path)?;
        let frame_size = 24 + PAGE_SIZE;
        assert!(
            bytes.len() >= 32 + frame_size,
            "crash must leave actual WAL frames"
        );
        assert_eq!(
            u32::from_be_bytes(bytes[8..12].try_into()?) as usize,
            PAGE_SIZE
        );
        assert_eq!((bytes.len() - 32) % frame_size, 0);
        let commits = bytes[32..]
            .chunks_exact(frame_size)
            .filter(|f| f[4..8] != [0; 4])
            .count();
        assert_eq!(
            commits,
            usize::from(point == "after"),
            "before-commit spill must contain no commit frame"
        );
        eprintln!(
            "{point}: {} WAL frames, {commits} commit frames before recovery",
            (bytes.len() - 32) / frame_size
        );
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
