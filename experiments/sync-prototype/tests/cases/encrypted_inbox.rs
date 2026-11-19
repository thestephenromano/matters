use super::{
    encrypted_inbox::{take_work, Config, Inbox, Point, Work, MAX_RECORDS},
    *,
};
use matters_sync_prototype::{Command, Kind, Replica, Request, Result as TestResult};
use rusqlite::Connection;

const GENERATION: Id = [81; 32];
const TASK: &str = "00000000-0000-4000-8000-000000000001";
fn okay(_: Point) -> TestResult<()> {
    Ok(())
}
fn fail(point: Point) -> impl FnMut(Point) -> TestResult<()> {
    move |p| {
        if p == point {
            Err(std::io::Error::other("injected SQL boundary").into())
        } else {
            Ok(())
        }
    }
}
struct Harness {
    s: Setup,
    config: Config,
    cek: Id,
    key: EpochKey,
    envelope: Vec<u8>,
}
impl Harness {
    fn new() -> Self {
        Self::with_receiver(Device::generate().unwrap())
    }
    fn with_receiver(receiver: Device) -> Self {
        let s = setup_with_receiver(receiver);
        let starts = vec![
            sign(&writer(&s.authority, &s.a, [71; 32], 71, None), &s.a).unwrap(),
            sign(&writer(&s.authority, &s.b, [72; 32], 72, None), &s.b).unwrap(),
        ];
        let config = Config {
            route: route(),
            document: DOC,
            generation: GENERATION,
            root: s.root.signing.verification_key.as_slice().to_vec(),
            genesis: s.genesis.clone(),
            writers: starts,
        };
        let cek = epoch_key(0);
        let envelope = s.authority.wrap_key(&s.a, s.b.id().unwrap(), &cek).unwrap();
        let key = s.authority.unwrap_key(&envelope, &s.b).unwrap();
        Self {
            s,
            config,
            cek,
            key,
            envelope,
        }
    }
    fn seal(&self, actor: u8, previous: Option<Hash>, plain: &[u8]) -> Vec<u8> {
        let author = if actor == 71 { &self.s.a } else { &self.s.b };
        let author_id = author.id().unwrap();
        let manifest = self.s.authority.manifest();
        let incarnation = [actor; 32];
        let record = random32().unwrap();
        let mut h = header(
            &self.s.authority,
            &author_id,
            &incarnation,
            &record,
            &manifest,
        );
        h.previous = previous.as_ref();
        seal_fixture(h, &self.cek, plain, author).unwrap()
    }
    fn inbox(&self, path: &std::path::Path) -> Inbox {
        Inbox::open(path, self.config.clone()).unwrap()
    }
}
fn source(actor: u64) -> Replica {
    let mut r = Replica::open_in_memory().unwrap();
    r.set_test_peer(actor).unwrap();
    r
}
fn create(r: &mut Replica) -> Vec<u8> {
    create_id(r, TASK)
}
fn create_id(r: &mut Replica, id: &str) -> Vec<u8> {
    r.execute(&Request::new(Command::Create {
        id: id.into(),
        kind: Kind::Task,
        parent: None,
        title: "private task".into(),
    }))
    .unwrap()
    .bytes
}
fn notes(r: &mut Replica, text: &str) -> Vec<u8> {
    r.execute(&Request::new(Command::InsertNotes {
        id: TASK.into(),
        index: 0,
        text: text.into(),
    }))
    .unwrap()
    .bytes
}

