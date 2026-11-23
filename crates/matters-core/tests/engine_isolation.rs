//! Untrusted engine bytes decoded in a real child process with limits
//! (#45, `crdt::isolation`). This binary is its own checker: `main` calls
//! the child entry first, as Matters' binaries do, so it runs without the
//! test harness (`harness = false`), its tests one after another.
//!
//! The hostile input is an engine update a member can sign: one change
//! whose op columns run-length encode `n` zero-length map deletions, which
//! read no value bytes (docs/SYNC_CHECKPOINTS.md, "Limits"). Under 100
//! bytes for any `n`, it costs Loro about 130 bytes of memory per op to
//! decode. The budgets here are deliberately low, so a small `n` exceeds
//! them quickly.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use matters_core::crdt::isolation::{
    self, Blob, Budget, Checker, ChildProcess, Verdict, CHECK_ARG, HELLO,
};
use matters_core::storage::checkpoint::Checkpointed;
use matters_core::storage::membership::{KeyStore, MemoryKeyStore, SyncStanding};
use matters_core::storage::receive::{self, Received};
use matters_core::{Database, MattersError, Task};
use matters_sync::authority::{parse, Body, DeviceKeys, Hash, Route};
use matters_sync::checkpoint::Cursor;
use matters_sync::record::record_hash;
use matters_sync::{seal, wire, ContentKey};

// The hostile update.

/// XXH32, which Loro checksums its blobs with.
fn xxh32(data: &[u8], seed: u32) -> u32 {
    const P1: u32 = 2654435761;
    const P2: u32 = 2246822519;
    const P3: u32 = 3266489917;
    const P4: u32 = 668265263;
    const P5: u32 = 374761393;
    let round = |acc: u32, lane: u32| {
        acc.wrapping_add(lane.wrapping_mul(P2))
            .rotate_left(13)
            .wrapping_mul(P1)
    };
    let word = |b: &[u8]| u32::from_le_bytes(b[..4].try_into().unwrap());
    let mut rest = data;
    let mut h = if data.len() >= 16 {
        let mut v = [
            seed.wrapping_add(P1).wrapping_add(P2),
            seed.wrapping_add(P2),
            seed,
            seed.wrapping_sub(P1),
        ];
        while rest.len() >= 16 {
            for (i, acc) in v.iter_mut().enumerate() {
                *acc = round(*acc, word(&rest[4 * i..]));
            }
            rest = &rest[16..];
        }
        v[0].rotate_left(1)
            .wrapping_add(v[1].rotate_left(7))
            .wrapping_add(v[2].rotate_left(12))
            .wrapping_add(v[3].rotate_left(18))
    } else {
        seed.wrapping_add(P5)
    };
    h = h.wrapping_add(data.len() as u32);
    while rest.len() >= 4 {
        h = h
            .wrapping_add(word(rest).wrapping_mul(P3))
            .rotate_left(17)
            .wrapping_mul(P4);
        rest = &rest[4..];
    }
    for &b in rest {
        h = h
            .wrapping_add(u32::from(b).wrapping_mul(P5))
            .rotate_left(11)
            .wrapping_mul(P1);
    }
    h ^= h >> 15;
    h = h.wrapping_mul(P2);
    h ^= h >> 13;
    h = h.wrapping_mul(P3);
    h ^ (h >> 16)
}

fn put_varint(out: &mut Vec<u8>, mut v: u64) {
    loop {
        let byte = (v & 0x7f) as u8;
        v >>= 7;
        if v == 0 {
            out.push(byte);
            return;
        }
        out.push(byte | 0x80);
    }
}

fn take_varint(bytes: &mut &[u8]) -> u64 {
    let mut v = 0;
    for shift in (0..64).step_by(7) {
        let byte = bytes[0];
        *bytes = &bytes[1..];
        v |= u64::from(byte & 0x7f) << shift;
        if byte & 0x80 == 0 {
            break;
        }
    }
    v
}

const LORO_SEED: u32 = u32::from_le_bytes(*b"LORO");

