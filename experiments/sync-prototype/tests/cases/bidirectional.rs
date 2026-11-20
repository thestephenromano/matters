use super::*;
use crate::atomic_sender::{batch, Sender};
use crate::encrypted_inbox::MERGE_CONFLICT;
use crate::encrypted_outbox::{Binding, Delivery};
use matters_sync_prototype::Fault;

struct Lab {
    dir: tempfile::TempDir,
    s: Setup,
    config: Config,
    keys: [EpochKey; 2],
    peers: [Option<Sender>; 2],
}
fn request() -> Request {
    Request::new(Command::Create {
        id: TASK.into(),
        kind: Kind::Task,
        parent: None,
        title: "two devices".into(),
    })
}
fn insert(text: &str) -> Request {
    Request::new(Command::InsertNotes {
        id: TASK.into(),
        index: 0,
        text: text.into(),
    })
}
fn rng() -> TestResult<Id> {
    Ok(random32()?)
}
fn binding(s: &Setup, side: usize) -> Binding {
    Binding {
        application: route().app,
        tenant: route().tenant,
        collection: route().collection,
        document: DOC,
        epoch: 0,
        manifest: s.authority.manifest().to_vec(),
        author: if side == 0 {
            s.a.id().unwrap()
        } else {
            s.b.id().unwrap()
        }
        .to_vec(),
    }
}
impl Lab {
    fn new() -> TestResult<Self> {
        let s = setup();
        assert_ne!(s.a.id()?, s.b.id()?);
        let config = Config {
            route: route(),
            document: DOC,
            generation: GENERATION,
            root: s.root.signing.verification_key.as_slice().to_vec(),
            genesis: s.genesis.clone(),
            writers: vec![],
        };
        let keys = [
            s.authority
                .unwrap_key(&s.authority.wrap_key(&s.a, s.a.id()?, &epoch_key(0))?, &s.a)?,
            s.authority
                .unwrap_key(&s.authority.wrap_key(&s.a, s.b.id()?, &epoch_key(0))?, &s.b)?,
        ];
        let mut lab = Self {
            dir: tempfile::tempdir()?,
            s,
            config,
            keys,
            peers: [None, None],
        };
        lab.reopen(0)?;
        lab.reopen(1)?;
        assert_ne!(lab.peer(0).writer(), lab.peer(1).writer());
        Ok(lab)
    }
    fn path(&self, side: usize) -> std::path::PathBuf {
        self.dir.path().join(format!("device-{side}.db"))
    }
    fn peer(&mut self, side: usize) -> &mut Sender {
        self.peers[side].as_mut().unwrap()
    }
    fn reopen(&mut self, side: usize) -> TestResult<()> {
        self.peers[side].take();
        let device = if side == 0 { &self.s.a } else { &self.s.b };
        self.peers[side] = Some(Sender::open_bidirectional(
            &self.path(side),
            binding(&self.s, side),
            self.config.clone(),
            rng,
            |m| Ok(sign(m, device)?),
            |_| Ok(()),
        )?);
        Ok(())
    }
    fn seal_all(&mut self, side: usize) -> TestResult<()> {
        let device = if side == 0 { &self.s.a } else { &self.s.b };
        let peer = self.peers[side].as_mut().unwrap();
        while let Some(batch) = peer.next_batch()? {
            peer.seal(
                batch,
                rng,
                |header, plain| Ok(seal_fixture(header, &epoch_key(0), plain, device)?),
                |_| Ok(()),
            )?;
        }
        Ok(())
    }
    fn deliver(&mut self, from: usize, to: usize, reverse: bool) -> TestResult<Vec<Vec<u8>>> {
        let mut delivery = Delivery::open(self.path(from), &binding(&self.s, from), false)?;
        let mut controls = delivery.announcements()?;
        let mut records = delivery.pending()?;
        if reverse {
            controls.reverse();
            records.reverse();
        }
        let peer = self.peer(to);
        for bytes in controls {
            peer.receive_writer(&bytes, okay)?;
        }
        let mut sent = vec![];
        for record in records {
            let n = peer.receive_cursor()? + 1;
            peer.receive_record(GENERATION, n, &record.bytes, okay)?;
            // Simulated receipt ACK follows durable ciphertext receipt, not application.
            delivery.acknowledge(record.id, |_| Ok(()))?;
            sent.push(record.bytes);
        }
        Ok(sent)
    }
    fn process(&mut self, side: usize) -> TestResult<()> {
        self.peers[side]
            .as_mut()
            .unwrap()
            .process_incoming(&self.keys[side], Fault::None, okay)
    }
    fn counts(&self, side: usize) -> TestResult<(i64, i64, i64)> {
        Ok(Connection::open(self.path(side))?.query_row("SELECT (SELECT count(*) FROM journal WHERE outgoing=1),(SELECT count(*) FROM journal WHERE outgoing=0),(SELECT count(*) FROM intents)",[], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?)))?)
    }
    fn same(&mut self) -> TestResult<()> {
        assert_eq!(self.peer(0).projection()?, self.peer(1).projection()?);
        Ok(())
    }
    fn bootstrap_task(&mut self) -> TestResult<()> {
        self.peer(0).execute(&request(), Fault::None)?;
        self.seal_all(0)?;
        self.deliver(0, 1, false)?;
        self.process(1)?;
        Ok(())
    }
}

