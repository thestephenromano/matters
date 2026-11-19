use super::*;
use crate::atomic_sender::{batch, Sender};
use crate::encrypted_outbox::{Binding, Delivery, Outbox, Point as OutPoint};
use matters_sync_prototype::Fault;
use std::path::Path;

fn binding(h: &Harness) -> Binding {
    Binding {
        application: route().app,
        tenant: route().tenant,
        collection: route().collection,
        document: DOC,
        epoch: 0,
        manifest: h.s.authority.manifest().to_vec(),
        author: h.s.a.id().unwrap().to_vec(),
    }
}
fn random() -> TestResult<Id> {
    Ok(random32()?)
}
fn open(path: &Path, h: &Harness) -> TestResult<Sender> {
    Sender::open(
        path,
        binding(h),
        random,
        |m| Ok(sign(m, &h.s.a)?),
        |_| Ok(()),
    )
}
fn creation() -> Request {
    Request::new(Command::Create {
        id: TASK.into(),
        kind: Kind::Task,
        parent: None,
        title: "private task".into(),
    })
}
fn edit(text: &str) -> Request {
    Request::new(Command::InsertNotes {
        id: TASK.into(),
        index: 0,
        text: text.into(),
    })
}
fn seal(sender: &mut Sender, request: &Request, h: &Harness) -> TestResult<Vec<u8>> {
    Ok(sender
        .seal(
            batch(&request.id)?,
            random,
            |header, plain| Ok(seal_fixture(header, &h.cek, plain, &h.s.a)?),
            |_| Ok(()),
        )?
        .bytes)
}
fn counts(path: &Path) -> TestResult<(i64, i64, i64)> {
    Ok(Connection::open(path)?.query_row("SELECT (SELECT count(*) FROM journal),(SELECT count(*) FROM intents),(SELECT count(*) FROM sealed)", [], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?)))?)
}
fn sql_projection(path: &Path) -> TestResult<matters_sync_prototype::Projection> {
    let value: String = Connection::open(path)?.query_row(
        "SELECT value FROM projection WHERE id='state'",
        [],
        |r| r.get(0),
    )?;
    Ok(serde_json::from_str(&value)?)
}

#[test]
fn atomic_edits_deliver_once_across_restart_and_reverse_delivery() -> TestResult<()> {
    let mut h = Harness::new();
    h.config.writers.clear();
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("sender.db");
    let mut sender = open(&path, &h)?;
    let first = Request::batch([
        creation().commands[0].clone(),
        edit("before restart").commands[0].clone(),
    ]);
    let record = sender.execute(&first, Fault::None)?;
    assert_eq!(sender.next_batch()?, Some(batch(&first.id)?));
    assert_eq!(counts(&path)?, (1, 1, 0));
    let first_bytes = seal(&mut sender, &first, &h)?;
    let retry = sender.seal(
        batch(&first.id)?,
        || panic!("retry entropy"),
        |_, _| panic!("retry sealing"),
        |_| Ok(()),
    )?;
    assert_eq!(retry.bytes, first_bytes);
    sender.checkpoint()?;
    let old = sender.writer();
    drop(sender);
    let mut sender = open(&path, &h)?;
    assert_ne!(sender.writer(), old);
    assert_eq!(sender.execute(&first, Fault::None)?, record);
    let second = edit("after restart");
    sender.execute(&second, Fault::None)?;
    let second_bytes = seal(&mut sender, &second, &h)?;
    sender.rebuild()?;
    assert_eq!(sql_projection(&path)?, sender.projection()?);
    let mut delivery = Delivery::open(&path, &binding(&h), false)?;
    let mut inbox = h.inbox(&dir.path().join("receiver.db"));
    inbox.receive(GENERATION, 1, &second_bytes, okay)?;
    for control in delivery.announcements()?.iter().rev() {
        inbox.receive_writer(control, okay)?;
    }
    inbox.process(&h.key, okay)?;
    assert!(inbox.projection()?.entities.is_empty());
    inbox.receive(GENERATION, 2, &first_bytes, okay)?;
    inbox.receive(GENERATION, 3, &second_bytes, okay)?;
    inbox.process(&h.key, okay)?;
    assert_eq!(inbox.projection()?, sender.projection()?);
    for r in delivery.pending()? {
        delivery.acknowledge(r.id, |_| Ok(()))?;
    }
    assert!(delivery.pending()?.is_empty());
    assert_eq!(
        delivery.record(batch(&first.id)?)?.unwrap().bytes,
        first_bytes
    );
    assert_eq!(counts(&path)?, (2, 2, 2));
    Ok(())
}

