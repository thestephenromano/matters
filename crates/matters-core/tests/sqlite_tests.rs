#[path = "../../../ci/sqlite_runtime.rs"]
mod sqlite_runtime;

#[test]
fn linked_sqlite_matches_approved_release() {
    let db = matters_core::storage::Database::open_in_memory().unwrap();
    db.with_conn(|conn| {
        sqlite_runtime::assert_runtime(conn)?;
        Ok(())
    })
    .unwrap();
}

#[path = "../../../ci/sqlite_wal_tests.rs"]
mod wal;

#[test]
fn file_open_rejects_memory_journal_fallback() {
    let error = matters_core::storage::Database::open(":memory:")
        .err()
        .expect("persistent open must reject a memory journal");
    assert!(error.to_string().contains("WAL"));
}

#[test]
fn file_open_selects_wal_and_reopen_preserves_it() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("actual-open.db");
    for _ in 0..2 {
        let opened = matters_core::storage::Database::open(&path).unwrap();
        let conn = rusqlite::Connection::open(&path).unwrap();
        let mode: String = conn
            .query_row("PRAGMA journal_mode", [], |r| r.get(0))
            .unwrap();
        assert_eq!(mode, "wal");
        drop(conn);
        drop(opened);
    }
}

#[test]
fn explicit_in_memory_open_preserves_memory_mode() {
    let db = matters_core::storage::Database::open_in_memory().unwrap();
    db.with_conn(|conn| {
        assert_eq!(
            conn.query_row("PRAGMA journal_mode", [], |r| r.get::<_, String>(0))?,
            "memory"
        );
        Ok(())
    })
    .unwrap();
}