#[test]
fn independent_devices_edit_receive_edit_and_restart_without_echo() -> TestResult<()> {
    let mut lab = Lab::new()?;
    lab.bootstrap_task()?;
    assert_eq!(lab.counts(1)?, (0, 1, 0));
    lab.peer(1).execute(&insert("from B"), Fault::None)?;
    lab.seal_all(1)?;
    lab.deliver(1, 0, false)?;
    lab.process(0)?;
    lab.same()?;
    assert_eq!(lab.counts(0)?, (1, 1, 1));
    assert_eq!(lab.counts(1)?, (1, 1, 1));
    for side in 0..2 {
        lab.peer(side).checkpoint()?;
        lab.reopen(side)?;
        lab.process(side)?;
    }
    lab.same()?;
    lab.peer(0).execute(&insert("A again "), Fault::None)?;
    lab.seal_all(0)?;
    lab.deliver(0, 1, true)?;
    lab.process(1)?;
    lab.same()?;
    assert_eq!(
        lab.peer(1).projection()?.entities[TASK].notes,
        "A again from B"
    );
    assert_eq!(lab.counts(1)?, (1, 2, 1));
    let actors = [lab.peer(0).writer().actor, lab.peer(1).writer().actor];
    let identities = [lab.s.a.id()?, lab.s.b.id()?]
        .map(|id| id.iter().map(|b| format!("{b:02x}")).collect::<String>());
    // Public fixture metadata only; no signing secrets or content keys.
    println!(
        "TWO_DEVICE_TRACE {}",
        serde_json::json!({
            "scenario": "A edits, B receives and edits, A receives, both restart, A edits again",
            "database_files": ["device-0.db", "device-1.db"],
            "device_public_ids": identities,
            "current_engine_actors": actors,
            "local_remote_intent_counts": [lab.counts(0)?, lab.counts(1)?],
            "final_notes": lab.peer(0).projection()?.entities[TASK].notes,
            "equal_projections": true,
        })
    );
    Ok(())
}

#[test]
fn concurrent_notes_and_own_echoes_converge_under_reversed_duplicate_delivery() -> TestResult<()> {
    let mut lab = Lab::new()?;
    lab.bootstrap_task()?;
    for (side, text) in [(0, "alpha"), (1, "beta")] {
        lab.peer(side).execute(&insert(text), Fault::None)?;
        lab.seal_all(side)?;
    }
    let a = lab.deliver(0, 1, true)?;
    let b = lab.deliver(1, 0, true)?;
    for (side, records) in [(0, &a), (1, &b), (0, &b), (1, &a)] {
        for bytes in records {
            let peer = lab.peer(side);
            let n = peer.receive_cursor()? + 1;
            peer.receive_record(GENERATION, n, bytes, okay)?;
        }
    }
    lab.process(1)?;
    lab.process(0)?;
    lab.same()?;
    let notes = lab.peer(0).projection()?.entities[TASK].notes.clone();
    assert_eq!(notes.matches("alpha").count(), 1);
    assert_eq!(notes.matches("beta").count(), 1);
    assert_eq!(lab.counts(0)?, (2, 1, 2));
    assert_eq!(lab.counts(1)?, (1, 2, 1));
    for side in 0..2 {
        lab.reopen(side)?;
        lab.process(side)?;
    }
    lab.same()?;
    Ok(())
}