#[test]
fn reversed_records_survive_restart_and_apply_once_without_echo() -> TestResult<()> {
    let h = Harness::new();
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("inbox.db");
    let mut sender = source(71);
    let first = h.seal(71, None, &create(&mut sender));
    let second = h.seal(71, Some(hash(&first)), &notes(&mut sender, "once"));
    let mut inbox = h.inbox(&path);
    inbox.receive(GENERATION, 1, &second, okay)?;
    inbox.process(&h.key, okay)?;
    assert_eq!(inbox.entries()?[0].reason, "predecessor-not-applied");
    assert!(inbox.projection()?.entities.is_empty());
    drop(inbox);
    let mut inbox = h.inbox(&path);
    assert_eq!(inbox.cursor()?, 1);
    assert_eq!(inbox.entries()?[0].bytes, second);
    inbox.receive(GENERATION, 2, &first, okay)?;
    inbox.process(&h.key, okay)?;
    assert_eq!(inbox.projection()?, sender.projection()?);
    inbox.receive(GENERATION, 2, &first, okay)?;
    inbox.receive(GENERATION, 3, &second, okay)?;
    inbox.process(&h.key, okay)?;
    assert_eq!(
        inbox
            .entries()?
            .iter()
            .filter(|e| e.applied.is_some())
            .count(),
        2
    );
    assert_eq!(inbox.entries()?[2].status, "duplicate");
    assert_eq!(inbox.projection()?.entities[TASK].notes, "once");
    drop(inbox);
    let mut inbox = h.inbox(&path);
    inbox.process(&h.key, okay)?;
    assert_eq!(inbox.projection()?, sender.projection()?);
    let conn = Connection::open(&path)?;
    let tables: i64 = conn.query_row(
        "SELECT count(*) FROM sqlite_master WHERE type='table'",
        [],
        |r| r.get(0),
    )?;
    assert_eq!(tables, 3); // Content, metadata and writer receipts; no outgoing queue.
    conn.execute(
        "UPDATE inbox_meta SET projection=?1",
        [serde_json::to_string(
            &matters_sync_prototype::Projection::default(),
        )?],
    )?;
    inbox.process(&h.key, okay)?;
    assert_eq!(inbox.projection()?, sender.projection()?);
    Ok(())
}

#[test]
fn cross_writer_causal_dependency_drains_after_missing_update_arrives() -> TestResult<()> {
    let h = Harness::new();
    let dir = tempfile::tempdir()?;
    let mut a = source(71);
    let first_plain = create(&mut a);
    let mut b = source(72);
    b.receive(&matters_sync_prototype::Record {
        id: "a".into(),
        bytes: first_plain.clone(),
    })?;
    let second = h.seal(72, None, &notes(&mut b, "from B"));
    let first = h.seal(71, None, &first_plain);
    let mut inbox = h.inbox(&dir.path().join("inbox.db"));
    inbox.receive(GENERATION, 1, &second, okay)?;
    take_work();
    inbox.process(&h.key, okay)?;
    assert_eq!(inbox.entries()?[0].reason, "causal-dependency");
    // Missing dependencies are visible in metadata: no engine copy is made.
    assert_eq!(take_work().stagings, 0);
    inbox.receive(GENERATION, 2, &first, okay)?;
    inbox.process(&h.key, okay)?;
    assert_eq!(inbox.projection()?, b.projection()?);
    assert!(inbox.entries()?.iter().all(|e| e.status == "applied"));
    Ok(())
}

#[test]
fn rejected_record_and_its_blocked_descendant_are_durable_and_visible() -> TestResult<()> {
    let h = Harness::new();
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("inbox.db");
    let bad = h.seal(71, None, b"not a Loro update");
    let child = h.seal(71, Some(hash(&bad)), &create(&mut source(71)));
    let mut inbox = h.inbox(&path);
    inbox.receive(GENERATION, 1, &bad, okay)?;
    inbox.receive(GENERATION, 2, &child, okay)?;
    inbox.process(&h.key, okay)?;
    assert_eq!(inbox.entries()?[0].reason, "engine-format");
    assert_eq!(
        inbox.entries()?[1].reason,
        "blocked-by-rejected-predecessor"
    );
    let before = inbox.entries()?;
    drop(inbox);
    let mut inbox = h.inbox(&path);
    inbox.process(&h.key, okay)?;
    assert_eq!(inbox.entries()?, before);
    assert_eq!(inbox.cursor()?, 2);
    assert!(inbox.projection()?.entities.is_empty());
    Ok(())
}

