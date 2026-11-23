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
    // B's writer is claimed, as its first seal would claim it, so tests can
    // forge or withhold its records.
    b.claim_writer_now().unwrap();
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
    b.claim_writer_now().unwrap();
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
    b.claim_writer_now().unwrap();
    let clone = Database::open(&copy).unwrap();
    clone.resume_sync(&p.b_keys).unwrap();
    clone.claim_writer_now().unwrap();
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
    // Its signature was checked: kept as signed evidence, not in the
    // budget of records nobody could authenticate.
    let authenticated: bool =
        p.a.with_conn(|c| {
            Ok(c.query_row(
                "SELECT authenticated FROM sync_pending WHERE digest = ?1",
                [record_hash(&from_clone).as_slice()],
                |r| r.get(0),
            )?)
        })
        .unwrap();
    assert!(authenticated);
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

#[test]
fn rereading_ones_own_log_after_rotations_changes_nothing() {
    let p = pair();
    p.a.rotate_epoch().unwrap();
    let before = p.a.sync_membership().unwrap();
    let mut log = vec![p.a.sync_controls().unwrap().remove(0)];
    log.extend(
        p.a.outgoing_controls()
            .unwrap()
            .into_iter()
            .map(|o| o.bytes),
    );
    for bytes in &log {
        match p.a.apply_control(bytes, None) {
            Ok(_) => {}
            // The genesis is no control to apply again.
            Err(MattersError::Authority(matters_sync::authority::Error::Unsupported)) => {}
            Err(other) => panic!("{}: {other:?}", parse(bytes).unwrap().message.body.kind()),
        }
    }
    assert_eq!(p.a.sync_membership().unwrap(), before);
}

#[test]
fn a_second_definition_of_an_admitted_writer_freezes_its_lineage() {
    let p = pair();
    let a_keys = keys(&p.a_keys);
    let m = p.a.sync_membership().unwrap();
    let incarnation = p.a.writer_incarnation().unwrap();
    // A's device signs its admitted incarnation again, with another
    // predecessor: competing definitions, never a replay.
    let conflicting = matters_sync::authority::sign(
        &matters_sync::authority::Message {
            route: route(),
            signer: a_keys.id(),
            body: Body::WriterStart(matters_sync::authority::WriterStart {
                manifest: m.manifest,
                document: m.document,
                device: a_keys.id(),
                incarnation,
                actor: 7,
                previous: Some(fresh()),
            }),
        },
        a_keys.signing_key(),
        fresh(),
    )
    .unwrap();
    assert!(matches!(
        p.b.apply_control(&conflicting, None),
        Err(MattersError::Authority(
            matters_sync::authority::Error::Fork
        ))
    ));
}

#[test]
fn a_page_applies_in_one_go_each_record_with_its_own_outcome() {
    let p = pair();
    p.a.add_task(&mut Task::new(id(3), "from A, three", 2.0))
        .unwrap();
    let from_a = records(&p.a);
    let mut forged = from_a[1].clone();
    let at = forged.len() - 3400;
    forged[at] ^= 1;
    // Out of chain order, with a forgery of the second and a duplicate.
    let page: Vec<&[u8]> = vec![&from_a[2], &from_a[0], &forged, &from_a[1], &from_a[0]];
    let outcomes = p.b.receive_records(&page).unwrap();
    assert_eq!(
        outcomes[0].as_ref().unwrap(),
        &Received::Pending(Waiting::Predecessor)
    );
    assert_eq!(outcomes[1].as_ref().unwrap(), &Received::Applied);
    assert!(
        matches!(outcomes[2], Err(MattersError::Validation(_))),
        "the forgery alone is refused"
    );
    assert_eq!(outcomes[3].as_ref().unwrap(), &Received::Applied);
    assert_eq!(outcomes[4].as_ref().unwrap(), &Received::Duplicate);
    // The early third applied in the same batch, once its predecessor did.
    assert!(p.b.pending_records().unwrap().is_empty());
    assert_eq!(
        titles(&p.b),
        ["from A, one", "from A, three", "from A, two"],
        "every applied record is in the rows"
    );
    // An empty page changes nothing.
    assert!(p.b.receive_records(&[]).unwrap().is_empty());
}

