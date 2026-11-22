//! The internal sync review's authority findings (#45): revocation by
//! closing frontier on removal and recovery, no lifetime caps on
//! transitions or writer incarnations, fresh replays that skip forged
//! objects, a replaced invitation, and a fence between two handles on one
//! database.

use matters_core::storage::membership::{KeyStore, MemoryKeyStore, SyncStanding};
use matters_core::storage::receive::{Received, Waiting};
use matters_core::{Database, MattersError, Task};
use matters_sync::authority::{parse, Body, DeviceKeys, Hash, Invitation, Route};
use matters_sync::record::record_hash;

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

fn keys(store: &MemoryKeyStore) -> DeviceKeys {
    let names = store.names();
    store.load(&names[0]).unwrap().unwrap().keys().unwrap()
}

fn device_id(store: &MemoryKeyStore) -> Hash {
    keys(store).id()
}

/// Deliver everything `from` published to `to`, as a relay would: in
/// order, with `to`'s envelope beside each transition. Returns each
/// control's outcome.
fn deliver_controls(from: &Database, to: &Database, to_id: &Hash) -> Vec<Result<(), String>> {
    let outgoing = from.outgoing_controls().unwrap();
    let mut outcomes = Vec::new();
    for o in &outgoing {
        let body = parse(&o.bytes).unwrap().message.body;
        let envelope = match body {
            Body::Membership(_) | Body::RootRecovery(_) => {
                let manifest = record_hash(&o.bytes);
                outgoing
                    .iter()
                    .find_map(|e| match parse(&e.bytes).unwrap().message.body {
                        Body::KeyEnvelope(k) if k.recipient == *to_id && k.manifest == manifest => {
                            Some(e.bytes.clone())
                        }
                        _ => None,
                    })
            }
            _ => None,
        };
        outcomes.push(
            to.apply_control(&o.bytes, envelope.as_deref())
                .map(|_| ())
                .map_err(|e| e.to_string()),
        );
    }
    outcomes
}

fn records(db: &Database) -> Vec<Vec<u8>> {
    db.outgoing_records()
        .unwrap()
        .into_iter()
        .map(|r| r.bytes)
        .collect()
}

fn envelopes_for(admin: &Database, device: &Hash) -> Vec<Vec<u8>> {
    admin
        .outgoing_controls()
        .unwrap()
        .into_iter()
        .filter(|o| {
            matches!(parse(&o.bytes).unwrap().message.body,
                Body::KeyEnvelope(ref k) if k.recipient == *device)
        })
        .map(|o| o.bytes)
        .collect()
}

fn recovery_envelopes(db: &Database) -> Vec<Vec<u8>> {
    db.outgoing_controls()
        .unwrap()
        .into_iter()
        .filter(|o| {
            matches!(parse(&o.bytes).unwrap().message.body,
                Body::KeyEnvelope(ref k) if k.recipient == k.recipient_key_fingerprint)
        })
        .map(|o| o.bytes)
        .collect()
}

fn has(db: &Database, n: u128) -> bool {
    db.get_task(&id(n)).unwrap().is_some()
}

/// Join `joiner` to `admin`'s collection through a fresh invitation.
fn join(admin: &Database, joiner: &Database, store: &MemoryKeyStore) {
    let shown = admin.invite().unwrap();
    let request = joiner.start_enrollment(store, &shown).unwrap();
    admin.admit(&request, &shown).unwrap();
    let me = parse(&request).unwrap().message.signer;
    assert_eq!(
        joiner
            .finish_enrollment(
                store,
                &admin.sync_controls().unwrap(),
                &envelopes_for(admin, &me)
            )
            .unwrap(),
        SyncStanding::Active
    );
}

struct Three {
    dir: tempfile::TempDir,
    a: Database,
    a_keys: MemoryKeyStore,
    b: Database,
    b_keys: MemoryKeyStore,
    c: Database,
    c_keys: MemoryKeyStore,
}

/// A (admin), B and C, joined in that order; every control delivered.
fn three() -> Three {
    let dir = tempfile::tempdir().unwrap();
    let open = |name: &str| Database::open(dir.path().join(name)).unwrap();
    let (a, b, c) = (open("a.db"), open("b.db"), open("c.db"));
    let (a_keys, b_keys, c_keys) = (
        MemoryKeyStore::default(),
        MemoryKeyStore::default(),
        MemoryKeyStore::default(),
    );
    a.create_collection(&a_keys, route()).unwrap();
    join(&a, &b, &b_keys);
    join(&a, &c, &c_keys);
    // B learns of C; A and C learn of B's writer.
    deliver_controls(&a, &b, &device_id(&b_keys));
    deliver_controls(&b, &a, &device_id(&a_keys));
    deliver_controls(&b, &c, &device_id(&c_keys));
    Three {
        dir,
        a,
        a_keys,
        b,
        b_keys,
        c,
        c_keys,
    }
}

