//! This device's membership of a sync collection (#37): keys in a keyring,
//! the control log and envelopes in SQLite, every write sealed while
//! active, and the standing surviving restarts.

use std::process::Command;

use matters_core::storage::membership::{DeviceSeeds, KeyStore, MemoryKeyStore, SyncStanding};
use matters_core::{Database, MattersError, Task};
use matters_sync::authority::{
    self, key_commitment, parse, sign, Authority, Body, DeviceKeys, EpochKey, Membership, Message,
    RecoverySecret, Route,
};
use matters_sync::{verify, wire, ContentKey};

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

fn fresh() -> [u8; 32] {
    let mut bytes = [0; 32];
    bytes[..16].copy_from_slice(uuid::Uuid::new_v4().as_bytes());
    bytes[16..].copy_from_slice(uuid::Uuid::new_v4().as_bytes());
    bytes
}

/// This device's own keys, read back from the keyring as a test may.
fn device_keys(store: &MemoryKeyStore) -> DeviceKeys {
    let names = store.names();
    assert_eq!(names.len(), 1, "one keyring entry");
    store.load(&names[0]).unwrap().unwrap().keys().unwrap()
}

/// The authority as another device sees it, from the stored controls.
fn mirror(db: &Database) -> Authority {
    let controls = db.sync_controls().unwrap();
    let Body::Genesis(g) = parse(&controls[0]).unwrap().message.body else {
        unreachable!()
    };
    let mut a = Authority::bootstrap(&controls[0], route(), &g.root_key).unwrap();
    for c in &controls[1..] {
        match parse(c).unwrap().message.body {
            Body::Membership(_) => {
                let _ = a.apply_membership(c);
            }
            Body::RootRecovery(_) => {
                a.apply_recovery(c).unwrap();
            }
            Body::WriterStart(_) | Body::WriterHandoff(_) => {
                let _ = a.admit_writer(c, db.sync_membership().unwrap().document);
            }
            _ => unreachable!(),
        }
    }
    a
}

/// The current epoch's key, from this device's stored envelope.
fn current_key(db: &Database, a: &Authority, device: &DeviceKeys) -> EpochKey {
    let envelopes = db.sync_envelopes().unwrap();
    let (_, bytes) = envelopes
        .iter()
        .find(|(m, _)| *m == a.manifest())
        .expect("an envelope for the current epoch");
    a.unwrap_key(bytes, device).unwrap()
}

/// The admin's next rotation with `key` as the new epoch's content key,
/// and this device's envelope of it.
fn rotation(a: &Authority, admin: &DeviceKeys, key: &ContentKey) -> (Vec<u8>, Vec<u8>) {
    let epoch = a.epoch().unwrap() + 1;
    let generation = a.generation().unwrap();
    let transition = sign(
        &Message {
            route: route(),
            signer: admin.id(),
            body: Body::Membership(Membership {
                generation,
                sequence: a.sequence().unwrap() + 1,
                previous: a.manifest(),
                roster: a.members().unwrap(),
                admin: admin.id(),
                epoch,
                recovery_key: a.recovery().unwrap(),
                key_commitment: key_commitment(&route(), generation, epoch, key).unwrap(),
            }),
        },
        admin.signing_key(),
        fresh(),
    )
    .unwrap();
    let mut next = a.clone();
    next.apply_membership(&transition).unwrap();
    let envelope = next.wrap_key(admin, admin.id(), key, fresh()).unwrap();
    (transition, envelope)
}

/// Check the newest sealed record: this device's signature, the current
/// epoch and manifest, and that it opens with `key`.
fn newest_record_opens(db: &Database, a: &Authority, device: &DeviceKeys, key: &EpochKey) {
    let records = db.outgoing_records().unwrap();
    let record = records.last().expect("a sealed record");
    let document = db.sync_membership().unwrap().document;
    let parsed = wire::parse(&record.bytes, route().content(&document)).unwrap();
    assert_eq!(parsed.header.epoch, a.epoch().unwrap());
    assert_eq!(parsed.header.manifest, &a.manifest());
    assert_eq!(parsed.header.author, &device.id());
    let verified = verify(parsed, device.verification_key()).unwrap();
    verified.open(key.content_key()).unwrap();
}

fn journal_len(db: &Database) -> usize {
    db.outgoing_updates(0).unwrap().len()
}