#[test]
fn signed_fork_freezes_chain_across_restart_without_erasing_prior_projection() -> TestResult<()> {
    let h = Harness::new();
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("inbox.db");
    let mut a = source(71);
    let first_plain = create(&mut a);
    let first = h.seal(71, None, &first_plain);
    let mut inbox = h.inbox(&path);
    inbox.receive(GENERATION, 1, &first, okay)?;
    inbox.process(&h.key, okay)?;
    let accepted = inbox.projection()?;
    // Even a signed branch with an invalid CRDT payload is fork evidence.
    let competitor = h.seal(71, None, b"invalid payload");
    inbox.receive(GENERATION, 2, &competitor, okay)?;
    inbox.process(&h.key, okay)?;
    assert!(inbox.entries()?.iter().all(|e| e.status == "fork"));
    assert_eq!(inbox.projection()?, accepted);
    drop(inbox);
    let mut inbox = h.inbox(&path);
    let child = h.seal(71, Some(hash(&first)), &notes(&mut a, "must not apply"));
    inbox.receive(GENERATION, 3, &child, okay)?;
    inbox.process(&h.key, okay)?;
    assert_eq!(inbox.entries()?[2].status, "fork");
    assert_eq!(inbox.projection()?, accepted);
    Ok(())
}

#[test]
fn forged_competitor_does_not_freeze_an_authorized_chain() -> TestResult<()> {
    let h = Harness::new();
    let dir = tempfile::tempdir()?;
    let mut a = source(71);
    let first = h.seal(71, None, &create(&mut a));
    let mut forged = h.seal(71, None, b"forged");
    *forged.last_mut().unwrap() ^= 1;
    let second = h.seal(71, Some(hash(&first)), &notes(&mut a, "accepted"));
    let mut inbox = h.inbox(&dir.path().join("inbox.db"));
    for (index, bytes) in [&first, &forged, &second].iter().enumerate() {
        inbox.receive(GENERATION, index as i64 + 1, bytes, okay)?;
    }
    inbox.process(&h.key, okay)?;
    assert_eq!(inbox.entries()?[1].status, "quarantined");
    assert_eq!(inbox.projection()?, a.projection()?);
    Ok(())
}

#[test]
fn reject_context_actor_snapshot_and_domain_violations() -> TestResult<()> {
    for variant in 0..5 {
        let h = Harness::new();
        let dir = tempfile::tempdir()?;
        let doc = LoroDoc::new();
        doc.set_peer_id(if variant == 1 { 999 } else { 71 })?;
        doc.get_text("notes").insert(0, "test")?;
        let plain = if variant == 2 {
            doc.export(ExportMode::Snapshot)?
        } else if variant == 3 {
            let invalid = LoroDoc::new();
            invalid.set_peer_id(71)?;
            invalid.get_tree("home").create(None)?; // Missing required task metadata.
            invalid.export(ExportMode::all_updates())?
        } else {
            doc.export(ExportMode::all_updates())?
        };
        let author = h.s.a.id()?;
        let manifest = h.s.authority.manifest();
        let id = random32()?;
        let mut header = header(&h.s.authority, &author, &[71; 32], &id, &manifest);
        if variant == 0 {
            header.context.tenant = &[99; 16];
        }
        if variant == 4 {
            header.kind = wire::Kind::Snapshot;
        }
        let record = seal_fixture(header, &h.cek, &plain, &h.s.a)?;
        let mut inbox = h.inbox(&dir.path().join("inbox.db"));
        inbox.receive(GENERATION, 1, &record, okay)?;
        inbox.process(&h.key, okay)?;
        assert_eq!(
            inbox.entries()?[0].status,
            "quarantined",
            "variant {variant}"
        );
        assert!(inbox.projection()?.entities.is_empty());
    }
    Ok(())
}

#[test]
fn receipt_and_projection_transactions_rollback_and_retry_idempotently() -> TestResult<()> {
    let h = Harness::new();
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("inbox.db");
    let mut a = source(71);
    let first = h.seal(71, None, &create(&mut a));
    let mut inbox = h.inbox(&path);
    assert!(inbox
        .receive(GENERATION, 1, &first, fail(Point::BeforeReceipt))
        .is_err());
    assert_eq!(inbox.cursor()?, 0);
    assert!(inbox.entries()?.is_empty());
    assert!(inbox
        .receive(GENERATION, 1, &first, fail(Point::AfterReceipt))
        .is_err());
    assert_eq!(inbox.cursor()?, 1);
    inbox.receive(GENERATION, 1, &first, okay)?;
    let conn = Connection::open(&path)?;
    conn.execute_batch("CREATE TRIGGER reject_projection BEFORE UPDATE OF projection ON inbox_meta BEGIN SELECT RAISE(ABORT,'injected'); END;")?;
    assert!(inbox.process(&h.key, okay).is_err());
    assert_eq!(inbox.entries()?[0].applied, None);
    assert!(inbox.projection()?.entities.is_empty());
    conn.execute_batch("DROP TRIGGER reject_projection;")?;
    assert!(inbox.process(&h.key, fail(Point::BeforeApply)).is_err());
    assert_eq!(inbox.entries()?[0].applied, None);
    assert!(inbox.process(&h.key, fail(Point::AfterApply)).is_err());
    assert_eq!(inbox.entries()?[0].applied, Some(1));
    inbox.process(&h.key, okay)?;
    assert_eq!(inbox.projection()?, a.projection()?);
    Ok(())
}