#[test]
fn a_removed_devices_records_apply_only_within_the_admins_signed_frontier() {
    let t = three();
    let (a_id, c_id) = (device_id(&t.a_keys), device_id(&t.c_keys));
    // B writes; A holds it, then removes B.
    t.b.add_task(&mut Task::new(id(1), "before", 0.0)).unwrap();
    let first = records(&t.b);
    for r in &first {
        assert_eq!(t.a.receive_record(r).unwrap(), Received::Applied);
    }
    t.a.remove_member(&device_id(&t.b_keys)).unwrap();
    // B has not heard, and keeps sealing under the old epoch, chained on
    // its head.
    t.b.add_task(&mut Task::new(id(2), "after", 1.0)).unwrap();
    let all = records(&t.b);
    let late = all.last().unwrap();
    assert_eq!(
        parse_previous(late),
        Some(record_hash(first.last().unwrap()))
    );
    // A refuses it: it follows the closing head A signed.
    assert_eq!(
        t.a.receive_record(late).unwrap(),
        Received::Pending(Waiting::AfterClose)
    );
    assert!(!has(&t.a, 2));
    // C learns of the removal before any of B's records: the one within
    // the frontier still applies, the later one does not.
    deliver_controls(&t.a, &t.c, &c_id);
    assert_eq!(t.c.sync_membership().unwrap().members.len(), 2);
    for r in &first {
        assert_eq!(t.c.receive_record(r).unwrap(), Received::Applied);
    }
    assert_eq!(
        t.c.receive_record(late).unwrap(),
        Received::Pending(Waiting::AfterClose)
    );
    assert!(has(&t.c, 1) && !has(&t.c, 2));
    // B reopens: its new writer claim is refused by A and by C.
    drop(t.b);
    let b = Database::open(t.dir.path().join("b.db")).unwrap();
    b.resume_sync(&t.b_keys).unwrap();
    let claim = b
        .outgoing_controls()
        .unwrap()
        .into_iter()
        .rev()
        .find(|o| {
            matches!(
                parse(&o.bytes).unwrap().message.body,
                Body::WriterHandoff(_)
            )
        })
        .expect("B's reopen hands off");
    for (db, _) in [(&t.a, a_id), (&t.c, c_id)] {
        assert!(matches!(
            db.apply_control(&claim.bytes, None),
            Err(MattersError::Authority(
                matters_sync::authority::Error::Revoked
            ))
        ));
    }
    // A record of that refused writer never applies either.
    b.add_task(&mut Task::new(id(3), "reopened", 2.0)).unwrap();
    let newest = records(&b).pop().unwrap();
    assert!(t.c.receive_record(&newest).is_err());
    assert!(!has(&t.c, 3));
}

fn parse_previous(bytes: &[u8]) -> Option<Hash> {
    let p = matters_sync::wire::parse_for_route(bytes, &[1; 16], &[2; 16], &[3; 16]).unwrap();
    p.header.previous.copied()
}

#[test]
fn a_recovery_closes_the_lost_generations_writers_at_what_it_holds() {
    let dir = tempfile::tempdir().unwrap();
    let a = Database::open(dir.path().join("a.db")).unwrap();
    let a_keys = MemoryKeyStore::default();
    let words = a.create_collection(&a_keys, route()).unwrap();
    a.add_task(&mut Task::new(id(1), "held", 0.0)).unwrap();
    let held = records(&a);
    // A is lost (say, stolen). C recovers from the words and what the
    // relay holds, its records included.
    let c = Database::open(dir.path().join("c.db")).unwrap();
    let c_keys = MemoryKeyStore::default();
    c.recover_collection_holding(
        &c_keys,
        &words,
        route(),
        &a.sync_controls().unwrap(),
        &recovery_envelopes(&a),
        &held,
    )
    .unwrap();
    // The stolen A, unaware, keeps writing under generation 0.
    a.add_task(&mut Task::new(id(2), "stolen", 1.0)).unwrap();
    let late = records(&a).pop().unwrap();
    for r in &held {
        assert_eq!(c.receive_record(r).unwrap(), Received::Applied);
    }
    assert_eq!(
        c.receive_record(&late).unwrap(),
        Received::Pending(Waiting::AfterClose)
    );
    assert!(has(&c, 1) && !has(&c, 2));
    // A's new writer claim is refused.
    drop(a);
    let a = Database::open(dir.path().join("a.db")).unwrap();
    a.resume_sync(&a_keys).unwrap();
    let claim = a.outgoing_controls().unwrap().pop().unwrap();
    assert!(matches!(
        parse(&claim.bytes).unwrap().message.body,
        Body::WriterHandoff(_)
    ));
    assert!(c.apply_control(&claim.bytes, None).is_err());
}