#[test]
fn a_new_collection_seals_every_write_and_resumes_after_reopening() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("m.db");
    let store = MemoryKeyStore::default();
    let first_writer = {
        let db = Database::open(&path).unwrap();
        let words = db.create_collection(&store, route()).unwrap();
        assert_eq!(words.split(' ').count(), 24);
        RecoverySecret::from_words(&words).unwrap();
        let device = device_keys(&store);
        let membership = db.sync_membership().unwrap();
        assert_eq!(membership.device, device.id());
        assert_eq!(membership.members, vec![device.id()]);
        assert_eq!(
            (membership.standing, membership.generation, membership.epoch),
            (SyncStanding::Active, Some(0), Some(0))
        );
        db.add_task(&mut Task::new(id(1), "one", 0.0)).unwrap();
        let a = mirror(&db);
        newest_record_opens(&db, &a, &device, &current_key(&db, &a, &device));
        db.writer_incarnation().unwrap()
    };
    let db = Database::open(&path).unwrap();
    // Until the keys are loaded, writes are refused and nothing is journalled.
    let before = journal_len(&db);
    assert!(matches!(
        db.add_task(&mut Task::new(id(2), "two", 1.0)),
        Err(MattersError::Validation(_))
    ));
    assert!(matches!(
        db.resume_sync(&MemoryKeyStore::default()),
        Err(MattersError::Validation(_))
    ));
    assert_eq!(journal_len(&db), before, "missing keys change nothing");
    assert_eq!(db.resume_sync(&store).unwrap(), SyncStanding::Active);
    assert_ne!(
        db.writer_incarnation().unwrap(),
        first_writer,
        "a new writer"
    );
    db.add_task(&mut Task::new(id(2), "two", 1.0)).unwrap();
    let device = device_keys(&store);
    let a = mirror(&db);
    newest_record_opens(&db, &a, &device, &current_key(&db, &a, &device));
    assert_eq!(db.outgoing_records().unwrap().len(), 2);
}

#[test]
fn a_keyring_holding_another_devices_keys_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("m.db");
    let store = MemoryKeyStore::default();
    Database::open(&path)
        .unwrap()
        .create_collection(&store, route())
        .unwrap();
    let name = store.names().remove(0);
    let other = DeviceSeeds::from_bytes(&[7; DeviceSeeds::BYTES]).unwrap();
    store.store(&name, &other).unwrap();
    let db = Database::open(&path).unwrap();
    assert!(matches!(
        db.resume_sync(&store),
        Err(MattersError::Validation(_))
    ));
    assert!(DeviceSeeds::from_bytes(&[0; 63]).is_err());
}

#[test]
fn transitions_commit_with_their_envelope_and_survive_restart() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("m.db");
    let store = MemoryKeyStore::default();
    let db = Database::open(&path).unwrap();
    let words = db.create_collection(&store, route()).unwrap();
    let admin = device_keys(&store);
    let a = mirror(&db);
    let one = ContentKey::new([0x11; 32]);
    let (transition, envelope) = rotation(&a, &admin, &one);
    // Without the new epoch's envelope a continuing member refuses it.
    let controls = db.sync_controls().unwrap().len();
    assert!(matches!(
        db.apply_control(&transition, None),
        Err(MattersError::EnvelopeMissing)
    ));
    assert_eq!(db.sync_controls().unwrap().len(), controls);
    assert_eq!(db.sync_membership().unwrap().epoch, Some(0));
    // A record that is no control, or an envelope for another device.
    assert!(matches!(
        db.apply_control(&db.sync_controls().unwrap()[0], None),
        Err(MattersError::Authority(authority::Error::Unsupported))
    ));
    // A damaged envelope fails its signature: nothing is stored.
    let mut wrong = envelope.clone();
    let at = wrong.len() - 3400; // inside the signed envelope, before the signature
    wrong[at] ^= 1;
    assert!(db.apply_control(&transition, Some(&wrong)).is_err());
    assert_eq!(db.sync_controls().unwrap().len(), controls);
    // With it, the epoch advances and sealing continues under it.
    assert_eq!(
        db.apply_control(&transition, Some(&envelope)).unwrap(),
        SyncStanding::Active
    );
    assert_eq!(db.sync_membership().unwrap().epoch, Some(1));
    assert_eq!(
        db.apply_control(&transition, Some(&envelope)).unwrap(),
        SyncStanding::Active,
        "a replay"
    );
    assert_eq!(db.sync_controls().unwrap().len(), controls + 1);
    db.add_task(&mut Task::new(id(1), "one", 0.0)).unwrap();
    let a = mirror(&db);
    newest_record_opens(&db, &a, &admin, &current_key(&db, &a, &admin));
    drop(db);
    // After a restart the authority replays to the same epoch.
    let db = Database::open(&path).unwrap();
    db.resume_sync(&store).unwrap();
    assert_eq!(db.sync_membership().unwrap().epoch, Some(1));
    db.add_task(&mut Task::new(id(2), "two", 1.0)).unwrap();
    newest_record_opens(&db, &a, &admin, &current_key(&db, &a, &admin));
    // A recovery to a new device removes this one: sealing stops, writes
    // are kept locally, and the standing survives a restart.
    let recovery = RecoverySecret::from_words(&words)
        .unwrap()
        .keys(&route())
        .unwrap();
    let successor = DeviceKeys::generate(fresh(), fresh()).unwrap();
    let recover = a
        .recover(&recovery, &successor, &ContentKey::new([0x22; 32]), fresh())
        .unwrap();
    assert_eq!(
        db.apply_control(&recover, None).unwrap(),
        SyncStanding::Revoked
    );
    let sealed = db.outgoing_records().unwrap().len();
    let journal = journal_len(&db);
    db.add_task(&mut Task::new(id(3), "three", 2.0)).unwrap();
    assert_eq!(
        db.outgoing_records().unwrap().len(),
        sealed,
        "nothing sealed"
    );
    assert_eq!(journal_len(&db), journal + 1, "the edit is kept, unsent");
    drop(db);
    let db = Database::open(&path).unwrap();
    db.add_task(&mut Task::new(id(4), "four", 3.0)).unwrap();
    assert_eq!(db.resume_sync(&store).unwrap(), SyncStanding::Revoked);
    assert_eq!(db.sync_membership().unwrap().generation, Some(1));
}