#[test]
fn failed_sql_at_every_local_write_and_commit_rolls_back_domain_and_intent() -> TestResult<()> {
    for target in ["intents", "journal", "projection", "commit"] {
        let h = Harness::new();
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("sender.db");
        let mut sender = open(&path, &h)?;
        let first = creation();
        sender.execute(&first, Fault::None)?;
        seal(&mut sender, &first, &h)?;
        let before = sender.projection()?;
        let db = Connection::open(&path)?;
        if target == "commit" {
            db.execute_batch("CREATE TABLE fault_parent(id INTEGER PRIMARY KEY); CREATE TABLE fault_child(id INTEGER REFERENCES fault_parent(id) DEFERRABLE INITIALLY DEFERRED); CREATE TRIGGER fail_write AFTER INSERT ON journal BEGIN INSERT INTO fault_child VALUES(1); END;")?;
        } else {
            db.execute_batch(&format!("CREATE TRIGGER fail_write BEFORE INSERT ON {target} BEGIN SELECT RAISE(ABORT,'injected SQL rejection'); END;"))?;
        }
        let request = edit("once");
        assert!(sender.execute(&request, Fault::None).is_err(), "{target}");
        assert_eq!(sender.projection()?, before, "{target}");
        assert_eq!(sql_projection(&path)?, before, "{target}");
        assert_eq!(counts(&path)?, (1, 1, 1), "{target}");
        db.execute_batch("DROP TRIGGER fail_write;")?;
        sender.execute(&request, Fault::None)?;
        assert_eq!(sender.projection()?.entities[TASK].notes, "once");
        assert_eq!(counts(&path)?, (2, 2, 1));
    }
    Ok(())
}

#[test]
fn uncertain_ack_and_request_conflict_never_duplicate_local_edits() -> TestResult<()> {
    let h = Harness::new();
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("sender.db");
    let mut sender = open(&path, &h)?;
    let first = creation();
    sender.execute(&first, Fault::None)?;
    let request = edit("once");
    assert!(sender.execute(&request, Fault::BeforeCommit).is_err());
    assert_eq!(counts(&path)?, (1, 1, 0));
    assert!(sender.execute(&request, Fault::AfterCommit).is_err());
    let saved = sender.execute(&request, Fault::None)?;
    assert_eq!(sender.projection()?.entities[TASK].notes, "once");
    let mut conflict = edit("different");
    conflict.id = request.id.clone();
    assert!(sender.execute(&conflict, Fault::None).is_err());
    assert_eq!(sender.execute(&request, Fault::None)?, saved);
    drop(sender);
    // Reopening re-issues both never-sealed requests as the new writer. A
    // retry then returns the re-issued receipt: the original operations were
    // never sent and never can be.
    let mut sender = open(&path, &h)?;
    let reissued = sender.execute(&request, Fault::None)?;
    assert_eq!(reissued.id, saved.id);
    assert_ne!(reissued.bytes, saved.bytes);
    assert_eq!(sender.execute(&request, Fault::None)?, reissued);
    assert_eq!(sender.projection()?.entities[TASK].notes, "once");
    assert_eq!(counts(&path)?, (2, 2, 0));
    assert!(sender.recovery_required()?.is_empty());
    Ok(())
}