/// An update by `peer` of one change holding `n` zero-length deletions of
/// one map key. It is Loro's own encoding of a single deletion, with the
/// op columns (four run-length columns of one run each) rewritten to say
/// `n`, and the checksum set again.
fn bomb(peer: u64, n: u64) -> Vec<u8> {
    let doc = loro::LoroDoc::new();
    doc.set_peer_id(peer).unwrap();
    let map = doc.get_map("m");
    map.insert("k", 1).unwrap();
    doc.commit();
    let before = doc.oplog_vv();
    map.delete("k").unwrap();
    doc.commit();
    let honest = doc.export(loro::ExportMode::updates(&before)).unwrap();
    // Magic (4), checksum (16, XXH32 in its last 4), mode (2), then one
    // block, its length first.
    assert_eq!(
        xxh32(&honest[20..], LORO_SEED).to_le_bytes(),
        honest[16..20]
    );
    let mut body = &honest[22..];
    let len = take_varint(&mut body) as usize;
    assert_eq!(len, body.len(), "one block");
    // The block (postcard): five counts, then eight byte strings, the
    // sixth of them the op columns.
    let mut block = Vec::new();
    for _ in 0..5 {
        put_varint(&mut block, take_varint(&mut body));
    }
    for field in 0..8 {
        let len = take_varint(&mut body) as usize;
        let (bytes, rest) = body.split_at(len);
        body = rest;
        if field != 5 {
            put_varint(&mut block, len as u64);
            block.extend(bytes);
            continue;
        }
        // Container (delta), key (delta), value kind (8: a deletion),
        // length: each one run of `n`, its count zigzag-encoded.
        let mut ops = vec![1, 4];
        for value in [0, 0, 8, 0] {
            let mut column = Vec::new();
            put_varint(&mut column, 2 * n);
            column.push(value);
            put_varint(&mut ops, column.len() as u64);
            ops.extend(column);
        }
        put_varint(&mut block, ops.len() as u64);
        block.extend(ops);
    }
    let mut out = honest[..22].to_vec();
    put_varint(&mut out, block.len() as u64);
    out.extend(block);
    let sum = xxh32(&out[20..], LORO_SEED);
    out[16..20].copy_from_slice(&sum.to_le_bytes());
    out
}

/// Ops enough to pass the low budgets below many times over, in any build.
const OPS: u64 = 1_000_000;

fn low(memory: u64, cpu_seconds: u64, wall: Duration) -> Budget {
    Budget {
        memory,
        memory_per_mib: 0,
        cpu_seconds,
        cpu_millis_per_mib: 0,
        wall,
    }
}

const MIB: u64 = 1 << 20;

fn this_binary() -> ChildProcess {
    let budget = low(16 * MIB, 20, Duration::from_secs(30));
    ChildProcess::new(std::env::current_exe().unwrap()).with_budgets(budget, budget)
}

/// Counts the checks it passes on, and their blobs.
struct Counted {
    inner: ChildProcess,
    calls: AtomicUsize,
    blobs: AtomicUsize,
}

impl Counted {
    fn asked(&self) -> (usize, usize) {
        (
            self.calls.load(Ordering::SeqCst),
            self.blobs.load(Ordering::SeqCst),
        )
    }
}

impl Checker for Counted {
    fn check(&self, kind: Blob, blobs: &[&[u8]]) -> matters_core::Result<Vec<Verdict>> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.blobs.fetch_add(blobs.len(), Ordering::SeqCst);
        self.inner.check(kind, blobs)
    }
}

fn refused_for_budget(verdict: &Verdict) -> bool {
    matches!(verdict, Verdict::Refuse(r) if r.starts_with("the engine could not decode it within its budget"))
}

/// Out of address space, the decoder's allocation fails, which aborts.
fn refused_for_memory(verdict: &Verdict) -> bool {
    refused_for_budget(verdict)
        && matches!(verdict, Verdict::Refuse(r) if r.ends_with("it stopped the decoder (signal 6)"))
}

// The checker, on its own.

fn the_bomb_decodes_in_process_with_a_small_count() {
    // The fixture is what it claims: Loro takes it, and its one change
    // holds the ops it says, so its cost grows with them.
    let small = bomb(5, 3);
    let meta = loro::LoroDoc::decode_import_blob_meta(&small, true).unwrap();
    assert_eq!(meta.change_num, 1);
    assert!(meta.partial_end_vv.contains_key(&5));
    assert!(bomb(5, OPS).len() < 100);
}

