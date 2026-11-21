//! Receiving other devices' sealed records (#39): two joined databases
//! exchange their controls and records as a relay would deliver them, in
//! order and out of it, with duplicates, forgeries, a writer's close and a
//! cloned device.

use matters_core::storage::membership::{KeyStore, MemoryKeyStore, SyncStanding};
use matters_core::storage::receive::{Received, Waiting};
use matters_core::{Database, MattersError, Task};
use matters_sync::authority::{parse, Body, DeviceKeys, Hash};
use matters_sync::record::record_hash;
use matters_sync::{seal, wire, ContentKey};

fn id(n: u128) -> String {
    uuid::Uuid::from_u128(n).to_string()
}

fn route() -> matters_sync::authority::Route {
    matters_sync::authority::Route {
        application: [1; 16],
        tenant: [2; 16],
        collection: [3; 16],
    }
}

fn keys(store: &MemoryKeyStore) -> DeviceKeys {
    let names = store.names();
    store.load(&names[0]).unwrap().unwrap().keys().unwrap()
}

fn fresh() -> [u8; 32] {
    let mut b = [0; 32];
    b[..16].copy_from_slice(uuid::Uuid::new_v4().as_bytes());
    b[16..].copy_from_slice(uuid::Uuid::new_v4().as_bytes());
    b
}

/// Deliver everything `from` published to `to`, as a relay would: in
/// order, with `to`'s envelope beside each transition.
fn deliver_controls(from: &Database, to: &Database, to_id: &Hash) {
    let outgoing = from.outgoing_controls().unwrap();
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
        let _ = to.apply_control(&o.bytes, envelope.as_deref());
    }
}

fn records(db: &Database) -> Vec<Vec<u8>> {
    db.outgoing_records()
        .unwrap()
        .into_iter()
        .map(|r| r.bytes)
        .collect()
}

/// The titles of the tests' tasks this database holds, sorted.
fn titles(db: &Database) -> Vec<String> {
    let mut t: Vec<String> = [1, 2, 3, 9]
        .into_iter()
        .filter_map(|n| db.get_task(&id(n)).unwrap())
        .map(|t| t.title)
        .collect();
    t.sort();
    t
}

struct Pair {
    dir: tempfile::TempDir,
    a: Database,
    a_keys: MemoryKeyStore,
    b: Database,
    b_keys: MemoryKeyStore,
}

/// A with two tasks sealed at epoch 0, then B joined.
fn pair() -> Pair {
    let dir = tempfile::tempdir().unwrap();
    let a = Database::open(dir.path().join("a.db")).unwrap();
    let a_keys = MemoryKeyStore::default();
    a.create_collection(&a_keys, route()).unwrap();
    a.add_task(&mut Task::new(id(1), "from A, one", 0.0))
        .unwrap();
    a.add_task(&mut Task::new(id(2), "from A, two", 1.0))
        .unwrap();
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
        dir,
        a,
        a_keys,
        b,
        b_keys,
    }
}

#[test]
fn each_device_receives_the_others_records_and_both_converge() {
    let p = pair();
    // B reads what A wrote before it joined, under the retained epoch 0 key.
    for r in records(&p.a) {
        assert_eq!(p.b.receive_record(&r).unwrap(), Received::Applied);
    }
    assert_eq!(titles(&p.b), ["from A, one", "from A, two"]);
    // B writes; A admits B's writer, then receives B's record.
    p.b.add_task(&mut Task::new(id(3), "from B", 2.0)).unwrap();
    deliver_controls(&p.b, &p.a, &keys(&p.a_keys).id());
    for r in records(&p.b) {
        assert_eq!(p.a.receive_record(&r).unwrap(), Received::Applied);
    }
    assert_eq!(titles(&p.a), titles(&p.b));
    assert_eq!(titles(&p.a).len(), 3);
    // Again, or a device's own record coming back: nothing changes.
    for r in records(&p.a).into_iter().chain(records(&p.b)) {
        assert_eq!(p.b.receive_record(&r).unwrap(), Received::Duplicate);
    }
    assert!(p.b.pending_records().unwrap().is_empty());
}

#[test]
fn out_of_order_records_wait_across_a_restart_and_then_apply_in_chain_order() {
    let p = pair();
    p.a.add_task(&mut Task::new(id(9), "from A, three", 2.0))
        .unwrap();
    let sent = records(&p.a);
    assert_eq!(sent.len(), 3);
    // The last, then the middle: both wait for their predecessors.
    for r in [&sent[2], &sent[1]] {
        assert_eq!(
            p.b.receive_record(r).unwrap(),
            Received::Pending(Waiting::Predecessor)
        );
    }
    assert_eq!(titles(&p.b), Vec::<String>::new());
    drop(p.b);
    let b = Database::open(p.dir.path().join("b.db")).unwrap();
    b.resume_sync(&p.b_keys).unwrap();
    assert_eq!(
        b.pending_records().unwrap().len(),
        2,
        "kept across the restart"
    );
    // The first arrives: all three apply, in chain order, though the
    // pending ones are retried in arrival order.
    assert_eq!(b.receive_record(&sent[0]).unwrap(), Received::Applied);
    assert!(b.pending_records().unwrap().is_empty());
    assert_eq!(titles(&b), ["from A, one", "from A, three", "from A, two"]);
}