#[test]
fn unsealed_local_edit_defers_remote_application_and_reissues_before_merge() -> TestResult<()> {
    let mut lab = Lab::new()?;
    lab.bootstrap_task()?;
    lab.peer(1).execute(&insert("earlier"), Fault::None)?;
    lab.seal_all(1)?;
    lab.deliver(1, 0, false)?;
    lab.process(0)?;
    let local = insert("local");
    lab.peer(0).execute(&local, Fault::None)?;
    lab.peer(1).execute(&insert("remote"), Fault::None)?;
    lab.seal_all(1)?;
    lab.deliver(1, 0, false)?;
    let view = lab.peer(0).projection()?;
    assert!(lab
        .process(0)
        .unwrap_err()
        .to_string()
        .contains("seal local intents"));
    assert_eq!(lab.peer(0).projection()?, view);
    assert_eq!(lab.counts(0)?, (2, 1, 2));
    assert_eq!(lab.peer(0).receive_cursor()?, 2);
    let old = lab.peer(0).writer();
    lab.peer(0).checkpoint()?;
    lab.reopen(0)?;
    assert_ne!(old, lab.peer(0).writer());
    assert_eq!(lab.peer(0).projection()?, view);
    lab.peer(0).execute(&local, Fault::None)?;
    assert!(lab.peer(0).recovery_required()?.is_empty());
    lab.seal_all(0)?;
    lab.process(0)?;
    lab.deliver(0, 1, true)?;
    lab.process(1)?;
    lab.same()?;
    let notes = lab.peer(0).projection()?.entities[TASK].notes.clone();
    assert_eq!(notes.matches("local").count(), 1);
    assert_eq!(notes.matches("remote").count(), 1);
    assert_eq!(notes.matches("earlier").count(), 1);
    Ok(())
}

#[test]
fn receipt_survives_failed_shared_apply_and_retry_never_echoes() -> TestResult<()> {
    for target in ["journal", "projection", "received_records", "commit"] {
        let mut lab = Lab::new()?;
        lab.peer(0).execute(&request(), Fault::None)?;
        lab.seal_all(0)?;
        lab.deliver(0, 1, false)?;
        let db = Connection::open(lab.path(1))?;
        if target == "commit" {
            db.execute_batch("CREATE TABLE fault_parent(id INTEGER PRIMARY KEY); CREATE TABLE fault_child(id INTEGER REFERENCES fault_parent(id) DEFERRABLE INITIALLY DEFERRED); CREATE TRIGGER reject_apply AFTER INSERT ON journal BEGIN INSERT INTO fault_child VALUES(1); END;")?;
        } else {
            let verb = if target == "received_records" {
                "UPDATE"
            } else {
                "INSERT"
            };
            db.execute_batch(&format!("CREATE TRIGGER reject_apply BEFORE {verb} ON {target} BEGIN SELECT RAISE(ABORT,'injected shared apply'); END;"))?;
        }
        assert!(lab.process(1).is_err(), "{target}");
        assert_eq!(lab.counts(1)?, (0, 0, 0));
        assert_eq!(lab.peer(1).receive_cursor()?, 1);
        assert!(lab.peer(1).projection()?.entities.is_empty());
        assert!(lab
            .peer(1)
            .received_entries()?
            .iter()
            .all(|r| r.applied.is_none()));
        db.execute_batch("DROP TRIGGER reject_apply;")?;
        lab.process(1)?;
        lab.process(1)?;
        assert_eq!(lab.counts(1)?, (0, 1, 0));
        lab.same()?;
    }
    Ok(())
}

