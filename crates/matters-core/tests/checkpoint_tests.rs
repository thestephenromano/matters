//! Checkpoints in Matters' database (#40, docs/SYNC_CHECKPOINTS.md). Joined
//! devices exchange records and checkpoint parts as a relay would deliver
//! them. The tests cover:
//! - a member with history accepting a checkpoint and importing nothing;
//! - a fresh device starting from one and following the tail;
//! - pending records travelling with a checkpoint;
//! - covered records met again after a relay restore;
//! - dishonest and wrong-epoch checkpoints, refused and kept;
//! - an older checkpoint never replacing a newer one;
//! - restarts during creation and publication.

use std::process::Command;

use matters_core::storage::checkpoint::{Checkpointed, DOCUMENT_SCHEMA, ENGINE};
use matters_core::storage::membership::{KeyStore, MemoryKeyStore, SyncStanding};
use matters_core::storage::receive::Received;
use matters_core::{Database, Task};
use matters_sync::authority::{parse, Body, DeviceKeys, Hash, Route};
use matters_sync::checkpoint::{self, Assembled, Checkpoint, Cursor, Sealed};
use matters_sync::record::record_hash;
use matters_sync::store::Log;
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

const GENERATION: [u8; 16] = [9; 16];

fn cursor(sequence: u64) -> Option<Cursor> {
    Some(Cursor {
        generation: GENERATION,
        sequence,
    })
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
/// envelope beside each transition.
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

/// Everything `db` wrote is acknowledged, as the relay would.
fn acknowledge_all(db: &Database) {
    for r in db.outgoing_records().unwrap() {
        db.acknowledge_record(&r.record_id).unwrap();
    }
}

fn parts(db: &Database) -> Vec<Vec<u8>> {
    db.outgoing_checkpoint_parts()
        .unwrap()
        .into_iter()
        .map(|(_, b)| b)
        .collect()
}

/// Every task's title and notes, by ID.
fn view(db: &Database) -> Vec<String> {
    (1..=12)
        .map(|n| {
            db.get_task(&id(n))
                .unwrap()
                .map_or("-".into(), |t| format!("{}|{}", t.title, t.notes))
        })
        .collect()
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
}

/// A with two tasks, B joined, each knowing the other's writer, and B
/// holding A's records.
fn pair() -> Pair {
    let dir = tempfile::tempdir().unwrap();
    let a = Database::open(dir.path().join("a.db")).unwrap();
    let a_keys = MemoryKeyStore::default();
    a.create_collection(&a_keys, route()).unwrap();
    a.add_task(&mut Task::new(id(1), "from A, one", 0.0))
        .unwrap();
    a.add_task(&mut Task::new(id(2), "from A, two", 1.0))
        .unwrap();
    let (b, _) = join(&a, &dir.path().join("b.db"));
    deliver_controls(&b, &a);
    for r in records(&a) {
        assert_eq!(b.receive_record(&r).unwrap(), Received::Applied);
    }
    Pair { dir, a, a_keys, b }
}

fn outcome(received: Received) -> Checkpointed {
    match received {
        Received::Checkpoint(c) => c,
        other => panic!("not a checkpoint part: {other:?}"),
    }
}

#[test]
fn a_checkpoint_waits_for_the_devices_own_records_to_be_acknowledged() {
    let p = pair();
    assert!(p.a.checkpoint_due(2).unwrap(), "two updates, no checkpoint");
    assert!(!p.a.checkpoint_due(3).unwrap());
    assert_eq!(p.a.create_checkpoint(cursor(2)).unwrap(), None);
    assert!(parts(&p.a).is_empty());
    acknowledge_all(&p.a);
    let made = p.a.create_checkpoint(cursor(2)).unwrap().unwrap();
    assert_eq!(parts(&p.a).len(), 1);
    // Nothing changed since: the same checkpoint, no second set of parts.
    assert_eq!(p.a.create_checkpoint(cursor(2)).unwrap(), Some(made));
    assert_eq!(parts(&p.a).len(), 1);
    let coverage = p.a.latest_checkpoint().unwrap().unwrap();
    assert_eq!(coverage.id().unwrap(), made);
    assert_eq!(
        (coverage.engine.as_str(), coverage.schema),
        (ENGINE, DOCUMENT_SCHEMA)
    );
    assert_eq!(coverage.writers.len(), 1, "A's chain, and nothing of B yet");
    assert_eq!(coverage.writers[0].records, 2);
    // Spaced from the latest: due again only after more updates.
    assert!(!p.a.checkpoint_due(1).unwrap());
    p.a.add_task(&mut Task::new(id(6), "one more", 5.0))
        .unwrap();
    assert!(p.a.checkpoint_due(1).unwrap());
    assert!(!p.a.checkpoint_due(2).unwrap());
}

#[test]
fn a_member_with_history_accepts_a_checkpoint_and_imports_nothing() {
    let p = pair();
    acknowledge_all(&p.a);
    let made = p.a.create_checkpoint(cursor(2)).unwrap().unwrap();
    let before = view(&p.b);
    let delivered = parts(&p.a);
    assert_eq!(
        outcome(p.b.receive_record(&delivered[0]).unwrap()),
        Checkpointed::Accepted
    );
    assert_eq!(view(&p.b), before);
    assert_eq!(
        p.b.latest_checkpoint().unwrap().unwrap().id().unwrap(),
        made
    );
    // Delivered again, it is a duplicate; its own author ignores it too.
    assert_eq!(
        outcome(p.b.receive_record(&delivered[0]).unwrap()),
        Checkpointed::Accepted
    );
    assert_eq!(
        p.a.receive_record(&delivered[0]).unwrap(),
        Received::Duplicate
    );
    assert!(p.b.pending_records().unwrap().is_empty());
}

#[test]
fn a_fresh_device_starts_from_a_checkpoint_and_follows_the_tail() {
    let p = pair();
    p.b.add_task(&mut Task::new(id(3), "from B", 2.0)).unwrap();
    for r in records(&p.b) {
        p.a.receive_record(&r).unwrap();
    }
    acknowledge_all(&p.a);
    // The relay holds A's two and B's one: sequences 1 to 3.
    p.a.create_checkpoint(cursor(3)).unwrap().unwrap();
    let (c, _) = join(&p.a, &p.dir.path().join("c.db"));
    deliver_controls(&p.b, &c);
    assert!(c.start_from_checkpoint(&parts(&p.a), GENERATION).unwrap());
    assert_eq!(view(&c), view(&p.a));
    assert_eq!(
        c.sync_cursor(Log::Records).unwrap().unwrap(),
        cursor(3).unwrap().relay_cursor(),
        "it reads on from the checkpoint's cursor"
    );
    // The tail: records after the cursor, from both writers.
    p.a.set_task_notes(&id(1), "after the checkpoint").unwrap();
    let tail_b = records(&p.b).len();
    p.b.rename_task(&id(3), "from B, renamed").unwrap();
    for r in records(&p.a)
        .into_iter()
        .chain(records(&p.b).into_iter().skip(tail_b))
    {
        assert_eq!(c.receive_record(&r).unwrap(), Received::Applied);
    }
    for r in records(&p.b).into_iter().skip(tail_b) {
        p.a.receive_record(&r).unwrap();
    }
    assert_eq!(view(&c), view(&p.a));
    assert!(c.pending_records().unwrap().is_empty());
    // A device that holds something cannot start from a checkpoint.
    assert!(c.start_from_checkpoint(&parts(&p.a), GENERATION).is_err());
}

#[test]
fn a_checkpoint_from_another_relay_generation_starts_nothing() {
    let p = pair();
    acknowledge_all(&p.a);
    p.a.create_checkpoint(cursor(2)).unwrap().unwrap();
    let (c, _) = join(&p.a, &p.dir.path().join("c.db"));
    assert!(!c.start_from_checkpoint(&parts(&p.a), [0; 16]).unwrap());
    assert_eq!(view(&c), vec!["-".to_string(); 12]);
    assert!(c.sync_cursor(Log::Records).unwrap().is_none());
}

#[test]
fn pending_records_travel_with_a_checkpoint_and_apply_later() {
    let p = pair();
    p.b.add_task(&mut Task::new(id(3), "B one", 2.0)).unwrap();
    p.b.add_task(&mut Task::new(id(4), "B two", 3.0)).unwrap();
    let from_b = records(&p.b);
    // A has B's second record, still waiting for the first.
    assert!(matches!(
        p.a.receive_record(&from_b[1]).unwrap(),
        Received::Pending(_)
    ));
    acknowledge_all(&p.a);
    p.a.create_checkpoint(cursor(4)).unwrap().unwrap();
    assert_eq!(p.a.latest_checkpoint().unwrap().unwrap().pending.len(), 1);
    let (c, _) = join(&p.a, &p.dir.path().join("c.db"));
    deliver_controls(&p.b, &c);
    assert!(c.start_from_checkpoint(&parts(&p.a), GENERATION).unwrap());
    assert_eq!(c.pending_records().unwrap().len(), 1, "carried across");
    // The missing first record arrives: both apply.
    assert_eq!(c.receive_record(&from_b[0]).unwrap(), Received::Applied);
    assert!(c.pending_records().unwrap().is_empty());
    assert_eq!(c.get_task(&id(4)).unwrap().unwrap().title, "B two");
}

#[test]
fn covered_records_met_again_after_a_relay_restore_are_recognised() {
    let p = pair();
    let from_a = records(&p.a);
    acknowledge_all(&p.a);
    p.a.create_checkpoint(cursor(2)).unwrap().unwrap();
    let (c, _) = join(&p.a, &p.dir.path().join("c.db"));
    assert!(c.start_from_checkpoint(&parts(&p.a), GENERATION).unwrap());
    let before = view(&c);
    // The relay is restored and C reads from sequence 1: A's covered
    // records arrive again, out of order, then a new one.
    let _ = c.receive_record(&from_a[1]).unwrap();
    let _ = c.receive_record(&from_a[0]).unwrap();
    assert!(c.pending_records().unwrap().is_empty(), "both recognised");
    assert_eq!(view(&c), before);
    assert_eq!(c.receive_record(&from_a[0]).unwrap(), Received::Duplicate);
    p.a.rename_task(&id(2), "after the restore").unwrap();
    let next = records(&p.a).pop().unwrap();
    assert_eq!(c.receive_record(&next).unwrap(), Received::Applied);
    assert_eq!(view(&c), view(&p.a));
    // C's own checkpoint lists A's chain at its full length.
    acknowledge_all(&c);
    c.create_checkpoint(cursor(3)).unwrap().unwrap();
    let writer = &c.latest_checkpoint().unwrap().unwrap().writers[0];
    assert_eq!(writer.records, 3);
}

/// The epoch key A seals under, from its stored envelope.
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

/// A's honest checkpoint, opened with A's keys.
fn opened(a: &Database, device: &DeviceKeys) -> Checkpoint {
    let (key, _, epoch) = current_key(a, device);
    let document = a.sync_membership().unwrap().document;
    let route = route();
    let plaintexts: Vec<Vec<u8>> = parts(a)
        .iter()
        .map(|bytes| {
            let p = wire::parse(bytes, route.content(&document)).unwrap();
            matters_sync::verify(p, device.verification_key())
                .unwrap()
                .open(&key)
                .unwrap()
        })
        .collect();
    let sealed = Sealed {
        author: device.id(),
        incarnation: a.writer_incarnation().unwrap(),
        epoch,
    };
    let with: Vec<(Sealed, &[u8])> = plaintexts
        .iter()
        .map(|p| (sealed.clone(), p.as_slice()))
        .collect();
    match checkpoint::assemble(&with).unwrap() {
        Assembled::Complete(c) => *c,
        other => panic!("{other:?}"),
    }
}

/// `c`'s parts, sealed and signed by A as snapshot records.
fn sealed_by(a: &Database, device: &DeviceKeys, c: &Checkpoint) -> Vec<Vec<u8>> {
    let (key, manifest, epoch) = current_key(a, device);
    let document = a.sync_membership().unwrap().document;
    let route = route();
    let incarnation = a.writer_incarnation().unwrap();
    let batch = fresh();
    c.parts()
        .unwrap()
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
                logical_batch: &batch,
                previous: None,
            };
            seal(header, &key, part, device.signing_key(), fresh()).unwrap()
        })
        .collect()
}

