//! Sealing recorded writes in their own transaction (#36): each local update
//! becomes one signed, encrypted record that commits with the edit, record
//! IDs never repeat across crashes, copies or concurrent handles, and retry
//! returns the stored bytes.

use std::collections::BTreeSet;
use std::process::Command;

use matters_core::storage::sealing::SealingIdentity;
use matters_core::{Database, Task};
use matters_sync::{generate_signing_key, record_hash, verify, wire, ContentKey, VerificationKey};

fn id(n: u128) -> String {
    uuid::Uuid::from_u128(n).to_string()
}

/// The device's identity and the key a receiver would check it with.
fn identity() -> (SealingIdentity, VerificationKey) {
    let (signing_key, public) = generate_signing_key([7; 32]);
    (
        SealingIdentity {
            application: [1; 16],
            tenant: [2; 16],
            collection: [3; 16],
            document: [4; 16],
            epoch: 0,
            manifest: [5; 48],
            author: [6; 48],
            content_key: ContentKey::new([9; 32]),
            signing_key,
        },
        public,
    )
}

fn context() -> wire::Context<'static> {
    wire::Context {
        application: &[1; 16],
        tenant: &[2; 16],
        collection: &[3; 16],
        document: &[4; 16],
    }
}

fn sealing(db: &Database) -> VerificationKey {
    let (identity, public) = identity();
    db.start_sealing(identity).unwrap();
    public
}

fn journal(db: &Database) -> Vec<(i64, Vec<u8>)> {
    db.outgoing_updates(0).unwrap()
}

#[test]
fn each_recorded_write_commits_one_record_that_verifies_and_opens() {
    let db = Database::open_in_memory().unwrap();
    db.enable_sync().unwrap();
    let public = sealing(&db);
    db.add_task(&mut Task::new(id(1), "one", 0.0)).unwrap();
    db.rename_task(&id(1), "two").unwrap();
    db.set_task_notes(&id(1), "notes").unwrap();

    let records = db.outgoing_records().unwrap();
    let updates = journal(&db);
    assert_eq!(records.len(), updates.len());
    let mut previous = None;
    for (record, (seq, update)) in records.iter().zip(&updates) {
        assert_eq!(record.seq, *seq);
        let parsed = wire::parse(&record.bytes, context()).unwrap();
        assert_eq!(parsed.header.record_id, &record.record_id);
        assert_eq!(
            parsed.header.incarnation,
            &db.writer_incarnation().unwrap(),
            "this open's writer"
        );
        assert_eq!(
            parsed.header.previous.copied(),
            previous,
            "the writer's chain"
        );
        let verified = verify(parsed, &public).unwrap();
        assert_eq!(&verified.open(&ContentKey::new([9; 32])).unwrap(), update);
        previous = Some(record_hash(&record.bytes));
    }
}

#[test]
fn retries_return_the_stored_bytes_and_acknowledged_records_leave_the_queue() {
    let db = Database::open_in_memory().unwrap();
    db.enable_sync().unwrap();
    sealing(&db);
    db.add_task(&mut Task::new(id(1), "one", 0.0)).unwrap();
    db.rename_task(&id(1), "two").unwrap();
    let first = db.outgoing_records().unwrap();
    assert_eq!(
        db.outgoing_records().unwrap(),
        first,
        "identical bytes on retry"
    );
    db.acknowledge_record(&first[0].record_id).unwrap();
    db.acknowledge_record(&first[0].record_id).unwrap(); // again: no change
    assert_eq!(db.outgoing_records().unwrap(), first[1..]);
    assert!(db.acknowledge_record(&[0; 32]).is_err(), "unknown record");
}

#[test]
fn copies_and_reopens_seal_as_new_writers_with_fresh_record_ids() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("a.db");
    let copy = dir.path().join("b.db");
    let first_writer = {
        let db = Database::open(&path).unwrap();
        db.enable_sync().unwrap();
        sealing(&db);
        db.add_task(&mut Task::new(id(1), "one", 0.0)).unwrap();
        db.writer_incarnation().unwrap()
    };
    // A backup copy, restored elsewhere: both continue editing.
    std::fs::copy(&path, &copy).unwrap();
    let (a, b) = (
        Database::open(&path).unwrap(),
        Database::open(&copy).unwrap(),
    );
    sealing(&a);
    sealing(&b);
    let writers: BTreeSet<_> = [
        first_writer,
        a.writer_incarnation().unwrap(),
        b.writer_incarnation().unwrap(),
    ]
    .into_iter()
    .collect();
    assert_eq!(writers.len(), 3, "every open is a new writer");
    for n in 0..20 {
        a.rename_task(&id(1), &format!("a{n}")).unwrap();
        b.rename_task(&id(1), &format!("b{n}")).unwrap();
    }
    let mut ids = BTreeSet::new();
    let mut starts = std::collections::BTreeMap::<[u8; 32], usize>::new();
    for db in [&a, &b] {
        for record in db.outgoing_records().unwrap() {
            let parsed = wire::parse(&record.bytes, context()).unwrap();
            if ids.insert(record.record_id) && parsed.header.previous.is_none() {
                *starts.entry(*parsed.header.incarnation).or_default() += 1;
            }
        }
    }
    // Each writer starts its own chain exactly once.
    assert_eq!(starts.len(), 3);
    assert!(starts.values().all(|n| *n == 1), "{starts:?}");
    // The shared first record appears in both; every other ID is distinct.
    assert_eq!(ids.len(), 1 + 20 + 20);
}