#[test]
fn a_fork_freezes_sealing_and_its_evidence_survives_restart() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("m.db");
    let store = MemoryKeyStore::default();
    let db = Database::open(&path).unwrap();
    db.create_collection(&store, route()).unwrap();
    let admin = device_keys(&store);
    let a = mirror(&db);
    let (first, envelope) = rotation(&a, &admin, &ContentKey::new([1; 32]));
    let (second, other) = rotation(&a, &admin, &ContentKey::new([2; 32]));
    db.apply_control(&first, Some(&envelope)).unwrap();
    assert!(matches!(
        db.apply_control(&second, Some(&other)),
        Err(MattersError::Authority(authority::Error::Fork))
    ));
    assert_eq!(db.sync_membership().unwrap().standing, SyncStanding::Frozen);
    let sealed = db.outgoing_records().unwrap().len();
    db.add_task(&mut Task::new(id(1), "one", 0.0)).unwrap();
    assert_eq!(db.outgoing_records().unwrap().len(), sealed);
    drop(db);
    let db = Database::open(&path).unwrap();
    assert_eq!(db.resume_sync(&store).unwrap(), SyncStanding::Frozen);
    let transitions = db
        .sync_controls()
        .unwrap()
        .iter()
        .filter(|c| matches!(parse(c).unwrap().message.body, Body::Membership(_)))
        .count();
    assert_eq!(transitions, 2, "both successors kept");
}

/// Child process for the crash test: create a collection and crash just
/// before it commits. Ignored in ordinary runs.
#[test]
#[ignore]
fn collection_crash_worker() {
    let Ok(path) = std::env::var("MATTERS_CRASH_DB") else {
        return;
    };
    let db = Database::open(&path).unwrap();
    db.create_collection(&MemoryKeyStore::default(), route())
        .unwrap();
    unreachable!("the crash point exits the process");
}

#[test]
fn a_crash_before_the_collection_commits_leaves_sync_off_and_retryable() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("m.db");
    drop(Database::open(&path).unwrap());
    let status = Command::new(std::env::current_exe().unwrap())
        .args([
            "collection_crash_worker",
            "--exact",
            "--ignored",
            "--nocapture",
        ])
        .env("MATTERS_CRASH_DB", &path)
        .env("MATTERS_CRASH_AT", "collection-before-commit")
        .status()
        .unwrap();
    assert_eq!(status.code(), Some(86));
    let db = Database::open(&path).unwrap();
    assert!(!db.sync_enabled());
    assert!(db.sync_controls().unwrap().is_empty());
    // Nothing half-made blocks a retry on the same database.
    let store = MemoryKeyStore::default();
    db.create_collection(&store, route()).unwrap();
    assert!(db.sync_enabled());
    db.add_task(&mut Task::new(id(1), "one", 0.0)).unwrap();
    assert_eq!(db.outgoing_records().unwrap().len(), 1);
}

#[test]
fn seeds_never_print() {
    let seeds = DeviceSeeds::from_bytes(&[0xab; DeviceSeeds::BYTES]).unwrap();
    assert_eq!(format!("{seeds:?}"), "DeviceSeeds(redacted)");
}