#[test]
fn binding_cursor_capacity_and_foreign_database_guards_preserve_recovery() -> TestResult<()> {
    let h = Harness::new();
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("inbox.db");
    let mut inbox = h.inbox(&path);
    assert!(inbox.receive([0; 32], 1, b"raw", okay).is_err());
    assert!(inbox.receive(GENERATION, 2, b"raw", okay).is_err());
    assert!(inbox
        .receive(GENERATION, 1, &vec![0; wire::MAX_WIRE_BYTES + 1], okay)
        .is_err());
    for i in 1..=MAX_RECORDS {
        inbox.receive(GENERATION, i, b"retained", okay)?;
    }
    assert!(inbox
        .receive(GENERATION, MAX_RECORDS + 1, b"full", okay)
        .is_err());
    assert!(inbox.receive(GENERATION, 1, b"changed", okay).is_err());
    drop(inbox);
    let mut inbox = h.inbox(&path);
    inbox.process(&h.key, okay)?;
    inbox.receive(GENERATION, 1, b"retained", okay)?;
    assert_eq!(inbox.entries()?.len() as i64, MAX_RECORDS);
    let mut changed_generation = h.config.clone();
    changed_generation.generation = [99; 32];
    assert!(Inbox::open(&path, changed_generation).is_err());
    let mut other = h.config.clone();
    other.route.tenant = [99; 16];
    assert!(Inbox::open(&path, other).is_err());
    assert!(Inbox::open(":memory:", h.config.clone()).is_err());
    let foreign = dir.path().join("foreign.db");
    Connection::open(&foreign)?.execute_batch("CREATE TABLE SQLiteCache(x);")?;
    assert!(Inbox::open(&foreign, h.config.clone()).is_err());
    Ok(())
}

