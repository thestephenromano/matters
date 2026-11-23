//! Writer claims are signed lazily (#45): each open of a joined database is
//! a new writer incarnation and fences older handles at once, but its
//! signed claim enters the authority log only with its first seal, in that
//! seal's transaction. An open that never writes leaves no signed trace.
//! The tests cover:
//! - the log's growth: opens with no write add no control, the first write
//!   adds exactly one;
//! - the handoff's predecessor: the last claimed writer, at its head;
//! - another device receiving every record, and only claimed writers in
//!   checkpoints and closing frontiers;
//! - joining, resuming and recovering, which claim nothing;
//! - checkpoints, which seal and so claim;
//! - two handles on one database, and a failed batch keeping the actor;
//! - a crash between the claim and the seal, which share one commit.

use std::path::{Path, PathBuf};
use std::process::Command;

use matters_core::storage::checkpoint::Checkpointed;
use matters_core::storage::membership::{DeviceSeeds, KeyStore, MemoryKeyStore, SyncStanding};
use matters_core::storage::receive::Received;
use matters_core::{Database, MattersError, Task};
use matters_sync::authority::{parse, Body, Hash, Id, Route};
use matters_sync::checkpoint::Cursor;
use matters_sync::{record_hash, wire};

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

/// One writer claim: its incarnation, and for a handoff its predecessor
/// and closing head.
#[derive(Debug, PartialEq, Eq)]
struct Claim {
    incarnation: Id,
    handoff: Option<(Id, Option<Hash>)>,
}

/// The writer claims among `controls`, in order.
fn claims_in(controls: &[Vec<u8>]) -> Vec<Claim> {
    controls
        .iter()
        .filter_map(|c| match parse(c).unwrap().message.body {
            Body::WriterStart(s) => Some(Claim {
                incarnation: s.incarnation,
                handoff: None,
            }),
            Body::WriterHandoff(h) => Some(Claim {
                incarnation: h.incarnation,
                handoff: Some((h.previous, h.closing)),
            }),
            _ => None,
        })
        .collect()
}

/// The writer claims `db` stored, in order.
fn claims(db: &Database) -> Vec<Claim> {
    claims_in(&db.sync_controls().unwrap())
}

fn outgoing(db: &Database) -> Vec<Vec<u8>> {
    db.outgoing_controls()
        .unwrap()
        .into_iter()
        .map(|o| o.bytes)
        .collect()
}

fn records(db: &Database) -> Vec<Vec<u8>> {
    db.outgoing_records()
        .unwrap()
        .into_iter()
        .map(|r| r.bytes)
        .collect()
}

/// The writer incarnation a sealed record names.
fn incarnation_of(db: &Database, record: &[u8]) -> Id {
    let document = db.sync_membership().unwrap().document;
    *wire::parse(record, route().content(&document))
        .unwrap()
        .header
        .incarnation
}

/// Open `path` and resume sync with `keys`.
fn reopen(path: &Path, keys: &dyn KeyStore) -> Database {
    let db = Database::open(path).unwrap();
    assert_eq!(db.resume_sync(keys).unwrap(), SyncStanding::Active);
    db
}

/// Apply to `to` every writer claim `from` published.
fn deliver_claims(from: &Database, to: &Database) {
    for bytes in outgoing(from) {
        if !claims_in(std::slice::from_ref(&bytes)).is_empty() {
            to.apply_control(&bytes, None).unwrap();
        }
    }
}

/// A key store in files, so a child process can load the seeds.
struct FileKeys(PathBuf);

impl FileKeys {
    fn file(&self, name: &str) -> PathBuf {
        self.0.join(name.replace('/', "_"))
    }
}

impl KeyStore for FileKeys {
    fn load(&self, name: &str) -> matters_core::Result<Option<DeviceSeeds>> {
        match std::fs::read(self.file(name)) {
            Ok(bytes) => DeviceSeeds::from_bytes(&bytes).map(Some),
            Err(_) => Ok(None),
        }
    }

    fn store(&self, name: &str, seeds: &DeviceSeeds) -> matters_core::Result<()> {
        std::fs::write(self.file(name), seeds.to_bytes()).unwrap();
        Ok(())
    }
}

struct Pair {
    dir: tempfile::TempDir,
    a: Database,
    b: Database,
    b_path: PathBuf,
    b_keys: MemoryKeyStore,
}

