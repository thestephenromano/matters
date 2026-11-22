//! Matters' database as a sync client's local store (#39), driven through
//! the `LocalStore` trait exactly as the client drives it. The tests cover:
//! - identity and session signatures;
//! - both outgoing queues and their acknowledgements;
//! - how each kind of received control maps to an outcome;
//! - records;
//! - cursors;
//! - a relay restore.

use matters_core::storage::membership::{KeyStore, MemoryKeyStore, SyncStanding};
use matters_core::{Database, Task};
use matters_sync::authority::{parse, verify_session, Body, DeviceKeys, Route};
use matters_sync::record::record_hash;
use matters_sync::store::{ControlOutcome, LocalStore, Log};

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

struct Pair {
    _dir: tempfile::TempDir,
    a: Database,
    b: Database,
    b_keys: MemoryKeyStore,
}

/// A with a task, and B joined out of band.
fn pair() -> Pair {
    let dir = tempfile::tempdir().unwrap();
    let a = Database::open(dir.path().join("a.db")).unwrap();
    a.create_collection(&MemoryKeyStore::default(), route())
        .unwrap();
    a.add_task(&mut Task::new(id(1), "from A", 0.0)).unwrap();
    let b = Database::open(dir.path().join("b.db")).unwrap();
    let b_keys = MemoryKeyStore::default();
    let shown = a.invite().unwrap();
    let request = b.start_enrollment(&b_keys, &shown).unwrap();
    let b_id = parse(&request).unwrap().message.signer;
    a.admit(&request, &shown).unwrap();
    let envelopes: Vec<Vec<u8>> = a
        .outgoing_controls()
        .unwrap()
        .into_iter()
        .filter(|o| {
            matches!(parse(&o.bytes).unwrap().message.body,
                Body::KeyEnvelope(ref k) if k.recipient == b_id)
        })
        .map(|o| o.bytes)
        .collect();
    assert_eq!(
        b.finish_enrollment(&b_keys, &a.sync_controls().unwrap(), &envelopes)
            .unwrap(),
        SyncStanding::Active
    );
    Pair {
        _dir: dir,
        a,
        b,
        b_keys,
    }
}

/// The store as the client sees it.
fn store(db: &Database) -> &dyn LocalStore {
    db
}

#[test]
fn identity_and_session_signatures_are_the_enrolled_devices() {
    let p = pair();
    let b = store(&p.b);
    let device = keys(&p.b_keys);
    assert_eq!(b.route().unwrap(), route());
    assert_eq!(b.device().unwrap(), device.id());
    let challenge = [7; 32];
    let signature = b.sign_session(&challenge).unwrap();
    verify_session(
        device.verification_key(),
        &route(),
        &device.id(),
        &challenge,
        &signature,
    )
    .unwrap();
    assert!(verify_session(
        device.verification_key(),
        &route(),
        &device.id(),
        &[8; 32],
        &signature
    )
    .is_err());
    // A database that never joined has no identity to sync as.
    let dir = tempfile::tempdir().unwrap();
    let alone = Database::open(dir.path().join("alone.db")).unwrap();
    assert!(store(&alone).route().is_err());
    assert!(store(&alone).device().is_err());
    assert!(store(&alone).sign_session(&challenge).is_err());
}

#[test]
fn both_queues_drain_only_on_acknowledgement() {
    let p = pair();
    let a = store(&p.a);
    let controls = a.outgoing_controls().unwrap();
    assert!(controls.len() >= 3, "genesis, writer, B's admission");
    a.acknowledge_control(controls[0].0).unwrap();
    assert_eq!(a.outgoing_controls().unwrap().len(), controls.len() - 1);
    let records = a.outgoing_records().unwrap();
    assert_eq!(records.len(), 1);
    // Asked again before acknowledgement: the same bytes.
    assert_eq!(a.outgoing_records().unwrap(), records);
    a.acknowledge_record(&records[0].0).unwrap();
    assert!(a.outgoing_records().unwrap().is_empty());
    assert!(
        a.acknowledge_record(&[0; 32]).is_err(),
        "not a record of A's"
    );
}