#[test]
fn no_op_and_invalid_batches_do_not_create_transport_records() -> TestResult<()> {
    let h = Harness::new();
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("sender.db");
    let mut sender = open(&path, &h)?;
    let noop = Request::batch([]);
    let r = sender.execute(&noop, Fault::None)?;
    assert_eq!(sender.execute(&noop, Fault::None)?, r);
    assert_eq!(counts(&path)?, (1, 0, 0));
    assert_eq!(sender.next_batch()?, None);
    let mut invalid = creation();
    invalid.id = "not-a-uuid".into();
    assert!(sender.execute(&invalid, Fault::None).is_err());
    let mixed = Request::batch([
        creation().commands[0].clone(),
        edit("absent").commands[0].clone(),
        creation().commands[0].clone(),
    ]);
    assert!(sender.execute(&mixed, Fault::None).is_err());
    assert!(sender.projection()?.entities.is_empty());
    assert_eq!(counts(&path)?, (1, 0, 0));
    Ok(())
}

#[test]
fn stale_owner_and_incompatible_store_modes_cannot_bypass_capture() -> TestResult<()> {
    let h = Harness::new();
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("sender.db");
    let mut stale = open(&path, &h)?;
    let mut current = open(&path, &h)?;
    assert!(stale.execute(&creation(), Fault::None).is_err());
    assert!(stale.checkpoint().is_err());
    assert!(stale.rebuild().is_err());
    assert!(Replica::open(&path).is_err());
    assert!(Outbox::open(&path, binding(&h), random).is_err());
    assert!(Outbox::open_announced(
        &path,
        binding(&h),
        random,
        |m| Ok(sign(m, &h.s.a)?),
        |_| Ok(())
    )
    .is_err());
    let mode: String =
        Connection::open(&path)?.query_row("PRAGMA journal_mode", [], |r| r.get(0))?;
    assert_eq!(mode, "wal");
    let mut wrong = binding(&h);
    wrong.tenant = [99; 16];
    assert!(Sender::open(&path, wrong, random, |m| Ok(sign(m, &h.s.a)?), |_| Ok(())).is_err());
    current.execute(&creation(), Fault::None)?;
    assert_eq!(counts(&path)?, (1, 1, 0));
    let standalone = dir.path().join("plain.db");
    let _plain = Replica::open(&standalone)?;
    assert!(open(&standalone, &h).is_err());
    let standalone = dir.path().join("outbox.db");
    let _queue = Outbox::open(&standalone, binding(&h), random)?;
    assert!(open(&standalone, &h).is_err());
    let foreign = dir.path().join("foreign.db");
    Connection::open(&foreign)?.execute_batch("CREATE TABLE SQLiteCache(value);")?;
    assert!(open(&foreign, &h).is_err());
    Ok(())
}

#[test]
fn exhausted_intent_budget_cannot_save_an_unqueued_edit() -> TestResult<()> {
    let h = Harness::new();
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("sender.db");
    let mut sender = open(&path, &h)?;
    sender.execute(&creation(), Fault::None)?;
    for _ in 1..16 {
        sender.execute(&edit("x"), Fault::None)?;
    }
    let before = sender.projection()?;
    assert!(sender.execute(&edit("overflow"), Fault::None).is_err());
    assert_eq!(sender.projection()?, before);
    assert_eq!(sql_projection(&path)?, before);
    assert_eq!(counts(&path)?, (16, 16, 0));
    Ok(())
}

#[test]
fn oversized_export_rolls_back_the_edit_and_intent() -> TestResult<()> {
    let h = Harness::new();
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("sender.db");
    let mut sender = open(&path, &h)?;
    sender.execute(&creation(), Fault::None)?;
    let before = sender.projection()?;
    let mut rng = 42u64;
    let text: String = (0..3 * wire::MAX_PLAINTEXT_BYTES)
        .map(|_| {
            rng ^= rng << 13;
            rng ^= rng >> 7;
            rng ^= rng << 17;
            (32 + (rng % 95) as u8) as char
        })
        .collect();
    let error = sender.execute(&edit(&text), Fault::None).unwrap_err();
    assert!(
        error.to_string().contains("payload exceeds bound"),
        "{error}"
    );
    assert_eq!(sender.projection()?, before);
    assert_eq!(sql_projection(&path)?, before);
    assert_eq!(counts(&path)?, (1, 1, 0));
    Ok(())
}