/// A, the admin, with no rows and no writer claimed, and B joined to it.
fn pair() -> Pair {
    let dir = tempfile::tempdir().unwrap();
    let a = Database::open(dir.path().join("a.db")).unwrap();
    a.create_collection(&MemoryKeyStore::default(), route())
        .unwrap();
    let b_path = dir.path().join("b.db");
    let b = Database::open(&b_path).unwrap();
    let b_keys = MemoryKeyStore::default();
    let shown = a.invite().unwrap();
    let request = b.start_enrollment(&b_keys, &shown).unwrap();
    let joiner = parse(&request).unwrap().message.signer;
    a.admit(&request, &shown).unwrap();
    let envelopes: Vec<Vec<u8>> = outgoing(&a)
        .into_iter()
        .filter(|bytes| {
            matches!(parse(bytes).unwrap().message.body,
                Body::KeyEnvelope(ref k) if k.recipient == joiner)
        })
        .collect();
    assert_eq!(
        b.finish_enrollment(&b_keys, &a.sync_controls().unwrap(), &envelopes)
            .unwrap(),
        SyncStanding::Active
    );
    Pair {
        dir,
        a,
        b,
        b_path,
        b_keys,
    }
}

#[test]
fn opens_with_no_write_sign_nothing_and_the_first_write_signs_one_claim() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("m.db");
    let keys = MemoryKeyStore::default();
    let db = Database::open(&path).unwrap();
    db.create_collection(&keys, route()).unwrap();
    assert!(claims(&db).is_empty(), "no rows: nothing sealed or claimed");
    let published = outgoing(&db).len();

    db.add_task(&mut Task::new(id(1), "one", 0.0)).unwrap();
    let first = db.writer_incarnation().unwrap();
    assert_eq!(
        claims(&db),
        [Claim {
            incarnation: first,
            handoff: None,
        }],
        "the first write claims the writer, as a root"
    );
    let queued = outgoing(&db);
    assert_eq!(queued.len(), published + 1, "and queues it to publish");
    assert_eq!(queued.last(), db.sync_controls().unwrap().last());
    db.add_task(&mut Task::new(id(2), "two", 1.0)).unwrap();
    assert_eq!(claims(&db).len(), 1, "a second write claims nothing");
    let head = record_hash(records(&db).last().unwrap());
    drop(db);

    // Opens that write nothing: a fresh writer each, and no control.
    let controls = reopen(&path, &keys).sync_controls().unwrap();
    let mut idle = Vec::new();
    for _ in 0..3 {
        let db = reopen(&path, &keys);
        idle.push(db.writer_incarnation().unwrap());
        assert_eq!(db.sync_controls().unwrap(), controls);
        assert_eq!(outgoing(&db).len(), published + 1);
    }

    // The next writing open hands off from the last claimed writer, at the
    // head of its chain, skipping every open that claimed nothing.
    let db = reopen(&path, &keys);
    db.add_task(&mut Task::new(id(3), "three", 2.0)).unwrap();
    let all = claims(&db);
    assert_eq!(all.len(), 2, "exactly one more claim");
    assert_eq!(
        all[1],
        Claim {
            incarnation: db.writer_incarnation().unwrap(),
            handoff: Some((first, Some(head))),
        }
    );
    assert!(!idle.contains(&db.writer_incarnation().unwrap()));
    assert_eq!(outgoing(&db).len(), published + 2);
}

#[test]
fn a_collection_made_from_existing_rows_claims_with_its_first_record() {
    let dir = tempfile::tempdir().unwrap();
    let db = Database::open(dir.path().join("m.db")).unwrap();
    db.add_task(&mut Task::new(id(1), "kept", 0.0)).unwrap();
    db.create_collection(&MemoryKeyStore::default(), route())
        .unwrap();
    let sealed = records(&db);
    assert!(!sealed.is_empty());
    let writer = db.writer_incarnation().unwrap();
    assert_eq!(
        claims(&db),
        [Claim {
            incarnation: writer,
            handoff: None,
        }]
    );
    assert!(sealed.iter().all(|r| incarnation_of(&db, r) == writer));
    db.add_task(&mut Task::new(id(2), "later", 1.0)).unwrap();
    assert_eq!(
        claims(&db).len(),
        1,
        "the import's claim covers later writes"
    );
}

