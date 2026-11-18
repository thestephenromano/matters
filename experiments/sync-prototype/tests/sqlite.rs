#[path = "../../../ci/sqlite_runtime.rs"]
mod sqlite_runtime;

#[test]
fn linked_sqlite_matches_approved_release() {
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    sqlite_runtime::assert_runtime(&conn).unwrap();
}

#[path = "../../../ci/sqlite_wal_tests.rs"]
mod wal;

#[test]
fn file_open_rejects_memory_journal_fallback() {
    let error = matters_sync_prototype::Replica::open(":memory:")
        .err()
        .expect("persistent open must reject a memory journal");
    assert!(error.to_string().contains("journal mode"));
}

#[test]
fn file_open_selects_wal_and_reopen_preserves_it() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("actual-open.db");
    for _ in 0..2 {
        let opened = matters_sync_prototype::Replica::open(&path).unwrap();
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
fn explicit_in_memory_replica_is_usable() {
    let mut replica = matters_sync_prototype::Replica::open_in_memory().unwrap();
    replica
        .execute(&matters_sync_prototype::Request::new(
            matters_sync_prototype::Command::Create {
                id: uuid::Uuid::from_u128(1).to_string(),
                kind: matters_sync_prototype::Kind::Task,
                parent: None,
                title: "volatile fixture".into(),
            },
        ))
        .unwrap();
    assert_eq!(replica.journal_len().unwrap(), 1);
}