fn the_child_refuses_only_the_blobs_that_exceed_its_budget() {
    let honest = bomb(5, 1);
    let hostile = bomb(5, OPS);
    let verdicts = this_binary()
        .check(Blob::Update, &[&honest, &hostile, &honest, &hostile])
        .unwrap();
    assert_eq!(verdicts[0], Verdict::Pass);
    assert!(refused_for_memory(&verdicts[1]), "{verdicts:?}");
    assert_eq!(verdicts[2], Verdict::Pass);
    assert!(refused_for_memory(&verdicts[3]), "{verdicts:?}");
    assert_eq!(verdicts.len(), 4);
}

/// An honest update of `tasks` tasks created in one commit.
fn created(tasks: u128) -> Vec<u8> {
    use matters_core::crdt::{Anchor, Command, Document, Kind};
    let mut doc = Document::new(5).unwrap();
    let before = doc.version();
    let commands: Vec<Command> = (0..tasks)
        .map(|n| Command::Create {
            id: id(100 + n),
            kind: Kind::Task,
            parent: None,
            at: Anchor::Last,
            title: format!("Task {n}"),
            created_at: 0,
        })
        .collect();
    doc.apply(&commands, 1).unwrap();
    doc.updates_since(&before).unwrap()
}

fn the_running_binary_with_the_default_budgets_tells_them_apart() {
    let verdicts = ChildProcess::this_executable()
        .check(Blob::Update, &[&created(2000), &bomb(5, 1_000_000_000)])
        .unwrap();
    assert_eq!(verdicts[0], Verdict::Pass);
    assert!(refused_for_budget(&verdicts[1]), "{verdicts:?}");
}

fn reading_a_blob_is_not_part_of_its_budget() {
    // Larger than the budget, but cheap to decode: it is no engine blob.
    let large = vec![0; 40 * MIB as usize];
    let verdicts = this_binary()
        .check(Blob::Update, &[&bomb(5, 1), &large])
        .unwrap();
    assert_eq!(verdicts, vec![Verdict::Pass, Verdict::Pass]);
}

/// A limit of process `pid`, soft and hard, from /proc/<pid>/limits.
fn limit_of(pid: u32, name: &str) -> (String, String) {
    let limits = std::fs::read_to_string(format!("/proc/{pid}/limits")).unwrap();
    let line = limits.lines().find(|l| l.starts_with(name)).unwrap();
    let fields: Vec<&str> = line[name.len()..].split_whitespace().collect();
    (fields[0].into(), fields[1].into())
}

