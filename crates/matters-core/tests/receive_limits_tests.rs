//! The limits of durable receive closed after the internal review (#45,
//! docs/SYNC_DURABLE_RECEIVE.md):
//! - a genuine record that a flood of forgeries evicted from the
//!   unauthenticated budget is read again from the relay's log, once, when
//!   the manifest changes;
//! - a lineage frozen on fork evidence is recovered by an admin's closing
//!   frontier: the records on the closed prefix apply, the rest never do;
//! - a replacement writer's content waits until the closing head of the
//!   writer it replaced is applied.

use matters_core::storage::membership::{KeyStore, MemoryKeyStore, SyncStanding};
use matters_core::storage::receive::{self, Received, Waiting};
use matters_core::{Database, MattersError, Task};
use matters_sync::authority::{
    self as auth, parse, Body, DeviceKeys, Hash, Message, Route, WriterHandoff,
};
use matters_sync::checkpoint::Cursor;
use matters_sync::record::record_hash;
use matters_sync::store::{LocalStore, Log};
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
            Err(MattersError::Authority(auth::Error::Unsupported)) => {}
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

fn title(db: &Database, n: u128) -> Option<String> {
    db.get_task(&id(n)).unwrap().map(|t| t.title)
}

fn count(db: &Database, sql: &str) -> i64 {
    db.with_conn(|c| Ok(c.query_row(sql, [], |r| r.get(0))?))
        .unwrap()
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

struct Devices {
    dir: tempfile::TempDir,
    a: Database,
    b: Database,
    b_keys: MemoryKeyStore,
    c: Database,
}

/// A, the admin, with two tasks; B and C joined, and every device knowing
/// every other's writer. Nobody has received a record.
fn devices() -> Devices {
    let dir = tempfile::tempdir().unwrap();
    let a = Database::open(dir.path().join("a.db")).unwrap();
    a.create_collection(&MemoryKeyStore::default(), route())
        .unwrap();
    a.add_task(&mut Task::new(id(1), "from A, one", 0.0))
        .unwrap();
    a.add_task(&mut Task::new(id(2), "from A, two", 1.0))
        .unwrap();
    let (b, b_keys) = join(&a, &dir.path().join("b.db"));
    let (c, _) = join(&a, &dir.path().join("c.db"));
    // Writers are claimed at their first seal; these fixtures sign records
    // and handoffs of B's and C's writers directly, so claim them now.
    b.claim_writer_now().unwrap();
    c.claim_writer_now().unwrap();
    deliver_controls(&b, &a);
    deliver_controls(&c, &a);
    deliver_controls(&a, &b);
    deliver_controls(&b, &c);
    Devices {
        dir,
        a,
        b,
        b_keys,
        c,
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
    let mut a = auth::Authority::bootstrap(&controls[0], route(), &g.root_key).unwrap();
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

/// A data record `device` signs in `db`'s current epoch.
fn data(
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

/// `device`'s signed handoff from its writer `previous` to a new writer
/// `incarnation` with engine actor `actor`, closing `previous` at
/// `closing`, under `db`'s current manifest.
fn handoff(
    db: &Database,
    device: &DeviceKeys,
    previous: [u8; 32],
    closing: Option<Hash>,
    incarnation: [u8; 32],
    actor: u64,
) -> Vec<u8> {
    let m = db.sync_membership().unwrap();
    let message = Message {
        route: route(),
        signer: device.id(),
        body: Body::WriterHandoff(WriterHandoff {
            manifest: m.manifest,
            document: m.document,
            device: device.id(),
            incarnation,
            actor,
            previous,
            closing,
        }),
    };
    auth::sign(&message, device.signing_key(), fresh()).unwrap()
}

fn reason(db: &Database, record: &[u8]) -> Option<String> {
    let document = db.sync_membership().unwrap().document;
    let rid = *wire::parse(record, route().content(&document))
        .unwrap()
        .header
        .record_id;
    db.pending_records()
        .unwrap()
        .into_iter()
        .find(|(r, _)| *r == rid)
        .map(|(_, why)| why)
}

// Limit 1: a genuine record a flood evicted is read again.

/// A record naming `manifest`, which the receiver does not know, signed by
/// nobody it knows, under a record ID of its own.
fn forged(document: &[u8; 16], manifest: &Hash) -> Vec<u8> {
    let (sk, _) = matters_sync::generate_signing_key([3; 32]);
    let route = route();
    let header = wire::Header {
        context: route.content(document),
        kind: wire::Kind::Data,
        epoch: 99,
        manifest,
        author: &[0xCD; 48],
        incarnation: &[0x11; 32],
        record_id: &fresh(),
        logical_batch: &fresh(),
        previous: None,
    };
    seal(header, &ContentKey::new([1; 32]), b"junk", &sk, fresh()).unwrap()
}

/// More forgeries than the unauthenticated budget keeps.
fn flood(db: &Database) -> Vec<Vec<u8>> {
    let document = db.sync_membership().unwrap().document;
    (0..=receive::UNAUTHENTICATED_ROWS)
        .map(|_| forged(&document, &[0xAB; 48]))
        .collect()
}

const GENERATION: [u8; 16] = [5; 16];

fn at(sequence: u64) -> Vec<u8> {
    Cursor {
        generation: GENERATION,
        sequence,
    }
    .relay_cursor()
}

/// Receive `records` as the relay's log entries `first`, `first + 1`, ...
/// of `GENERATION`, and save the cursor past them, as the client does.
fn page(db: &Database, first: u64, records: &[Vec<u8>]) -> Vec<Result<Received, MattersError>> {
    let entries: Vec<(u64, &[u8])> = records
        .iter()
        .enumerate()
        .map(|(n, r)| (first + n as u64, r.as_slice()))
        .collect();
    let outcomes = db.receive_page(GENERATION, &entries).unwrap();
    db.save_sync_cursor(Log::Records, &at(first + records.len() as u64 - 1))
        .unwrap();
    outcomes
}

#[test]
fn a_genuine_record_a_flood_evicted_is_read_again_once_its_manifest_arrives() {
    let d = devices();
    let mut log = records(&d.a);
    assert_eq!(page(&d.b, 1, &log).len(), 2);
    // A rotates and writes. The relay has the record before B reads the
    // rotation, and serves a flood of forgeries after it.
    d.a.rotate_epoch().unwrap();
    d.a.add_task(&mut Task::new(id(3), "epoch 2", 2.0)).unwrap();
    let genuine = records(&d.a).pop().unwrap();
    log.push(genuine.clone());
    log.extend(flood(&d.b));
    let outcomes = page(&d.b, 3, &log[2..]);
    assert_eq!(
        outcomes[0].as_ref().unwrap(),
        &Received::Pending(Waiting::Key)
    );
    assert_eq!(reason(&d.b, &genuine), None, "evicted by the flood");
    let high = log.len() as u64;
    // Nothing new can authenticate it yet: the cursor stays.
    assert!(!d.b.rewind_records().unwrap());
    assert_eq!(d.b.sync_cursor(Log::Records).unwrap(), Some(at(high)));
    // The rotation arrives: the cursor moves back to just before it, once,
    // as the sync client asks it to.
    deliver_controls(&d.a, &d.b);
    LocalStore::rewind_records(&d.b).unwrap();
    assert_eq!(d.b.sync_cursor(Log::Records).unwrap(), Some(at(2)));
    assert!(!d.b.rewind_records().unwrap(), "once for each change");
    // Read again from there, as the client gives it a page, it applies.
    let entries: Vec<(u64, Vec<u8>)> = (3..).zip(log[2..].iter().cloned()).collect();
    let outcomes = LocalStore::receive_page(&d.b, GENERATION, &entries).unwrap();
    assert_eq!(outcomes.len(), entries.len());
    assert!(outcomes[0].is_ok());
    assert!(outcomes.iter().all(Result::is_ok), "applied or kept");
    d.b.save_sync_cursor(Log::Records, &at(high)).unwrap();
    assert_eq!(title(&d.b, 3).as_deref(), Some("epoch 2"));
    // The flood, evicted again, waits for the next change: no rewind.
    assert!(!d.b.rewind_records().unwrap());
    assert_eq!(d.b.sync_cursor(Log::Records).unwrap(), Some(at(high)));
    d.a.rotate_epoch().unwrap();
    deliver_controls(&d.a, &d.b);
    assert!(d.b.rewind_records().unwrap());
    assert_eq!(d.b.sync_cursor(Log::Records).unwrap(), Some(at(3)));
}

/// A remembered place, as an eviction left it.
fn rewind_row(db: &Database) -> Option<(Vec<u8>, i64, Vec<u8>)> {
    db.with_conn(|c| {
        use rusqlite::OptionalExtension;
        Ok(c.query_row(
            "SELECT generation, sequence, manifest FROM sync_rewind",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?)
    })
    .unwrap()
}

/// `db` evicts records read as log entries `first...`, then the manifest
/// changes.
fn evicted_at(d: &Devices, first: u64) {
    page(&d.b, first, &flood(&d.b));
    d.a.rotate_epoch().unwrap();
    deliver_controls(&d.a, &d.b);
}

#[test]
fn a_rewind_moves_back_only_within_its_generation() {
    let d = devices();
    // The evicted record was the last read: the cursor moves back by one.
    let flood = flood(&d.b);
    let n = flood.len() as u64;
    page(&d.b, 10, &flood);
    // The earliest evicted is entry 10, kept with the manifest then.
    let (generation, sequence, manifest) = rewind_row(&d.b).unwrap();
    assert_eq!(
        (generation, sequence, manifest),
        (
            GENERATION.to_vec(),
            10,
            d.b.sync_membership().unwrap().manifest.to_vec()
        )
    );
    d.a.rotate_epoch().unwrap();
    deliver_controls(&d.a, &d.b);
    d.b.save_sync_cursor(Log::Records, &at(10)).unwrap();
    assert!(d.b.rewind_records().unwrap());
    assert_eq!(d.b.sync_cursor(Log::Records).unwrap(), Some(at(9)));
    assert_eq!(rewind_row(&d.b), None);
    // A cursor behind it stays where it is, and so does one of another
    // generation; either way the place is forgotten.
    evicted_at(&d, 20);
    let earliest = rewind_row(&d.b).unwrap().1 as u64;
    d.b.save_sync_cursor(Log::Records, &at(earliest - 1))
        .unwrap();
    assert!(!d.b.rewind_records().unwrap());
    assert_eq!(
        d.b.sync_cursor(Log::Records).unwrap(),
        Some(at(earliest - 1))
    );
    assert_eq!(rewind_row(&d.b), None);
    evicted_at(&d, 20);
    let other = Cursor {
        generation: [6; 16],
        sequence: 100,
    }
    .relay_cursor();
    d.b.save_sync_cursor(Log::Records, &other).unwrap();
    assert!(!d.b.rewind_records().unwrap());
    assert_eq!(d.b.sync_cursor(Log::Records).unwrap(), Some(other));
    assert_eq!(rewind_row(&d.b), None);
    // Evictions keep the earliest place across pages of one generation;
    // another generation's replaces it.
    d.b.sync_generation_changed().unwrap();
    page(&d.b, 40 + n, &self::flood(&d.b));
    page(&d.b, 30, &self::flood(&d.b));
    page(&d.b, 50 + n, &self::flood(&d.b));
    assert_eq!(rewind_row(&d.b).unwrap().1, 30);
    let other: Vec<Vec<u8>> = self::flood(&d.b);
    let entries: Vec<(u64, &[u8])> = other
        .iter()
        .enumerate()
        .map(|(k, r)| (200 + k as u64, r.as_slice()))
        .collect();
    d.b.receive_page([6; 16], &entries).unwrap();
    let (generation, sequence, _) = rewind_row(&d.b).unwrap();
    assert_eq!((generation, sequence), ([6; 16].to_vec(), 51 + n as i64));
    // A generation change forgets it, and where kept rows were read.
    d.b.sync_generation_changed().unwrap();
    assert_eq!(rewind_row(&d.b), None);
    assert_eq!(
        count(
            &d.b,
            "SELECT COUNT(*) FROM sync_pending WHERE origin IS NOT NULL"
        ),
        0
    );
    // Records received without their place remember none.
    let borrowed: Vec<Vec<u8>> = self::flood(&d.b);
    let borrowed: Vec<&[u8]> = borrowed.iter().map(Vec::as_slice).collect();
    d.b.receive_records(&borrowed).unwrap();
    assert_eq!(rewind_row(&d.b), None);
}

// Limit 2: a frozen lineage recovered by an admin's closing frontier.

struct Forked {
    d: Devices,
    b: DeviceKeys,
    incarnation: [u8; 32],
    /// B's records r1, r2, r3, r4 in its one chain.
    chain: Vec<Vec<u8>>,
}

/// B writes a chain of four records, which A, the admin, applies.
fn forked() -> Forked {
    let d = devices();
    for n in 0..4 {
        d.b.add_task(&mut Task::new(id(10 + n), "from B", n as f64))
            .unwrap();
    }
    let chain = records(&d.b);
    assert_eq!(chain.len(), 4);
    for r in &chain {
        assert_eq!(d.a.receive_record(r).unwrap(), Received::Applied);
    }
    Forked {
        b: keys(&d.b_keys),
        incarnation: d.b.writer_incarnation().unwrap(),
        chain,
        d,
    }
}

impl Forked {
    /// B's signed record after `previous` that no chain of its holds.
    fn branch(&self, previous: Option<&[u8]>, upto: usize, key: &str) -> Vec<u8> {
        data(
            &self.d.b,
            &self.b,
            self.incarnation,
            previous.map(record_hash),
            &update_after(&self.d.b, upto, actor(&self.d.b), key),
        )
    }

    /// A removes B, closing its writer at the head A holds, and C takes
    /// the transition.
    fn remove_b(&self) {
        self.d.a.remove_member(&self.b.id()).unwrap();
        deliver_controls(&self.d.a, &self.d.c);
    }
}

#[test]
fn an_admins_closing_frontier_recovers_a_frozen_lineage_up_to_its_head() {
    let f = forked();
    let c = &f.d.c;
    let [r1, r2, r3, r4] = [0, 1, 2, 3].map(|n| f.chain[n].clone());
    assert_eq!(c.receive_record(&r1).unwrap(), Received::Applied);
    assert_eq!(c.receive_record(&r2).unwrap(), Received::Applied);
    // A second successor of r1 freezes B's lineage on C.
    let fork = f.branch(Some(&r1), 1, "fork");
    assert_eq!(
        c.receive_record(&fork).unwrap(),
        Received::Pending(Waiting::Frozen)
    );
    // Another successor of C's head, which A never saw, and the rest of
    // the chain wait with it.
    let off = f.branch(Some(&r2), 2, "off");
    for r in [&off, &r3, &r4] {
        assert_eq!(
            c.receive_record(r).unwrap(),
            Received::Pending(Waiting::Frozen)
        );
    }
    // B, still a member, hands its writer off at r4, and the membership
    // changes: B's own handoff decides nothing about its fork.
    let claim = handoff(
        &f.d.b,
        &f.b,
        f.incarnation,
        Some(record_hash(&r4)),
        fresh(),
        0x5EED_0040,
    );
    for db in [&f.d.a, c] {
        db.apply_control(&claim, None).unwrap();
    }
    f.d.a.rotate_epoch().unwrap();
    deliver_controls(&f.d.a, c);
    assert_eq!(reason(c, &r3).as_deref(), Some("frozen"));
    assert_eq!(title(c, 12), None);
    // A removes B: its frontier closes B's writers, and the one handed off
    // ends at r4 through them. On C the closed prefix after its head
    // applies; the fork and the other branch never do.
    f.remove_b();
    assert_eq!(title(c, 12).as_deref(), Some("from B"));
    assert_eq!(title(c, 13).as_deref(), Some("from B"));
    assert_eq!(reason(c, &fork).as_deref(), Some("after-close"));
    assert_eq!(reason(c, &off).as_deref(), Some("after-close"));
    let past = f.branch(Some(&r4), 4, "past");
    assert_eq!(
        c.receive_record(&past).unwrap(),
        Received::Pending(Waiting::AfterClose)
    );
    // The evidence stays.
    assert_eq!(c.frozen_lineages().unwrap()[0].0, f.b.id());
    // B is invited again as a new member, and what it writes applies.
    let (again, _) = join(&f.d.a, &f.d.dir.path().join("b-again.db"));
    deliver_controls(&again, &f.d.a);
    deliver_controls(&f.d.a, c);
    again
        .add_task(&mut Task::new(id(30), "B again", 0.0))
        .unwrap();
    // Its writer is claimed by that first write.
    deliver_controls(&again, c);
    let fresh = records(&again).pop().unwrap();
    assert_eq!(c.receive_record(&fresh).unwrap(), Received::Applied);
}

#[test]
fn a_closed_prefix_that_does_not_lead_on_from_what_was_applied_applies_nothing() {
    let f = forked();
    let c = &f.d.c;
    let [r1, r2, r3, r4] = [0, 1, 2, 3].map(|n| f.chain[n].clone());
    // C applied a branch of r1 before r2; r2 then freezes the lineage.
    let branch = f.branch(Some(&r1), 1, "branch");
    assert_eq!(c.receive_record(&r1).unwrap(), Received::Applied);
    assert_eq!(c.receive_record(&branch).unwrap(), Received::Applied);
    for r in [&r2, &r3, &r4] {
        assert_eq!(
            c.receive_record(r).unwrap(),
            Received::Pending(Waiting::Frozen)
        );
    }
    // A closes B's writer at r4, on the chain C did not apply.
    f.remove_b();
    for r in [&r2, &r3, &r4] {
        assert_eq!(reason(c, r).as_deref(), Some("after-close"));
    }
    assert_eq!(title(c, 11), None);
}

#[test]
fn a_recovered_chain_received_out_of_order_applies_once_its_gap_fills() {
    let f = forked();
    let c = &f.d.c;
    let [r1, r2, r3, r4] = [0, 1, 2, 3].map(|n| f.chain[n].clone());
    assert_eq!(c.receive_record(&r1).unwrap(), Received::Applied);
    // A second root freezes the lineage while C's head is r1.
    let root = f.branch(None, 0, "root");
    assert_eq!(
        c.receive_record(&root).unwrap(),
        Received::Pending(Waiting::Frozen)
    );
    for r in [&r4, &r2] {
        assert_eq!(
            c.receive_record(r).unwrap(),
            Received::Pending(Waiting::Frozen)
        );
    }
    // Closed at r4: r4 is on the prefix, r2 not known to be until r3 is.
    f.remove_b();
    assert_eq!(reason(c, &r4).as_deref(), Some("predecessor"));
    assert_eq!(reason(c, &r2).as_deref(), Some("frozen"));
    assert_eq!(title(c, 11), None);
    // r3 arrives: the walk from r4 now reaches C's head through r2, and
    // the three apply in order.
    assert_eq!(
        c.receive_record(&r3).unwrap(),
        Received::Pending(Waiting::Predecessor)
    );
    for n in 11..14 {
        assert_eq!(title(c, n).as_deref(), Some("from B"), "{n}");
    }
    assert_eq!(reason(c, &root).as_deref(), Some("frozen"));
}

#[test]
fn a_competing_handoff_is_recovered_by_the_frontier_through_the_admitted_one() {
    let f = forked();
    let c = &f.d.c;
    let [r1, r2, r3, r4] = [0, 1, 2, 3].map(|n| f.chain[n].clone());
    assert_eq!(c.receive_record(&r1).unwrap(), Received::Applied);
    // B signs two handoffs from its writer. A and C admit the first; the
    // second freezes B's lineage on C.
    let first = handoff(
        &f.d.b,
        &f.b,
        f.incarnation,
        Some(record_hash(&r4)),
        fresh(),
        0x5EED_0001,
    );
    let second = handoff(
        &f.d.b,
        &f.b,
        f.incarnation,
        Some(record_hash(&r2)),
        fresh(),
        0x5EED_0002,
    );
    f.d.a.apply_control(&first, None).unwrap();
    c.apply_control(&first, None).unwrap();
    assert!(matches!(
        c.apply_control(&second, None),
        Err(MattersError::Authority(auth::Error::Fork))
    ));
    for r in [&r2, &r3] {
        assert_eq!(
            c.receive_record(r).unwrap(),
            Received::Pending(Waiting::Frozen)
        );
    }
    assert_eq!(
        c.unauthenticated_pending().unwrap().0,
        0,
        "their signatures were checked"
    );
    // A removes B. Its frontier names the replacement; the handed-off
    // writer is closed at r4 through it, on A and C alike.
    f.remove_b();
    assert_eq!(title(c, 11), None, "r4 is not here yet");
    assert_eq!(
        c.receive_record(&r4).unwrap(),
        Received::Pending(Waiting::Predecessor)
    );
    assert_eq!(title(c, 13).as_deref(), Some("from B"));
    assert!(c.pending_records().unwrap().is_empty());
}

// Limit 3: a replacement writer's content waits for the chain it replaced.

struct Replaced {
    d: Devices,
    b: DeviceKeys,
    old: [u8; 32],
    /// B's records r1, r2 under its first writer.
    chain: Vec<Vec<u8>>,
}

fn replaced() -> Replaced {
    let d = devices();
    for n in 0..2 {
        d.b.add_task(&mut Task::new(id(10 + n), "from B", n as f64))
            .unwrap();
    }
    let chain = records(&d.b);
    Replaced {
        b: keys(&d.b_keys),
        old: d.b.writer_incarnation().unwrap(),
        chain,
        d,
    }
}

impl Replaced {
    /// A handoff from `previous` closing at `closing`, taken by A and C,
    /// and the new writer's first record: an update with no dependency on
    /// the old chain.
    fn hand_off(
        &self,
        previous: [u8; 32],
        closing: Option<Hash>,
        actor: u64,
    ) -> ([u8; 32], Vec<u8>) {
        let incarnation = fresh();
        let claim = handoff(&self.d.b, &self.b, previous, closing, incarnation, actor);
        for db in [&self.d.a, &self.d.c] {
            db.apply_control(&claim, None).unwrap();
        }
        let first = data(
            &self.d.b,
            &self.b,
            incarnation,
            None,
            &update_after(&self.d.b, 0, actor, "replacement"),
        );
        (incarnation, first)
    }
}

#[test]
fn a_replacements_first_record_waits_for_the_closing_head_it_follows() {
    let p = replaced();
    let (r1, r2) = (&p.chain[0], &p.chain[1]);
    let (_, first) = p.hand_off(p.old, Some(record_hash(r2)), 0x5EED_0010);
    let c = &p.d.c;
    assert_eq!(c.receive_record(r1).unwrap(), Received::Applied);
    assert_eq!(
        c.receive_record(&first).unwrap(),
        Received::Pending(Waiting::Handoff)
    );
    // Other pages do not try it again; the closing head does.
    let before = receive::authentications();
    c.receive_records(&[]).unwrap();
    assert_eq!(receive::authentications(), before);
    let outcomes = c.receive_records(&[r2]).unwrap();
    assert_eq!(outcomes[0].as_ref().unwrap(), &Received::Applied);
    assert!(c.pending_records().unwrap().is_empty());
    // Where the closing head is held already, it applies at once.
    assert_eq!(p.d.a.receive_record(r1).unwrap(), Received::Applied);
    assert_eq!(p.d.a.receive_record(r2).unwrap(), Received::Applied);
    assert_eq!(p.d.a.receive_record(&first).unwrap(), Received::Applied);
}

#[test]
fn an_empty_intermediate_writer_does_not_bypass_an_earlier_closing_head() {
    let p = replaced();
    let (r1, r2) = (&p.chain[0], &p.chain[1]);
    let (middle, _) = p.hand_off(p.old, Some(record_hash(r2)), 0x5EED_0020);
    let (_, first) = p.hand_off(middle, None, 0x5EED_0021);
    let c = &p.d.c;
    assert_eq!(c.receive_record(r1).unwrap(), Received::Applied);
    assert_eq!(
        c.receive_record(&first).unwrap(),
        Received::Pending(Waiting::Handoff)
    );
    assert_eq!(c.receive_record(r2).unwrap(), Received::Applied);
    assert!(c.pending_records().unwrap().is_empty());
}

#[test]
fn a_refused_record_in_the_closed_prefix_blocks_the_replacement_for_good() {
    let p = replaced();
    let r1 = &p.chain[0];
    // B closes its first writer at a record that carries another actor's
    // update: every receiver refuses it.
    let foreign = loro::LoroDoc::new();
    foreign.set_peer_id(424242).unwrap();
    foreign.get_map("x").insert("k", 1).unwrap();
    foreign.commit();
    let bad = data(
        &p.d.b,
        &p.b,
        p.old,
        Some(record_hash(r1)),
        &foreign.export(loro::ExportMode::all_updates()).unwrap(),
    );
    let (_, first) = p.hand_off(p.old, Some(record_hash(&bad)), 0x5EED_0030);
    let c = &p.d.c;
    assert_eq!(c.receive_record(r1).unwrap(), Received::Applied);
    assert!(c.receive_record(&bad).is_err());
    assert_eq!(
        c.receive_record(&first).unwrap(),
        Received::Pending(Waiting::Handoff)
    );
    assert_eq!(c.receive_records(&[]).unwrap().len(), 0);
    assert_eq!(reason(c, &first).as_deref(), Some("handoff"));
}