#[test]
fn dishonest_and_wrong_epoch_checkpoints_are_refused_and_kept() {
    let p = pair();
    let first = record_hash(&records(&p.a)[0]);
    acknowledge_all(&p.a);
    p.a.create_checkpoint(cursor(2)).unwrap().unwrap();
    let a_dev = keys(&p.a_keys);
    let honest = opened(&p.a, &a_dev);
    let before = view(&p.b);
    let mut cases: Vec<(&str, Checkpoint)> = Vec::new();
    // Another head where B holds A's chain as far.
    let mut c = honest.clone();
    c.coverage.writers[0].head = [7; 48];
    cases.push(("another head", c));
    // A version the snapshot does not hold.
    let mut c = honest.clone();
    c.coverage.version = Vec::new();
    cases.push(("does not match its coverage", c));
    // A's chain claimed under another author.
    let mut c = honest.clone();
    c.coverage.writers[0].author = [8; 48];
    cases.push(("another author", c));
    // Fewer records, at their true head, than the counter shows.
    let mut c = honest.clone();
    c.coverage.writers[0].records = 1;
    c.coverage.writers[0].head = first;
    cases.push(("counter disagrees", c));
    // A claimed head that B holds, but at another position (#45).
    let mut c = honest.clone();
    c.coverage.writers[0].records = 1;
    cases.push(("another head", c));
    // Coverage naming another epoch than its records.
    let mut c = honest.clone();
    c.coverage.epoch = 7;
    cases.push(("epoch", c));
    for (why, forged) in &cases {
        // Rebuild the statement around the forged coverage's own payload.
        let rebuilt = Checkpoint::new(
            checkpoint::Statement {
                engine: forged.coverage.engine.clone(),
                schema: forged.coverage.schema,
                epoch: forged.coverage.epoch,
                writers: forged.coverage.writers.clone(),
                version: forged.coverage.version.clone(),
                frontiers: forged.coverage.frontiers.clone(),
                cursor: forged.coverage.cursor.clone(),
            },
            forged.snapshot.clone(),
            forged.pending.clone(),
        )
        .unwrap();
        let records = sealed_by(&p.a, &a_dev, &rebuilt);
        match outcome(p.b.receive_record(&records[0]).unwrap()) {
            Checkpointed::Refused(reason) => {
                assert!(reason.contains(why), "{why}: refused as {reason}")
            }
            other => panic!("{why}: {other:?}"),
        }
        assert_eq!(view(&p.b), before, "{why}: nothing imported");
    }
    // Each is kept as evidence under its ID, but for the wrong epoch: that
    // is its author's sealing, not the checkpoint's, so its parts are
    // quarantined instead, and another member's parts of the same
    // checkpoint would still stand (#45).
    let kept = p.b.checkpoints().unwrap();
    assert_eq!(kept.len(), cases.len() - 1);
    assert!(kept.iter().all(|(_, state, _)| state == "refused"));
    assert!(p
        .b
        .quarantined_records()
        .unwrap()
        .iter()
        .any(|(_, (reason, _))| reason.contains("epoch")));
    assert!(p.b.latest_checkpoint().unwrap().is_none());
    // The honest one is still accepted afterwards.
    assert_eq!(
        outcome(p.b.receive_record(&parts(&p.a)[0]).unwrap()),
        Checkpointed::Accepted
    );
}