#[test]
fn forty_reopens_across_two_devices_keep_sealing_and_receiving() {
    let dir = tempfile::tempdir().unwrap();
    let paths = [dir.path().join("a.db"), dir.path().join("b.db")];
    let stores = [MemoryKeyStore::default(), MemoryKeyStore::default()];
    let mut dbs = [
        Database::open(&paths[0]).unwrap(),
        Database::open(&paths[1]).unwrap(),
    ];
    dbs[0].create_collection(&stores[0], route()).unwrap();
    join(&dbs[0], &dbs[1], &stores[1]);
    let ids = [device_id(&stores[0]), device_id(&stores[1])];
    for n in 0..40u128 {
        let (me, other) = ((n % 2) as usize, ((n + 1) % 2) as usize);
        // Reopen, as each app start does: a new writer, handed off.
        dbs[me] = Database::open(&paths[me]).unwrap();
        assert_eq!(
            dbs[me].resume_sync(&stores[me]).unwrap(),
            SyncStanding::Active
        );
        dbs[me]
            .add_task(&mut Task::new(id(100 + n), "again", n as f64))
            .unwrap();
        for outcome in deliver_controls(&dbs[me], &dbs[other], &ids[other]) {
            outcome.unwrap();
        }
        for r in records(&dbs[me]) {
            dbs[other].receive_record(&r).unwrap();
        }
        assert!(has(&dbs[other], 100 + n), "cycle {n}");
    }
    for db in &dbs {
        assert!((0..40).all(|n| has(db, 100 + n)));
    }
}

#[test]
fn a_forged_claim_request_or_envelope_does_not_block_joining_or_recovery() {
    let dir = tempfile::tempdir().unwrap();
    let a = Database::open(dir.path().join("a.db")).unwrap();
    let a_keys = MemoryKeyStore::default();
    let words = a.create_collection(&a_keys, route()).unwrap();
    a.add_task(&mut Task::new(id(1), "from A", 0.0)).unwrap();
    let b = Database::open(dir.path().join("b.db")).unwrap();
    let b_keys = MemoryKeyStore::default();
    let shown = a.invite().unwrap();
    let request = b.start_enrollment(&b_keys, &shown).unwrap();
    a.admit(&request, &shown).unwrap();
    let b_id = parse(&request).unwrap().message.signer;
    // A writer claim with its signature broken, and the request itself,
    // injected into the log; a forged envelope ahead of the real ones.
    let mut controls = a.sync_controls().unwrap();
    let mut unsigned = controls
        .iter()
        .find(|c| matches!(parse(c).unwrap().message.body, Body::WriterStart(_)))
        .unwrap()
        .clone();
    let n = unsigned.len();
    unsigned[n - 1] ^= 1;
    controls.insert(1, unsigned);
    controls.insert(2, request.clone());
    let forge = |bytes: &[u8]| {
        let mut f = bytes.to_vec();
        let n = f.len();
        f[n - 2] ^= 1;
        f
    };
    let real = envelopes_for(&a, &b_id);
    let mut envelopes: Vec<Vec<u8>> = real.iter().map(|e| forge(e)).collect();
    envelopes.extend(real);
    assert_eq!(
        b.finish_enrollment(&b_keys, &controls, &envelopes).unwrap(),
        SyncStanding::Active
    );
    // What it stored replays on the next open.
    drop(b);
    let b = Database::open(dir.path().join("b.db")).unwrap();
    assert_eq!(b.resume_sync(&b_keys).unwrap(), SyncStanding::Active);
    for r in records(&a) {
        b.receive_record(&r).unwrap();
    }
    assert!(has(&b, 1));

    // Recovery, with the same injected objects and forged recovery
    // envelopes first.
    let real = recovery_envelopes(&a);
    let mut envelopes: Vec<Vec<u8>> = real.iter().map(|e| forge(e)).collect();
    envelopes.extend(real);
    let r = Database::open(dir.path().join("r.db")).unwrap();
    let recovered = r
        .recover_collection_holding(
            &MemoryKeyStore::default(),
            &words,
            route(),
            &controls,
            &envelopes,
            &records(&a),
        )
        .unwrap();
    assert_eq!(recovered.standing, SyncStanding::Active);
    assert!(recovered.unrecoverable.is_empty());
    for rec in records(&a) {
        r.receive_record(&rec).unwrap();
    }
    assert!(has(&r, 1));
}