#[test]
fn receivers_see_only_claimed_writers_and_apply_every_record() {
    let p = pair();
    // Joining and resuming claim nothing.
    assert!(claims(&p.b).is_empty(), "joining claims nothing");
    drop(p.b);
    let mut idle = Vec::new();
    for _ in 0..2 {
        let b = reopen(&p.b_path, &p.b_keys);
        idle.push(b.writer_incarnation().unwrap());
        assert!(claims(&b).is_empty(), "resuming claims nothing");
    }
    let b = reopen(&p.b_path, &p.b_keys);
    b.add_task(&mut Task::new(id(1), "from B", 0.0)).unwrap();
    b.add_task(&mut Task::new(id(2), "from B again", 1.0))
        .unwrap();
    let writer = b.writer_incarnation().unwrap();
    assert_eq!(
        claims(&b),
        [Claim {
            incarnation: writer,
            handoff: None,
        }]
    );
    // A receives the claim before the records, as the client pushes them,
    // and applies every record: each names a claimed writer.
    deliver_claims(&b, &p.a);
    for r in records(&b) {
        assert_eq!(incarnation_of(&b, &r), writer);
        assert_eq!(p.a.receive_record(&r).unwrap(), Received::Applied);
    }
    assert!(p.a.get_task(&id(2)).unwrap().is_some());

    // A's checkpoint lists B's chain, and no open that claimed nothing.
    p.a.add_task(&mut Task::new(id(3), "from A", 2.0)).unwrap();
    for r in p.a.outgoing_records().unwrap() {
        p.a.acknowledge_record(&r.record_id).unwrap();
    }
    let cursor = Cursor {
        generation: [9; 16],
        sequence: 3,
    };
    p.a.create_checkpoint(Some(cursor)).unwrap().unwrap();
    let listed: Vec<Id> =
        p.a.latest_checkpoint()
            .unwrap()
            .unwrap()
            .writers
            .iter()
            .map(|w| w.incarnation)
            .collect();
    assert_eq!(listed.len(), 2, "A's chain and B's");
    assert!(listed.contains(&writer));
    assert!(idle.iter().all(|i| !listed.contains(i)));

    // Removing B closes its claimed writer at what A holds of it, and
    // nothing else of B.
    let b_id = b.sync_membership().unwrap().device;
    p.a.remove_member(&b_id).unwrap();
    let removal = outgoing(&p.a)
        .into_iter()
        .rev()
        .find(|c| matches!(parse(c).unwrap().message.body, Body::Membership(_)))
        .unwrap();
    let Body::Membership(m) = parse(&removal).unwrap().message.body else {
        unreachable!()
    };
    let closed: Vec<(Id, Option<Hash>)> = m
        .frontier
        .iter()
        .filter(|c| c.device == b_id)
        .map(|c| (c.incarnation, c.head))
        .collect();
    assert_eq!(
        closed,
        [(writer, Some(record_hash(records(&b).last().unwrap())))]
    );
    drop(p.dir);
}

#[test]
fn a_checkpoint_seals_and_so_claims_the_writer_once() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("m.db");
    let keys = MemoryKeyStore::default();
    let db = Database::open(&path).unwrap();
    db.create_collection(&keys, route()).unwrap();
    db.add_task(&mut Task::new(id(1), "one", 0.0)).unwrap();
    for r in db.outgoing_records().unwrap() {
        db.acknowledge_record(&r.record_id).unwrap();
    }
    drop(db);
    let db = reopen(&path, &keys);
    let before = claims(&db).len();
    let cursor = |sequence| {
        Some(Cursor {
            generation: [9; 16],
            sequence,
        })
    };
    db.create_checkpoint(cursor(1)).unwrap().unwrap();
    let all = claims(&db);
    assert_eq!(all.len(), before + 1, "its snapshot records need a writer");
    assert_eq!(all[before].incarnation, db.writer_incarnation().unwrap());
    let parts = db.outgoing_checkpoint_parts().unwrap();
    let document = db.sync_membership().unwrap().document;
    for (_, bytes) in &parts {
        let header = wire::parse(bytes, route().content(&document))
            .unwrap()
            .header;
        assert_eq!(header.incarnation, &db.writer_incarnation().unwrap());
    }
    // A later write and checkpoint of the same open claim nothing more.
    db.add_task(&mut Task::new(id(2), "two", 1.0)).unwrap();
    for r in db.outgoing_records().unwrap() {
        db.acknowledge_record(&r.record_id).unwrap();
    }
    db.create_checkpoint(cursor(2)).unwrap().unwrap();
    assert_eq!(claims(&db).len(), before + 1);
    // The writer sealed no data record before its write: the write's
    // record is the first of its chain.
    let writer = db.writer_incarnation().unwrap();
    let own: i64 = db
        .with_conn(|c| {
            Ok(c.query_row(
                "SELECT COUNT(*) FROM sync_records WHERE incarnation = ?1",
                [writer.as_slice()],
                |r| r.get(0),
            )?)
        })
        .unwrap();
    assert_eq!(own, 1);
}

