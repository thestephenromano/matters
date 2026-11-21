//! Returning to local-only use (#42): leaving a collection keeps every row
//! and a backup, refuses while edits have not reached the relay unless the
//! user chose to keep them local, and leaves the database ready to sync
//! again later.

use matters_core::storage::membership::MemoryKeyStore;
use matters_core::{Database, MattersError, Task};
use matters_sync::authority::Route;

fn id(n: u128) -> String {
    uuid::Uuid::from_u128(n).to_string()
}

fn route() -> Route {
    Route {
        application: [1; 16],
        tenant: [2; 16],
        collection: [3; 16],
    }
}

fn sync_rows(db: &Database) -> i64 {
    db.with_conn(|c| {
        let tables: Vec<String> = c
            .prepare("SELECT name FROM sqlite_master WHERE type = 'table' AND name LIKE 'sync%'")?
            .query_map([], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        let mut total = 0;
        for t in tables {
            total += c.query_row(&format!("SELECT COUNT(*) FROM \"{t}\""), [], |r| {
                r.get::<_, i64>(0)
            })?;
        }
        Ok(total)
    })
    .unwrap()
}

#[test]
fn leaving_keeps_every_row_and_refuses_to_strand_unsent_edits_silently() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("matters.db");
    let db = Database::open(&path).unwrap();
    db.create_collection(&MemoryKeyStore::default(), route())
        .unwrap();
    db.add_task(&mut Task::new(id(1), "kept", 0.0)).unwrap();
    // One edit has not reached the relay: refused, nothing changed.
    assert!(matches!(
        db.leave_collection(false),
        Err(MattersError::UnsentWork(1))
    ));
    assert!(db.sync_enabled());
    // The user chooses to keep it on this device only.
    let left = db.leave_collection(true).unwrap();
    assert_eq!(left.unsent, 1);
    assert!(!db.sync_enabled());
    assert_eq!(sync_rows(&db), 0, "nothing of the collection is left");
    assert_eq!(db.get_task(&id(1)).unwrap().unwrap().title, "kept");
    // The backup is the database as it was, sync and all.
    let backup = Database::open(left.backup.unwrap()).unwrap();
    assert!(backup.sync_enabled());
    assert_eq!(backup.get_task(&id(1)).unwrap().unwrap().title, "kept");
    // Local use goes on, across a restart, without keys or a relay.
    db.add_task(&mut Task::new(id(2), "local", 1.0)).unwrap();
    drop(db);
    let db = Database::open(&path).unwrap();
    assert!(!db.sync_enabled());
    assert_eq!(db.get_task(&id(2)).unwrap().unwrap().title, "local");
    // And it can sync again later, its rows migrating as any database's do.
    db.create_collection(&MemoryKeyStore::default(), route())
        .unwrap();
    assert!(
        !db.outgoing_records().unwrap().is_empty(),
        "the rows are records again"
    );
    assert_eq!(db.get_task(&id(1)).unwrap().unwrap().title, "kept");
    assert_eq!(db.get_task(&id(2)).unwrap().unwrap().title, "local");
}

#[test]
fn leaving_after_everything_reached_the_relay_needs_no_choice() {
    let dir = tempfile::tempdir().unwrap();
    let db = Database::open(dir.path().join("matters.db")).unwrap();
    db.create_collection(&MemoryKeyStore::default(), route())
        .unwrap();
    db.add_task(&mut Task::new(id(1), "sent", 0.0)).unwrap();
    for r in db.outgoing_records().unwrap() {
        db.acknowledge_record(&r.record_id).unwrap();
    }
    assert_eq!(db.leave_collection(false).unwrap().unsent, 0);
    // A database that does not sync has nothing to leave.
    assert!(db.leave_collection(false).is_err());
}
