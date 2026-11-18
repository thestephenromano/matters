// Shared test-only deployment gate; update with upstream provenance and evidence.
// Exact identity prevents a system SQLite or stale bundled archive from passing.
pub fn assert_runtime(conn: &rusqlite::Connection) -> rusqlite::Result<()> {
    let (version, source_id): (String, String) =
        conn.query_row("SELECT sqlite_version(), sqlite_source_id()", [], |row| {
            Ok((row.get(0)?, row.get(1)?))
        })?;
    eprintln!("SQLite runtime: {version}; source_id: {source_id}");
    assert_eq!(version, "3.54.1", "unexpected linked SQLite version");
    assert_eq!(
        source_id,
        "2026-10-16 14:45:20 555e07ef33f69a0957d24d8e555b702464a131d6f1607f4c3c1e4551e8e6ca76",
        "unexpected linked SQLite source"
    );
    Ok(())
}
