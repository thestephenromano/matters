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
    // More records than the counter shows.
    let mut c = honest.clone();
    c.coverage.writers[0].records = 1;
    cases.push(("counter disagrees", c));
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
    let kept = p.b.checkpoints().unwrap();
    assert_eq!(kept.len(), cases.len());
    assert!(kept.iter().all(|(_, state, _)| state == "refused"));
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
