//! Fuzz entry points for receiving (#45): the bodies of the cargo-fuzz
//! targets `engine_import` and `receive` in `fuzz/`, shared with the
//! stable replay test (`tests/fuzz_replay_tests.rs`). Only with the
//! `fuzzing` feature.
//!
//! - [`engine_import`] gives member-authored engine bytes to everything
//!   that decodes them: an update's peers and dependencies, a snapshot's
//!   version, a merge, and the projection after it.
//! - [`receive`] is stateful: a database that has joined a collection
//!   receives a sequence of records and controls, some raw, some sealed
//!   and signed by an admitted member's writer around fuzzed plaintext or a
//!   fuzzed engine edit, so the receiver's logic past authentication is
//!   reached. Whatever arrives, the store never fails, the engine and the
//!   rows stay in step, and local writes go on.
//!
//! The fixture's keys come from the operating system's randomness when it
//! is first built: never use them for anything real.

use std::path::PathBuf;
use std::sync::OnceLock;

use matters_sync::authority::{self, Authority, Body, DeviceKeys, Hash, Route};
use matters_sync::record::record_hash;
use matters_sync::{seal, wire, ContentKey};

use crate::crdt::{self, Document};
use crate::storage::membership::{KeyStore, MemoryKeyStore};
use crate::{Database, Task};

/// Engine bytes from a member: decoded, merged and projected, every
/// failure an error.
pub fn engine_import(data: &[u8]) {
    let _ = crdt::update_peers(data);
    let _ = crdt::snapshot_version(data);
    let mut doc = Document::new(1).expect("an empty document");
    let _ = doc.missing_dependency(data);
    if doc.merge_unindexed(data).is_ok() {
        let _ = doc.settle();
        let _ = doc.snapshot();
    }
    // Into a document with history of its own, too.
    let mut held = Document::new(2).expect("an empty document");
    let seed = loro::LoroDoc::new();
    let _ = seed.set_peer_id(3);
    let _ = seed.get_map("x").insert("held", 1);
    seed.commit();
    if let Ok(history) = seed.export(loro::ExportMode::all_updates()) {
        let _ = held.merge(&history);
    }
    if held.merge_unindexed(data).is_ok() {
        let _ = held.settle();
    }
}

fn route() -> Route {
    Route {
        application: [1; 16],
        tenant: [2; 16],
        collection: [3; 16],
    }
}

/// A collection A administers and B writes to: a saved copy of A's
/// database, which knows B's writer, and what B seals with.
struct Fixture {
    dir: PathBuf,
    template: PathBuf,
    a_keys: MemoryKeyStore,
    b: DeviceKeys,
    key: ContentKey,
    manifest: Hash,
    epoch: u64,
    document: [u8; 16],
    incarnation: [u8; 32],
    actor: u64,
}

fn keys_of(store: &MemoryKeyStore) -> DeviceKeys {
    let names = store.names();
    store
        .load(&names[0])
        .expect("fixture keys")
        .expect("fixture keys")
        .keys()
        .expect("fixture keys")
}

fn deliver(from: &Database, to: &Database) {
    for o in from.outgoing_controls().expect("fixture controls") {
        let _ = to.apply_control(&o.bytes, None);
    }
}

/// The content key of `db`'s current epoch, as `device` unwraps it.
fn content_key(db: &Database, device: &DeviceKeys) -> ContentKey {
    let m = db.sync_membership().expect("fixture membership");
    let (_, envelope) = db
        .sync_envelopes()
        .expect("fixture envelopes")
        .into_iter()
        .find(|(manifest, _)| *manifest == m.manifest)
        .expect("fixture envelope");
    let controls = db.sync_controls().expect("fixture controls");
    let Body::Genesis(g) = authority::parse(&controls[0])
        .expect("fixture genesis")
        .message
        .body
    else {
        unreachable!("the first control is the genesis")
    };
    let mut a = Authority::bootstrap(&controls[0], route(), &g.root_key).expect("fixture root");
    for c in &controls[1..] {
        match authority::parse(c).expect("fixture control").message.body {
            Body::Membership(_) => {
                let _ = a.apply_membership(c);
            }
            Body::WriterStart(_) | Body::WriterHandoff(_) => {
                let _ = a.admit_writer(c, m.document);
            }
            _ => {}
        }
    }
    a.unwrap_history_key(&envelope, device)
        .expect("fixture key")
        .into_content_key()
}

/// The fixture's databases live in a directory of the system's temporary
/// directory, named for the process that made it.
const FIXTURE_PREFIX: &str = "matters-fuzz-";