#[test]
fn wal_reader_keeps_snapshot_while_writer_contention_leaves_no_partial_save() -> TestResult<()> {
    let h = Harness::new();
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("sender.db");
    let mut sender = open(&path, &h)?;
    let reader = Connection::open(&path)?;
    reader.execute_batch("BEGIN;")?;
    let count = || reader.query_row("SELECT count(*) FROM journal", [], |r| r.get::<_, i64>(0));
    assert_eq!(count()?, 0);
    let first = creation();
    sender.execute(&first, Fault::None)?;
    seal(&mut sender, &first, &h)?;
    assert_eq!(count()?, 0);
    assert_eq!(counts(&path)?, (1, 1, 1));
    reader.execute_batch("COMMIT; BEGIN IMMEDIATE;")?;
    let request = edit("after contention");
    let before = sender.projection()?;
    assert!(sender.execute(&request, Fault::None).is_err());
    assert_eq!(sender.projection()?, before);
    assert_eq!(counts(&path)?, (1, 1, 1));
    reader.execute_batch("ROLLBACK;")?;
    sender.execute(&request, Fault::None)?;
    assert_eq!(
        sender.projection()?.entities[TASK].notes,
        "after contention"
    );
    assert_eq!(counts(&path)?, (2, 2, 1));
    Ok(())
}

#[test]
fn uncertain_seal_retries_fresh_reservation_without_repeating_domain_edit() -> TestResult<()> {
    let mut h = Harness::new();
    h.config.writers.clear();
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("sender.db");
    let mut sender = open(&path, &h)?;
    let request = creation();
    sender.execute(&request, Fault::None)?;
    let mut uncertain = Vec::new();
    assert!(sender
        .seal(
            batch(&request.id)?,
            random,
            |header, plain| {
                uncertain = seal_fixture(header, &h.cek, plain, &h.s.a)?;
                Ok(uncertain.clone())
            },
            |p| if p == OutPoint::BeforeOutboxCommit {
                Err(std::io::Error::other("uncertain seal").into())
            } else {
                Ok(())
            }
        )
        .is_err());
    assert_eq!(counts(&path)?, (1, 1, 0));
    assert!(Delivery::open(&path, &binding(&h), true)?
        .pending()?
        .is_empty());
    let bytes = seal(&mut sender, &request, &h)?;
    let context = binding(&h);
    let before = wire::parse(&uncertain, context.context())?;
    let after = wire::parse(&bytes, context.context())?;
    assert_ne!(before.header.record_id, after.header.record_id);
    assert_eq!(before.header.previous, after.header.previous);
    assert_eq!(before.header.logical_batch, after.header.logical_batch);
    let mut inbox = h.inbox(&dir.path().join("receiver.db"));
    for control in Delivery::open(&path, &binding(&h), true)?.announcements()? {
        inbox.receive_writer(&control, okay)?;
    }
    inbox.receive(GENERATION, 1, &bytes, okay)?;
    inbox.process(&h.key, okay)?;
    assert_eq!(inbox.projection()?, sender.projection()?);
    assert_eq!(counts(&path)?, (1, 1, 1));
    Ok(())
}