#[test]
fn a_record_waits_for_its_epochs_key_and_applies_with_the_transition() {
    let p = pair();
    let b_id = keys(&p.b_keys).id();
    // A rotates and writes in the new epoch; its record reaches B first.
    p.a.rotate_epoch().unwrap();
    p.a.add_task(&mut Task::new(id(3), "epoch 2", 2.0)).unwrap();
    let newest = records(&p.a).pop().unwrap();
    for r in records(&p.a).iter().take(2) {
        p.b.receive_record(r).unwrap();
    }
    assert_eq!(
        p.b.receive_record(&newest).unwrap(),
        Received::Pending(Waiting::Key)
    );
    // The rotation and B's envelope arrive: the record applies.
    deliver_controls(&p.a, &p.b, &b_id);
    assert!(p.b.pending_records().unwrap().is_empty());
    assert!(titles(&p.b).contains(&"epoch 2".to_owned()));
}

#[test]
fn a_record_waits_for_its_writers_claim_and_applies_when_it_arrives() {
    let p = pair();
    p.b.add_task(&mut Task::new(id(3), "from B", 2.0)).unwrap();
    let b_record = records(&p.b).remove(0);
    assert_eq!(
        p.a.receive_record(&b_record).unwrap(),
        Received::Pending(Waiting::Writer)
    );
    // B's writer claim arrives: the pending record applies at once.
    deliver_controls(&p.b, &p.a, &keys(&p.a_keys).id());
    assert!(p.a.pending_records().unwrap().is_empty());
    assert!(titles(&p.a).contains(&"from B".to_owned()));
}

#[test]
fn forged_records_are_refused_and_not_kept() {
    let p = pair();
    let mut forged = records(&p.a).remove(0);
    let at = forged.len() - 3400; // the ciphertext, under the signature
    forged[at] ^= 1;
    assert!(matches!(
        p.b.receive_record(&forged),
        Err(MattersError::Validation(_))
    ));
    assert!(p.b.pending_records().unwrap().is_empty());
    assert!(p.b.receive_record(&[0xff; 64]).is_err());
}

/// The current epoch's key, from `db`'s own envelope of it.
fn current_key(db: &Database, device: &DeviceKeys) -> (ContentKey, Hash, u64) {
    let m = db.sync_membership().unwrap();
    let (_, envelope) = db
        .sync_envelopes()
        .unwrap()
        .into_iter()
        .find(|(manifest, _)| *manifest == m.manifest)
        .unwrap();
    let controls = db.sync_controls().unwrap();
    let Body::Genesis(g) = parse(&controls[0]).unwrap().message.body else {
        unreachable!()
    };
    let mut a =
        matters_sync::authority::Authority::bootstrap(&controls[0], route(), &g.root_key).unwrap();
    for c in &controls[1..] {
        match parse(c).unwrap().message.body {
            Body::Membership(_) => {
                a.apply_membership(c).unwrap();
            }
            Body::WriterStart(_) | Body::WriterHandoff(_) => {
                let _ = a.admit_writer(c, m.document);
            }
            _ => {}
        }
    }
    let key = a
        .unwrap_history_key(&envelope, device)
        .unwrap()
        .into_content_key();
    (key, m.manifest, m.epoch.unwrap())
}

/// A record sealed by `device` as `incarnation`, after `previous`,
/// carrying `update`.
fn forge_as(
    db: &Database,
    device: &DeviceKeys,
    incarnation: [u8; 32],
    previous: Option<Hash>,
    update: &[u8],
) -> Vec<u8> {
    let (key, manifest, epoch) = current_key(db, device);
    let document = db.sync_membership().unwrap().document;
    let route = route();
    let header = wire::Header {
        context: route.content(&document),
        kind: wire::Kind::Data,
        epoch,
        manifest: &manifest,
        author: &device.id(),
        incarnation: &incarnation,
        record_id: &fresh(),
        logical_batch: &fresh(),
        previous: previous.as_ref(),
    };
    seal(header, &key, update, device.signing_key(), fresh()).unwrap()
}

#[test]
fn an_update_carrying_another_actor_than_its_writers_is_refused() {
    let p = pair();
    deliver_controls(&p.b, &p.a, &keys(&p.a_keys).id());
    // B's own writer signs a record whose update was made by another actor.
    let foreign = loro::LoroDoc::new();
    foreign.set_peer_id(424242).unwrap();
    foreign.get_map("x").insert("k", 1).unwrap();
    foreign.commit();
    let update = foreign.export(loro::ExportMode::all_updates()).unwrap();
    let b = keys(&p.b_keys);
    let record = forge_as(&p.b, &b, p.b.writer_incarnation().unwrap(), None, &update);
    match p.a.receive_record(&record) {
        Err(MattersError::Validation(m)) => assert!(m.contains("another actor"), "{m}"),
        other => panic!("{other:?}"),
    }
    assert!(p.a.pending_records().unwrap().is_empty());
}