const POINTS: [Point; 4] = [
    Point::BeforeReceipt,
    Point::AfterReceipt,
    Point::BeforeApply,
    Point::AfterApply,
];
#[test]
#[ignore = "child process invoked by abrupt_exit_preserves_receipt_and_application_boundaries"]
fn inbox_crash_worker() -> TestResult<()> {
    if ["MATTERS_INBOX_CRASH_DIR", "MATTERS_INBOX_CRASH_POINT"]
        .iter()
        .all(|k| std::env::var_os(k).is_none())
    {
        return Ok(());
    }
    let dir = std::path::PathBuf::from(std::env::var("MATTERS_INBOX_CRASH_DIR")?);
    let index: usize = std::env::var("MATTERS_INBOX_CRASH_POINT")?.parse()?;
    let config: Config = serde_json::from_slice(&std::fs::read(dir.join("config.json"))?)?;
    let key = config.authority()?.unwrap_key(
        &std::fs::read(dir.join("envelope"))?,
        &outbox_cases::crash_device(),
    )?;
    let hook = |point| {
        if point == POINTS[index] {
            std::process::exit(120 + index as i32);
        }
        Ok(())
    };
    let mut inbox = Inbox::open(dir.join("inbox.db"), config)?;
    inbox.receive(GENERATION, 1, &std::fs::read(dir.join("record"))?, hook)?;
    inbox.process(&key, hook)?;
    panic!("crash hook did not run")
}
#[test]
fn abrupt_exit_preserves_receipt_and_application_boundaries() -> TestResult<()> {
    for index in 0..POINTS.len() {
        let h = Harness::with_receiver(outbox_cases::crash_device());
        let dir = tempfile::tempdir()?;
        let mut a = source(71);
        let first = h.seal(71, None, &create(&mut a));
        std::fs::write(
            dir.path().join("config.json"),
            serde_json::to_vec(&h.config)?,
        )?;
        std::fs::write(dir.path().join("envelope"), &h.envelope)?;
        std::fs::write(dir.path().join("record"), &first)?;
        let output = std::process::Command::new(std::env::current_exe()?)
            .args(["--exact", "inbox_cases::inbox_crash_worker", "--ignored"])
            .env("MATTERS_INBOX_CRASH_DIR", dir.path())
            .env("MATTERS_INBOX_CRASH_POINT", index.to_string())
            .output()?;
        assert_eq!(
            output.status.code(),
            Some(120 + index as i32),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let mut inbox = h.inbox(&dir.path().join("inbox.db"));
        assert_eq!(inbox.cursor()?, if index == 0 { 0 } else { 1 });
        assert_eq!(inbox.projection()?.entities.contains_key(TASK), index == 3);
        inbox.receive(GENERATION, 1, &first, okay)?;
        inbox.process(&h.key, okay)?;
        assert_eq!(inbox.projection()?, a.projection()?);
        assert_eq!(inbox.entries()?[0].applied, Some(1));
    }
    Ok(())
}

#[test]
fn signed_undecryptable_competitor_is_still_fork_evidence() -> TestResult<()> {
    let h = Harness::new();
    let dir = tempfile::tempdir()?;
    let first = h.seal(71, None, &create(&mut source(71)));
    let author = h.s.a.id()?;
    let manifest = h.s.authority.manifest();
    let id = random32()?;
    let header = header(&h.s.authority, &author, &[71; 32], &id, &manifest);
    let second = seal_fixture(header, &[13; 32], b"wrong content key", &h.s.a)?;
    let mut inbox = h.inbox(&dir.path().join("inbox.db"));
    inbox.receive(GENERATION, 1, &first, okay)?;
    inbox.receive(GENERATION, 2, &second, okay)?;
    inbox.process(&h.key, okay)?;
    assert!(inbox.entries()?.iter().all(|r| r.status == "fork"));
    assert!(inbox.projection()?.entities.is_empty());
    Ok(())
}

#[test]
fn concurrent_handles_and_busy_retry_share_committed_progress() -> TestResult<()> {
    let h = Harness::new();
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("inbox.db");
    let mut a = source(71);
    let first = h.seal(71, None, &create(&mut a));
    let second = h.seal(71, Some(hash(&first)), &notes(&mut a, "retained"));
    let mut one = h.inbox(&path);
    let mut two = h.inbox(&path);
    let connection = Connection::open(&path)?;
    assert_eq!(
        connection.query_row("PRAGMA journal_mode", [], |r| r.get::<_, String>(0))?,
        "wal"
    );
    connection.execute_batch("BEGIN IMMEDIATE;")?;
    assert!(one.receive(GENERATION, 1, &first, okay).is_err());
    assert_eq!(two.cursor()?, 0);
    connection.execute_batch("ROLLBACK;")?;
    one.receive(GENERATION, 1, &first, okay)?;
    two.process(&h.key, okay)?;
    one.receive(GENERATION, 2, &second, okay)?;
    one.process(&h.key, okay)?;
    two.process(&h.key, okay)?;
    assert_eq!(two.projection()?, a.projection()?);
    assert_eq!(two.entries()?[1].applied, Some(2));
    Ok(())
}

#[test]
fn unrelated_epoch_key_is_an_operational_error_without_quarantine() -> TestResult<()> {
    let h = Harness::new();
    let other = Harness::new();
    let dir = tempfile::tempdir()?;
    let first = h.seal(71, None, &create(&mut source(71)));
    let mut inbox = h.inbox(&dir.path().join("inbox.db"));
    inbox.receive(GENERATION, 1, &first, okay)?;
    let before = inbox.entries()?;
    assert!(inbox.process(&other.key, okay).is_err());
    assert_eq!(inbox.entries()?, before);
    inbox.process(&h.key, okay)?;
    assert_eq!(inbox.entries()?[0].status, "applied");
    Ok(())
}

#[test]
fn overlapping_actor_history_cannot_reapply_a_logical_edit() -> TestResult<()> {
    let h = Harness::new();
    let dir = tempfile::tempdir()?;
    let mut a = source(71);
    let plain = create(&mut a);
    let first = h.seal(71, None, &plain);
    let repeated = h.seal(71, Some(hash(&first)), &plain);
    let mut inbox = h.inbox(&dir.path().join("inbox.db"));
    inbox.receive(GENERATION, 1, &first, okay)?;
    inbox.receive(GENERATION, 2, &repeated, okay)?;
    inbox.process(&h.key, okay)?;
    assert_eq!(inbox.entries()?[1].reason, "actor-overlap");
    assert_eq!(inbox.entries()?[1].applied, None);
    assert_eq!(inbox.projection()?, a.projection()?);
    Ok(())
}

// Review of #107: regression witnesses.

fn sealed_with_id(
    h: &Harness,
    actor: u8,
    record: &Id,
    previous: Option<Hash>,
    plain: &[u8],
) -> Vec<u8> {
    let author = if actor == 71 { &h.s.a } else { &h.s.b };
    let author_id = author.id().unwrap();
    let manifest = h.s.authority.manifest();
    let incarnation = [actor; 32];
    let mut header = header(&h.s.authority, &author_id, &incarnation, record, &manifest);
    header.previous = previous.as_ref();
    seal_fixture(header, &h.cek, plain, author).unwrap()
}
fn record_id(bytes: &[u8]) -> Id {
    *wire::parse(bytes, route().content(&DOC))
        .unwrap()
        .header
        .record_id
}
fn statuses(inbox: &Inbox) -> TestResult<std::collections::BTreeMap<Vec<u8>, (String, String)>> {
    Ok(inbox
        .entries()?
        .into_iter()
        .map(|e| (e.bytes, (e.status, e.reason)))
        .collect())
}

#[test]
fn review_cross_writer_record_id_reuse_freezes_neither_writer_in_either_order() -> TestResult<()> {
    // An admitted writer copies another writer's cleartext record ID. Either
    // party (or the relay) could be at fault, so neither chain is frozen, and
    // the verdict cannot depend on which copy was delivered first.
    let h = Harness::new();
    let dir = tempfile::tempdir()?;
    let mut a = source(71);
    let first = h.seal(71, None, &create(&mut a));
    let reused = sealed_with_id(
        &h,
        72,
        &record_id(&first),
        None,
        &create_id(&mut source(72), "00000000-0000-4000-8000-000000000002"),
    );
    let child = h.seal(71, Some(hash(&first)), &notes(&mut a, "still live"));
    let mut views = Vec::new();
    for (name, order) in [
        ("ab.db", [&first, &reused, &child]),
        ("ba.db", [&reused, &first, &child]),
    ] {
        let mut inbox = h.inbox(&dir.path().join(name));
        for (i, bytes) in order.iter().enumerate() {
            inbox.receive(GENERATION, i as i64 + 1, bytes, okay)?;
        }
        inbox.process(&h.key, okay)?;
        let rows = inbox.entries()?;
        assert!(
            rows.iter().all(|r| r.status == "applied"),
            "{name}: {rows:?}"
        );
        let flagged: Vec<_> = rows.iter().map(|r| r.record_id_conflict).collect();
        assert_eq!(flagged.iter().filter(|f| **f).count(), 2, "{name}");
        assert!(rows.iter().any(|r| r.bytes == first) && rows.iter().any(|r| r.bytes == reused));
        let view = inbox.projection()?;
        assert_eq!(view.entities[TASK].notes, "still live");
        views.push((statuses(&inbox)?, view));
    }
    assert_eq!(views[0], views[1]);
    Ok(())
}

#[test]
fn review_same_writer_record_id_equivocation_freezes_only_that_writer() -> TestResult<()> {
    let h = Harness::new();
    let dir = tempfile::tempdir()?;
    let first = h.seal(71, None, &create(&mut source(71)));
    let mut other = source(71);
    create(&mut other);
    let equivocation = sealed_with_id(&h, 71, &record_id(&first), None, &notes(&mut other, "x"));
    let b = h.seal(72, None, &create(&mut source(72)));
    let mut inbox = h.inbox(&dir.path().join("inbox.db"));
    for (i, bytes) in [&first, &equivocation, &b].iter().enumerate() {
        inbox.receive(GENERATION, i as i64 + 1, bytes, okay)?;
    }
    inbox.process(&h.key, okay)?;
    let rows = inbox.entries()?;
    assert_eq!(rows[0].status, "fork");
    assert_eq!(rows[1].status, "fork");
    assert_eq!(rows[2].status, "applied");
    Ok(())
}

#[test]
fn review_concurrent_records_reach_the_same_verdicts_in_either_delivery_order() -> TestResult<()> {
    // Each record is valid in its own causal past; their merge violates the
    // one-entity-per-UUID invariant.
    let h = Harness::new();
    let a = h.seal(71, None, &create(&mut source(71)));
    let b = h.seal(72, None, &create(&mut source(72)));
    let dir = tempfile::tempdir()?;
    let mut results = Vec::new();
    for (name, order) in [("ab.db", [&a, &b]), ("ba.db", [&b, &a])] {
        let mut inbox = h.inbox(&dir.path().join(name));
        for (i, bytes) in order.iter().enumerate() {
            inbox.receive(GENERATION, i as i64 + 1, bytes, okay)?;
        }
        inbox.process(&h.key, okay)?;
        assert!(inbox.entries()?.iter().all(|r| r.status == "applied"));
        // The concurrent creations converge to an original and a conflict
        // copy (#113) on every receiver; nothing is rejected.
        assert_eq!(inbox.merge_conflict()?, None);
        let view = inbox.projection()?;
        let copies: Vec<_> = view
            .entities
            .values()
            .filter(|e| e.identity_conflict.as_deref() == Some(TASK))
            .collect();
        assert_eq!(copies.len(), 1);
        results.push((statuses(&inbox)?, view));
    }
    assert_eq!(results[0], results[1]);
    Ok(())
}

#[test]
fn record_may_not_introduce_a_merge_repair_in_its_own_causal_past() -> TestResult<()> {
    // A writer that already holds TASK creates a second node under the same
    // UUID. Only concurrency may produce a conflict copy, so this is rejected.
    let h = Harness::new();
    let dir = tempfile::tempdir()?;
    let first_plain = create(&mut source(71));
    let first = h.seal(71, None, &first_plain);
    let doc = LoroDoc::new();
    doc.set_peer_id(71)?;
    doc.import(&first_plain)?;
    let before = doc.oplog_vv();
    let tree = doc.get_tree("home");
    let node = tree.create(None)?;
    let meta = tree.get_meta(node)?;
    meta.insert("id", TASK)?;
    meta.insert("kind", "\"Task\"")?;
    meta.insert("title", "duplicate")?;
    meta.insert_container("notes", loro::LoroText::new())?;
    for key in ["deadline", "completion"] {
        meta.insert(key, "null")?;
    }
    meta.insert("schedule", "\"Unscheduled\"")?;
    meta.insert("trash", false)?;
    meta.insert("checked", false)?;
    doc.commit();
    let second = h.seal(
        71,
        Some(hash(&first)),
        &doc.export(ExportMode::updates(&before))?,
    );
    let mut inbox = h.inbox(&dir.path().join("inbox.db"));
    inbox.receive(GENERATION, 1, &first, okay)?;
    inbox.receive(GENERATION, 2, &second, okay)?;
    inbox.process(&h.key, okay)?;
    let rows = inbox.entries()?;
    assert_eq!(rows[0].status, "applied");
    assert_eq!(
        (rows[1].status.as_str(), rows[1].reason.as_str()),
        ("quarantined", "domain-repair")
    );
    Ok(())
}

#[test]
fn committed_epoch_key_makes_decryption_failure_a_verdict_on_the_record() -> TestResult<()> {
    // #112: the epoch key handle can only hold the committed key, so a wrong
    // key cannot be supplied locally ...
    let h = Harness::new();
    assert!(h
        .s
        .authority
        .wrap_key(&h.s.a, h.s.b.id()?, &random32()?)
        .is_err());
    // ... and a signed record the receiver cannot open is the record's fault.
    let dir = tempfile::tempdir()?;
    let mut a = source(71);
    let author = h.s.a.id()?;
    let manifest = h.s.authority.manifest();
    let id = random32()?;
    let header = header(&h.s.authority, &author, &[71; 32], &id, &manifest);
    let sealed_elsewhere = seal_fixture(header, &random32()?, &create(&mut a), &h.s.a)?;
    let child = h.seal(71, Some(hash(&sealed_elsewhere)), &notes(&mut a, "later"));
    let mut inbox = h.inbox(&dir.path().join("inbox.db"));
    inbox.receive(GENERATION, 1, &sealed_elsewhere, okay)?;
    inbox.receive(GENERATION, 2, &child, okay)?;
    inbox.process(&h.key, okay)?;
    let rows = inbox.entries()?;
    assert_eq!(
        (rows[0].status.as_str(), rows[0].reason.as_str()),
        ("quarantined", "authenticated-decryption")
    );
    assert_eq!(rows[1].reason, "blocked-by-rejected-predecessor");
    Ok(())
}

#[test]
fn review_processing_does_not_repeat_authentication_or_staging_for_settled_history(
) -> TestResult<()> {
    let h = Harness::new();
    let dir = tempfile::tempdir()?;
    let mut a = source(71);
    let mut previous = None;
    let mut inbox = h.inbox(&dir.path().join("inbox.db"));
    for i in 1..=4 {
        let plain = if i == 1 {
            create(&mut a)
        } else {
            notes(&mut a, "x")
        };
        let record = h.seal(71, previous, &plain);
        previous = Some(hash(&record));
        inbox.receive(GENERATION, i, &record, okay)?;
    }
    let bad = h.seal(72, None, b"not a Loro update");
    inbox.receive(GENERATION, 5, &bad, okay)?;
    take_work();
    inbox.process(&h.key, okay)?;
    // One signature check to derive facts and one to decrypt each candidate;
    // each record is staged once and the rejected one is not retried.
    assert_eq!(
        take_work(),
        Work {
            authentications: 10,
            stagings: 4,
            writer_checks: 0
        }
    );
    inbox.process(&h.key, okay)?;
    assert_eq!(take_work(), Work::default());
    assert_eq!(inbox.projection()?, a.projection()?);
    drop(inbox);
    let mut inbox = h.inbox(&dir.path().join("inbox.db"));
    let next = h.seal(71, previous, &notes(&mut a, "y"));
    inbox.receive(GENERATION, 6, &next, okay)?;
    take_work();
    inbox.process(&h.key, okay)?;
    // New row: one authentication for its facts and one to decrypt it.
    assert_eq!(
        take_work(),
        Work {
            authentications: 2,
            stagings: 1,
            writer_checks: 0
        }
    );
    assert_eq!(inbox.projection()?, a.projection()?);
    Ok(())
}

#[test]
fn review_duplicate_detection_uses_full_bytes_not_the_latest_record_id_claim() -> TestResult<()> {
    let h = Harness::new();
    let dir = tempfile::tempdir()?;
    let first = h.seal(71, None, &create(&mut source(71)));
    let mut other = source(71);
    create(&mut other);
    let equivocation = sealed_with_id(&h, 71, &record_id(&first), None, &notes(&mut other, "x"));
    let mut inbox = h.inbox(&dir.path().join("inbox.db"));
    for (i, bytes) in [&first, &equivocation, &first].iter().enumerate() {
        inbox.receive(GENERATION, i as i64 + 1, bytes, okay)?;
    }
    inbox.process(&h.key, okay)?;
    let rows = inbox.entries()?;
    assert_eq!(rows[2].status, "duplicate", "{:?}", rows[2]);
    Ok(())
}

#[test]
fn review_foreign_database_with_only_non_table_objects_is_refused() -> TestResult<()> {
    let h = Harness::new();
    for object in [
        "CREATE VIEW v AS SELECT 1",
        "CREATE VIEW SQLiteView AS SELECT 1",
    ] {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("foreign.db");
        let conn = Connection::open(&path)?;
        conn.execute_batch(object)?;
        assert!(Inbox::open(&path, h.config.clone()).is_err(), "{object}");
        let objects: i64 =
            conn.query_row("SELECT count(*) FROM sqlite_master", [], |r| r.get(0))?;
        assert_eq!(objects, 1, "{object}");
    }
    Ok(())
}

#[test]
fn review_replacement_writer_starts_are_refused_at_open() -> TestResult<()> {
    let h = Harness::new();
    let dir = tempfile::tempdir()?;
    let mut config = h.config.clone();
    config.writers.push(sign(
        &writer(&h.s.authority, &h.s.a, [73; 32], 73, Some([71; 32])),
        &h.s.a,
    )?);
    assert!(Inbox::open(dir.path().join("inbox.db"), config).is_err());
    assert!(!dir.path().join("inbox.db").exists());
    Ok(())
}

#[path = "writer_lifecycle.rs"]
mod lifecycle;