#[test]
fn a_new_invitation_replaces_a_missed_one_and_an_enrollment_can_be_cancelled() {
    let dir = tempfile::tempdir().unwrap();
    let a = Database::open(dir.path().join("a.db")).unwrap();
    a.create_collection(&MemoryKeyStore::default(), route())
        .unwrap();
    let b = Database::open(dir.path().join("b.db")).unwrap();
    let b_keys = MemoryKeyStore::default();
    // The first invitation's request never reaches the admin.
    let missed = a.invite().unwrap();
    let first = b.start_enrollment(&b_keys, &missed).unwrap();
    assert_eq!(b.start_enrollment(&b_keys, &missed).unwrap(), first);
    let second = a.invite().unwrap();
    let request = b.start_enrollment(&b_keys, &second).unwrap();
    assert_ne!(request, first, "bound to the new invitation");
    assert_eq!(b.start_enrollment(&b_keys, &second).unwrap(), request);
    assert_eq!(b_keys.names().len(), 1, "the same device keys");
    assert!(a.admit(&first, &second).is_err());
    a.admit(&request, &second).unwrap();
    let me = parse(&request).unwrap().message.signer;
    assert_eq!(
        b.finish_enrollment(
            &b_keys,
            &a.sync_controls().unwrap(),
            &envelopes_for(&a, &me)
        )
        .unwrap(),
        SyncStanding::Active
    );
    // Another collection's invitation is refused while joining, until the
    // enrollment is cancelled.
    let other = Database::open(dir.path().join("o.db")).unwrap();
    other
        .create_collection(
            &MemoryKeyStore::default(),
            Route {
                collection: [9; 16],
                ..route()
            },
        )
        .unwrap();
    let elsewhere = Invitation::parse(&other.invite().unwrap().encode().unwrap()).unwrap();
    let c = Database::open(dir.path().join("c.db")).unwrap();
    let c_keys = MemoryKeyStore::default();
    c.start_enrollment(&c_keys, &a.invite().unwrap()).unwrap();
    assert!(c.start_enrollment(&c_keys, &elsewhere).is_err());
    assert!(c.cancel_enrollment().unwrap());
    assert!(!c.cancel_enrollment().unwrap());
    assert_eq!(c.enrolling().unwrap(), None);
    c.start_enrollment(&c_keys, &elsewhere).unwrap();
    assert_eq!(c.enrolling().unwrap().unwrap().0.collection, [9; 16]);
}

#[test]
fn an_older_handle_is_fenced_once_a_newer_open_takes_over_the_writer() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("a.db");
    let keys = MemoryKeyStore::default();
    let first = Database::open(&path).unwrap();
    first.create_collection(&keys, route()).unwrap();
    first.add_task(&mut Task::new(id(1), "one", 0.0)).unwrap();
    // A second front end (or the command line) opens the same database.
    let second = Database::open(&path).unwrap();
    second.resume_sync(&keys).unwrap();
    let sealed = records(&second).len();
    assert!(matches!(
        first.add_task(&mut Task::new(id(2), "lost?", 1.0)),
        Err(MattersError::Superseded)
    ));
    assert!(!has(&second, 2), "nothing was written");
    assert_eq!(records(&second).len(), sealed, "nothing was sealed");
    second.add_task(&mut Task::new(id(3), "kept", 2.0)).unwrap();
    assert_eq!(records(&second).len(), sealed + 1);
    // Reloading the first handle makes it the writer again.
    drop(first);
    let first = Database::open(&path).unwrap();
    first.resume_sync(&keys).unwrap();
    first
        .add_task(&mut Task::new(id(4), "after reload", 3.0))
        .unwrap();
    let stale = second.add_task(&mut Task::new(id(5), "stale", 4.0));
    assert!(matches!(stale, Err(MattersError::Superseded)), "{stale:?}");
}