/// Remove the fixture directories of processes that have exited: a fuzzer
/// or test process cannot remove its own at exit.
fn remove_stale_fixtures() {
    let Ok(entries) = std::fs::read_dir(std::env::temp_dir()) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(pid) = name
            .to_str()
            .and_then(|n| n.strip_prefix(FIXTURE_PREFIX))
            .and_then(|p| p.parse::<u32>().ok())
        else {
            continue;
        };
        if pid != std::process::id() && !std::path::Path::new(&format!("/proc/{pid}")).exists() {
            let _ = std::fs::remove_dir_all(entry.path());
        }
    }
}

/// Remove this process's fixture directory, as a test does when it is
/// done with [`receive`]. A later call builds the fixture's databases anew
/// only if the directory is gone, so call it last.
pub fn remove_fixture() {
    if let Some(f) = FIXTURE.get() {
        let _ = std::fs::remove_dir_all(&f.dir);
    }
}

static FIXTURE: OnceLock<Fixture> = OnceLock::new();

fn fixture() -> &'static Fixture {
    FIXTURE.get_or_init(|| {
        remove_stale_fixtures();
        let dir = std::env::temp_dir().join(format!("{FIXTURE_PREFIX}{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("fixture directory");
        let template = dir.join("a.db");
        let a_keys = MemoryKeyStore::default();
        let b_keys = MemoryKeyStore::default();
        let a = Database::open(&template).expect("fixture database");
        a.create_collection(&a_keys, route())
            .expect("fixture collection");
        let b = Database::open(dir.join("b.db")).expect("fixture database");
        let shown = a.invite().expect("fixture invitation");
        let request = b
            .start_enrollment(&b_keys, &shown)
            .expect("fixture request");
        let b_id = authority::parse(&request)
            .expect("fixture request")
            .message
            .signer;
        a.admit(&request, &shown).expect("fixture admission");
        let envelopes: Vec<Vec<u8>> = a
            .outgoing_controls()
            .expect("fixture controls")
            .into_iter()
            .filter(|o| {
                matches!(authority::parse(&o.bytes).map(|p| p.message.body),
                    Ok(Body::KeyEnvelope(ref k)) if k.recipient == b_id)
            })
            .map(|o| o.bytes)
            .collect();
        b.finish_enrollment(&b_keys, &a.sync_controls().expect("controls"), &envelopes)
            .expect("fixture enrollment");
        deliver(&b, &a);
        let device = keys_of(&b_keys);
        let membership = b.sync_membership().expect("fixture membership");
        let actor = b
            .outgoing_controls()
            .expect("fixture controls")
            .iter()
            .find_map(|o| match authority::parse(&o.bytes).ok()?.message.body {
                Body::WriterStart(w) => Some(w.actor),
                Body::WriterHandoff(w) => Some(w.actor),
                _ => None,
            })
            .expect("fixture writer");
        let fixture = Fixture {
            key: content_key(&b, &device),
            manifest: membership.manifest,
            epoch: membership.epoch.expect("fixture epoch"),
            document: membership.document,
            incarnation: b.writer_incarnation().expect("fixture writer"),
            actor,
            b: device,
            dir: dir.clone(),
            template: template.clone(),
            a_keys,
        };
        drop(a);
        drop(b);
        fixture
    })
}

/// A record B's writer seals: `kind`, after `previous`, carrying
/// `plaintext`, its record ID derived from `n`.
fn sealed_by_b(
    f: &Fixture,
    kind: wire::Kind,
    previous: Option<Hash>,
    n: u32,
    plaintext: &[u8],
) -> Option<Vec<u8>> {
    let route = route();
    let mut record_id = [0x5a; 32];
    record_id[..4].copy_from_slice(&n.to_be_bytes());
    let header = wire::Header {
        context: route.content(&f.document),
        kind,
        epoch: f.epoch,
        manifest: &f.manifest,
        author: &f.b.id(),
        incarnation: &f.incarnation,
        record_id: &record_id,
        logical_batch: &[0x5b; 32],
        previous: previous.as_ref(),
    };
    seal(header, &f.key, plaintext, f.b.signing_key(), [0x5c; 32]).ok()
}

/// A small engine edit by `actor` on top of `mirror`, from `script`: its
/// update. Some edits break the document's layout on purpose.
fn edit(mirror: &loro::LoroDoc, actor: u64, script: &[u8]) -> Option<Vec<u8>> {
    let before = mirror.oplog_vv();
    mirror.set_peer_id(actor).ok()?;
    let tree = mirror.get_tree("home");
    for op in script.chunks(2) {
        let (code, value) = (op[0], op.get(1).copied().unwrap_or(0));
        match code % 5 {
            0 => {
                mirror
                    .get_map("x")
                    .insert(&format!("k{}", value % 8), i64::from(value))
                    .ok()?;
            }
            1 => {
                let node = tree.create(loro::TreeParentId::Root).ok()?;
                let meta = tree.get_meta(node).ok()?;
                meta.insert(
                    "id",
                    format!("00000000-0000-0000-0000-0000000000{value:02x}"),
                )
                .ok()?;
                meta.insert("kind", "task").ok()?;
                meta.insert("title", format!("t{value}")).ok()?;
            }
            2 => {
                // A node with no ID or kind.
                tree.create(loro::TreeParentId::Root).ok()?;
            }
            3 => {
                let nodes = tree.nodes();
                if let Some(node) = nodes.get(usize::from(value) % nodes.len().max(1)) {
                    let _ = tree.delete(*node);
                }
            }
            _ => {
                mirror
                    .get_text("t")
                    .insert(0, &char::from(b'a' + value % 26).to_string())
                    .ok()?;
            }
        }
    }
    mirror.commit();
    mirror.export(loro::ExportMode::updates(&before)).ok()
}

/// A database that joined a collection receives a fuzzed sequence of
/// operations, each `[op, length (2 bytes, big-endian), payload]`:
/// - 0: raw bytes, as a record;
/// - 1: a data record B's writer seals around the payload, after the
///   record its first byte picks among those sealed so far (or none);
/// - 2: a data record B's writer seals around an engine edit the payload
///   scripts, after the last such record;
/// - 3: a checkpoint part B's writer seals around the payload;
/// - 4: the payload as a control;
/// - 5: the records so far, received as one page.
///
/// Whatever arrives: receiving never fails as a store, a refused record
/// is the only error, the rows can be rebuilt from the engine, and a local
/// write still works.
pub fn receive(data: &[u8]) {
    let f = fixture();
    let case = f.dir.join("case.db");
    for suffix in ["", "-wal", "-shm"] {
        let _ = std::fs::remove_file(format!("{}{suffix}", case.display()));
    }
    std::fs::copy(&f.template, &case).expect("a copy of the fixture");
    let db = Database::open(&case).expect("the fixture opens");
    db.resume_sync(&f.a_keys).expect("the fixture resumes");
    let mirror = loro::LoroDoc::new();
    let mut chain: Vec<Hash> = Vec::new();
    let mut edits: Option<Hash> = None;
    let mut page: Vec<Vec<u8>> = Vec::new();
    let mut n = 0u32;
    let mut rest = data;
    let flush = |page: &mut Vec<Vec<u8>>| {
        let borrowed: Vec<&[u8]> = page.iter().map(Vec::as_slice).collect();
        let outcomes = db
            .receive_records(&borrowed)
            .expect("receiving never fails as a store");
        assert_eq!(outcomes.len(), page.len());
        for outcome in outcomes {
            if let Err(e) = outcome {
                assert!(
                    matches!(&e, crate::MattersError::Validation(m) if m.starts_with("record refused")),
                    "only a refusal is a record's error: {e}"
                );
            }
        }
        page.clear();
    };
    while let [op, hi, lo, tail @ ..] = rest {
        let len = usize::from(u16::from_be_bytes([*hi, *lo])).min(tail.len());
        let (payload, next) = tail.split_at(len);
        rest = next;
        n += 1;
        match op % 6 {
            0 => page.push(payload.to_vec()),
            1 => {
                let previous = payload
                    .first()
                    .and_then(|&p| {
                        (p != 0).then(|| chain.get(usize::from(p - 1) % chain.len().max(1)))
                    })
                    .flatten()
                    .copied();
                if let Some(r) = sealed_by_b(f, wire::Kind::Data, previous, n, payload) {
                    chain.push(record_hash(&r));
                    page.push(r);
                }
            }
            2 => {
                if let Some(update) = edit(&mirror, f.actor, payload) {
                    if let Some(r) = sealed_by_b(f, wire::Kind::Data, edits, n, &update) {
                        let h = record_hash(&r);
                        chain.push(h);
                        edits = Some(h);
                        page.push(r);
                    }
                }
            }
            3 => {
                if let Some(r) = sealed_by_b(f, wire::Kind::Snapshot, None, n, payload) {
                    page.push(r);
                }
            }
            4 => {
                let _ = db.apply_control(payload, None);
            }
            _ => flush(&mut page),
        }
    }
    flush(&mut page);
    db.rebuild_from_engine()
        .expect("the rows rebuild from the engine");
    db.add_task(&mut Task::new(
        "00000000-0000-0000-0000-00000000ffff".to_string(),
        "local",
        0.0,
    ))
    .expect("a local write still works");
    drop(db);
}