#[test]
fn a_snapshot_in_place_of_an_update_is_refused() {
    let p = pair();
    deliver_controls(&p.b, &p.a, &keys(&p.a_keys).id());
    // B's admitted writer and actor, signing a snapshot rather than an update.
    let actor =
        p.b.outgoing_controls()
            .unwrap()
            .iter()
            .find_map(|o| match parse(&o.bytes).unwrap().message.body {
                Body::WriterStart(w) => Some(w.actor),
                Body::WriterHandoff(w) => Some(w.actor),
                _ => None,
            })
            .unwrap();
    let doc = loro::LoroDoc::new();
    doc.set_peer_id(actor).unwrap();
    doc.get_map("x").insert("k", 1).unwrap();
    doc.commit();
    let snapshot = doc.export(loro::ExportMode::Snapshot).unwrap();
    let b = keys(&p.b_keys);
    let record = forge_as(&p.b, &b, p.b.writer_incarnation().unwrap(), None, &snapshot);
    match p.a.receive_record(&record) {
        Err(MattersError::Validation(m)) => assert!(m.contains("not an engine update"), "{m}"),
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_record_after_its_writers_signed_close_is_kept_and_never_applied() {
    let p = pair();
    p.b.add_task(&mut Task::new(id(3), "before the close", 2.0))
        .unwrap();
    let old = p.b.writer_incarnation().unwrap();
    let head = record_hash(&records(&p.b).remove(0));
    // B restarts: its new writer hands off, closing the old chain at `head`.
    drop(p.b);
    let b = Database::open(p.dir.path().join("b.db")).unwrap();
    b.resume_sync(&p.b_keys).unwrap();
    deliver_controls(&b, &p.a, &keys(&p.a_keys).id());
    for r in records(&b) {
        p.a.receive_record(&r).unwrap();
    }
    // A record the old writer signs after its close never applies.
    let dk = keys(&p.b_keys);
    let late = {
        let doc = loro::LoroDoc::new();
        doc.set_peer_id(1).unwrap();
        doc.get_map("x").insert("k", 1).unwrap();
        doc.commit();
        doc.export(loro::ExportMode::all_updates()).unwrap()
    };
    let record = forge_as(&b, &dk, old, Some(head), &late);
    assert_eq!(
        p.a.receive_record(&record).unwrap(),
        Received::Pending(Waiting::AfterClose)
    );
    // Its predecessor, signed before the close, did apply.
    assert!(titles(&p.a).contains(&"before the close".to_owned()));
    // Later deliveries retry it and still never apply it.
    for r in records(&b) {
        p.a.receive_record(&r).unwrap();
    }
    assert_eq!(p.a.pending_records().unwrap()[0].1, "after-close");
}

#[test]
fn a_cloned_device_freezes_its_writer_lineage_and_its_records_wait() {
    let p = pair();
    drop(p.b);
    // A backup of B is restored beside B: both open as B.
    let copy = p.dir.path().join("b-copy.db");
    std::fs::copy(p.dir.path().join("b.db"), &copy).unwrap();
    let b = Database::open(p.dir.path().join("b.db")).unwrap();
    b.resume_sync(&p.b_keys).unwrap();
    let clone = Database::open(&copy).unwrap();
    clone.resume_sync(&p.b_keys).unwrap();
    let a_id = keys(&p.a_keys).id();
    deliver_controls(&b, &p.a, &a_id);
    // The clone's handoff competes with B's from the same predecessor.
    let competing = clone
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
        .unwrap();
    assert!(matches!(
        p.a.apply_control(&competing.bytes, None),
        Err(MattersError::Authority(
            matters_sync::authority::Error::Fork
        ))
    ));
    clone
        .add_task(&mut Task::new(id(9), "from the clone", 9.0))
        .unwrap();
    let from_clone = records(&clone).pop().unwrap();
    assert_eq!(
        p.a.receive_record(&from_clone).unwrap(),
        Received::Pending(Waiting::Frozen)
    );
    assert!(!titles(&p.a).contains(&"from the clone".to_owned()));
}

#[test]
fn another_devices_envelope_is_accepted_and_stored_nowhere() {
    let p = pair();
    let b_id = keys(&p.b_keys).id();
    let before = p.b.sync_envelopes().unwrap();
    // A's recovery envelopes and history for others are not B's to keep.
    for o in p.a.outgoing_controls().unwrap() {
        if let Body::KeyEnvelope(k) = parse(&o.bytes).unwrap().message.body {
            if k.recipient != b_id {
                assert_eq!(
                    p.b.apply_control(&o.bytes, None).unwrap(),
                    SyncStanding::Active
                );
            }
        }
    }
    assert_eq!(p.b.sync_envelopes().unwrap(), before);
}