#[test]
fn concurrent_handles_on_one_file_never_share_a_record_id() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("a.db");
    let a = Database::open(&path).unwrap();
    a.enable_sync().unwrap();
    sealing(&a);
    a.add_task(&mut Task::new(id(1), "one", 0.0)).unwrap();
    let b = Database::open(&path).unwrap();
    sealing(&b);
    let (ta, tb) = (a.clone(), b.clone());
    let threads = [
        std::thread::spawn(move || {
            (0..15).for_each(|n| ta.set_task_notes(&id(1), &format!("a{n}")).unwrap())
        }),
        std::thread::spawn(move || {
            (0..15).for_each(|n| tb.set_task_notes(&id(1), &format!("b{n}")).unwrap())
        }),
    ];
    for t in threads {
        t.join().unwrap();
    }
    let records = Database::open(&path).unwrap().outgoing_records().unwrap();
    let ids: BTreeSet<_> = records.iter().map(|r| r.record_id).collect();
    assert_eq!(ids.len(), records.len());
    assert_eq!(records.len(), 31);
}

#[test]
fn without_a_sealing_identity_nothing_is_sealed() {
    let db = Database::open_in_memory().unwrap();
    db.enable_sync().unwrap();
    db.add_task(&mut Task::new(id(1), "one", 0.0)).unwrap();
    assert!(db.outgoing_records().unwrap().is_empty());
    assert_eq!(db.writer_incarnation(), None);
    let off = Database::open_in_memory().unwrap();
    let (identity, _) = identity();
    assert!(off.start_sealing(identity).is_err(), "sync is off");
}

/// Child process for the crash test: seal one write, then crash at
/// `MATTERS_CRASH_AT` during the next. Ignored in ordinary runs.
#[test]
#[ignore]
fn sealing_crash_worker() {
    let Ok(path) = std::env::var("MATTERS_CRASH_DB") else {
        return;
    };
    let db = Database::open(&path).unwrap();
    sealing(&db);
    let point = std::env::var("MATTERS_CRASH_AT").unwrap();
    std::env::remove_var("MATTERS_CRASH_AT");
    db.rename_task(&id(1), "sealed").unwrap();
    std::env::set_var("MATTERS_CRASH_AT", point);
    db.rename_task(&id(1), "interrupted").unwrap();
    unreachable!("the crash point exits the process");
}

#[test]
fn a_crash_at_commit_leaves_the_record_whole_or_absent_and_ids_unique() {
    for (point, records) in [("before-commit", 2), ("after-commit", 3)] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("matters.db");
        {
            let db = Database::open(&path).unwrap();
            db.enable_sync().unwrap();
            sealing(&db);
            db.add_task(&mut Task::new(id(1), "first", 0.0)).unwrap();
        }
        let status = Command::new(std::env::current_exe().unwrap())
            .args([
                "sealing_crash_worker",
                "--exact",
                "--ignored",
                "--nocapture",
            ])
            .env("MATTERS_CRASH_DB", &path)
            .env("MATTERS_CRASH_AT", point)
            .status()
            .unwrap();
        assert_eq!(status.code(), Some(86), "{point}");
        let db = Database::open(&path).unwrap();
        let stored = db.outgoing_records().unwrap();
        assert_eq!(
            stored.len(),
            records,
            "{point}: one record per committed write"
        );
        assert_eq!(stored.len(), journal(&db).len(), "{point}");
        // The retried edit seals under a new ID.
        sealing(&db);
        db.rename_task(&id(1), "retried").unwrap();
        let ids: BTreeSet<_> = db
            .outgoing_records()
            .unwrap()
            .iter()
            .map(|r| r.record_id)
            .collect();
        assert_eq!(ids.len(), records + 1, "{point}");
    }
}