fn the_child_limits_itself_for_each_blob() {
    use std::io::{Read, Write};
    let mut child = std::process::Command::new(std::env::current_exe().unwrap())
        .args([CHECK_ARG, "update", &(16 * MIB).to_string(), "0", "20", "0"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let pid = child.id();
    let mut stdout = child.stdout.take().unwrap();
    let mut hello = vec![0; HELLO.len()];
    stdout.read_exact(&mut hello).unwrap();
    assert_eq!(hello, HELLO);
    // Before any blob: no core file, and first for the out-of-memory
    // killer.
    assert_eq!(
        limit_of(pid, "Max core file size"),
        ("0".into(), "0".into())
    );
    let adjusted = std::fs::read_to_string(format!("/proc/{pid}/oom_score_adj")).unwrap();
    assert_eq!(adjusted.trim(), "1000");
    let blob = bomb(5, 1);
    let mut stdin = child.stdin.take().unwrap();
    stdin.write_all(&(blob.len() as u64).to_le_bytes()).unwrap();
    stdin.write_all(&blob).unwrap();
    stdin.flush().unwrap();
    let mut passed = [0];
    stdout.read_exact(&mut passed).unwrap();
    // Waiting for the next blob, it holds this one's limits: its address
    // space then and the budget, and its processor time then and the
    // budget.
    let status = std::fs::read_to_string(format!("/proc/{pid}/status")).unwrap();
    let size: u64 = status
        .lines()
        .find_map(|l| l.strip_prefix("VmSize:"))
        .unwrap()
        .trim()
        .trim_end_matches("kB")
        .trim()
        .parse::<u64>()
        .unwrap()
        * 1024;
    let (soft, _) = limit_of(pid, "Max address space");
    let room = soft.parse::<u64>().unwrap() as i64 - size as i64;
    assert!(
        (8 * MIB as i64..=16 * MIB as i64).contains(&room),
        "{room} bytes of room"
    );
    let (soft, _) = limit_of(pid, "Max cpu time");
    let soft: u64 = soft.parse().unwrap();
    assert!((21..=23).contains(&soft), "{soft} s");
    drop(stdin);
    assert!(child.wait().unwrap().success());
}

fn processor_time_is_a_budget_too() {
    // Room for the memory a second of decoding takes, in any build, but
    // not the time the whole run would.
    let budget = low(4096 * MIB, 1, Duration::from_secs(30));
    let checker = ChildProcess::new(std::env::current_exe().unwrap()).with_budgets(budget, budget);
    let verdicts = checker
        .check(Blob::Update, &[&bomb(5, 1_000_000_000)])
        .unwrap();
    match &verdicts[..] {
        [Verdict::Refuse(r)] => assert!(r.ends_with("it ran out of processor time"), "{r}"),
        other => panic!("{other:?}"),
    }
}

fn quick(args: &[&str]) -> ChildProcess {
    let budget = low(16 * MIB, 0, Duration::from_secs(1));
    ChildProcess::new(std::env::current_exe().unwrap())
        .with_args(args)
        .with_budgets(budget, budget)
}

fn a_child_past_its_deadline_is_killed_and_its_blob_refused() {
    let started = Instant::now();
    let verdicts = quick(&["--hang"]).check(Blob::Update, &[b"x"]).unwrap();
    match &verdicts[..] {
        [Verdict::Refuse(r)] => assert!(r.ends_with("it ran out of time"), "{r}"),
        other => panic!("{other:?}"),
    }
    assert!(started.elapsed() < Duration::from_secs(20));
}

fn a_child_that_quits_early_refuses_its_blob() {
    let verdicts = quick(&["--quits"]).check(Blob::Update, &[b"x"]).unwrap();
    match &verdicts[..] {
        [Verdict::Refuse(r)] => assert!(
            r.ends_with("it stopped the decoder (exit status: 1)"),
            "{r}"
        ),
        other => panic!("{other:?}"),
    }
}

fn what_is_no_checker_judges_nothing() {
    for (checker, why) in [
        (quick(&["--silent"]), "did not start"),
        (quick(&["--killed"]), "was killed"),
        (quick(&["--unusable"]), "could not limit itself"),
        (quick(&["--wrong"]), "did not start"),
        // It closed its output on a byte that is no verdict, and is killed.
        (quick(&["--garbles"]), "was killed"),
        (ChildProcess::new("/bin/true"), "did not start"),
        (
            ChildProcess::new("/nonexistent/matters"),
            "/nonexistent/matters",
        ),
    ] {
        match checker.check(Blob::Update, &[b"x"]) {
            Err(MattersError::Engine(m)) => assert!(m.contains(why), "{m}"),
            other => panic!("{why}: {other:?}"),
        }
    }
    // Its own arguments malformed, the child does not start checking.
    let out = std::process::Command::new(std::env::current_exe().unwrap())
        .args([CHECK_ARG, "update", "1", "2"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(3));
    assert!(out.stdout.is_empty());
}

fn a_child_with_no_input_greets_and_ends() {
    let out = std::process::Command::new(std::env::current_exe().unwrap())
        .args([CHECK_ARG, "snapshot", "1", "0", "1", "0"])
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap();
    assert!(out.status.success(), "{out:?}");
    assert_eq!(out.stdout, HELLO);
}

// Receiving.

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
    // Its writer is claimed, as its first seal would claim it, so tests can
    // sign records as it.
    joiner.claim_writer_now().unwrap();
    (joiner, keys)
}

struct Pair {
    dir: tempfile::TempDir,
    a: Database,
    b: Database,
    b_keys: MemoryKeyStore,
}

/// A with a task, then B joined; A knows B's writer.
fn pair() -> Pair {
    let dir = tempfile::tempdir().unwrap();
    let a = Database::open(dir.path().join("a.db")).unwrap();
    a.create_collection(&MemoryKeyStore::default(), route())
        .unwrap();
    a.add_task(&mut Task::new(id(1), "from A", 0.0)).unwrap();
    let (b, b_keys) = join(&a, &dir.path().join("b.db"));
    deliver_controls(&b, &a);
    Pair { dir, a, b, b_keys }
}

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

/// A data record `db`'s device signs, first in its writer's chain.
fn signed(db: &Database, device: &DeviceKeys, update: &[u8]) -> Vec<u8> {
    let (key, manifest, epoch) = current_key(db, device);
    let document = db.sync_membership().unwrap().document;
    let route = route();
    let header = wire::Header {
        context: route.content(&document),
        kind: wire::Kind::Data,
        epoch,
        manifest: &manifest,
        author: &device.id(),
        incarnation: &db.writer_incarnation().unwrap(),
        record_id: &fresh(),
        logical_batch: &fresh(),
        previous: None,
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

fn quarantined(db: &Database, record: &[u8]) -> Option<String> {
    let p = wire::parse(
        record,
        route().content(&db.sync_membership().unwrap().document),
    )
    .unwrap();
    db.quarantined_records()
        .unwrap()
        .into_iter()
        .find(|(rid, _)| rid == p.header.record_id)
        .map(|(_, (reason, _))| reason)
}

fn install_counted() -> Arc<Counted> {
    let counted = Arc::new(Counted {
        inner: this_binary(),
        calls: AtomicUsize::new(0),
        blobs: AtomicUsize::new(0),
    });
    isolation::install(counted.clone());
    counted
}

fn a_hostile_update_is_refused_for_good_and_the_page_applies() {
    let counted = install_counted();
    let p = pair();
    p.b.add_task(&mut Task::new(id(2), "from B", 1.0)).unwrap();
    let genuine = records(&p.b).pop().unwrap();
    let hostile = signed(&p.b, &keys(&p.b_keys), &bomb(actor(&p.b), OPS));
    let outcomes = p.a.receive_records(&[&hostile]).unwrap();
    match &outcomes[0] {
        Err(MattersError::Validation(m)) => assert!(
            m.contains("the engine could not decode it within its budget"),
            "{m}"
        ),
        other => panic!("{other:?}"),
    }
    let reason = quarantined(&p.a, &hostile).unwrap();
    assert!(reason.contains("within its budget"), "{reason}");
    assert_eq!(counted.asked(), (1, 1));
    // Delivered again, as a relay may, beside a genuine record: refused
    // again on the verdict already reached, and the page applies.
    let outcomes = p.a.receive_records(&[&hostile, &genuine]).unwrap();
    match &outcomes[0] {
        Err(MattersError::Validation(m)) => assert!(m.contains("within its budget"), "{m}"),
        other => panic!("{other:?}"),
    }
    assert_eq!(outcomes[1].as_ref().unwrap(), &Received::Applied);
    assert_eq!(
        p.a.get_task(&id(2)).unwrap().map(|t| t.title).as_deref(),
        Some("from B")
    );
    assert_eq!(counted.asked(), (2, 2), "only the genuine update was new");
    // Reopened, the database rebuilds its engine from what it took.
    drop(p.a);
    let a = Database::open(p.dir.path().join("a.db")).unwrap();
    assert!(a.get_task(&id(2)).unwrap().is_some());
}

fn records_met_again_are_not_authenticated_ahead() {
    install_counted();
    let p = pair();
    p.b.add_task(&mut Task::new(id(2), "from B", 1.0)).unwrap();
    let sent = records(&p.b);
    // Twice in one page: authenticated once.
    let before = receive::authentications();
    let outcomes = p.a.receive_records(&[&sent[0], &sent[0]]).unwrap();
    assert_eq!(outcomes[0].as_ref().unwrap(), &Received::Applied);
    assert_eq!(outcomes[1].as_ref().unwrap(), &Received::Duplicate);
    assert_eq!(receive::authentications() - before, 1);
    // Held already: not authenticated at all.
    let before = receive::authentications();
    let outcomes = p.a.receive_records(&[&sent[0]]).unwrap();
    assert_eq!(outcomes[0].as_ref().unwrap(), &Received::Duplicate);
    assert_eq!(receive::authentications(), before);
}

fn a_record_kept_waiting_meets_the_checker_when_it_applies() {
    let counted = install_counted();
    let p = pair();
    for n in 2..5 {
        p.b.add_task(&mut Task::new(id(n), "from B", n as f64))
            .unwrap();
    }
    let sent = records(&p.b);
    // The second and third arrive first, and wait for the first: their
    // updates are checked together, ahead of their turn.
    for outcome in p.a.receive_records(&[&sent[1], &sent[2]]).unwrap() {
        assert!(matches!(outcome, Ok(Received::Pending(_))), "{outcome:?}");
    }
    assert_eq!(counted.asked(), (1, 2));
    // The first applies, then the two kept, on the verdicts reached.
    assert_eq!(
        p.a.receive_records(&[&sent[0]]).unwrap()[0]
            .as_ref()
            .unwrap(),
        &Received::Applied
    );
    assert_eq!(counted.asked(), (2, 3));
    assert!(p.a.get_task(&id(4)).unwrap().is_some());
}

fn with_no_isolation_installed_the_same_page_differs() {
    // What the tests above rely on: without the child, the hostile update
    // is not refused for its cost (it waits for an operation it names).
    isolation::install(Arc::new(isolation::InProcess));
    let p = pair();
    let hostile = signed(&p.b, &keys(&p.b_keys), &bomb(actor(&p.b), 3));
    let outcomes = p.a.receive_records(&[&hostile]).unwrap();
    assert!(
        matches!(outcomes[0], Ok(Received::Pending(_))),
        "{outcomes:?}"
    );
}

fn a_hostile_update_handed_over_directly_is_not_taken() {
    install_counted();
    let db = Database::open_in_memory().unwrap();
    db.enable_sync().unwrap();
    match db.receive_update(&bomb(9, OPS)) {
        Err(MattersError::Validation(m)) => assert!(m.contains("within its budget"), "{m}"),
        other => panic!("{other:?}"),
    }
}

fn a_snapshot_past_its_budget_is_refused_as_a_checkpoint() {
    // Snapshots get no room at all here, updates some: an
    // honest snapshot is refused, so the refusal's path is the one a
    // hostile one takes.
    let room = low(64 * MIB, 20, Duration::from_secs(30));
    let none = low(0, 20, Duration::from_secs(30));
    isolation::install(Arc::new(
        ChildProcess::new(std::env::current_exe().unwrap()).with_budgets(room, none),
    ));
    let p = pair();
    for n in 0..10 {
        p.a.add_task(&mut Task::new(id(10 + n), "a task with a title", n as f64))
            .unwrap();
    }
    let sent = records(&p.a);
    for r in p.a.outgoing_records().unwrap() {
        p.a.acknowledge_record(&r.record_id).unwrap();
    }
    p.a.create_checkpoint(Some(Cursor {
        generation: [9; 16],
        sequence: sent.len() as u64,
    }))
    .unwrap()
    .unwrap();
    let parts: Vec<Vec<u8>> =
        p.a.outgoing_checkpoint_parts()
            .unwrap()
            .into_iter()
            .map(|(_, b)| b)
            .collect();
    // B judges it from the relay's log, and refuses it.
    let borrowed: Vec<&[u8]> = parts.iter().map(Vec::as_slice).collect();
    let outcomes = p.b.receive_records(&borrowed).unwrap();
    match outcomes.last().unwrap() {
        Ok(Received::Checkpoint(Checkpointed::Refused(r))) => {
            assert!(r.contains("within its budget"), "{r}")
        }
        other => panic!("{other:?}"),
    }
    // A fresh device does not start from it.
    let (c, _) = join(&p.a, &p.dir.path().join("c.db"));
    match c.start_from_checkpoint(&parts, [9; 16]) {
        Err(MattersError::Validation(m)) => assert!(m.contains("within its budget"), "{m}"),
        other => panic!("{other:?}"),
    }
    assert!(c.holds_nothing_synced().unwrap());
    // With room, it starts.
    isolation::install(Arc::new(this_binary()));
    assert!(c.start_from_checkpoint(&parts, [9; 16]).unwrap());
}

/// The test modes of this binary as a child that is no proper checker.
fn impostor(mode: &str) {
    let mut out = std::io::stdout();
    let greet = |out: &mut std::io::Stdout| {
        use std::io::Write;
        out.write_all(HELLO).unwrap();
        out.flush().unwrap();
    };
    match mode {
        "--hang" => {
            greet(&mut out);
            std::thread::sleep(Duration::from_secs(60));
        }
        "--silent" => std::thread::sleep(Duration::from_secs(60)),
        "--killed" => {
            greet(&mut out);
            std::process::Command::new("kill")
                .args(["-KILL", &std::process::id().to_string()])
                .status()
                .unwrap();
            std::thread::sleep(Duration::from_secs(60));
        }
        "--unusable" => {
            greet(&mut out);
            std::process::exit(3);
        }
        "--wrong" => {
            use std::io::Write;
            out.write_all(&vec![b'?'; HELLO.len()]).unwrap();
            out.flush().unwrap();
            std::thread::sleep(Duration::from_secs(60));
        }
        "--garbles" => {
            use std::io::Write;
            greet(&mut out);
            out.write_all(b"?").unwrap();
            out.flush().unwrap();
            std::thread::sleep(Duration::from_secs(60));
        }
        "--quits" => {
            greet(&mut out);
            std::process::exit(1);
        }
        _ => return,
    }
    std::process::exit(0);
}

fn main() {
    if let Some(mode) = std::env::args().nth(1) {
        impostor(&mode);
    }
    isolation::run_if_requested();
    let tests: &[(&str, fn())] = &[
        (
            "the_bomb_decodes_in_process_with_a_small_count",
            the_bomb_decodes_in_process_with_a_small_count,
        ),
        (
            "the_child_refuses_only_the_blobs_that_exceed_its_budget",
            the_child_refuses_only_the_blobs_that_exceed_its_budget,
        ),
        (
            "the_running_binary_with_the_default_budgets_tells_them_apart",
            the_running_binary_with_the_default_budgets_tells_them_apart,
        ),
        (
            "reading_a_blob_is_not_part_of_its_budget",
            reading_a_blob_is_not_part_of_its_budget,
        ),
        (
            "the_child_limits_itself_for_each_blob",
            the_child_limits_itself_for_each_blob,
        ),
        (
            "processor_time_is_a_budget_too",
            processor_time_is_a_budget_too,
        ),
        (
            "a_child_that_quits_early_refuses_its_blob",
            a_child_that_quits_early_refuses_its_blob,
        ),
        (
            "a_child_past_its_deadline_is_killed_and_its_blob_refused",
            a_child_past_its_deadline_is_killed_and_its_blob_refused,
        ),
        (
            "what_is_no_checker_judges_nothing",
            what_is_no_checker_judges_nothing,
        ),
        (
            "a_child_with_no_input_greets_and_ends",
            a_child_with_no_input_greets_and_ends,
        ),
        (
            "a_hostile_update_is_refused_for_good_and_the_page_applies",
            a_hostile_update_is_refused_for_good_and_the_page_applies,
        ),
        (
            "records_met_again_are_not_authenticated_ahead",
            records_met_again_are_not_authenticated_ahead,
        ),
        (
            "a_record_kept_waiting_meets_the_checker_when_it_applies",
            a_record_kept_waiting_meets_the_checker_when_it_applies,
        ),
        (
            "with_no_isolation_installed_the_same_page_differs",
            with_no_isolation_installed_the_same_page_differs,
        ),
        (
            "a_hostile_update_handed_over_directly_is_not_taken",
            a_hostile_update_handed_over_directly_is_not_taken,
        ),
        (
            "a_snapshot_past_its_budget_is_refused_as_a_checkpoint",
            a_snapshot_past_its_budget_is_refused_as_a_checkpoint,
        ),
    ];
    // Like the harness: names given filter the tests; flags are ignored.
    let filters: Vec<String> = std::env::args()
        .skip(1)
        .filter(|a| !a.starts_with('-'))
        .collect();
    let mut ran = 0;
    for (name, test) in tests {
        if !filters.is_empty() && !filters.iter().any(|f| name.contains(f.as_str())) {
            continue;
        }
        eprintln!("test {name} ...");
        test();
        eprintln!("test {name} ... ok");
        ran += 1;
    }
    eprintln!("{ran} engine isolation tests passed");
}