#[test]
fn received_controls_map_to_what_the_client_does_next() {
    let p = pair();
    let b = store(&p.b);
    let controls = p.a.sync_controls().unwrap();
    // The genesis and everything B already holds, replayed: done.
    for c in &controls {
        assert_eq!(b.receive_control(c, None).unwrap(), ControlOutcome::Done);
    }
    // Garbage, and a control from another collection: refused, skipped.
    assert_eq!(
        b.receive_control(&[0xff; 40], None).unwrap(),
        ControlOutcome::Refused
    );
    // A rotation that keeps B: it needs B's envelope, then applies.
    p.a.rotate_epoch().unwrap();
    let outgoing = p.a.outgoing_controls().unwrap();
    let b_id = keys(&p.b_keys).id();
    let (transition, envelope) = outgoing
        .iter()
        .rev()
        .find_map(|o| match parse(&o.bytes).unwrap().message.body {
            Body::Membership(_) => {
                let manifest = record_hash(&o.bytes);
                let envelope =
                    outgoing
                        .iter()
                        .find_map(|e| match parse(&e.bytes).unwrap().message.body {
                            Body::KeyEnvelope(k)
                                if k.recipient == b_id && k.manifest == manifest =>
                            {
                                Some(e.bytes.clone())
                            }
                            _ => None,
                        })?;
                Some((o.bytes.clone(), envelope))
            }
            _ => None,
        })
        .unwrap();
    assert_eq!(
        b.receive_control(&transition, None).unwrap(),
        ControlOutcome::NeedsEnvelope
    );
    assert_eq!(
        b.receive_control(&transition, Some(&envelope)).unwrap(),
        ControlOutcome::Done
    );
    // B's removal: it stops, and its own authority no longer holds it a
    // member.
    assert!(b.is_current_member().unwrap());
    p.a.remove_member(&b_id).unwrap();
    let removal =
        p.a.outgoing_controls()
            .unwrap()
            .into_iter()
            .rev()
            .find(|o| matches!(parse(&o.bytes).unwrap().message.body, Body::Membership(_)))
            .unwrap();
    assert_eq!(
        b.receive_control(&removal.bytes, None).unwrap(),
        ControlOutcome::Stopped
    );
    assert!(!b.is_current_member().unwrap());
    assert!(store(&p.a).is_current_member().unwrap());
}

#[test]
fn records_apply_through_the_store_and_forgeries_are_errors() {
    let p = pair();
    let b = store(&p.b);
    let records = store(&p.a).outgoing_records().unwrap();
    // A signature broken in transit, before B ever saw the record.
    let mut forged = records[0].1.clone();
    let last = forged.len() - 1;
    forged[last] ^= 1;
    assert!(b.receive_record(&forged).is_err());
    assert!(p.b.pending_records().unwrap().is_empty(), "not kept");
    b.receive_record(&records[0].1).unwrap();
    assert_eq!(p.b.get_task(&id(1)).unwrap().unwrap().title, "from A");
    // Again: a duplicate by its ID, which is fine.
    b.receive_record(&records[0].1).unwrap();
}

#[test]
fn cursors_are_per_log_and_a_relay_restore_requeues_everything() {
    let p = pair();
    let a = store(&p.a);
    assert_eq!(a.cursor(Log::Authority).unwrap(), None);
    a.save_cursor(Log::Authority, b"authority-1").unwrap();
    a.save_cursor(Log::Records, b"records-1").unwrap();
    a.save_cursor(Log::Records, b"records-2").unwrap();
    assert_eq!(
        a.cursor(Log::Authority).unwrap().as_deref(),
        Some(&b"authority-1"[..])
    );
    assert_eq!(
        a.cursor(Log::Records).unwrap().as_deref(),
        Some(&b"records-2"[..])
    );
    let controls = a.outgoing_controls().unwrap();
    for (seq, _) in &controls {
        a.acknowledge_control(*seq).unwrap();
    }
    let records = a.outgoing_records().unwrap();
    for (id, _) in &records {
        a.acknowledge_record(id).unwrap();
    }
    a.generation_changed().unwrap();
    assert_eq!(a.cursor(Log::Authority).unwrap(), None);
    assert_eq!(a.cursor(Log::Records).unwrap(), None);
    assert_eq!(a.outgoing_controls().unwrap(), controls);
    assert_eq!(a.outgoing_records().unwrap(), records);
}