#[test]
fn an_older_checkpoint_never_replaces_a_newer_one() {
    let p = pair();
    acknowledge_all(&p.a);
    let older = p.a.create_checkpoint(cursor(2)).unwrap().unwrap();
    let older_parts = parts(&p.a);
    for (id, _) in p.a.outgoing_checkpoint_parts().unwrap() {
        p.a.acknowledge_checkpoint_part(&id).unwrap();
    }
    p.a.add_task(&mut Task::new(id(5), "later", 4.0)).unwrap();
    // B holds what the newer one covers, so it can verify it.
    p.b.receive_record(&records(&p.a).pop().unwrap()).unwrap();
    acknowledge_all(&p.a);
    let newer = p.a.create_checkpoint(cursor(4)).unwrap().unwrap();
    let newer_parts = parts(&p.a);
    // Delivered newest first, as a relay restoring a backup might.
    assert_eq!(
        outcome(p.b.receive_record(&newer_parts[0]).unwrap()),
        Checkpointed::Accepted
    );
    assert_eq!(
        outcome(p.b.receive_record(&older_parts[0]).unwrap()),
        Checkpointed::Superseded
    );
    assert_eq!(
        p.b.latest_checkpoint().unwrap().unwrap().id().unwrap(),
        newer
    );
    assert_ne!(older, newer);
}