/// Run in a child process: receive the page in `MATTERS_CRASH_PAGE` into
/// the database in `MATTERS_CRASH_DB`, with the keys in
/// `MATTERS_CRASH_KEYS`; `MATTERS_CRASH_AT` interrupts it before the
/// commit. Ignored in ordinary runs; the parent test runs it.
#[test]
#[ignore]
fn receive_crash_worker() {
    let Ok(path) = std::env::var("MATTERS_CRASH_DB") else {
        return;
    };
    let keys = MemoryKeyStore::default();
    let saved = std::fs::read(std::env::var("MATTERS_CRASH_KEYS").unwrap()).unwrap();
    let (name, seeds) = saved.split_at(saved.iter().position(|b| *b == 0).unwrap());
    keys.store(
        std::str::from_utf8(name).unwrap(),
        &matters_core::storage::membership::DeviceSeeds::from_bytes(&seeds[1..]).unwrap(),
    )
    .unwrap();
    let db = Database::open(&path).unwrap();
    db.resume_sync(&keys).unwrap();
    let page: Vec<Vec<u8>> = std::fs::read_dir(std::env::var("MATTERS_CRASH_PAGE").unwrap())
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .map(|p| std::fs::read(p).unwrap())
        .collect();
    let borrowed: Vec<&[u8]> = page.iter().map(Vec::as_slice).collect();
    let _ = db.receive_records(&borrowed);
    unreachable!("the crash point exits the process");
}

#[test]
fn a_crash_before_a_page_commits_applies_none_of_it() {
    let p = pair();
    let from_a = records(&p.a);
    let dir = p.dir.path();
    let page = dir.join("page");
    std::fs::create_dir(&page).unwrap();
    for (n, r) in from_a.iter().enumerate() {
        std::fs::write(page.join(format!("{n:03}")), r).unwrap();
    }
    let name = p.b_keys.names().remove(0);
    let mut saved = name.clone().into_bytes();
    saved.push(0);
    saved.extend(p.b_keys.load(&name).unwrap().unwrap().to_bytes());
    std::fs::write(dir.join("keys"), saved).unwrap();
    let path = dir.join("b.db");
    let b = p.b;
    drop(b);
    let status = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "receive_crash_worker",
            "--exact",
            "--ignored",
            "--nocapture",
        ])
        .env("MATTERS_CRASH_DB", &path)
        .env("MATTERS_CRASH_KEYS", dir.join("keys"))
        .env("MATTERS_CRASH_PAGE", &page)
        .env("MATTERS_CRASH_AT", "receive-before-commit")
        .status()
        .unwrap();
    assert_eq!(status.code(), Some(86), "exited at the crash point");
    // Nothing of the page: no rows, no pending, the engine agreeing.
    let b = Database::open(&path).unwrap();
    b.resume_sync(&p.b_keys).unwrap();
    assert!(titles(&b).is_empty());
    assert!(b.pending_records().unwrap().is_empty());
    // Received again, it all applies.
    let borrowed: Vec<&[u8]> = from_a.iter().map(Vec::as_slice).collect();
    for outcome in b.receive_records(&borrowed).unwrap() {
        assert_eq!(outcome.unwrap(), Received::Applied);
    }
    assert_eq!(titles(&b), ["from A, one", "from A, two"]);
}

#[test]
fn a_task_just_received_can_be_edited_here_at_once() {
    let p = pair();
    let from_a = records(&p.a);
    let page: Vec<&[u8]> = from_a.iter().map(Vec::as_slice).collect();
    p.b.receive_records(&page).unwrap();
    // The engine's index is rebuilt when a local command first needs it.
    p.b.rename_task(&id(2), "renamed on B").unwrap();
    p.b.complete_task(&id(1)).unwrap();
    assert_eq!(p.b.get_task(&id(2)).unwrap().unwrap().title, "renamed on B");
    // And the edits travel back, once A knows B's writer.
    deliver_controls(&p.b, &p.a, &keys(&p.a_keys).id());
    for r in records(&p.b) {
        p.a.receive_record(&r).unwrap();
    }
    assert_eq!(p.a.get_task(&id(2)).unwrap().unwrap().title, "renamed on B");
    assert!(p
        .a
        .get_task(&id(1))
        .unwrap()
        .unwrap()
        .completed_at
        .is_some());
}