#[test]
fn a_checkpoint_from_an_open_that_claimed_with_it_is_accepted_elsewhere() {
    let p = pair();
    p.a.add_task(&mut Task::new(id(1), "from A", 0.0)).unwrap();
    deliver_claims(&p.a, &p.b);
    for r in records(&p.a) {
        assert_eq!(p.b.receive_record(&r).unwrap(), Received::Applied);
        p.a.acknowledge_record(&p.a.outgoing_records().unwrap()[0].record_id)
            .unwrap();
    }
    // B never wrote: its checkpoint claims its writer.
    for r in p.b.outgoing_records().unwrap() {
        p.b.acknowledge_record(&r.record_id).unwrap();
    }
    assert!(claims(&p.b)
        .iter()
        .all(|c| { c.incarnation != p.b.writer_incarnation().unwrap() }));
    let cursor = Cursor {
        generation: [9; 16],
        sequence: 1,
    };
    p.b.create_checkpoint(Some(cursor)).unwrap().unwrap();
    deliver_claims(&p.b, &p.a);
    let parts = p.b.outgoing_checkpoint_parts().unwrap();
    let Received::Checkpoint(outcome) = p.a.receive_record(&parts[0].1).unwrap() else {
        panic!("a checkpoint part")
    };
    assert_eq!(outcome, Checkpointed::Accepted);
}

#[test]
fn a_recovered_collection_claims_a_writer_only_when_it_writes() {
    let dir = tempfile::tempdir().unwrap();
    let a = Database::open(dir.path().join("a.db")).unwrap();
    let words = a
        .create_collection(&MemoryKeyStore::default(), route())
        .unwrap();
    a.add_task(&mut Task::new(id(1), "from A", 0.0)).unwrap();
    let envelopes: Vec<Vec<u8>> = outgoing(&a)
        .into_iter()
        .filter(|b| {
            matches!(parse(b).unwrap().message.body,
                Body::KeyEnvelope(ref k) if k.recipient == k.recipient_key_fingerprint)
        })
        .collect();
    let c_path = dir.path().join("c.db");
    let c = Database::open(&c_path).unwrap();
    let c_keys = MemoryKeyStore::default();
    c.recover_collection(
        &c_keys,
        &words,
        route(),
        &a.sync_controls().unwrap(),
        &envelopes,
    )
    .unwrap();
    let inherited = claims(&a).len();
    assert_eq!(claims(&c).len(), inherited, "the recovery claims nothing");
    drop(c);
    let c = reopen(&c_path, &c_keys);
    assert_eq!(claims(&c).len(), inherited);
    c.add_task(&mut Task::new(id(2), "recovered", 1.0)).unwrap();
    let all = claims(&c);
    assert_eq!(all.len(), inherited + 1);
    assert_eq!(
        all[inherited],
        Claim {
            incarnation: c.writer_incarnation().unwrap(),
            handoff: None,
        },
        "the recovered device's first writer is a root"
    );
}

#[test]
fn an_unclaimed_newer_open_fences_an_older_handle() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("m.db");
    let keys = MemoryKeyStore::default();
    let db = Database::open(&path).unwrap();
    db.create_collection(&keys, route()).unwrap();
    db.add_task(&mut Task::new(id(1), "one", 0.0)).unwrap();
    let claimed = db.writer_incarnation().unwrap();
    let head = record_hash(records(&db).last().unwrap());
    drop(db);
    // Two handles, as the app and matters-syncd: neither has written.
    let older = reopen(&path, &keys);
    let newer = reopen(&path, &keys);
    let controls = newer.sync_controls().unwrap();
    assert!(matches!(
        older.add_task(&mut Task::new(id(2), "fenced", 1.0)),
        Err(MattersError::Superseded)
    ));
    assert_eq!(
        newer.sync_controls().unwrap(),
        controls,
        "a fenced write claims nothing"
    );
    let cursor = Cursor {
        generation: [9; 16],
        sequence: 1,
    };
    assert!(matches!(
        older.create_checkpoint(Some(cursor)),
        Err(MattersError::Superseded)
    ));
    assert_eq!(newer.sync_controls().unwrap(), controls);
    // The newer one's first write hands off from the last claimed writer,
    // not from the older handle's unclaimed one.
    newer.add_task(&mut Task::new(id(3), "kept", 2.0)).unwrap();
    let all = claims(&newer);
    assert_eq!(
        all.last().unwrap(),
        &Claim {
            incarnation: newer.writer_incarnation().unwrap(),
            handoff: Some((claimed, Some(head))),
        }
    );
    assert_eq!(all.len(), 2);
    // Reloaded, the older handle's next open fences the newer one in turn.
    drop(older);
    let older = reopen(&path, &keys);
    assert!(matches!(
        newer.add_task(&mut Task::new(id(4), "stale", 3.0)),
        Err(MattersError::Superseded)
    ));
    older
        .add_task(&mut Task::new(id(5), "after reload", 4.0))
        .unwrap();
    assert_eq!(claims(&older).len(), 3);
}