#[test]
fn publication_survives_a_restart_and_a_relay_restore() {
    let p = pair();
    acknowledge_all(&p.a);
    p.a.create_checkpoint(cursor(2)).unwrap().unwrap();
    let first = p.a.outgoing_checkpoint_parts().unwrap();
    // A restarts before the relay acknowledged the parts: the same bytes
    // are offered again.
    let path = p.dir.path().join("a.db");
    drop(p.a);
    let a = Database::open(&path).unwrap();
    a.resume_sync(&p.a_keys).unwrap();
    assert_eq!(a.outgoing_checkpoint_parts().unwrap(), first);
    for (id, _) in &first {
        assert!(a.acknowledge_checkpoint_part(id).unwrap());
    }
    assert!(a.outgoing_checkpoint_parts().unwrap().is_empty());
    // A record that is not a part is not acknowledged as one.
    assert!(!a.acknowledge_checkpoint_part(&[0; 32]).unwrap());
    // The relay is restored: the parts are queued again, unchanged.
    a.sync_generation_changed().unwrap();
    assert_eq!(a.outgoing_checkpoint_parts().unwrap(), first);
}

/// Run in a child process: resume the database named by
/// `MATTERS_CRASH_DB` with the seeds in `MATTERS_CRASH_KEYS`, then make a
/// checkpoint that `MATTERS_CRASH_AT` interrupts. Ignored in ordinary
/// runs; the parent test runs it.
#[test]
#[ignore]
fn checkpoint_crash_worker() {
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
    db.create_checkpoint(cursor(2)).unwrap();
    unreachable!("the crash point exits the process");
}

