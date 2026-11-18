#[path = "../../../ci/sqlite_runtime.rs"]
mod sqlite_runtime;

#[test]
fn linked_sqlite_matches_approved_release() {
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    let (version, source): (String, String) = conn
        .query_row("SELECT sqlite_version(), sqlite_source_id()", [], |row| {
            Ok((row.get(0)?, row.get(1)?))
        })
        .unwrap();
    sqlite_runtime::assert_runtime(&version, &source);
}

#[path = "../../../ci/sqlite_wal_tests.rs"]
mod wal;