#[test]
fn restored_backup_reissues_unsealed_work_and_receivers_still_detect_the_fork() -> TestResult<()> {
    let mut h = Harness::new();
    h.config.writers.clear();
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("sender.db");
    let backup = dir.path().join("backup.db");
    let mut sender = open(&path, &h)?;
    let first = creation();
    sender.execute(&first, Fault::None)?;
    Connection::open(&path)?.execute("VACUUM INTO ?1", [backup.to_str().unwrap()])?;
    let original = seal(&mut sender, &first, &h)?;
    // The restored copy cannot know the original sealed the creation since:
    // it re-issues it as its own new writer, and editing continues.
    let mut restored = open(&backup, &h)?;
    assert_eq!(restored.projection()?, sender.projection()?);
    assert!(restored.recovery_required()?.is_empty());
    let reissued = seal(&mut restored, &first, &h)?;
    restored.execute(&edit("after restore"), Fault::None)?;
    assert_eq!(counts(&backup)?, (2, 2, 1));
    // Its handoff closes the old writer empty, which the original's record
    // contradicts: receivers freeze the device rather than pick a branch.
    let mut inbox = h.inbox(&dir.path().join("receiver.db"));
    for bytes in Delivery::open(&backup, &binding(&h), true)?.announcements()? {
        inbox.receive_writer(&bytes, okay)?;
    }
    inbox.receive(GENERATION, 1, &original, okay)?;
    inbox.receive(GENERATION, 2, &reissued, okay)?;
    inbox.process(&h.key, okay)?;
    assert!(inbox.entries()?.iter().all(|e| e.status == "fork"));
    Ok(())
}

#[test]
#[ignore = "invoked by atomic_sender_process_exit_boundaries"]
fn atomic_sender_crash_worker() -> TestResult<()> {
    if ["MATTERS_ATOMIC_DIR", "MATTERS_ATOMIC_POINT"]
        .iter()
        .all(|k| std::env::var_os(k).is_none())
    {
        return Ok(());
    }
    let dir = std::path::PathBuf::from(std::env::var("MATTERS_ATOMIC_DIR")?);
    let index: usize = std::env::var("MATTERS_ATOMIC_POINT")?.parse()?;
    let binding: Binding = serde_json::from_slice(&std::fs::read(dir.join("binding.json"))?)?;
    let request: Request = serde_json::from_slice(&std::fs::read(dir.join("request.json"))?)?;
    let device = outbox_cases::crash_device();
    let hook = |p| {
        let point = match index {
            0 => OutPoint::BeforeWriterCommit,
            1 => OutPoint::AfterWriterCommit,
            4 => OutPoint::AfterReservationCommit,
            5 => OutPoint::AfterSeal,
            6 => OutPoint::BeforeOutboxCommit,
            _ => OutPoint::AfterOutboxCommit,
        };
        if p == point {
            std::process::exit(160 + index as i32);
        }
        Ok(())
    };
    let mut sender = Sender::open(
        &dir.join("sender.db"),
        binding,
        random,
        |m| Ok(sign(m, &device)?),
        hook,
    )?;
    sender.execute(
        &request,
        match index {
            2 => Fault::ExitBeforeCommit,
            3 => Fault::ExitAfterCommit,
            _ => Fault::None,
        },
    )?;
    sender.seal(
        batch(&request.id)?,
        random,
        |header, plain| Ok(seal_fixture(header, &epoch_key(0), plain, &device)?),
        hook,
    )?;
    panic!("crash point not reached")
}