#[test]
fn a_crash_while_creating_a_checkpoint_leaves_nothing_and_loses_nothing() {
    let p = pair();
    acknowledge_all(&p.a);
    let path = p.dir.path().join("a.db");
    let name = p.a_keys.names().remove(0);
    let mut saved = name.clone().into_bytes();
    saved.push(0);
    saved.extend(p.a_keys.load(&name).unwrap().unwrap().to_bytes());
    let keyfile = p.dir.path().join("keys");
    std::fs::write(&keyfile, saved).unwrap();
    let before = view(&p.a);
    drop(p.a);
    let status = Command::new(std::env::current_exe().unwrap())
        .args([
            "checkpoint_crash_worker",
            "--exact",
            "--ignored",
            "--nocapture",
        ])
        .env("MATTERS_CRASH_DB", &path)
        .env("MATTERS_CRASH_KEYS", &keyfile)
        .env("MATTERS_CRASH_AT", "checkpoint-before-commit")
        .status()
        .unwrap();
    assert_eq!(status.code(), Some(86), "exited at the crash point");
    let a = Database::open(&path).unwrap();
    a.resume_sync(&p.a_keys).unwrap();
    assert!(a.outgoing_checkpoint_parts().unwrap().is_empty());
    assert!(a.latest_checkpoint().unwrap().is_none());
    assert_eq!(view(&a), before);
    assert!(
        a.outgoing_records().unwrap().is_empty(),
        "nothing re-queued"
    );
    // Made again after the restart, it completes.
    a.create_checkpoint(cursor(2)).unwrap().unwrap();
    assert_eq!(a.outgoing_checkpoint_parts().unwrap().len(), 1);
}

#[test]
fn versions_encode_canonically_whatever_order_they_were_built_in() {
    use matters_core::crdt::{decode_version, encode_version};
    let mut forward = loro::VersionVector::new();
    let mut backward = loro::VersionVector::new();
    let peers = [(3u64, 7), (1, 2), (900, 1), (42, 10)];
    for (peer, counter) in peers {
        forward.insert(peer, counter);
    }
    for (peer, counter) in peers.iter().rev() {
        backward.insert(*peer, *counter);
    }
    assert_eq!(encode_version(&forward), encode_version(&backward));
    let decoded = decode_version(&encode_version(&forward)).unwrap();
    assert_eq!(
        decoded.into_iter().collect::<Vec<_>>(),
        [(1, 2), (3, 7), (42, 10), (900, 1)]
    );
    // Only the canonical form decodes.
    let mut swapped = encode_version(&forward);
    swapped.rotate_left(12);
    assert!(decode_version(&swapped).is_err(), "out of order");
    assert!(decode_version(&[0; 13]).is_err(), "a partial entry");
}

