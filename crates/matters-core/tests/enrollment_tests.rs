//! Two devices forming a collection (#37): the admin invites, a new device
//! asks to join and finishes with the admin's transition and envelopes,
//! and the admin removes and rotates. What the admin signs for others is
//! kept and resent byte for byte.

use std::process::Command;

use matters_core::storage::membership::{KeyStore, MemoryKeyStore, SyncStanding};
use matters_core::{Database, MattersError, Task};
use matters_sync::authority::{parse, Authority, Body, DeviceKeys, Hash, Invitation, Route};
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

fn keys(store: &MemoryKeyStore) -> DeviceKeys {
    let names = store.names();
    assert_eq!(names.len(), 1);
    store.load(&names[0]).unwrap().unwrap().keys().unwrap()
}

/// The authority as any device sees it, from a database's controls.
fn mirror(db: &Database) -> Authority {
    let controls = db.sync_controls().unwrap();
    let Body::Genesis(g) = parse(&controls[0]).unwrap().message.body else {
        unreachable!()
    };
    let mut a = Authority::bootstrap(&controls[0], route(), &g.root_key).unwrap();
    for c in &controls[1..] {
        let _ = a.apply_membership(c);
    }
    a
}

/// The content key of the epoch `manifest` names, from `db`'s own
/// envelope of it.
fn key_of(db: &Database, a: &Authority, device: &DeviceKeys, manifest: Hash) -> ContentKey {
    let (_, envelope) = db
        .sync_envelopes()
        .unwrap()
        .into_iter()
        .find(|(m, _)| *m == manifest)
        .expect("an envelope for that epoch");
    a.unwrap_history_key(&envelope, device)
        .unwrap()
        .into_content_key()
}

/// Whether `db`'s newest sealed record opens with `key`, signed by `author`.
fn newest_opens(db: &Database, author: &DeviceKeys, key: &ContentKey) -> bool {
    let records = db.outgoing_records().unwrap();
    let document = db.sync_membership().unwrap().document;
    let parsed = wire::parse(&records.last().unwrap().bytes, route().content(&document)).unwrap();
    let verified = verify(parsed, author.verification_key()).unwrap();
    verified.open(key).is_ok()
}

/// The outgoing envelopes addressed to `device`.
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

fn requester(request: &[u8]) -> Hash {
    parse(request).unwrap().message.signer
}

struct Pair {
    _dir: tempfile::TempDir,
    a: Database,
    a_keys: MemoryKeyStore,
    b: Database,
    b_keys: MemoryKeyStore,
    b_path: std::path::PathBuf,
}

/// A, with one record sealed at epoch 0, and B joined through a scanned
/// invitation.
fn joined() -> Pair {
    let dir = tempfile::tempdir().unwrap();
    let a = Database::open(dir.path().join("a.db")).unwrap();
    let a_keys = MemoryKeyStore::default();
    a.create_collection(&a_keys, route()).unwrap();
    a.add_task(&mut Task::new(id(1), "from A", 0.0)).unwrap();
    let b_path = dir.path().join("b.db");
    let b = Database::open(&b_path).unwrap();
    let b_keys = MemoryKeyStore::default();
    let shown = a.invite().unwrap();
    let scanned = Invitation::parse(&shown.encode().unwrap()).unwrap();
    let request = b.start_enrollment(&b_keys, &scanned).unwrap();
    assert_eq!(
        b.start_enrollment(&b_keys, &scanned).unwrap(),
        request,
        "asking again returns the same request"
    );
    a.admit(&request, &shown).unwrap();
    let standing = b
        .finish_enrollment(
            &b_keys,
            &a.sync_controls().unwrap(),
            &envelopes_for(&a, &requester(&request)),
        )
        .unwrap();
    assert_eq!(standing, SyncStanding::Active);
    Pair {
        _dir: dir,
        a,
        a_keys,
        b,
        b_keys,
        b_path,
    }
}

