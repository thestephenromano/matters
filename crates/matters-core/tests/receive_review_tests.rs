//! Regressions for the internal review of durable receive and checkpoints
//! (#45, docs/SYNC_DURABLE_RECEIVE.md). Joined databases exchange records
//! as a relay would, including records a member or the relay crafted:
//! - one bad record in a page never stops the rest, nor later pages;
//! - records this device cannot authenticate yet are kept within a budget,
//!   and bytes under a genuine record's ID never displace it;
//! - a record whose engine dependencies are missing waits;
//! - a second signed successor of one record freezes its writer's lineage;
//! - a reversed chain applies with linear work;
//! - checkpoints are grouped by author, capped, and an unverifiable one
//!   never overrides one this device can verify;
//! - a panic while the database is locked does not disable it.

use matters_core::storage::checkpoint::Checkpointed;
use matters_core::storage::membership::{KeyStore, MemoryKeyStore, SyncStanding};
use matters_core::storage::receive::{self, Received, Waiting};
use matters_core::{Database, MattersError, Task};
use matters_sync::authority::{parse, Body, DeviceKeys, Hash, Route};
use matters_sync::checkpoint::{self, Checkpoint, Cursor};
use matters_sync::record::record_hash;
use matters_sync::{seal, wire, ContentKey};

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

fn fresh() -> [u8; 32] {
    let mut b = [0; 32];
    b[..16].copy_from_slice(uuid::Uuid::new_v4().as_bytes());
    b[16..].copy_from_slice(uuid::Uuid::new_v4().as_bytes());
    b
}

/// Deliver everything `from` published to `to`, in order, with `to`'s
/// envelope beside each transition. Every control must be taken.
fn deliver_controls(from: &Database, to: &Database) {
    let to_id = to.sync_identity().unwrap().1;
    let outgoing = from.outgoing_controls().unwrap();
    for o in &outgoing {
        let envelope = match parse(&o.bytes).unwrap().message.body {
            Body::Membership(_) | Body::RootRecovery(_) => {
                let manifest = record_hash(&o.bytes);
                outgoing
                    .iter()
                    .find_map(|e| match parse(&e.bytes).unwrap().message.body {
                        Body::KeyEnvelope(k) if k.recipient == to_id && k.manifest == manifest => {
                            Some(e.bytes.clone())
                        }
                        _ => None,
                    })
            }
            _ => None,
        };
        match to.apply_control(&o.bytes, envelope.as_deref()) {
            Ok(_) => {}
            // Controls `to` itself signed, met again.
            Err(MattersError::Authority(matters_sync::authority::Error::Unsupported)) => {}
            Err(other) => panic!("a control was not taken: {other:?}"),
        }
    }
}

fn records(db: &Database) -> Vec<Vec<u8>> {
    db.outgoing_records()
        .unwrap()
        .into_iter()
        .map(|r| r.bytes)
        .collect()
}

fn acknowledge_all(db: &Database) {
    for r in db.outgoing_records().unwrap() {
        db.acknowledge_record(&r.record_id).unwrap();
    }
}

fn title(db: &Database, n: u128) -> Option<String> {
    db.get_task(&id(n)).unwrap().map(|t| t.title)
}

/// Join a new device through `admin`, out of band as an invitation is.
fn join(admin: &Database, path: &std::path::Path) -> (Database, MemoryKeyStore) {
    let joiner = Database::open(path).unwrap();
    let keys = MemoryKeyStore::default();
    let shown = admin.invite().unwrap();
    let request = joiner.start_enrollment(&keys, &shown).unwrap();
    let joiner_id = parse(&request).unwrap().message.signer;
    admin.admit(&request, &shown).unwrap();
    let envelopes: Vec<Vec<u8>> = admin
        .outgoing_controls()
        .unwrap()
        .into_iter()
        .filter(|o| {
            matches!(parse(&o.bytes).unwrap().message.body,
                Body::KeyEnvelope(ref k) if k.recipient == joiner_id)
        })
        .map(|o| o.bytes)
        .collect();
    assert_eq!(
        joiner
            .finish_enrollment(&keys, &admin.sync_controls().unwrap(), &envelopes)
            .unwrap(),
        SyncStanding::Active
    );
    (joiner, keys)
}

struct Pair {
    dir: tempfile::TempDir,
    a: Database,
    a_keys: MemoryKeyStore,
    b: Database,
    b_keys: MemoryKeyStore,
}