#[test]
fn atomic_sender_process_exit_boundaries() -> TestResult<()> {
    for index in 0..8 {
        let mut h = Harness::with_receiver(outbox_cases::crash_device());
        h.config.writers.clear();
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("sender.db");
        let bind = Binding {
            author: h.s.b.id()?.to_vec(),
            ..binding(&h)
        };
        let request = creation();
        std::fs::write(dir.path().join("binding.json"), serde_json::to_vec(&bind)?)?;
        std::fs::write(
            dir.path().join("request.json"),
            serde_json::to_vec(&request)?,
        )?;
        let output = std::process::Command::new(std::env::current_exe()?)
            .args([
                "--exact",
                "inbox_cases::atomic_sender_cases::atomic_sender_crash_worker",
                "--ignored",
            ])
            .env("MATTERS_ATOMIC_DIR", dir.path())
            .env("MATTERS_ATOMIC_POINT", index.to_string())
            .output()?;
        let expected = match index {
            2 => 91,
            3 => 92,
            _ => 160 + index,
        };
        assert_eq!(
            output.status.code(),
            Some(expected),
            "{index}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        if index == 0 {
            assert!(!matters_sync_prototype::has_user_schema(
                &Connection::open(&path)?
            )?);
        } else {
            assert_eq!(
                counts(&path)?,
                (
                    i64::from(index >= 3),
                    i64::from(index >= 3),
                    i64::from(index == 7)
                )
            );
        }
        let mut sender = Sender::open(
            &path,
            bind.clone(),
            random,
            |m| Ok(sign(m, &h.s.b)?),
            |_| Ok(()),
        )?;
        if index >= 3 {
            assert_eq!(sender.projection()?.entities[TASK].title, "private task");
            sender.execute(&request, Fault::None)?;
            assert_eq!(counts(&path)?.0, 1);
        } else {
            assert!(sender.projection()?.entities.is_empty());
        }
        if (3..7).contains(&index) {
            // Saved but never sealed: re-issued by the new writer, then
            // sealed and received like any other edit.
            assert!(sender.recovery_required()?.is_empty());
            sender.seal(
                batch(&request.id)?,
                random,
                |header, plain| Ok(seal_fixture(header, &h.cek, plain, &h.s.b)?),
                |_| Ok(()),
            )?;
            sender.execute(&edit("not blocked"), Fault::None)?;
        }
        if index >= 3 {
            let delivery = Delivery::open(&path, &bind, true)?;
            let mut inbox = h.inbox(&dir.path().join("receiver.db"));
            for bytes in delivery.announcements()? {
                inbox.receive_writer(&bytes, okay)?;
            }
            inbox.receive(GENERATION, 1, &delivery.pending()?[0].bytes, okay)?;
            inbox.process(&h.key, okay)?;
            assert_eq!(
                inbox.projection()?.entities[TASK].title,
                "private task",
                "{index}: the creation was received"
            );
        }
    }
    Ok(())
}

fn writers(path: &Path) -> TestResult<i64> {
    Ok(Connection::open(path)?.query_row("SELECT count(*) FROM writers", [], |r| r.get(0))?)
}

#[test]
fn review_delivered_intents_do_not_count_against_the_in_flight_budget() -> TestResult<()> {
    let h = Harness::new();
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("sender.db");
    let mut sender = open(&path, &h)?;
    let mut delivery = Delivery::open(&path, &binding(&h), false)?;
    // Three times the budget of edits, each sealed and acknowledged.
    for n in 0..48 {
        let request = if n == 0 { creation() } else { edit("x") };
        sender.execute(&request, Fault::None)?;
        seal(&mut sender, &request, &h)?;
        let id = delivery.record(batch(&request.id)?)?.unwrap().id;
        delivery.acknowledge(id, |_| Ok(()))?;
    }
    assert_eq!(counts(&path)?, (48, 48, 48));
    // Sixteen undelivered edits still fill it.
    for _ in 0..16 {
        sender.execute(&edit("y"), Fault::None)?;
    }
    assert!(sender.execute(&edit("overflow"), Fault::None).is_err());
    assert_eq!(counts(&path)?.1, 64);
    Ok(())
}