#[test]
fn a_second_device_joins_and_both_seal_under_the_new_epoch_with_history() {
    let p = joined();
    let (ma, mb) = (
        p.a.sync_membership().unwrap(),
        p.b.sync_membership().unwrap(),
    );
    assert_eq!((ma.epoch, mb.epoch), (Some(1), Some(1)));
    assert_eq!(ma.manifest, mb.manifest);
    assert_eq!(ma.members, mb.members);
    assert_eq!(ma.members.len(), 2);
    assert_eq!(ma.document, mb.document, "one document for the collection");
    // The admin published the transition, B's envelope, the recovery
    // envelope and B's history for epoch 0.
    let outgoing = p.a.outgoing_controls().unwrap();
    assert_eq!(outgoing.len(), 4);
    assert!(matches!(
        parse(&outgoing[0].bytes).unwrap().message.body,
        Body::Membership(_)
    ));
    // Both seal under epoch 1 with the same key.
    let (ka, kb) = (keys(&p.a_keys), keys(&p.b_keys));
    let a = mirror(&p.a);
    let current = key_of(&p.a, &a, &ka, a.manifest());
    p.a.add_task(&mut Task::new(id(2), "A again", 1.0)).unwrap();
    p.b.add_task(&mut Task::new(id(3), "from B", 2.0)).unwrap();
    assert!(newest_opens(&p.a, &ka, &current));
    assert!(newest_opens(&p.b, &kb, &current));
    // B holds epoch 0's key too: it can read what A wrote before it joined.
    let first = p.a.outgoing_records().unwrap()[0].clone();
    assert_eq!(
        p.b.sync_envelopes().unwrap().len(),
        2,
        "current and retained"
    );
    // Epoch 0's manifest is the one the admission transition names.
    let Body::Membership(admission) = parse(&p.a.sync_controls().unwrap()[1])
        .unwrap()
        .message
        .body
    else {
        unreachable!()
    };
    let old = key_of(&p.b, &mirror(&p.b), &kb, admission.previous);
    let parsed = wire::parse(&first.bytes, route().content(&ma.document)).unwrap();
    verify(parsed, ka.verification_key())
        .unwrap()
        .open(&old)
        .unwrap();
    // Acknowledged controls leave the queue; the rest stay byte for byte.
    p.a.acknowledge_control(outgoing[0].seq).unwrap();
    assert_eq!(p.a.outgoing_controls().unwrap(), outgoing[1..]);
    assert!(p.a.acknowledge_control(9999).is_err());
    // B resumes after a restart.
    drop(p.b);
    let b = Database::open(&p.b_path).unwrap();
    assert_eq!(b.resume_sync(&p.b_keys).unwrap(), SyncStanding::Active);
    assert_eq!(b.sync_membership().unwrap().epoch, Some(1));
}

#[test]
fn removal_and_rotation_reach_the_members_and_only_the_admin_makes_them() {
    let p = joined();
    let b_id = keys(&p.b_keys).id();
    // B is a member, not the admin.
    for refused in [
        p.b.invite().map(|_| ()),
        p.b.rotate_epoch(),
        p.b.remove_member(&keys(&p.a_keys).id()),
    ] {
        assert!(matches!(refused, Err(MattersError::Validation(_))));
    }
    assert!(matches!(
        p.a.remove_member(&keys(&p.a_keys).id()),
        Err(MattersError::Validation(_))
    ));
    assert!(matches!(
        p.a.remove_member(&[9; 48]),
        Err(MattersError::NotFound { id, .. }) if id == "09".repeat(48)
    ));
    // A rotation reaches B with its envelope.
    let before = p.a.outgoing_controls().unwrap().len();
    p.a.rotate_epoch().unwrap();
    let outgoing = p.a.outgoing_controls().unwrap();
    assert_eq!(
        outgoing.len(),
        before + 3,
        "transition, B's envelope, recovery"
    );
    let rotation = &outgoing[before].bytes;
    let mine = envelopes_for(&p.a, &b_id).pop().unwrap();
    assert_eq!(
        p.b.apply_control(rotation, Some(&mine)).unwrap(),
        SyncStanding::Active
    );
    assert_eq!(p.b.sync_membership().unwrap().epoch, Some(2));
    // A removal gives B no envelope, and B stops sealing.
    p.a.remove_member(&b_id).unwrap();
    let outgoing = p.a.outgoing_controls().unwrap();
    let removal = &outgoing[before + 3].bytes;
    assert_eq!(outgoing.len(), before + 5, "transition and recovery only");
    assert_eq!(envelopes_for(&p.a, &b_id).len(), 3, "nothing new for B");
    assert_eq!(
        p.b.apply_control(removal, None).unwrap(),
        SyncStanding::Revoked
    );
    assert_eq!(p.a.sync_membership().unwrap().members.len(), 1);
}