#[test]
fn a_joining_device_signs_in_with_its_keys_and_finishes_once_admitted() {
    use matters_core::storage::Enrolling;
    use matters_sync::store::JoiningStore;
    let dir = tempfile::tempdir().unwrap();
    let a = Database::open(dir.path().join("a.db")).unwrap();
    a.create_collection(&MemoryKeyStore::default(), route())
        .unwrap();
    let b = std::sync::Arc::new(Database::open(dir.path().join("b.db")).unwrap());
    assert_eq!(b.enrolling().unwrap(), None);
    let keys = std::sync::Arc::new(MemoryKeyStore::default());
    let joining = Enrolling {
        db: b.clone(),
        keys: keys.clone(),
    };
    assert!(joining.route().is_err(), "not joining yet");
    let invitation = a.invite().unwrap();
    let request = b.start_enrollment(keys.as_ref(), &invitation).unwrap();
    let device = parse(&request).unwrap().message.signer;
    assert_eq!(joining.route().unwrap(), route());
    assert_eq!(joining.device().unwrap(), device);
    assert_eq!(b.enrolling().unwrap(), Some((route(), device)));
    // It signs with the key it enrolled.
    let signature = joining.sign_session(&[5; 32]).unwrap();
    let enrolled = keys
        .load(&keys.names()[0])
        .unwrap()
        .unwrap()
        .keys()
        .unwrap();
    verify_session(
        enrolled.verification_key(),
        &route(),
        &device,
        &[5; 32],
        &signature,
    )
    .unwrap();
    // Before the admission: not yet. After it: joined. Anything else wrong
    // is an error, not a wait.
    assert!(!joining.finish(&a.sync_controls().unwrap(), &[]).unwrap());
    assert!(joining.finish(&[], &[]).is_err(), "no genesis");
    a.admit(&request, &invitation).unwrap();
    let envelopes: Vec<Vec<u8>> = a
        .outgoing_controls()
        .unwrap()
        .into_iter()
        .filter(|o| {
            matches!(parse(&o.bytes).unwrap().message.body,
                Body::KeyEnvelope(ref k) if k.recipient == device)
        })
        .map(|o| o.bytes)
        .collect();
    // The transition without this device's envelope: not yet, either.
    assert!(!joining.finish(&a.sync_controls().unwrap(), &[]).unwrap());
    assert!(joining
        .finish(&a.sync_controls().unwrap(), &envelopes)
        .unwrap());
    assert!(b.sync_enabled());
    assert_eq!(b.enrolling().unwrap(), None, "finished");
    // Keys missing from the keyring are an error, not a silent wait.
    let c = std::sync::Arc::new(Database::open(dir.path().join("c.db")).unwrap());
    c.start_enrollment(&MemoryKeyStore::default(), &a.invite().unwrap())
        .unwrap();
    assert!(c
        .sign_enrolling_session(&MemoryKeyStore::default(), &[1; 32])
        .is_err());
}