#[test]
fn review_a_standalone_open_cannot_take_over_an_integrated_store() -> TestResult<()> {
    let h = Harness::new();
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("sender.db");
    let mut sender = open(&path, &h)?;
    let first = creation();
    sender.execute(&first, Fault::None)?;
    seal(&mut sender, &first, &h)?;
    drop(sender);
    let before = writers(&path)?;
    // As if the standalone open checked before the Sender created the file:
    // hide the domain marker for the duration of the open.
    let conn = Connection::open(&path)?;
    conn.execute_batch("ALTER TABLE prototype_meta RENAME TO hidden_meta;")?;
    drop(conn);
    // Announced, like the Sender's own outbox, so only the composition
    // tells them apart.
    assert!(Outbox::open_announced(
        &path,
        binding(&h),
        random,
        |m| Ok(sign(m, &h.s.a)?),
        |_| Ok(())
    )
    .is_err());
    let conn = Connection::open(&path)?;
    conn.execute_batch("ALTER TABLE hidden_meta RENAME TO prototype_meta;")?;
    drop(conn);
    assert_eq!(
        writers(&path)?,
        before,
        "no standalone writer took ownership"
    );
    let mut sender = open(&path, &h)?;
    sender.execute(&edit("still integrated"), Fault::None)?;
    let mode: String =
        Connection::open(&path)?.query_row("PRAGMA journal_mode", [], |r| r.get(0))?;
    assert_eq!(mode, "wal");
    Ok(())
}

#[test]
fn review_a_short_concurrent_write_delays_an_edit_instead_of_failing_it() -> TestResult<()> {
    let h = Harness::new();
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("sender.db");
    let mut sender = open(&path, &h)?;
    sender.execute(&creation(), Fault::None)?;
    let (locked, wait) = std::sync::mpsc::channel();
    let other = path.clone();
    // An acknowledgement-sized write on another connection.
    let writer = std::thread::spawn(move || -> rusqlite::Result<()> {
        let conn = Connection::open(other)?;
        conn.execute_batch("BEGIN IMMEDIATE;")?;
        locked.send(()).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(200));
        conn.execute_batch("COMMIT;")
    });
    wait.recv()?;
    sender.execute(&edit("after a short wait"), Fault::None)?;
    writer.join().unwrap()?;
    assert_eq!(counts(&path)?.0, 2);
    Ok(())
}

#[test]
fn review_the_writer_commit_point_precedes_the_rest_of_the_open() -> TestResult<()> {
    let mut h = Harness::new();
    h.config.writers.clear();
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("sender.db");
    let mut sender = open(&path, &h)?;
    let first = creation();
    sender.execute(&first, Fault::None)?;
    let first_bytes = seal(&mut sender, &first, &h)?;
    drop(sender);
    // The second phase of the open (the projection) fails after the writer
    // and its announcement have committed.
    Connection::open(&path)?.execute_batch(
        "CREATE TRIGGER refuse_projection BEFORE INSERT ON projection
         BEGIN SELECT RAISE(ABORT, 'injected projection failure'); END;",
    )?;
    let before = writers(&path)?;
    let mut points = vec![];
    let failed = Sender::open(
        &path,
        binding(&h),
        random,
        |m| Ok(sign(m, &h.s.a)?),
        |p| {
            points.push(p);
            Ok(())
        },
    );
    assert!(failed.is_err());
    assert_eq!(
        points,
        [OutPoint::BeforeWriterCommit, OutPoint::AfterWriterCommit],
        "the commit point is reached before the open fails"
    );
    assert_eq!(
        writers(&path)?,
        before + 1,
        "an announced writer was committed"
    );
    Connection::open(&path)?.execute_batch("DROP TRIGGER refuse_projection;")?;
    // The orphaned announced writer is an empty link in the signed chain:
    // the next open hands off past it and its edits are received.
    let mut sender = open(&path, &h)?;
    let request = edit("after the failed open");
    sender.execute(&request, Fault::None)?;
    let next = seal(&mut sender, &request, &h)?;
    let mut inbox = h.inbox(&dir.path().join("receiver.db"));
    for bytes in Delivery::open(&path, &binding(&h), true)?.announcements()? {
        inbox.receive_writer(&bytes, okay)?;
    }
    inbox.receive(GENERATION, 1, &first_bytes, okay)?;
    inbox.receive(GENERATION, 2, &next, okay)?;
    inbox.process(&h.key, okay)?;
    assert!(inbox
        .writer_entries()?
        .iter()
        .all(|r| r.status == "admitted"));
    assert_eq!(inbox.projection()?, sender.projection()?);
    Ok(())
}