#[test]
fn finishing_waits_for_the_admins_transition_and_a_current_envelope() {
    let dir = tempfile::tempdir().unwrap();
    let a = Database::open(dir.path().join("a.db")).unwrap();
    a.create_collection(&MemoryKeyStore::default(), route())
        .unwrap();
    let b = Database::open(dir.path().join("b.db")).unwrap();
    let b_keys = MemoryKeyStore::default();
    assert!(
        b.finish_enrollment(&b_keys, &[], &[]).is_err(),
        "not started"
    );
    let shown = a.invite().unwrap();
    let request = b.start_enrollment(&b_keys, &shown).unwrap();
    let genesis_only = a.sync_controls().unwrap();
    // Refused for not being added yet, before envelopes are considered.
    assert!(matches!(
        b.finish_enrollment(&b_keys, &genesis_only, &[]),
        Err(MattersError::Validation(m)) if m.contains("not added this device")
    ));
    a.admit(&request, &shown).unwrap();
    let controls = a.sync_controls().unwrap();
    let mine = envelopes_for(&a, &requester(&request));
    let history: Vec<_> = mine
        .iter()
        .filter(|e| match parse(e).unwrap().message.body {
            Body::KeyEnvelope(k) => k.manifest != mirror(&a).manifest(),
            _ => false,
        })
        .cloned()
        .collect();
    assert!(matches!(
        b.finish_enrollment(&b_keys, &controls, &history),
        Err(MattersError::Validation(_))
    ));
    assert!(!b.sync_enabled(), "nothing half-joined");
    // While joining, an invitation differing in any one of application,
    // tenant, collection or root is another collection's, and refused.
    for field in 0..4 {
        let mut other = Invitation::parse(&shown.encode().unwrap()).unwrap();
        match field {
            0 => other.route.application = [9; 16],
            1 => other.route.tenant = [9; 16],
            2 => other.route.collection = [9; 16],
            _ => other.root = [9; 48],
        }
        assert!(
            matches!(
                b.start_enrollment(&b_keys, &other),
                Err(MattersError::Validation(m)) if m.contains("another collection")
            ),
            "field {field}"
        );
    }
    // Another collection's invitation while joining is refused.
    let other = Database::open(dir.path().join("c.db")).unwrap();
    let mut elsewhere = route();
    elsewhere.collection = [9; 16];
    other
        .create_collection(&MemoryKeyStore::default(), elsewhere)
        .unwrap();
    assert!(b
        .start_enrollment(&b_keys, &other.invite().unwrap())
        .is_err());
    assert_eq!(
        b.finish_enrollment(&b_keys, &controls, &mine).unwrap(),
        SyncStanding::Active
    );
    assert!(
        b.start_enrollment(&b_keys, &shown).is_err(),
        "already syncing"
    );
}

/// Child process for the crash test: resume the admin and admit the
/// request in MATTERS_REQUEST under MATTERS_INVITATION, crashing before
/// the commit. Ignored in ordinary runs.
#[test]
#[ignore]
fn admit_crash_worker() {
    let Ok(path) = std::env::var("MATTERS_CRASH_DB") else {
        return;
    };
    let unhex = |s: String| -> Vec<u8> {
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
            .collect()
    };
    let store = MemoryKeyStore::default();
    let seeds = unhex(std::env::var("MATTERS_SEEDS").unwrap());
    store
        .store(
            &std::env::var("MATTERS_KEY_NAME").unwrap(),
            &matters_core::storage::membership::DeviceSeeds::from_bytes(&seeds).unwrap(),
        )
        .unwrap();
    let db = Database::open(&path).unwrap();
    db.resume_sync(&store).unwrap();
    let invitation =
        Invitation::parse(&unhex(std::env::var("MATTERS_INVITATION").unwrap())).unwrap();
    db.admit(
        &unhex(std::env::var("MATTERS_REQUEST").unwrap()),
        &invitation,
    )
    .unwrap();
    unreachable!("the crash point exits the process");
}

#[test]
fn a_crash_before_an_admission_commits_publishes_nothing_and_can_be_redone() {
    let dir = tempfile::tempdir().unwrap();
    let a_path = dir.path().join("a.db");
    let a_keys = MemoryKeyStore::default();
    let (invitation, shown) = {
        let a = Database::open(&a_path).unwrap();
        a.create_collection(&a_keys, route()).unwrap();
        let shown = a.invite().unwrap();
        (shown.encode().unwrap(), shown)
    };
    let b = Database::open(dir.path().join("b.db")).unwrap();
    let b_keys = MemoryKeyStore::default();
    let request = b.start_enrollment(&b_keys, &shown).unwrap();
    let name = a_keys.names().remove(0);
    let hex = |b: &[u8]| b.iter().map(|x| format!("{x:02x}")).collect::<String>();
    let seeds = a_keys.load(&name).unwrap().unwrap().to_bytes();
    let status = Command::new(std::env::current_exe().unwrap())
        .args(["admit_crash_worker", "--exact", "--ignored", "--nocapture"])
        .env("MATTERS_CRASH_DB", &a_path)
        .env("MATTERS_CRASH_AT", "membership-before-commit")
        .env("MATTERS_SEEDS", hex(&seeds))
        .env("MATTERS_KEY_NAME", &name)
        .env("MATTERS_INVITATION", hex(&invitation))
        .env("MATTERS_REQUEST", hex(&request))
        .status()
        .unwrap();
    assert_eq!(status.code(), Some(86));
    let a = Database::open(&a_path).unwrap();
    a.resume_sync(&a_keys).unwrap();
    assert_eq!(
        a.sync_membership().unwrap().epoch,
        Some(0),
        "nothing applied"
    );
    assert!(
        a.outgoing_controls().unwrap().is_empty(),
        "nothing to publish"
    );
    a.admit(&request, &shown).unwrap();
    assert_eq!(
        b.finish_enrollment(
            &b_keys,
            &a.sync_controls().unwrap(),
            &envelopes_for(&a, &requester(&request))
        )
        .unwrap(),
        SyncStanding::Active
    );
}
