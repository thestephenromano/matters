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
    // B's removal: it stops.
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