#[test]
fn review_an_integrated_store_never_replays_an_inbox() -> TestResult<()> {
    let h = Harness::new();
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("sender.db");
    let mut sender = open(&path, &h)?;
    sender.execute(&creation(), Fault::None)?;
    drop(sender);
    // A row no integrated API can write (receive is disabled).
    Connection::open(&path)?.execute(
        "INSERT INTO inbox(id,bytes,status,reason) VALUES ('stray',x'00','received','')",
        [],
    )?;
    let sender = open(&path, &h)?;
    assert_eq!(sender.projection()?.entities[TASK].title, "private task");
    Ok(())
}

#[test]
fn review_a_restart_before_sealing_reissues_the_edit_and_editing_continues() -> TestResult<()> {
    for checkpointed in [false, true] {
        let mut h = Harness::new();
        h.config.writers.clear();
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("sender.db");
        let mut sender = open(&path, &h)?;
        let first = creation();
        sender.execute(&first, Fault::None)?;
        let sealed = seal(&mut sender, &first, &h)?;
        // Saved, never sealed; with a checkpoint that already holds it.
        let stranded = edit("saved before the crash");
        sender.execute(&stranded, Fault::None)?;
        if checkpointed {
            sender.checkpoint()?;
        }
        drop(sender);

        let mut sender = open(&path, &h)?;
        assert!(sender.recovery_required()?.is_empty());
        assert_eq!(
            sender.projection()?.entities[TASK].notes,
            "saved before the crash"
        );
        let reissued = seal(&mut sender, &stranded, &h)?;
        let later = edit("after the restart ");
        sender.execute(&later, Fault::None)?;
        let later_bytes = seal(&mut sender, &later, &h)?;
        assert_eq!(counts(&path)?, (3, 3, 3));

        let mut inbox = h.inbox(&dir.path().join("receiver.db"));
        for bytes in Delivery::open(&path, &binding(&h), true)?.announcements()? {
            inbox.receive_writer(&bytes, okay)?;
        }
        for (n, bytes) in [sealed, reissued, later_bytes].iter().enumerate() {
            inbox.receive(GENERATION, n as i64 + 1, bytes, okay)?;
        }
        inbox.process(&h.key, okay)?;
        assert!(
            inbox.entries()?.iter().all(|e| e.status == "applied"),
            "checkpointed {checkpointed}: {:?}",
            inbox
                .entries()?
                .iter()
                .map(|e| &e.reason)
                .collect::<Vec<_>>()
        );
        assert_eq!(inbox.projection()?, sender.projection()?);
        // A second restart has nothing left to re-issue.
        drop(sender);
        let sender = open(&path, &h)?;
        assert_eq!(inbox.projection()?, sender.projection()?);
    }
    Ok(())
}

#[test]
fn review_a_failed_reissue_leaves_the_stranded_edit_for_the_next_open() -> TestResult<()> {
    let h = Harness::new();
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("sender.db");
    let mut sender = open(&path, &h)?;
    let first = creation();
    sender.execute(&first, Fault::None)?;
    drop(sender);
    let journal = || -> TestResult<Vec<u8>> {
        Ok(Connection::open(&path)?.query_row(
            "SELECT bytes FROM journal WHERE id=?1",
            [&first.id],
            |r| r.get(0),
        )?)
    };
    let before = journal()?;
    Connection::open(&path)?.execute_batch(
        "CREATE TRIGGER refuse_reissue BEFORE UPDATE OF writer ON intents
         BEGIN SELECT RAISE(ABORT, 'injected reissue failure'); END;",
    )?;
    assert!(open(&path, &h).is_err());
    assert_eq!(journal()?, before, "the journal row is unchanged");
    Connection::open(&path)?.execute_batch("DROP TRIGGER refuse_reissue;")?;
    let mut sender = open(&path, &h)?;
    assert_ne!(journal()?, before, "re-issued on the next open");
    assert!(sender.recovery_required()?.is_empty());
    seal(&mut sender, &first, &h)?;
    Ok(())
}