#[test]
fn lost_apply_ack_and_fenced_owner_do_not_repeat_or_publish_remote_work() -> TestResult<()> {
    let mut lab = Lab::new()?;
    lab.peer(0).execute(&request(), Fault::None)?;
    lab.seal_all(0)?;
    lab.deliver(0, 1, false)?;
    assert!(lab.peers[1]
        .as_mut()
        .unwrap()
        .process_incoming(&lab.keys[1], Fault::AfterCommit, okay)
        .is_err());
    lab.same()?;
    lab.process(1)?;
    assert_eq!(lab.counts(1)?, (0, 1, 0));
    let mut old = lab.peers[1].take().unwrap();
    lab.reopen(1)?;
    assert!(old
        .process_incoming(&lab.keys[1], Fault::None, okay)
        .is_err());
    lab.process(1)?;
    lab.same()?;
    assert!(Inbox::open(lab.path(1), lab.config.clone()).is_err());
    assert!(Sender::open(
        &lab.path(1),
        binding(&lab.s, 1),
        rng,
        |m| Ok(sign(m, &lab.s.b)?),
        |_| Ok(())
    )
    .is_err());
    let mut config = lab.config.clone();
    config.generation = [22; 32];
    assert!(Sender::open_bidirectional(
        &lab.path(1),
        binding(&lab.s, 1),
        config,
        rng,
        |m| Ok(sign(m, &lab.s.b)?),
        |_| Ok(())
    )
    .is_err());
    lab.peer(1).execute(&insert("still active"), Fault::None)?;
    Ok(())
}

#[test]
fn invalid_ciphertext_and_reversed_causal_input_preserve_local_work() -> TestResult<()> {
    let mut lab = Lab::new()?;
    lab.peer(0).execute(&request(), Fault::None)?;
    lab.peer(0).execute(&insert("one"), Fault::None)?;
    lab.peer(0).execute(&insert("two"), Fault::None)?;
    lab.seal_all(0)?;
    let records = lab.deliver(0, 1, true)?;
    let mut forged = records[0].clone();
    *forged.last_mut().unwrap() ^= 1;
    let n = lab.peer(1).receive_cursor()? + 1;
    lab.peer(1).receive_record(GENERATION, n, &forged, okay)?;
    lab.process(1)?;
    lab.same()?;
    assert_eq!(
        lab.peer(1)
            .received_entries()?
            .iter()
            .find(|e| e.sequence == n)
            .unwrap()
            .status,
        "quarantined"
    );
    assert_eq!(lab.counts(1)?, (0, 3, 0));
    lab.peer(1).execute(&insert("three"), Fault::None)?;
    lab.seal_all(1)?;
    lab.deliver(1, 0, false)?;
    lab.process(0)?;
    lab.same()?;
    assert_eq!(
        lab.peer(0).projection()?.entities[TASK].notes,
        "threetwoone"
    );
    Ok(())
}

#[test]
fn signed_local_fork_freezes_new_authorship_but_preserves_views_and_delivery() -> TestResult<()> {
    let mut lab = Lab::new()?;
    let request = request();
    let plain = lab.peer(0).execute(&request, Fault::None)?.bytes;
    lab.seal_all(0)?;
    let original = lab.deliver(0, 1, false)?.remove(0);
    lab.process(1)?;
    let bind = binding(&lab.s, 0);
    let parsed = wire::parse(&original, bind.context())?;
    let id = random32()?;
    let mut header = parsed.header;
    header.record_id = &id;
    let competing = seal_fixture(header, &epoch_key(0), &plain, &lab.s.a)?;
    for side in 0..2 {
        let peer = lab.peer(side);
        let n = peer.receive_cursor()? + 1;
        peer.receive_record(GENERATION, n, &competing, okay)?;
        lab.process(side)?;
    }
    let view = lab.peer(0).projection()?;
    assert!(lab
        .peer(0)
        .execute(&insert("forbidden"), Fault::None)
        .unwrap_err()
        .to_string()
        .contains("frozen"));
    assert!(lab
        .peer(0)
        .seal(
            [99; 32],
            || panic!("frozen RNG"),
            |_, _| panic!("frozen crypto"),
            |_| Ok(())
        )
        .unwrap_err()
        .to_string()
        .contains("frozen"));
    assert_eq!(
        lab.peer(0)
            .seal(
                batch(&request.id)?,
                || panic!("stored RNG"),
                |_, _| panic!("stored crypto"),
                |_| Ok(())
            )?
            .bytes,
        original
    );
    lab.reopen(0)?;
    assert_eq!(lab.peer(0).projection()?, view);
    assert!(lab
        .peer(0)
        .execute(&insert("still frozen"), Fault::None)
        .is_err());
    lab.peer(1)
        .execute(&insert("B can continue"), Fault::None)?;
    lab.seal_all(1)?;
    lab.deliver(1, 0, false)?;
    lab.process(0)?;
    lab.same()?;
    assert_eq!(
        lab.peer(0).projection()?.entities[TASK].notes,
        "B can continue"
    );
    Ok(())
}