#[test]
fn a_failed_batch_keeps_the_claimed_actor_and_its_claim() {
    let p = pair();
    let fail = |db: &Database, n: u128| {
        let refused = db.batch(|w| {
            w.add_task(&mut Task::new(id(n), "rolled back", 0.0))?;
            Err::<(), _>(MattersError::Validation("refused".into()))
        });
        assert!(refused.is_err());
    };
    // Before the claim: nothing is claimed by the failed batch.
    fail(&p.a, 9);
    assert!(claims(&p.a).is_empty());
    p.a.add_task(&mut Task::new(id(1), "one", 0.0)).unwrap();
    // After it: the engine reloads, keeping the actor the claim names.
    fail(&p.a, 10);
    p.a.add_task(&mut Task::new(id(2), "two", 1.0)).unwrap();
    assert_eq!(claims(&p.a).len(), 1);
    deliver_claims(&p.a, &p.b);
    for r in records(&p.a) {
        assert_eq!(p.b.receive_record(&r).unwrap(), Received::Applied);
    }
    assert!(p.b.get_task(&id(2)).unwrap().is_some());
    assert!(p.b.get_task(&id(10)).unwrap().is_none());
}

/// Child process for the crash test: resume the database named by
/// `MATTERS_CRASH_DB` with the seeds in `MATTERS_CRASH_KEYS`, then make the
/// open's first write, which `MATTERS_CRASH_AT` interrupts. Ignored in
/// ordinary runs; the parent test runs it.
#[test]
#[ignore]
fn claim_crash_worker() {
    let (Ok(path), Ok(keys)) = (
        std::env::var("MATTERS_CRASH_DB"),
        std::env::var("MATTERS_CRASH_KEYS"),
    ) else {
        return;
    };
    let db = reopen(Path::new(&path), &FileKeys(keys.into()));
    std::fs::write(format!("{path}.writer"), db.writer_incarnation().unwrap()).unwrap();
    db.add_task(&mut Task::new(id(2), "interrupted", 1.0))
        .unwrap();
    unreachable!("the crash point exits the process");
}

#[test]
fn a_crash_keeps_the_claim_and_its_first_record_together() {
    for (point, committed) in [("before-commit", false), ("after-commit", true)] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("m.db");
        let keys = FileKeys(dir.path().to_owned());
        let db = Database::open(&path).unwrap();
        db.create_collection(&keys, route()).unwrap();
        db.add_task(&mut Task::new(id(1), "one", 0.0)).unwrap();
        let claimed = db.writer_incarnation().unwrap();
        drop(db);
        let status = Command::new(std::env::current_exe().unwrap())
            .args(["claim_crash_worker", "--exact", "--ignored", "--nocapture"])
            .env("MATTERS_CRASH_DB", &path)
            .env("MATTERS_CRASH_KEYS", dir.path())
            .env("MATTERS_CRASH_AT", point)
            .status()
            .unwrap();
        assert_eq!(status.code(), Some(86), "{point}: exited at the point");
        let crashed: Id = std::fs::read(format!("{}.writer", path.display()))
            .unwrap()
            .try_into()
            .unwrap();

        let db = reopen(&path, &keys);
        let sealed = records(&db);
        let all = claims(&db);
        let claims_crashed = all.iter().any(|c| c.incarnation == crashed);
        let seals_crashed = sealed.iter().any(|r| incarnation_of(&db, r) == crashed);
        assert_eq!(claims_crashed, committed, "{point}: the claim");
        assert_eq!(seals_crashed, committed, "{point}: the record");
        assert_eq!(all.len(), 1 + usize::from(committed), "{point}");
        // The next writer hands off from whichever was last claimed.
        db.add_task(&mut Task::new(id(3), "after restart", 2.0))
            .unwrap();
        let previous = if committed { crashed } else { claimed };
        let last = claims(&db).pop().unwrap();
        assert_eq!(
            last.handoff.map(|(p, _)| p),
            Some(previous),
            "{point}: the handoff's predecessor"
        );
    }
}