#[test]
fn a_recovering_device_signs_in_as_the_root_and_recovers() {
    use matters_core::storage::Recovering;
    use matters_sync::store::RecoveringStore;
    let dir = tempfile::tempdir().unwrap();
    let a = Database::open(dir.path().join("a.db")).unwrap();
    let words = a
        .create_collection(&MemoryKeyStore::default(), route())
        .unwrap();
    a.add_task(&mut Task::new(id(1), "kept", 0.0)).unwrap();
    let r = std::sync::Arc::new(Database::open(dir.path().join("r.db")).unwrap());
    let recovering = Recovering {
        db: r.clone(),
        keys: MemoryKeyStore::default(),
        words: words.clone().into(),
        route: route(),
    };
    assert_eq!(recovering.route().unwrap(), route());
    let root = recovering.root().unwrap();
    let genesis = a.sync_controls().unwrap().remove(0);
    let Body::Genesis(g) = parse(&genesis).unwrap().message.body else {
        unreachable!()
    };
    assert_eq!(root, matters_sync::authority::root_id(&g.root_key).unwrap());
    let signature = recovering.sign_root_session(&[6; 32]).unwrap();
    verify_session(&g.root_key, &route(), &root, &[6; 32], &signature).unwrap();
    // The authority log as the relay holds it: controls, and every
    // envelope; only the recovery ones are used.
    let mut controls = Vec::new();
    let mut envelopes = Vec::new();
    for c in a.sync_controls().unwrap() {
        controls.push(c);
    }
    for o in a.outgoing_controls().unwrap() {
        if matches!(parse(&o.bytes).unwrap().message.body, Body::KeyEnvelope(_)) {
            envelopes.push(o.bytes);
        }
    }
    for (_, e) in a.sync_envelopes().unwrap() {
        envelopes.push(e);
    }
    recovering.recover(&controls, &envelopes).unwrap();
    assert!(r.sync_enabled());
    let outgoing = recovering.outgoing().unwrap();
    assert!(outgoing
        .iter()
        .any(|(_, b)| matches!(parse(b).unwrap().message.body, Body::RootRecovery(_))));
    for (seq, _) in &outgoing {
        recovering.acknowledge(*seq).unwrap();
    }
    assert!(recovering.outgoing().unwrap().is_empty());
    for record in a.outgoing_records().unwrap() {
        r.receive_record(&record.bytes).unwrap();
    }
    assert_eq!(r.get_task(&id(1)).unwrap().unwrap().title, "kept");
    // Other words are another root.
    let wrong = Recovering {
        db: r.clone(),
        keys: MemoryKeyStore::default(),
        words: matters_sync::authority::RecoverySecret::new([3; 32])
            .words()
            .unwrap()
            .into(),
        route: route(),
    };
    assert_ne!(wrong.root().unwrap(), root);
    assert!(Recovering {
        db: r,
        keys: MemoryKeyStore::default(),
        words: String::from("not words").into(),
        route: route(),
    }
    .root()
    .is_err());
}

#[test]
fn a_shared_key_store_is_the_store_it_shares() {
    use matters_core::storage::membership::DeviceSeeds;
    let inner = std::sync::Arc::new(MemoryKeyStore::default());
    let shared: std::sync::Arc<MemoryKeyStore> = inner.clone();
    let seeds = DeviceSeeds::from_bytes(&[7; DeviceSeeds::BYTES]).unwrap();
    KeyStore::store(&shared, "device", &seeds).unwrap();
    assert_eq!(inner.names(), ["device"], "stored in the one store");
    let loaded = KeyStore::load(&shared, "device").unwrap().unwrap();
    assert_eq!(loaded.to_bytes(), seeds.to_bytes());
    assert!(KeyStore::load(&shared, "other").unwrap().is_none());
}

#[test]
fn a_checkpoint_is_made_through_the_store_only_when_due() {
    use matters_sync::checkpoint::Cursor;
    let p = pair();
    let a = store(&p.a);
    for (seq, _) in a.outgoing_controls().unwrap() {
        a.acknowledge_control(seq).unwrap();
    }
    for (id, _) in a.outgoing_records().unwrap() {
        a.acknowledge_record(&id).unwrap();
    }
    let at = || Cursor {
        generation: [1; 16],
        sequence: 1,
    };
    // Not due: one update, and a checkpoint every hundred.
    assert!(!a.make_checkpoint(at(), 100).unwrap());
    assert!(p.a.outgoing_checkpoint_parts().unwrap().is_empty());
    // Due: made, and its parts are outgoing records.
    assert!(a.make_checkpoint(at(), 1).unwrap());
    let parts = p.a.outgoing_checkpoint_parts().unwrap();
    assert_eq!(parts.len(), 1);
    assert!(a
        .outgoing_records()
        .unwrap()
        .iter()
        .any(|(id, _)| *id == parts[0].0));
    // Made just now: not due again.
    assert!(!a.make_checkpoint(at(), 1).unwrap());
    assert!(!a.wants_checkpoint().unwrap(), "it holds the collection");
}