#[test]
fn recovery_refuses_a_remote_journal_row_after_stranded_local_work() -> TestResult<()> {
    let mut lab = Lab::new()?;
    lab.bootstrap_task()?;
    lab.peer(0).execute(&insert("stranded"), Fault::None)?;
    let remote = lab.peer(1).execute(&insert("interleaved"), Fault::None)?;
    let db = Connection::open(lab.path(0))?;
    db.execute(
        "INSERT INTO journal(id,bytes,outgoing) VALUES ('invalid-interleave',?1,0)",
        [&remote.bytes],
    )?;
    let before: Vec<u8> = db.query_row(
        "SELECT plaintext FROM intents ORDER BY seq DESC LIMIT 1",
        [],
        |r| r.get(0),
    )?;
    assert!(lab
        .reopen(0)
        .unwrap_err()
        .to_string()
        .contains("interleave accepted remote"));
    let after: Vec<u8> = db.query_row(
        "SELECT plaintext FROM intents ORDER BY seq DESC LIMIT 1",
        [],
        |r| r.get(0),
    )?;
    assert_eq!(before, after);
    Ok(())
}

#[test]
#[ignore = "invoked by bidirectional_process_exit_boundaries"]
fn bidirectional_crash_worker() -> TestResult<()> {
    if ["MATTERS_DUPLEX_DIR", "MATTERS_DUPLEX_POINT"]
        .iter()
        .all(|k| std::env::var_os(k).is_none())
    {
        return Ok(());
    }
    let dir = std::path::PathBuf::from(std::env::var("MATTERS_DUPLEX_DIR")?);
    let index: usize = std::env::var("MATTERS_DUPLEX_POINT")?.parse()?;
    let config: Config = serde_json::from_slice(&std::fs::read(dir.join("config.json"))?)?;
    let binding: Binding = serde_json::from_slice(&std::fs::read(dir.join("binding.json"))?)?;
    let device = outbox_cases::crash_device();
    let key = config
        .authority()?
        .unwrap_key(&std::fs::read(dir.join("key-envelope"))?, &device)?;
    let mut peer = Sender::open_bidirectional(
        &dir.join("receiver.db"),
        binding,
        config,
        rng,
        |m| Ok(sign(m, &device)?),
        |_| Ok(()),
    )?;
    peer.receive_writer(&std::fs::read(dir.join("writer"))?, okay)?;
    peer.receive_record(
        GENERATION,
        1,
        &std::fs::read(dir.join("record"))?,
        |point| {
            if (index == 0 && point == Point::BeforeReceipt)
                || (index == 1 && point == Point::AfterReceipt)
            {
                std::process::exit(170 + index as i32);
            }
            Ok(())
        },
    )?;
    peer.process_incoming(
        &key,
        if index == 2 {
            Fault::ExitBeforeCommit
        } else {
            Fault::ExitAfterCommit
        },
        okay,
    )?;
    panic!("crash point not reached")
}