/// A with two tasks sealed at epoch 0, then B joined; A knows B's writer.
/// B has received nothing.
fn pair() -> Pair {
    let dir = tempfile::tempdir().unwrap();
    let a = Database::open(dir.path().join("a.db")).unwrap();
    let a_keys = MemoryKeyStore::default();
    a.create_collection(&a_keys, route()).unwrap();
    a.add_task(&mut Task::new(id(1), "from A, one", 0.0))
        .unwrap();
    a.add_task(&mut Task::new(id(2), "from A, two", 1.0))
        .unwrap();
    let (b, b_keys) = join(&a, &dir.path().join("b.db"));
    deliver_controls(&b, &a);
    Pair {
        dir,
        a,
        a_keys,
        b,
        b_keys,
    }
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

/// A record `device` signs in `db`'s current epoch.
fn sealed(
    db: &Database,
    device: &DeviceKeys,
    kind: wire::Kind,
    incarnation: [u8; 32],
    previous: Option<Hash>,
    record_id: [u8; 32],
    plaintext: &[u8],
) -> Vec<u8> {
    let (key, manifest, epoch) = current_key(db, device);
    let document = db.sync_membership().unwrap().document;
    let route = route();
    let header = wire::Header {
        context: route.content(&document),
        kind,
        epoch,
        manifest: &manifest,
        author: &device.id(),
        incarnation: &incarnation,
        record_id: &record_id,
        logical_batch: &fresh(),
        previous: previous.as_ref(),
    };
    seal(header, &key, plaintext, device.signing_key(), fresh()).unwrap()
}

fn data(
    db: &Database,
    device: &DeviceKeys,
    incarnation: [u8; 32],
    previous: Option<Hash>,
    update: &[u8],
) -> Vec<u8> {
    sealed(
        db,
        device,
        wire::Kind::Data,
        incarnation,
        previous,
        fresh(),
        update,
    )
}

/// The engine actor `db`'s current writer was admitted with.
fn actor(db: &Database) -> u64 {
    db.outgoing_controls()
        .unwrap()
        .iter()
        .rev()
        .find_map(|o| match parse(&o.bytes).unwrap().message.body {
            Body::WriterStart(w) => Some(w.actor),
            Body::WriterHandoff(w) => Some(w.actor),
            _ => None,
        })
        .unwrap()
}

/// An update by `actor` that follows `db`'s own engine updates up to its
/// `upto`-th, setting `key`.
fn update_after(db: &Database, upto: usize, actor: u64, key: &str) -> Vec<u8> {
    let doc = loro::LoroDoc::new();
    for (_, u) in db.outgoing_updates(0).unwrap().iter().take(upto) {
        doc.import(u).unwrap();
    }
    let before = doc.oplog_vv();
    doc.set_peer_id(actor).unwrap();
    doc.get_map("x").insert(key, 1).unwrap();
    doc.commit();
    doc.export(loro::ExportMode::updates(&before)).unwrap()
}

fn quarantined(db: &Database, record: &[u8]) -> Option<(String, bool)> {
    let p = wire::parse(
        record,
        route().content(&db.sync_membership().unwrap().document),
    )
    .unwrap();
    db.quarantined_records()
        .unwrap()
        .into_iter()
        .find(|(rid, _)| rid == p.header.record_id)
        .map(|(_, r)| r)
}

// Finding 1: one poisoned record stops nothing else.

#[test]
fn a_page_with_a_poisoned_checkpoint_part_applies_its_other_records() {
    let p = pair();
    p.b.add_task(&mut Task::new(id(3), "from B", 2.0)).unwrap();
    let r0 = records(&p.b).pop().unwrap();
    let b = keys(&p.b_keys);
    let junk = sealed(
        &p.b,
        &b,
        wire::Kind::Snapshot,
        p.b.writer_incarnation().unwrap(),
        None,
        fresh(),
        b"not a part",
    );
    let outcomes = p.a.receive_records(&[&junk, &r0]).unwrap();
    assert!(
        matches!(&outcomes[0], Err(MattersError::Validation(m)) if m.contains("checkpoint part")),
        "{:?}",
        outcomes[0]
    );
    assert_eq!(outcomes[1].as_ref().unwrap(), &Received::Applied);
    assert_eq!(title(&p.a, 3).as_deref(), Some("from B"));
    // Kept as signed evidence; delivered again, the same, and nothing waits.
    let (reason, evidence) = quarantined(&p.a, &junk).unwrap();
    assert!(reason.contains("checkpoint part"), "{reason}");
    assert!(evidence);
    let again = p.a.receive_records(&[&junk, &r0]).unwrap();
    assert!(again[0].is_err());
    assert_eq!(again[1].as_ref().unwrap(), &Received::Duplicate);
    assert!(p.a.pending_records().unwrap().is_empty());
}

#[test]
fn a_bad_successor_sent_before_its_predecessor_is_refused_in_its_turn() {
    let p = pair();
    p.b.add_task(&mut Task::new(id(3), "from B", 2.0)).unwrap();
    let r0 = records(&p.b).pop().unwrap();
    // B's writer signs, as its record after r0, an update by another actor.
    let foreign = loro::LoroDoc::new();
    foreign.set_peer_id(424242).unwrap();
    foreign.get_map("x").insert("k", 1).unwrap();
    foreign.commit();
    let bad = data(
        &p.b,
        &keys(&p.b_keys),
        p.b.writer_incarnation().unwrap(),
        Some(record_hash(&r0)),
        &foreign.export(loro::ExportMode::all_updates()).unwrap(),
    );
    assert_eq!(
        p.a.receive_record(&bad).unwrap(),
        Received::Pending(Waiting::Predecessor)
    );
    // Its predecessor arrives: it applies, and the parked one is refused
    // then, without failing the page.
    for _ in 0..2 {
        let outcomes = p.a.receive_records(&[&r0]).unwrap();
        assert!(matches!(
            outcomes[0],
            Ok(Received::Applied) | Ok(Received::Duplicate)
        ));
    }
    assert_eq!(title(&p.a, 3).as_deref(), Some("from B"));
    assert!(p.a.pending_records().unwrap().is_empty());
    let (reason, evidence) = quarantined(&p.a, &bad).unwrap();
    assert!(reason.contains("another actor"), "{reason}");
    assert!(evidence, "a member signed it: its bytes are kept");
}

#[test]
fn an_update_that_breaks_the_projection_is_refused_and_the_page_applies() {
    let p = pair();
    p.b.add_task(&mut Task::new(id(3), "from B", 2.0)).unwrap();
    let genuine = records(&p.b).pop().unwrap();
    // B's writer signs a tree node with no ID: the document can't be
    // read with it.
    let doc = loro::LoroDoc::new();
    doc.set_peer_id(actor(&p.b)).unwrap();
    doc.get_tree("home")
        .create(loro::TreeParentId::Root)
        .unwrap();
    doc.commit();
    let bad = data(
        &p.b,
        &keys(&p.b_keys),
        p.b.writer_incarnation().unwrap(),
        None,
        &doc.export(loro::ExportMode::all_updates()).unwrap(),
    );
    let outcomes = p.a.receive_records(&[&bad, &genuine]).unwrap();
    match &outcomes[0] {
        Err(MattersError::Validation(m)) => assert!(m.contains("unreadable"), "{m}"),
        other => panic!("{other:?}"),
    }
    assert_eq!(outcomes[1].as_ref().unwrap(), &Received::Applied);
    assert_eq!(title(&p.a, 3).as_deref(), Some("from B"));
    // The engine and the rows agree, and local writes go on.
    p.a.add_task(&mut Task::new(id(9), "local", 9.0)).unwrap();
    assert_eq!(title(&p.a, 9).as_deref(), Some("local"));
    p.a.rebuild_from_engine().unwrap();
    assert_eq!(title(&p.a, 3).as_deref(), Some("from B"));
    assert!(quarantined(&p.a, &bad).is_some());
}

#[test]
fn a_store_failure_aborts_the_page_and_refuses_nothing() {
    let p = pair();
    let from_a = records(&p.a);
    // The store breaks under the page: SQLite fails, the record is fine.
    p.b.with_raw_conn(|c| {
        c.execute_batch("ALTER TABLE sync_received RENAME TO broken")?;
        Ok(())
    })
    .unwrap();
    assert!(
        p.b.receive_records(&[&from_a[0]]).is_err(),
        "the page fails"
    );
    assert!(
        p.b.quarantined_records().unwrap().is_empty(),
        "refused nothing"
    );
    p.b.with_raw_conn(|c| {
        c.execute_batch("ALTER TABLE broken RENAME TO sync_received")?;
        Ok(())
    })
    .unwrap();
    // Fetched again, it applies.
    let borrowed: Vec<&[u8]> = from_a.iter().map(Vec::as_slice).collect();
    for outcome in p.b.receive_records(&borrowed).unwrap() {
        assert_eq!(outcome.unwrap(), Received::Applied);
    }
    assert_eq!(title(&p.b, 2).as_deref(), Some("from A, two"));
}

// Finding 2: unauthenticated bytes are bounded, and displace nothing.

/// A record with a well-formed header naming `manifest`, which this device
/// does not know, signed by nobody it knows: its record ID is `n`.
fn unknown_manifest_template(document: &[u8; 16], manifest: &Hash, author: &Hash) -> Vec<u8> {
    let (sk, _) = matters_sync::generate_signing_key([3; 32]);
    let route = route();
    let header = wire::Header {
        context: route.content(document),
        kind: wire::Kind::Data,
        epoch: 99,
        manifest,
        author,
        incarnation: &[0x11; 32],
        record_id: &[0xEE; 32],
        logical_batch: &fresh(),
        previous: None,
    };
    seal(header, &ContentKey::new([1; 32]), b"junk", &sk, fresh()).unwrap()
}

fn with_record_id(template: &[u8], from: &[u8; 32], to: &[u8; 32]) -> Vec<u8> {
    let at = template
        .windows(32)
        .position(|w| w == from)
        .expect("the record ID is in the bytes");
    let mut out = template.to_vec();
    out[at..at + 32].copy_from_slice(to);
    out
}

#[test]
fn ten_thousand_forged_key_waiting_records_stay_within_their_budget() {
    let p = pair();
    let document = p.b.sync_membership().unwrap().document;
    let template = unknown_manifest_template(&document, &[0xAB; 48], &[0xCD; 48]);
    let mut kept = 0;
    for page in 0..20u32 {
        let forged: Vec<Vec<u8>> = (0..500u32)
            .map(|n| {
                let mut rid = [0u8; 32];
                rid[..4].copy_from_slice(&page.to_be_bytes());
                rid[4..8].copy_from_slice(&n.to_be_bytes());
                with_record_id(&template, &[0xEE; 32], &rid)
            })
            .collect();
        let borrowed: Vec<&[u8]> = forged.iter().map(Vec::as_slice).collect();
        for outcome in p.b.receive_records(&borrowed).unwrap() {
            if outcome.unwrap() == Received::Pending(Waiting::Key) {
                kept += 1;
            }
        }
    }
    assert_eq!(kept, 10_000, "each waits, for its delivery");
    let (rows, bytes) = p.b.unauthenticated_pending().unwrap();
    assert!(rows <= receive::UNAUTHENTICATED_ROWS, "{rows} rows kept");
    assert!(
        bytes <= receive::UNAUTHENTICATED_BYTES,
        "{bytes} bytes kept"
    );
    assert_eq!(p.b.pending_records().unwrap().len() as i64, rows);
    // Genuine records still apply, and none of the junk travels in a
    // checkpoint.
    for r in records(&p.a) {
        assert_eq!(p.b.receive_record(&r).unwrap(), Received::Applied);
    }
    acknowledge_all(&p.b);
    let made = p.b.create_checkpoint(None).unwrap().unwrap();
    let coverage = p.b.latest_checkpoint().unwrap().unwrap();
    assert_eq!(coverage.id().unwrap(), made);
    assert!(
        coverage.pending.is_empty(),
        "unauthenticated rows never travel"
    );
}

#[test]
fn bytes_under_a_genuine_records_id_do_not_displace_it() {
    let p = pair();
    for r in records(&p.a) {
        p.b.receive_record(&r).unwrap();
    }
    // A rotates and writes; the record reaches B before the rotation, and
    // so do other bytes under its record ID.
    p.a.rotate_epoch().unwrap();
    p.a.add_task(&mut Task::new(id(3), "epoch 2", 2.0)).unwrap();
    let genuine = records(&p.a).pop().unwrap();
    let mut junk = genuine.clone();
    let at = junk.len() - 3400;
    junk[at] ^= 1;
    for r in [&junk, &genuine] {
        assert_eq!(
            p.b.receive_record(r).unwrap(),
            Received::Pending(Waiting::Key)
        );
    }
    assert_eq!(p.b.pending_records().unwrap().len(), 2, "both kept apart");
    // The rotation arrives: the genuine one applies, the junk is refused.
    deliver_controls(&p.a, &p.b);
    assert_eq!(title(&p.b, 3).as_deref(), Some("epoch 2"));
    assert!(p.b.pending_records().unwrap().is_empty());
    let refused = p.b.quarantined_records().unwrap();
    assert_eq!(refused.len(), 1);
    assert!(!refused[0].1 .1, "unsigned bytes are not kept");
}

#[test]
fn a_forged_record_naming_the_next_epoch_does_not_stall_the_device() {
    let p = pair();
    for r in records(&p.a) {
        p.b.receive_record(&r).unwrap();
    }
    // The relay alone: it has the next epoch's manifest before B does, and
    // serves a record under it that nobody signed.
    p.a.rotate_epoch().unwrap();
    let next = p.a.sync_membership().unwrap().manifest;
    let a_id = keys(&p.a_keys).id();
    let document = p.b.sync_membership().unwrap().document;
    let template = unknown_manifest_template(&document, &next, &a_id);
    let forged = with_record_id(&template, &[0xEE; 32], &fresh());
    assert_eq!(
        p.b.receive_record(&forged).unwrap(),
        Received::Pending(Waiting::Key)
    );
    // The rotation arrives (every control taken), and the forgery is
    // refused then.
    deliver_controls(&p.a, &p.b);
    assert!(p.b.pending_records().unwrap().is_empty());
    assert!(quarantined(&p.b, &forged).is_some());
    // And B goes on receiving.
    p.a.add_task(&mut Task::new(id(3), "epoch 2", 2.0)).unwrap();
    let newest = records(&p.a).pop().unwrap();
    assert_eq!(p.b.receive_record(&newest).unwrap(), Received::Applied);
}

#[test]
fn a_known_manifest_record_is_checked_before_it_waits_for_its_key() {
    let p = pair();
    // B holds no envelope of epoch 0 here, but knows its roster: a record
    // claiming A's authorship with another key's signature is refused.
    p.b.with_raw_conn(|c| {
        c.execute_batch("DELETE FROM sync_envelopes")?;
        Ok(())
    })
    .unwrap();
    let genuine = records(&p.a).remove(0);
    let m = p.a.sync_membership().unwrap();
    let a_id = keys(&p.a_keys).id();
    let forged = with_record_id(
        &{
            let (sk, _) = matters_sync::generate_signing_key([9; 32]);
            let route = route();
            let header = wire::Header {
                context: route.content(&m.document),
                kind: wire::Kind::Data,
                epoch: m.epoch.unwrap(),
                manifest: &m.manifest,
                author: &a_id,
                incarnation: &p.a.writer_incarnation().unwrap(),
                record_id: &[0xEE; 32],
                logical_batch: &fresh(),
                previous: None,
            };
            seal(header, &ContentKey::new([1; 32]), b"junk", &sk, fresh()).unwrap()
        },
        &[0xEE; 32],
        &fresh(),
    );
    assert!(p.b.receive_record(&forged).is_err());
    assert_eq!(
        p.b.receive_record(&genuine).unwrap(),
        Received::Pending(Waiting::Key)
    );
    assert_eq!(p.b.unauthenticated_pending().unwrap().0, 0, "authenticated");
}

// Finding 3: missing engine dependencies.

#[test]
fn a_record_whose_dependencies_are_withheld_waits_and_then_applies() {
    let dir = tempfile::tempdir().unwrap();
    let a = Database::open(dir.path().join("a.db")).unwrap();
    let a_keys = MemoryKeyStore::default();
    a.create_collection(&a_keys, route()).unwrap();
    let (b, _) = join(&a, &dir.path().join("b.db"));
    let (c, _) = join(&a, &dir.path().join("c.db"));
    deliver_controls(&a, &b);
    for (from, to) in [(&b, &a), (&c, &a), (&b, &c), (&c, &b)] {
        deliver_controls(from, to);
    }
    // B writes; C receives it and edits on top of it.
    b.add_task(&mut Task::new(id(3), "from B", 2.0)).unwrap();
    let from_b = records(&b).pop().unwrap();
    assert_eq!(c.receive_record(&from_b).unwrap(), Received::Applied);
    c.rename_task(&id(3), "renamed on C").unwrap();
    let from_c = records(&c).pop().unwrap();
    // A gets C's edit while B's record is withheld: it waits, not applied.
    assert_eq!(
        a.receive_record(&from_c).unwrap(),
        Received::Pending(Waiting::Dependencies)
    );
    assert_eq!(title(&a, 3), None);
    assert_eq!(a.pending_records().unwrap()[0].1, "dependencies");
    // A checkpoint now carries it as pending, outside its snapshot.
    acknowledge_all(&a);
    a.create_checkpoint(None).unwrap().unwrap();
    let coverage = a.latest_checkpoint().unwrap().unwrap();
    assert_eq!(coverage.pending, vec![record_hash(&from_c)]);
    // B's record arrives: both apply, in dependency order.
    assert_eq!(a.receive_record(&from_b).unwrap(), Received::Applied);
    assert!(a.pending_records().unwrap().is_empty());
    assert_eq!(title(&a, 3).as_deref(), Some("renamed on C"));
}

#[test]
fn an_engine_holding_an_update_aside_reports_it_and_refuses_the_merge() {
    use matters_core::crdt::Document;
    let source = loro::LoroDoc::new();
    source.set_peer_id(7).unwrap();
    source.get_map("x").insert("dep", 1).unwrap();
    source.commit();
    let before = source.oplog_vv();
    source.set_peer_id(8).unwrap();
    source.get_map("x").insert("k", 2).unwrap();
    source.commit();
    let dependent = source.export(loro::ExportMode::updates(&before)).unwrap();
    let mut doc = Document::new(1).unwrap();
    assert_eq!(doc.missing_dependency(&dependent).unwrap(), Some((7, 0)));
    assert!(doc.merge_unindexed(&dependent).is_err());
    assert!(doc.has_pending(), "the engine says it holds it aside");
}

// Finding 4: forks inside a writer chain.

#[test]
fn a_second_signed_successor_freezes_the_writers_lineage() {
    let p = pair();
    p.b.add_task(&mut Task::new(id(3), "B one", 2.0)).unwrap();
    p.b.add_task(&mut Task::new(id(4), "B two", 3.0)).unwrap();
    let sent = records(&p.b);
    for r in &sent {
        assert_eq!(p.a.receive_record(r).unwrap(), Received::Applied);
    }
    let b = keys(&p.b_keys);
    let inc = p.b.writer_incarnation().unwrap();
    let other = data(
        &p.b,
        &b,
        inc,
        Some(record_hash(&sent[0])),
        &update_after(&p.b, 1, actor(&p.b), "fork"),
    );
    assert_eq!(
        p.a.receive_record(&other).unwrap(),
        Received::Pending(Waiting::Frozen)
    );
    let frozen = p.a.frozen_lineages().unwrap();
    assert_eq!(frozen.len(), 1);
    assert_eq!(frozen[0].0, b.id());
    assert!(
        frozen[0].1.contains("second signed successor"),
        "{}",
        frozen[0].1
    );
    // B's later records wait with it.
    p.b.add_task(&mut Task::new(id(5), "B three", 4.0)).unwrap();
    let later = records(&p.b).pop().unwrap();
    assert_eq!(
        p.a.receive_record(&later).unwrap(),
        Received::Pending(Waiting::Frozen)
    );
    assert_eq!(title(&p.a, 5), None);
}

#[test]
fn a_branch_before_the_closing_head_freezes_once_the_closed_chain_meets_it() {
    let p = pair();
    p.b.add_task(&mut Task::new(id(3), "B one", 2.0)).unwrap();
    p.b.add_task(&mut Task::new(id(4), "B two", 3.0)).unwrap();
    let sent = records(&p.b);
    let b = keys(&p.b_keys);
    let old = p.b.writer_incarnation().unwrap();
    // A branch from B's first record, which A applies: A has not seen B's
    // second record, nor its close.
    let branch = data(
        &p.b,
        &b,
        old,
        Some(record_hash(&sent[0])),
        &update_after(&p.b, 1, actor(&p.b), "branch"),
    );
    assert_eq!(p.a.receive_record(&sent[0]).unwrap(), Received::Applied);
    assert_eq!(p.a.receive_record(&branch).unwrap(), Received::Applied);
    // B restarts, handing off with its chain closed at its second record.
    drop(p.b);
    let b_db = Database::open(p.dir.path().join("b.db")).unwrap();
    b_db.resume_sync(&p.b_keys).unwrap();
    deliver_controls(&b_db, &p.a);
    // The closed chain's own second record meets the branch: a fork.
    assert_eq!(
        p.a.receive_record(&sent[1]).unwrap(),
        Received::Pending(Waiting::Frozen)
    );
    assert_eq!(p.a.frozen_lineages().unwrap()[0].0, b.id());
    // Nothing more of B's applies, neither on the branch nor the new writer.
    let past = data(
        &b_db,
        &b,
        old,
        Some(record_hash(&branch)),
        &update_after(&b_db, 1, actor(&b_db), "past"),
    );
    assert_eq!(
        p.a.receive_record(&past).unwrap(),
        Received::Pending(Waiting::Frozen)
    );
    b_db.add_task(&mut Task::new(id(5), "new writer", 4.0))
        .unwrap();
    let new = records(&b_db).pop().unwrap();
    assert_eq!(
        p.a.receive_record(&new).unwrap(),
        Received::Pending(Waiting::Frozen)
    );
    // What was applied stays.
    assert_eq!(title(&p.a, 3).as_deref(), Some("B one"));
}

// Finding 5: linear work for a reversed chain.

#[test]
fn a_reversed_chain_of_two_thousand_applies_with_bounded_work() {
    const N: u128 = 2000;
    let p = pair();
    for n in 0..N {
        p.a.add_task(&mut Task::new(id(100 + n), "t", n as f64))
            .unwrap();
    }
    let mut sent = records(&p.a);
    sent.reverse();
    let before = receive::authentications();
    for page in sent.chunks(250) {
        let borrowed: Vec<&[u8]> = page.iter().map(Vec::as_slice).collect();
        for outcome in p.b.receive_records(&borrowed).unwrap() {
            outcome.unwrap();
        }
    }
    let work = receive::authentications() - before;
    let records = sent.len() as u64;
    // Each record is authenticated when it arrives and once more when it
    // can apply; the drain re-checks nothing else.
    assert!(
        work <= 2 * records,
        "{work} authentications for {records} records"
    );
    assert!(p.b.pending_records().unwrap().is_empty());
    assert_eq!(title(&p.b, 100 + N - 1).as_deref(), Some("t"));
}

// Finding 6: checkpoint parts are grouped, capped and superseded.

const GENERATION: [u8; 16] = [9; 16];

fn cursor(sequence: u64) -> Option<Cursor> {
    Some(Cursor {
        generation: GENERATION,
        sequence,
    })
}

fn checkpoint_parts(db: &Database) -> Vec<Vec<u8>> {
    db.outgoing_checkpoint_parts()
        .unwrap()
        .into_iter()
        .map(|(_, b)| b)
        .collect()
}

fn parts_kept(db: &Database) -> i64 {
    db.with_conn(|c| {
        Ok(
            c.query_row("SELECT COUNT(*) FROM sync_checkpoint_parts", [], |r| {
                r.get(0)
            })?,
        )
    })
    .unwrap()
}

/// Checkpoint parts, sealed by `device` in `db`'s epoch, as its writer.
fn sealed_parts(db: &Database, device: &DeviceKeys, plaintexts: &[Vec<u8>]) -> Vec<Vec<u8>> {
    let (key, manifest, epoch) = current_key(db, device);
    let document = db.sync_membership().unwrap().document;
    let incarnation = db.writer_incarnation().unwrap();
    let route = route();
    plaintexts
        .iter()
        .map(|part| {
            let header = wire::Header {
                context: route.content(&document),
                kind: wire::Kind::Snapshot,
                epoch,
                manifest: &manifest,
                author: &device.id(),
                incarnation: &incarnation,
                record_id: &fresh(),
                logical_batch: &fresh(),
                previous: None,
            };
            seal(header, &key, part, device.signing_key(), fresh()).unwrap()
        })
        .collect()
}

/// `db`'s checkpoint `ID`'s plaintext parts, opened.
fn opened_parts(db: &Database, device: &DeviceKeys) -> Vec<Vec<u8>> {
    let (key, _, _) = current_key(db, device);
    let document = db.sync_membership().unwrap().document;
    checkpoint_parts(db)
        .iter()
        .map(|bytes| {
            let p = wire::parse(bytes, route().content(&document)).unwrap();
            matters_sync::verify(p, device.verification_key())
                .unwrap()
                .open(&key)
                .unwrap()
        })
        .collect()
}

#[test]
fn another_members_parts_do_not_spoil_an_honest_checkpoint() {
    let p = pair();
    for r in records(&p.a) {
        p.b.receive_record(&r).unwrap();
    }
    let (c, _) = join(&p.a, &p.dir.path().join("c.db"));
    deliver_controls(&p.b, &c);
    acknowledge_all(&p.a);
    p.a.create_checkpoint(cursor(2)).unwrap().unwrap();
    let a_dev = keys(&p.a_keys);
    let honest = opened_parts(&p.a, &a_dev);
    // B sends A's checkpoint ID with a chunk of its own.
    // A byte of the payload chunk, past the part's header, changed: still
    // a well-formed part of the same checkpoint ID.
    let mut spoiled = honest[0].clone();
    spoiled[120] ^= 1;
    let parsed = checkpoint::parse_part(&spoiled).unwrap();
    assert_eq!(parsed.id, checkpoint::parse_part(&honest[0]).unwrap().id);
    let b_dev = keys(&p.b_keys);
    let from_b = sealed_parts(&p.b, &b_dev, &[spoiled]);
    match c.receive_record(&from_b[0]).unwrap() {
        Received::Checkpoint(Checkpointed::Refused(_)) => {}
        other => panic!("{other:?}"),
    }
    assert!(c.checkpoints().unwrap().is_empty(), "nothing under its ID");
    // A's own parts of it are judged on their own.
    match c.receive_record(&checkpoint_parts(&p.a)[0]).unwrap() {
        Received::Checkpoint(Checkpointed::Accepted) => {}
        other => panic!("{other:?}"),
    }
}

#[test]
fn an_authors_parts_are_capped_and_superseded_by_its_newer_checkpoint() {
    let p = pair();
    for r in records(&p.a) {
        p.b.receive_record(&r).unwrap();
    }
    let a_dev = keys(&p.a_keys);
    // A two-part checkpoint A never finishes.
    let big = Checkpoint::new(
        checkpoint::Statement {
            engine: "loro-1".into(),
            schema: 1,
            epoch: p.a.sync_membership().unwrap().epoch.unwrap(),
            writers: Vec::new(),
            version: Vec::new(),
            frontiers: Vec::new(),
            cursor: None,
        },
        vec![7; checkpoint::CHUNK_BYTES + 1],
        Vec::new(),
    )
    .unwrap();
    let big_parts = big.parts().unwrap();
    assert_eq!(big_parts.len(), 2);
    // Its small last part under more record IDs than a checkpoint has
    // parts: past the cap, the author's parts of it are dropped.
    let copies = vec![big_parts[1].clone(); checkpoint::MAX_PARTS as usize + 1];
    let mut refused = 0;
    for r in sealed_parts(&p.a, &a_dev, &copies) {
        match p.b.receive_record(&r) {
            Ok(Received::Checkpoint(Checkpointed::Part)) => {}
            Err(_) => refused += 1,
            other => panic!("{other:?}"),
        }
    }
    assert_eq!(refused, 1, "the one past the cap");
    assert_eq!(parts_kept(&p.b), 0);
    // Its first part, kept; then A's newer, complete checkpoint
    // supersedes it.
    let first = sealed_parts(&p.a, &a_dev, &big_parts[..1]);
    assert_eq!(
        p.b.receive_record(&first[0]).unwrap(),
        Received::Checkpoint(Checkpointed::Part)
    );
    assert_eq!(parts_kept(&p.b), 1);
    acknowledge_all(&p.a);
    p.a.create_checkpoint(cursor(2)).unwrap().unwrap();
    assert_eq!(
        p.b.receive_record(&checkpoint_parts(&p.a)[0]).unwrap(),
        Received::Checkpoint(Checkpointed::Accepted)
    );
    assert_eq!(parts_kept(&p.b), 0);
}

// Finding 7: unverifiable claims decide nothing.

#[test]
fn an_unverifiable_checkpoint_never_overrides_one_this_device_can_verify() {
    let p = pair();
    for r in records(&p.a) {
        p.b.receive_record(&r).unwrap();
    }
    let a_dev = keys(&p.a_keys);
    let a_inc = p.a.writer_incarnation().unwrap();
    // A member's checkpoint claiming A's chain far past what B holds, its
    // snapshot made to match: nothing B holds refutes it.
    let doc = loro::LoroDoc::new();
    for (_, u) in p.a.outgoing_updates(0).unwrap() {
        doc.import(&u).unwrap();
    }
    doc.set_peer_id(actor(&p.a)).unwrap();
    doc.get_map("x").insert("inflated", 1).unwrap();
    doc.commit();
    let inflated = Checkpoint::new(
        checkpoint::Statement {
            engine: "loro-1".into(),
            schema: 1,
            epoch: p.a.sync_membership().unwrap().epoch.unwrap(),
            writers: vec![checkpoint::Writer {
                author: a_dev.id(),
                incarnation: a_inc,
                records: 50,
                head: [7; 48],
            }],
            version: matters_core::crdt::encode_version(&doc.oplog_vv()),
            frontiers: matters_core::crdt::encode_frontiers(&doc.oplog_frontiers()),
            cursor: cursor(100),
        },
        doc.export(loro::ExportMode::Snapshot).unwrap(),
        Vec::new(),
    )
    .unwrap();
    let forged = sealed_parts(&p.a, &a_dev, &inflated.parts().unwrap());
    assert_eq!(
        p.b.receive_record(&forged[0]).unwrap(),
        Received::Checkpoint(Checkpointed::Accepted),
        "accepted, unverified"
    );
    // A's honest checkpoint, which B can verify, is accepted, not
    // superseded by the inflated claim, and is the latest.
    acknowledge_all(&p.a);
    let honest = p.a.create_checkpoint(cursor(2)).unwrap().unwrap();
    assert_eq!(
        p.b.receive_record(&checkpoint_parts(&p.a)[0]).unwrap(),
        Received::Checkpoint(Checkpointed::Accepted)
    );
    assert_eq!(
        p.b.latest_checkpoint().unwrap().unwrap().id().unwrap(),
        honest
    );
}

// Finding 8: other bytes under a held record ID are evidence.

#[test]
fn another_record_under_an_applied_records_id_is_kept_as_evidence() {
    let p = pair();
    let from_a = records(&p.a);
    for r in &from_a {
        p.b.receive_record(r).unwrap();
    }
    // A member signs another record under A's first record's ID.
    let b = keys(&p.b_keys);
    let taken = *wire::parse(
        &from_a[0],
        route().content(&p.a.sync_membership().unwrap().document),
    )
    .unwrap()
    .header
    .record_id;
    p.b.add_task(&mut Task::new(id(3), "B", 2.0)).unwrap();
    let reuse = sealed(
        &p.b,
        &b,
        wire::Kind::Data,
        p.b.writer_incarnation().unwrap(),
        None,
        taken,
        &update_after(&p.b, 0, actor(&p.b), "reuse"),
    );
    // B receives it: B holds A's record under that ID.
    match p.b.receive_record(&reuse) {
        Err(MattersError::Validation(m)) => assert!(m.contains("record ID"), "{m}"),
        other => panic!("{other:?}"),
    }
    let (_, evidence) = quarantined(&p.b, &reuse).unwrap();
    assert!(evidence);
    // The genuine record is still a duplicate.
    assert_eq!(p.b.receive_record(&from_a[0]).unwrap(), Received::Duplicate);
}

// Finding 9: a panic does not disable the database.

#[test]
fn a_panic_while_locked_leaves_the_database_working() {
    let p = pair();
    for r in records(&p.a) {
        p.b.receive_record(&r).unwrap();
    }
    let b = std::panic::AssertUnwindSafe(&p.b);
    assert!(std::panic::catch_unwind(|| b.panic_holding_locks()).is_err());
    // The half-done write is rolled back, the engine rebuilt, and both
    // reads and writes work.
    assert_eq!(title(&p.b, 1).as_deref(), Some("from A, one"));
    p.b.add_task(&mut Task::new(id(3), "after", 2.0)).unwrap();
    assert_eq!(title(&p.b, 3).as_deref(), Some("after"));
    p.b.rebuild_from_engine().unwrap();
    assert_eq!(title(&p.b, 3).as_deref(), Some("after"));
    // And the edit made after it still travels: its writer's actor is kept.
    deliver_controls(&p.b, &p.a);
    let newest = records(&p.b).pop().unwrap();
    assert_eq!(p.a.receive_record(&newest).unwrap(), Received::Applied);
}

// Revocation (#45, with the authority's closing frontiers): a removed
// device's records are kept as revoked, within a per-author budget.

#[test]
fn a_removed_devices_records_are_kept_as_revoked_within_their_budget() {
    let p = pair();
    let (c, _) = join(&p.a, &p.dir.path().join("c.db"));
    deliver_controls(&p.b, &c);
    let b = keys(&p.b_keys);
    // B's epoch, which C holds a key of, before A removes B.
    let (key, manifest, epoch) = current_key(&p.b, &b);
    let document = p.b.sync_membership().unwrap().document;
    p.a.remove_member(&b.id()).unwrap();
    deliver_controls(&p.a, &c);
    // B, not told, signs on under a writer nobody admitted.
    let incarnation = fresh();
    let route = route();
    let count = receive::REVOKED_ROWS_PER_AUTHOR as usize + 20;
    let signed: Vec<Vec<u8>> = (0..count)
        .map(|_| {
            let header = wire::Header {
                context: route.content(&document),
                kind: wire::Kind::Data,
                epoch,
                manifest: &manifest,
                author: &b.id(),
                incarnation: &incarnation,
                record_id: &fresh(),
                logical_batch: &fresh(),
                previous: None,
            };
            seal(header, &key, b"after removal", b.signing_key(), fresh()).unwrap()
        })
        .collect();
    for page in signed.chunks(100) {
        let borrowed: Vec<&[u8]> = page.iter().map(Vec::as_slice).collect();
        for outcome in c.receive_records(&borrowed).unwrap() {
            assert_eq!(outcome.unwrap(), Received::Pending(Waiting::Revoked));
        }
    }
    let revoked = c
        .pending_records()
        .unwrap()
        .into_iter()
        .filter(|(_, reason)| reason == "revoked")
        .count();
    assert_eq!(
        revoked as i64,
        receive::REVOKED_ROWS_PER_AUTHOR,
        "the oldest evicted"
    );
    assert_eq!(c.unauthenticated_pending().unwrap().0, 0, "all signed");
}