#[test]
fn deletions_survive_a_checkpoint_and_a_stale_edit_after_it() {
    let p = pair();
    // A deletes a task for good; B has not heard yet, and edits it.
    p.a.trash_task(&id(2)).unwrap();
    p.a.empty_trash().unwrap();
    p.b.rename_task(&id(2), "edited while A deleted it")
        .unwrap();
    acknowledge_all(&p.a);
    p.a.create_checkpoint(cursor(4)).unwrap().unwrap();
    let (c, _) = join(&p.a, &p.dir.path().join("c.db"));
    deliver_controls(&p.b, &c);
    assert!(c.start_from_checkpoint(&parts(&p.a), GENERATION).unwrap());
    assert_eq!(c.get_task(&id(2)).unwrap(), None, "the deletion travelled");
    // B's stale edit reaches both: they agree, as the merge policy says.
    for r in records(&p.b) {
        p.a.receive_record(&r).unwrap();
        c.receive_record(&r).unwrap();
    }
    assert_eq!(view(&c), view(&p.a));
    assert!(c.pending_records().unwrap().is_empty());
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

/// `c` with `edit` applied to its coverage, sealed by A under a new ID.
fn reworked(
    a: &Database,
    device: &DeviceKeys,
    c: &Checkpoint,
    edit: impl FnOnce(&mut checkpoint::Statement),
) -> Vec<Vec<u8>> {
    let mut statement = checkpoint::Statement {
        engine: c.coverage.engine.clone(),
        schema: c.coverage.schema,
        epoch: c.coverage.epoch,
        writers: c.coverage.writers.clone(),
        version: c.coverage.version.clone(),
        frontiers: c.coverage.frontiers.clone(),
        cursor: c.coverage.cursor.clone(),
    };
    // Sealed now, under A's current epoch.
    statement.epoch = current_key(a, device).2;
    edit(&mut statement);
    let rebuilt = Checkpoint::new(statement, c.snapshot.clone(), c.pending.clone()).unwrap();
    sealed_by(a, device, &rebuilt)
}

#[test]
fn a_started_device_judges_checkpoints_against_chains_it_knows_by_length() {
    // A's chain at one record (C1), then two (C2). C starts from C2, then
    // receives A's records 3 and 4: it knows records 1 and 2 by length only.
    let dir = tempfile::tempdir().unwrap();
    let a = Database::open(dir.path().join("a.db")).unwrap();
    let a_keys = MemoryKeyStore::default();
    a.create_collection(&a_keys, route()).unwrap();
    let a_dev = keys(&a_keys);
    a.add_task(&mut Task::new(id(1), "one", 0.0)).unwrap();
    acknowledge_all(&a);
    a.create_checkpoint(cursor(1)).unwrap().unwrap();
    let c1 = parts(&a);
    for (id, _) in a.outgoing_checkpoint_parts().unwrap() {
        a.acknowledge_checkpoint_part(&id).unwrap();
    }
    a.add_task(&mut Task::new(id(2), "two", 1.0)).unwrap();
    let second = records(&a).pop().unwrap();
    acknowledge_all(&a);
    a.create_checkpoint(cursor(2)).unwrap().unwrap();
    let c2_parts = parts(&a);
    let c2 = opened(&a, &a_dev);
    let (c, _) = join(&a, &dir.path().join("c.db"));
    assert!(c.holds_nothing_synced().unwrap());
    assert!(c.start_from_checkpoint(&c2_parts, GENERATION).unwrap());
    assert!(
        !c.holds_nothing_synced().unwrap(),
        "it holds the collection now"
    );
    a.add_task(&mut Task::new(id(3), "three", 2.0)).unwrap();
    a.add_task(&mut Task::new(id(4), "four", 3.0)).unwrap();
    let later = records(&a);
    let third = record_hash(&later[0]);
    for r in &later {
        assert_eq!(c.receive_record(r).unwrap(), Received::Applied);
    }
    // At a length inside what C knows only by length: an older checkpoint,
    // kept, never the latest. Delivered again, it is still that.
    assert_eq!(
        outcome(c.receive_record(&c1[0]).unwrap()),
        Checkpointed::Superseded
    );
    assert_eq!(
        outcome(c.receive_record(&c1[0]).unwrap()),
        Checkpointed::Superseded
    );
    assert_eq!(parts_kept(&c), 0, "a decided checkpoint keeps no parts");
    // At the start's length: the head C started from, or a fork.
    let at_base = reworked(&a, &a_dev, &c2, |s| s.cursor = cursor(3));
    assert_eq!(
        outcome(c.receive_record(&at_base[0]).unwrap()),
        Checkpointed::Accepted
    );
    let forked = reworked(&a, &a_dev, &c2, |s| {
        s.cursor = cursor(4);
        s.writers[0].head = [7; 48];
    });
    assert!(matches!(
        outcome(c.receive_record(&forked[0]).unwrap()),
        Checkpointed::Refused(r) if r.contains("another head")
    ));
    assert_eq!(
        parts_kept(&c),
        1,
        "a refused checkpoint's parts are kept as evidence"
    );
    // Past the start's length: a head C received, or a fork.
    let held = reworked(&a, &a_dev, &c2, |s| {
        s.cursor = cursor(5);
        s.writers[0].records = 3;
        s.writers[0].head = third;
    });
    assert_eq!(
        outcome(c.receive_record(&held[0]).unwrap()),
        Checkpointed::Accepted
    );
    let unknown = reworked(&a, &a_dev, &c2, |s| {
        s.cursor = cursor(6);
        s.writers[0].records = 3;
        s.writers[0].head = record_hash(&second);
    });
    assert!(matches!(
        outcome(c.receive_record(&unknown[0]).unwrap()),
        Checkpointed::Refused(r) if r.contains("another head")
    ));
}

#[test]
fn a_checkpoint_from_a_newer_layout_is_kept_and_never_used() {
    let p = pair();
    acknowledge_all(&p.a);
    p.a.create_checkpoint(cursor(2)).unwrap().unwrap();
    let a_dev = keys(&p.a_keys);
    let honest = opened(&p.a, &a_dev);
    let newer = reworked(&p.a, &a_dev, &honest, |s| s.schema = DOCUMENT_SCHEMA + 1);
    assert_eq!(
        outcome(p.b.receive_record(&newer[0]).unwrap()),
        Checkpointed::Superseded
    );
    assert!(p.b.latest_checkpoint().unwrap().is_none());
    // The current layout is judged as usual.
    let same = reworked(&p.a, &a_dev, &honest, |s| s.schema = DOCUMENT_SCHEMA);
    assert_eq!(
        outcome(p.b.receive_record(&same[0]).unwrap()),
        Checkpointed::Accepted
    );
}

#[test]
fn a_checkpoint_naming_a_writer_not_yet_admitted_waits_for_it() {
    let p = pair();
    let (d, _) = join(&p.a, &p.dir.path().join("d.db"));
    // A restarts: a new writer, whose claim D has not received but B has.
    let path = p.dir.path().join("a.db");
    drop(p.a);
    let a = Database::open(&path).unwrap();
    a.resume_sync(&p.a_keys).unwrap();
    a.add_task(&mut Task::new(id(5), "after the restart", 4.0))
        .unwrap();
    deliver_controls(&a, &p.b);
    for r in records(&a) {
        p.b.receive_record(&r).unwrap();
    }
    // B, a writer D knows, makes a checkpoint listing A's new chain.
    acknowledge_all(&p.b);
    p.b.create_checkpoint(cursor(5)).unwrap().unwrap();
    assert_eq!(
        outcome(d.receive_record(&parts(&p.b)[0]).unwrap()),
        Checkpointed::Part,
        "complete, but naming a writer D cannot check yet"
    );
    assert!(d.latest_checkpoint().unwrap().is_none());
    assert_eq!(parts_kept(&d), 1);
    // The claim arrives: the waiting checkpoint is judged.
    deliver_controls(&a, &d);
    assert!(
        d.latest_checkpoint().unwrap().is_some(),
        "accepted once the writer is known"
    );
    assert_eq!(parts_kept(&d), 0);
}

#[test]
fn frontiers_encode_in_one_canonical_order() {
    use matters_core::crdt::encode_frontiers;
    let one = loro::LoroDoc::new();
    one.set_peer_id(9).unwrap();
    one.get_map("m").insert("a", 1).unwrap();
    one.commit();
    let two = loro::LoroDoc::new();
    two.set_peer_id(3).unwrap();
    two.get_map("m").insert("b", 2).unwrap();
    two.commit();
    one.import(&two.export(loro::ExportMode::all_updates()).unwrap())
        .unwrap();
    two.import(&one.export(loro::ExportMode::all_updates()).unwrap())
        .unwrap();
    let bytes = encode_frontiers(&one.oplog_frontiers());
    assert_eq!(bytes, encode_frontiers(&two.oplog_frontiers()));
    // Two heads, ascending by actor: (3, 0) then (9, 0).
    let mut expected = Vec::new();
    for (peer, counter) in [(3u64, 0i32), (9, 0)] {
        expected.extend(peer.to_be_bytes());
        expected.extend(counter.to_be_bytes());
    }
    assert_eq!(bytes, expected);
}

#[test]
fn a_checkpoint_naming_another_head_of_a_devices_own_chain_is_a_fork() {
    let p = pair();
    for n in 20..23 {
        p.b.add_task(&mut Task::new(id(n), "B's", n as f64))
            .unwrap();
    }
    deliver_controls(&p.b, &p.a);
    for r in records(&p.b) {
        p.a.receive_record(&r).unwrap();
    }
    acknowledge_all(&p.a);
    p.a.create_checkpoint(cursor(5)).unwrap().unwrap();
    let a_dev = keys(&p.a_keys);
    let honest = opened(&p.a, &a_dev);
    let b_inc = p.b.writer_incarnation().unwrap();
    // B's own chain, claimed shorter, with a head B never wrote.
    let forged = reworked(&p.a, &a_dev, &honest, |s| {
        let w = s
            .writers
            .iter_mut()
            .find(|w| w.incarnation == b_inc)
            .unwrap();
        w.records = 2;
        w.head = [9; 48];
    });
    assert!(matches!(
        outcome(p.b.receive_record(&forged[0]).unwrap()),
        Checkpointed::Refused(r) if r.contains("another head")
    ));
    // The honest one names B's chain as B holds it.
    assert_eq!(
        outcome(p.b.receive_record(&parts(&p.a)[0]).unwrap()),
        Checkpointed::Accepted
    );
}

#[test]
fn covered_records_met_again_in_chain_order_keep_their_positions() {
    let p = pair();
    let from_a = records(&p.a);
    assert_eq!(from_a.len(), 2);
    acknowledge_all(&p.a);
    p.a.create_checkpoint(cursor(2)).unwrap().unwrap();
    let (c, _) = join(&p.a, &p.dir.path().join("c.db"));
    assert!(c.start_from_checkpoint(&parts(&p.a), GENERATION).unwrap());
    // The first record waits; the covered head then walks back to it.
    let _ = c.receive_record(&from_a[0]).unwrap();
    let _ = c.receive_record(&from_a[1]).unwrap();
    assert!(c.pending_records().unwrap().is_empty(), "both recognised");
    let positions: Vec<(Vec<u8>, i64)> = c
        .with_conn(|conn| {
            let mut stmt =
                conn.prepare("SELECT digest, position FROM sync_covered ORDER BY position")?;
            let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
            Ok(rows.collect::<rusqlite::Result<_>>()?)
        })
        .unwrap();
    let expected: Vec<(Vec<u8>, i64)> = from_a
        .iter()
        .enumerate()
        .map(|(i, r)| (record_hash(r).to_vec(), i as i64 + 1))
        .collect();
    assert_eq!(positions, expected);
}

#[test]
fn a_checkpoint_carries_at_most_its_budget_of_pending_records() {
    let p = pair();
    acknowledge_all(&p.a);
    let carry = |n: usize| {
        p.a.with_raw_conn(|c| {
            c.execute("DELETE FROM sync_pending", [])?;
            Ok(c.execute(
                "WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < ?1)
                 INSERT INTO sync_pending (record_id, digest, bytes, reason, authenticated)
                     SELECT randomblob(32), randomblob(48), randomblob(24), 'carried', 1 FROM n",
                [n as i64],
            )?)
        })
        .unwrap()
    };
    carry(checkpoint::MAX_PENDING);
    let id = p.a.create_checkpoint(cursor(2)).unwrap();
    assert!(id.is_some(), "exactly the budget is carried");
    assert_eq!(
        p.a.latest_checkpoint().unwrap().unwrap().pending.len(),
        checkpoint::MAX_PENDING
    );
    carry(checkpoint::MAX_PENDING + 1);
    assert_eq!(
        p.a.create_checkpoint(cursor(3)).unwrap(),
        None,
        "past it, none"
    );
}