#[test]
fn bidirectional_process_exit_boundaries() -> TestResult<()> {
    for index in 0..4 {
        let h = Harness::with_receiver(outbox_cases::crash_device());
        let dir = tempfile::tempdir()?;
        let mut config = h.config.clone();
        config.writers.clear();
        let bind = Binding {
            application: route().app,
            tenant: route().tenant,
            collection: route().collection,
            document: DOC,
            epoch: 0,
            manifest: h.s.authority.manifest().to_vec(),
            author: h.s.b.id()?.to_vec(),
        };
        let mut source = source(71);
        let plain = create(&mut source);
        let ciphertext = h.seal(71, None, &plain);
        let announcement = sign(&writer(&h.s.authority, &h.s.a, [71; 32], 71, None), &h.s.a)?;
        std::fs::write(dir.path().join("config.json"), serde_json::to_vec(&config)?)?;
        std::fs::write(dir.path().join("binding.json"), serde_json::to_vec(&bind)?)?;
        std::fs::write(dir.path().join("key-envelope"), &h.envelope)?;
        std::fs::write(dir.path().join("writer"), &announcement)?;
        std::fs::write(dir.path().join("record"), &ciphertext)?;
        let output = std::process::Command::new(std::env::current_exe()?)
            .args([
                "--exact",
                "inbox_cases::bidirectional::bidirectional_crash_worker",
                "--ignored",
            ])
            .env("MATTERS_DUPLEX_DIR", dir.path())
            .env("MATTERS_DUPLEX_POINT", index.to_string())
            .output()?;
        assert_eq!(
            output.status.code(),
            Some([170, 171, 93, 94][index]),
            "{index}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let path = dir.path().join("receiver.db");
        let db = Connection::open(&path)?;
        let counts: (i64,i64,i64) = db.query_row("SELECT (SELECT count(*) FROM journal),(SELECT count(*) FROM intents),(SELECT cursor FROM inbox_meta)",[],|r| Ok((r.get(0)?,r.get(1)?,r.get(2)?)))?;
        assert_eq!(counts, (i64::from(index == 3), 0, i64::from(index > 0)));
        let mut peer = Sender::open_bidirectional(
            &path,
            bind,
            config,
            rng,
            |m| Ok(sign(m, &h.s.b)?),
            |_| Ok(()),
        )?;
        peer.receive_record(GENERATION, 1, &ciphertext, okay)?;
        peer.process_incoming(&h.key, Fault::None, okay)?;
        assert_eq!(peer.projection()?, source.projection()?);
        let incoming: i64 =
            db.query_row("SELECT count(*) FROM journal WHERE outgoing=0", [], |r| {
                r.get(0)
            })?;
        assert_eq!(incoming, 1);
    }
    Ok(())
}

// Review follow-up (#150): budgets are charged where work is created, so a
// pass that applies already retained work never runs out of capacity.

impl Lab {
    /// Seal and acknowledge `side`'s work without delivering it anywhere.
    fn author(&mut self, side: usize, edits: usize) -> TestResult<()> {
        for n in 0..edits {
            self.peer(side)
                .execute(&insert(&format!("{side}:{n} ")), Fault::None)?;
            self.seal_all(side)?;
            let mut delivery = Delivery::open(self.path(side), &binding(&self.s, side), false)?;
            for record in delivery.pending()? {
                delivery.acknowledge(record.id, |_| Ok(()))?;
            }
        }
        Ok(())
    }
    fn retained(&self, side: usize, table: &str) -> TestResult<i64> {
        Ok(Connection::open(self.path(side))?.query_row(
            &format!("SELECT count(*) FROM {table}"),
            [],
            |r| r.get(0),
        )?)
    }
}

#[test]
fn the_shared_receive_budget_counts_local_and_remote_rows_where_they_arrive() -> TestResult<()> {
    let mut lab = Lab::new()?;
    lab.bootstrap_task()?;
    // B: one remote row and 20 local ones; A authors 12 more.
    lab.author(1, 20)?;
    lab.process(1)?;
    for n in 0..12 {
        lab.peer(0)
            .execute(&insert(&format!("a{n} ")), Fault::None)?;
    }
    lab.seal_all(0)?;
    let records = Delivery::open(lab.path(0), &binding(&lab.s, 0), true)?.pending()?;
    assert_eq!(records.len(), 12);
    let mut refused = None;
    for (n, record) in records.iter().enumerate() {
        let next = lab.peer(1).receive_cursor()? + 1;
        if let Err(e) = lab
            .peer(1)
            .receive_record(GENERATION, next, &record.bytes, okay)
        {
            refused = Some((n, e.to_string()));
            break;
        }
    }
    // 1 + 11 remote and 20 local rows fill the 32-row budget exactly.
    let (n, reason) = refused.expect("remote receipt past the shared budget");
    assert_eq!(n, 11);
    assert!(reason.contains("capacity"), "{reason}");
    assert_eq!(lab.peer(1).receive_cursor()?, 12);

    // A full budget refuses new local work, not application.
    let before = lab.counts(1)?;
    let refused = lab.peer(1).execute(&insert("over budget"), Fault::None);
    assert!(refused.is_err());
    assert_eq!(lab.counts(1)?, before);
    lab.process(1)?;
    assert_eq!(lab.retained(1, "received_records")?, 32);
    assert!(lab
        .peer(1)
        .received_entries()?
        .iter()
        .all(|r| r.status == "applied"));
    Ok(())
}

#[test]
fn a_full_writer_budget_refuses_a_new_writer_and_never_blocks_application() -> TestResult<()> {
    let mut lab = Lab::new()?;
    lab.bootstrap_task()?;
    let mut refused = None;
    for _ in 0..40 {
        if let Err(e) = lab.reopen(1) {
            refused = Some(e.to_string());
            break;
        }
        lab.process(1)?;
    }
    let reason = refused.expect("a new writer past the writer budget");
    assert!(reason.contains("capacity"), "{reason}");
    // The refused open left no writer behind, and every retained
    // announcement fits the budget.
    assert_eq!(
        lab.retained(1, "writers")?,
        lab.retained(1, "writer_receipts")? - 1
    );
    assert_eq!(lab.retained(1, "writer_receipts")?, 32);
    Ok(())
}

#[test]
fn a_shared_merge_conflict_is_recorded_and_stops_application_without_restaging() -> TestResult<()> {
    let mut lab = Lab::new()?;
    lab.peer(0).execute(&request(), Fault::None)?;
    lab.seal_all(0)?;
    lab.deliver(0, 1, false)?;
    crate::encrypted_inbox::force_merge_conflict(true);
    let first = lab.process(1);
    crate::encrypted_inbox::force_merge_conflict(false);
    assert!(first.unwrap_err().to_string().contains(MERGE_CONFLICT));
    assert_eq!(
        lab.peer(1).merge_conflict()?.as_deref(),
        Some(MERGE_CONFLICT)
    );

    crate::encrypted_inbox::take_work();
    let again = lab.process(1);
    assert!(again.unwrap_err().to_string().contains(MERGE_CONFLICT));
    assert_eq!(crate::encrypted_inbox::take_work().stagings, 0);
    assert_eq!(lab.counts(1)?, (0, 0, 0));
    // Local work continues on the last good state.
    lab.peer(1).execute(&request(), Fault::None)?;
    Ok(())
}

#[test]
fn a_failed_shared_write_never_reaches_the_before_apply_point() -> TestResult<()> {
    for target in ["journal", "projection"] {
        let mut lab = Lab::new()?;
        lab.peer(0).execute(&request(), Fault::None)?;
        lab.seal_all(0)?;
        lab.deliver(0, 1, false)?;
        Connection::open(lab.path(1))?.execute_batch(&format!(
            "CREATE TRIGGER reject_apply BEFORE INSERT ON {target} BEGIN SELECT RAISE(ABORT,'injected'); END;"
        ))?;
        let mut points = vec![];
        let result =
            lab.peers[1]
                .as_mut()
                .unwrap()
                .process_incoming(&lab.keys[1], Fault::None, |p| {
                    points.push(p);
                    Ok(())
                });
        assert!(result.is_err(), "{target}");
        assert!(
            !points.contains(&Point::BeforeApply),
            "{target}: {points:?}"
        );
    }
    Ok(())
}

#[test]
fn a_sender_only_store_keeps_the_first_atomic_marker() -> TestResult<()> {
    let s = setup();
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("sender.db");
    let open = || {
        Sender::open(
            &path,
            binding(&s, 0),
            rng,
            |m| Ok(sign(m, &s.a)?),
            |_| Ok(()),
        )
    };
    open()?.execute(&request(), Fault::None)?;
    let db = Connection::open(&path)?;
    db.execute(
        "UPDATE prototype_meta SET value='matters-sync-atomic-outbox/1' WHERE key='format'",
        [],
    )?;
    open()?;
    let marker: String = db.query_row(
        "SELECT value FROM prototype_meta WHERE key='format'",
        [],
        |r| r.get(0),
    )?;
    assert_eq!(marker, "matters-sync-atomic-outbox/1");
    let lab = Lab::new()?;
    let marker: String = Connection::open(lab.path(0))?.query_row(
        "SELECT value FROM prototype_meta WHERE key='format'",
        [],
        |r| r.get(0),
    )?;
    assert_eq!(marker, "matters-sync-atomic-outbox/2");
    Ok(())
}
