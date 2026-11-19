use super::*;
use crate::encrypted_outbox::{Binding, Delivery, Outbox, Point as OutPoint};

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
fn announced(path: &std::path::Path, h: &Harness) -> TestResult<Outbox> {
    Outbox::open_announced(
        path,
        binding(h),
        random,
        |m| Ok(sign(m, &h.s.a)?),
        |_| Ok(()),
    )
}
fn handoff(h: &Harness, next: u8, previous: u8, closing: Option<Hash>) -> Vec<u8> {
    sign(
        &Message {
            route: route(),
            signer: h.s.a.id().unwrap(),
            body: Body::Handoff {
                manifest: h.s.authority.manifest(),
                document: DOC,
                device: h.s.a.id().unwrap(),
                incarnation: [next; 32],
                actor: next as u64,
                previous: [previous; 32],
                closing,
            },
        },
        &h.s.a,
    )
    .unwrap()
}
fn sealed(h: &Harness, actor: u8, plain: &[u8]) -> Vec<u8> {
    let author = h.s.a.id().unwrap();
    let manifest = h.s.authority.manifest();
    let id = random32().unwrap();
    seal_fixture(
        header(&h.s.authority, &author, &[actor; 32], &id, &manifest),
        &h.cek,
        plain,
        &h.s.a,
    )
    .unwrap()
}
fn imported(actor: u64, plain: &[u8]) -> Replica {
    let mut r = source(actor);
    r.receive(&matters_sync_prototype::Record {
        id: "history".into(),
        bytes: plain.to_vec(),
    })
    .unwrap();
    r
}

#[test]
fn ordinary_restart_delivers_old_ciphertext_and_new_actor_after_reversed_announcements(
) -> TestResult<()> {
    let mut h = Harness::new();
    h.config.writers.clear();
    let dir = tempfile::tempdir()?;
    let outgoing = dir.path().join("out.db");
    let incoming = dir.path().join("in.db");
    let mut q = announced(&outgoing, &h)?;
    let old = q.writer();
    let mut first_source = source(old.actor);
    let first_plain = create(&mut first_source);
    q.enqueue([1; 32], &first_plain, |_| Ok(()))?;
    let first = q.seal(
        [1; 32],
        random,
        |header, plain| Ok(seal_fixture(header, &h.cek, plain, &h.s.a)?),
        |_| Ok(()),
    )?;
    q.enqueue([2; 32], b"old unsent intent", |_| Ok(()))?;
    drop(q);
    let mut q = announced(&outgoing, &h)?;
    let new = q.writer();
    assert_ne!(old, new);
    assert_eq!(q.recovery_required()?.len(), 1);
    let mut next_source = imported(new.actor, &first_plain);
    let next_plain = notes(&mut next_source, "after restart");
    q.enqueue([3; 32], &next_plain, |_| Ok(()))?;
    let next = q.seal(
        [3; 32],
        random,
        |header, plain| Ok(seal_fixture(header, &h.cek, plain, &h.s.a)?),
        |_| Ok(()),
    )?;
    let delivery = Delivery::open(&outgoing, &binding(&h), true)?;
    let announcements = delivery.announcements()?;
    assert_eq!(announcements.len(), 2);
    let Body::Handoff {
        previous, closing, ..
    } = parse(&announcements[1])?.message.body
    else {
        panic!("handoff")
    };
    assert_eq!(previous, old.incarnation);
    assert_eq!(closing, Some(hash(&first.bytes)));
    assert_eq!(delivery.pending()?[0].bytes, first.bytes);
    let mut inbox = h.inbox(&incoming);
    inbox.receive(GENERATION, 1, &next.bytes, okay)?;
    inbox.process(&h.key, okay)?;
    assert_eq!(inbox.entries()?[0].reason, "writer-admission");
    for record in announcements.iter().rev() {
        inbox.receive_writer(record, okay)?;
    }
    inbox.process(&h.key, okay)?;
    assert_eq!(inbox.entries()?[0].reason, "writer-closing-history");
    drop(inbox);
    let mut inbox = h.inbox(&incoming);
    inbox.receive(GENERATION, 2, &first.bytes, okay)?;
    inbox.process(&h.key, okay)?;
    assert_eq!(inbox.projection()?, next_source.projection()?);
    assert!(inbox
        .writer_entries()?
        .iter()
        .all(|r| r.status == "admitted"));
    for record in &announcements {
        inbox.receive_writer(record, okay)?;
    }
    inbox.process(&h.key, okay)?;
    assert_eq!(inbox.writer_entries()?.len(), 2);
    assert_eq!(delivery.announcements()?, announcements);
    assert!(Outbox::open(&outgoing, binding(&h), random).is_err());
    Ok(())
}

#[test]
fn closing_prefix_accepts_delayed_ancestors_but_no_signed_extension() -> TestResult<()> {
    let h = Harness::new();
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("in.db");
    let mut a = source(71);
    let first = h.seal(71, None, &create(&mut a));
    let second = h.seal(71, Some(hash(&first)), &notes(&mut a, "last old"));
    let extra = h.seal(71, Some(hash(&second)), &notes(&mut a, "beyond boundary"));
    let mut inbox = h.inbox(&path);
    inbox.receive_writer(&handoff(&h, 73, 71, Some(hash(&second))), okay)?;
    inbox.receive(GENERATION, 1, &second, okay)?;
    inbox.process(&h.key, okay)?;
    assert_eq!(inbox.entries()?[0].reason, "writer-closing-history");
    inbox.receive(GENERATION, 2, &first, okay)?;
    inbox.process(&h.key, okay)?;
    assert_eq!(inbox.projection()?.entities[TASK].notes, "last old");
    let view = inbox.projection()?;
    inbox.receive(GENERATION, 3, &extra, okay)?;
    inbox.process(&h.key, okay)?;
    assert!(inbox.entries()?.iter().all(|r| r.status == "fork"));
    assert_eq!(inbox.projection()?, view);
    drop(inbox);
    let mut inbox = h.inbox(&path);
    inbox.process(&h.key, okay)?;
    assert_eq!(inbox.entries()?[2].bytes, extra);
    assert_eq!(inbox.entries()?[2].applied, None);
    Ok(())
}

#[test]
fn empty_handoffs_wait_for_all_earlier_nonempty_history() -> TestResult<()> {
    let h = Harness::new();
    let dir = tempfile::tempdir()?;
    let mut a = source(71);
    let plain = create(&mut a);
    let first = h.seal(71, None, &plain);
    let mut c = imported(74, &plain);
    let newest = sealed(&h, 74, &notes(&mut c, "new"));
    let mut inbox = h.inbox(&dir.path().join("in.db"));
    inbox.receive_writer(&handoff(&h, 74, 73, None), okay)?;
    inbox.receive_writer(&handoff(&h, 73, 71, Some(hash(&first))), okay)?;
    inbox.receive(GENERATION, 1, &newest, okay)?;
    inbox.process(&h.key, okay)?;
    assert_eq!(inbox.entries()?[0].reason, "writer-closing-history");
    inbox.receive(GENERATION, 2, &first, okay)?;
    inbox.process(&h.key, okay)?;
    assert_eq!(inbox.projection()?, c.projection()?);
    Ok(())
}

#[test]
fn competing_replacements_freeze_device_but_not_other_devices() -> TestResult<()> {
    let h = Harness::new();
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("in.db");
    let mut inbox = h.inbox(&path);
    let one = handoff(&h, 73, 71, None);
    let two = handoff(&h, 74, 71, None);
    inbox.receive_writer(&one, okay)?;
    inbox.receive_writer(&two, okay)?;
    let content = sealed(&h, 73, &create(&mut source(73)));
    let other = h.seal(72, None, &create(&mut source(72)));
    inbox.receive(GENERATION, 1, &content, okay)?;
    inbox.receive(GENERATION, 2, &other, okay)?;
    inbox.process(&h.key, okay)?;
    assert_eq!(inbox.entries()?[0].status, "fork");
    assert_eq!(inbox.entries()?[1].status, "applied");
    assert_eq!(inbox.writer_entries()?[1].status, "fork");
    drop(inbox);
    let mut inbox = h.inbox(&path);
    inbox.process(&h.key, okay)?;
    assert_eq!(inbox.writer_entries()?[1].bytes, two);
    assert_eq!(inbox.entries()?[0].status, "fork");
    Ok(())
}

#[test]
fn handoff_signature_covers_closing_head_and_forged_competitors_cannot_freeze() -> TestResult<()> {
    let h = Harness::new();
    let dir = tempfile::tempdir()?;
    let valid = handoff(&h, 73, 71, None);
    let parsed = parse(&valid)?;
    let mut changed = parsed.message.clone();
    if let Body::Handoff { closing, .. } = &mut changed.body {
        *closing = Some([5; 48]);
    }
    let forged = package(&changed, parsed.signature)?;
    let mut inbox = h.inbox(&dir.path().join("in.db"));
    inbox.receive_writer(&forged, okay)?;
    inbox.receive_writer(&valid, okay)?;
    let bytes = sealed(&h, 73, &create(&mut source(73)));
    inbox.receive(GENERATION, 1, &bytes, okay)?;
    inbox.process(&h.key, okay)?;
    assert_eq!(inbox.writer_entries()?[0].reason, "writer:Signature");
    assert_eq!(inbox.entries()?[0].status, "applied");
    Ok(())
}

#[test]
fn restored_outbox_produces_a_competing_handoff_without_signing_old_content() -> TestResult<()> {
    let mut h = Harness::new();
    h.config.writers.clear();
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("out.db");
    let backup = dir.path().join("backup.db");
    let q = announced(&path, &h)?;
    let writer = q.writer();
    drop(q);
    Connection::open(&path)?.execute("VACUUM INTO ?1", [backup.to_str().unwrap()])?;
    let _live = announced(&path, &h)?;
    let mut restored = announced(&backup, &h)?;
    assert_ne!(restored.writer(), writer);
    restored.enqueue([9; 32], b"new writer work", |_| Ok(()))?;
    let live = Delivery::open(&path, &binding(&h), true)?.announcements()?;
    let clone = Delivery::open(&backup, &binding(&h), true)?.announcements()?;
    assert_eq!(live[0], clone[0]);
    assert_ne!(live[1], clone[1]);
    let mut inbox = h.inbox(&dir.path().join("in.db"));
    for bytes in [&live[0], &live[1], &clone[1]] {
        inbox.receive_writer(bytes, okay)?;
    }
    inbox.process(&h.key, okay)?;
    assert_eq!(inbox.writer_entries()?[2].status, "fork");
    assert_eq!(restored.next_batch()?, Some([9; 32])); // Local unsent work remains available.
    Ok(())
}

#[test]
fn writer_receipt_and_admission_roll_back_together_with_projection_on_error() -> TestResult<()> {
    let h = Harness::new();
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("in.db");
    let transition = handoff(&h, 73, 71, None);
    let mut inbox = h.inbox(&path);
    assert!(inbox
        .receive_writer(&transition, fail(Point::BeforeWriterReceipt))
        .is_err());
    assert!(inbox.writer_entries()?.is_empty());
    assert!(inbox
        .receive_writer(&transition, fail(Point::AfterWriterReceipt))
        .is_err());
    assert_eq!(inbox.writer_entries()?.len(), 1);
    let conn = Connection::open(&path)?;
    conn.execute_batch("CREATE TRIGGER fail_writer_apply BEFORE UPDATE OF projection ON inbox_meta BEGIN SELECT RAISE(ABORT,'fail'); END;")?;
    assert!(inbox.process(&h.key, okay).is_err());
    assert_eq!(inbox.writer_entries()?[0].admitted, None);
    conn.execute_batch("DROP TRIGGER fail_writer_apply;")?;
    inbox.process(&h.key, okay)?;
    assert_eq!(inbox.writer_entries()?[0].admitted, Some(1));
    Ok(())
}

#[test]
fn announced_open_is_atomic_and_cannot_downgrade_or_change_signed_fields() -> TestResult<()> {
    let h = Harness::new();
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("out.db");
    let q = announced(&path, &h)?;
    let old = q.writer();
    drop(q);
    let before = Delivery::open(&path, &binding(&h), true)?.announcements()?;
    assert!(Outbox::open_announced(
        &path,
        binding(&h),
        random,
        |m| Ok(sign(m, &h.s.a)?),
        |p| {
            if p == OutPoint::BeforeWriterCommit {
                Err(std::io::Error::other("injected").into())
            } else {
                Ok(())
            }
        }
    )
    .is_err());
    assert_eq!(
        Delivery::open(&path, &binding(&h), true)?.announcements()?,
        before
    );
    assert!(Outbox::open_announced(
        &path,
        binding(&h),
        random,
        |m| {
            let mut m = m.clone();
            if let Body::Handoff { closing, .. } = &mut m.body {
                *closing = Some([99; 48]);
            }
            Ok(sign(&m, &h.s.a)?)
        },
        |_| Ok(())
    )
    .is_err());
    let next = announced(&path, &h)?;
    assert_ne!(next.writer(), old);
    let receipts = Delivery::open(&path, &binding(&h), true)?.announcements()?;
    assert_eq!(receipts.len(), 2);
    let Body::Handoff {
        closing, previous, ..
    } = parse(&receipts[1])?.message.body
    else {
        panic!("handoff")
    };
    assert_eq!(closing, None);
    assert_eq!(previous, old.incarnation);
    Ok(())
}

#[test]
fn high_bit_actor_identity_round_trips_through_restart() -> TestResult<()> {
    let mut h = Harness::new();
    let actor = (1_u64 << 63) + 17;
    h.config.writers[0] = sign(
        &writer(&h.s.authority, &h.s.a, [71; 32], actor, None),
        &h.s.a,
    )?;
    let mut sender = source(actor);
    let bytes = h.seal(71, None, &create(&mut sender));
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("in.db");
    let mut inbox = h.inbox(&path);
    inbox.receive(GENERATION, 1, &bytes, okay)?;
    inbox.process(&h.key, okay)?;
    assert_eq!(inbox.entries()?[0].status, "applied");
    drop(inbox);
    let mut inbox = h.inbox(&path);
    inbox.process(&h.key, okay)?;
    assert_eq!(inbox.projection()?, sender.projection()?);
    Ok(())
}

#[test]
fn independent_new_content_waits_for_the_signed_closing_prefix() -> TestResult<()> {
    let h = Harness::new();
    let dir = tempfile::tempdir()?;
    let old = h.seal(71, None, &create(&mut source(71)));
    let mut independent = source(73);
    let new = sealed(
        &h,
        73,
        &create_id(&mut independent, "00000000-0000-4000-8000-000000000002"),
    );
    let mut inbox = h.inbox(&dir.path().join("in.db"));
    inbox.receive_writer(&handoff(&h, 73, 71, Some(hash(&old))), okay)?;
    inbox.receive(GENERATION, 1, &new, okay)?;
    inbox.process(&h.key, okay)?;
    assert_eq!(inbox.entries()?[0].reason, "writer-closing-history");
    assert!(inbox.projection()?.entities.is_empty());
    inbox.receive(GENERATION, 2, &old, okay)?;
    inbox.process(&h.key, okay)?;
    assert_eq!(inbox.projection()?.entities.len(), 2);
    Ok(())
}

#[test]
fn an_empty_closure_cannot_erase_previously_accepted_history() -> TestResult<()> {
    let h = Harness::new();
    let dir = tempfile::tempdir()?;
    let old = h.seal(71, None, &create(&mut source(71)));
    let mut inbox = h.inbox(&dir.path().join("in.db"));
    inbox.receive(GENERATION, 1, &old, okay)?;
    inbox.process(&h.key, okay)?;
    let view = inbox.projection()?;
    inbox.receive_writer(&handoff(&h, 73, 71, None), okay)?;
    inbox.process(&h.key, okay)?;
    assert_eq!(inbox.entries()?[0].status, "fork");
    assert_eq!(inbox.entries()?[0].applied, Some(1));
    assert_eq!(inbox.projection()?, view);
    Ok(())
}

#[test]
fn writer_input_limits_and_legacy_replacements_fail_without_losing_retained_controls(
) -> TestResult<()> {
    let h = Harness::new();
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("in.db");
    let mut inbox = h.inbox(&path);
    assert!(inbox
        .receive_writer(&vec![0; MAX_CONTROL + 1], okay)
        .is_err());
    let legacy = sign(
        &writer(&h.s.authority, &h.s.a, [73; 32], 73, Some([71; 32])),
        &h.s.a,
    )?;
    inbox.receive_writer(&legacy, okay)?;
    for i in 1..MAX_RECORDS {
        inbox.receive_writer(&i.to_le_bytes(), okay)?;
    }
    assert!(inbox.receive_writer(b"full", okay).is_err());
    drop(inbox);
    let mut inbox = h.inbox(&path);
    inbox.receive_writer(&legacy, okay)?;
    inbox.process(&h.key, okay)?;
    assert_eq!(inbox.writer_entries()?.len() as i64, MAX_RECORDS);
    assert_eq!(inbox.writer_entries()?[0].reason, "writer:Unsupported");
    Ok(())
}

#[test]
#[ignore = "invoked by lifecycle_process_exit_boundaries"]
fn lifecycle_crash_worker() -> TestResult<()> {
    if ["MATTERS_WRITER_CRASH_DIR", "MATTERS_WRITER_CRASH_POINT"]
        .iter()
        .all(|k| std::env::var_os(k).is_none())
    {
        return Ok(());
    }
    let dir = std::path::PathBuf::from(std::env::var("MATTERS_WRITER_CRASH_DIR")?);
    let index: usize = std::env::var("MATTERS_WRITER_CRASH_POINT")?.parse()?;
    let config: Config = serde_json::from_slice(&std::fs::read(dir.join("config.json"))?)?;
    let device = outbox_cases::crash_device();
    if index < 2 {
        let bind = Binding {
            application: config.route.app,
            tenant: config.route.tenant,
            collection: config.route.collection,
            document: DOC,
            epoch: 0,
            manifest: hash(&config.genesis).to_vec(),
            author: device.id()?.to_vec(),
        };
        Outbox::open_announced(
            dir.join("out.db"),
            bind,
            random,
            |m| Ok(sign(m, &device)?),
            |p| {
                if (index == 0 && p == OutPoint::BeforeWriterCommit)
                    || (index == 1 && p == OutPoint::AfterWriterCommit)
                {
                    std::process::exit(140 + index as i32);
                }
                Ok(())
            },
        )?;
    } else {
        let key = config
            .authority()?
            .unwrap_key(&std::fs::read(dir.join("envelope"))?, &device)?;
        let hook = |p| {
            if p == [
                Point::BeforeWriterReceipt,
                Point::AfterWriterReceipt,
                Point::BeforeApply,
                Point::AfterApply,
            ][index - 2]
            {
                std::process::exit(140 + index as i32);
            }
            Ok(())
        };
        let mut inbox = Inbox::open(dir.join("in.db"), config)?;
        inbox.receive_writer(&std::fs::read(dir.join("handoff"))?, hook)?;
        inbox.process(&key, hook)?;
    }
    panic!("crash point not reached")
}

#[test]
fn lifecycle_process_exit_boundaries() -> TestResult<()> {
    for index in 0..6 {
        let h = Harness::with_receiver(outbox_cases::crash_device());
        let dir = tempfile::tempdir()?;
        let transition = handoff(&h, 73, 71, None);
        std::fs::write(
            dir.path().join("config.json"),
            serde_json::to_vec(&h.config)?,
        )?;
        std::fs::write(dir.path().join("envelope"), &h.envelope)?;
        std::fs::write(dir.path().join("handoff"), &transition)?;
        let output = std::process::Command::new(std::env::current_exe()?)
            .args([
                "--exact",
                "inbox_cases::lifecycle::lifecycle_crash_worker",
                "--ignored",
            ])
            .env("MATTERS_WRITER_CRASH_DIR", dir.path())
            .env("MATTERS_WRITER_CRASH_POINT", index.to_string())
            .output()?;
        assert_eq!(
            output.status.code(),
            Some(140 + index as i32),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        if index < 2 {
            let path = dir.path().join("out.db");
            let bind = Binding {
                author: h.s.b.id()?.to_vec(),
                ..binding(&h)
            };
            let retained = if index == 0 {
                assert!(!matters_sync_prototype::has_user_schema(
                    &Connection::open(&path)?
                )?);
                Vec::new()
            } else {
                Delivery::open(&path, &bind, true)?.announcements()?
            };
            let _next = Outbox::open_announced(
                &path,
                bind.clone(),
                random,
                |m| Ok(sign(m, &h.s.b)?),
                |_| Ok(()),
            )?;
            let controls = Delivery::open(&path, &bind, true)?.announcements()?;
            assert_eq!(controls.len(), index + 1);
            if index == 1 {
                assert_eq!(controls[0], retained[0]);
            }
            let mut authority = Authority::bootstrap(&h.s.genesis, route(), &h.config.root)?;
            for bytes in controls {
                authority.admit_writer(&bytes, DOC)?;
            }
        } else {
            let mut inbox = h.inbox(&dir.path().join("in.db"));
            let entries = inbox.writer_entries()?;
            assert_eq!(entries.len(), usize::from(index != 2));
            if index > 2 {
                assert_eq!(entries[0].admitted, if index == 5 { Some(1) } else { None });
            }
            inbox.receive_writer(&transition, okay)?;
            inbox.process(&h.key, okay)?;
            assert_eq!(inbox.writer_entries()?[0].admitted, Some(1));
        }
    }
    Ok(())
}

#[test]
fn closing_head_cannot_name_another_devices_record() -> TestResult<()> {
    let h = Harness::new();
    let dir = tempfile::tempdir()?;
    let other = h.seal(72, None, &create(&mut source(72)));
    let new = sealed(
        &h,
        73,
        &create_id(&mut source(73), "00000000-0000-4000-8000-000000000002"),
    );
    let mut inbox = h.inbox(&dir.path().join("in.db"));
    inbox.receive_writer(&handoff(&h, 73, 71, Some(hash(&other))), okay)?;
    inbox.receive(GENERATION, 1, &other, okay)?;
    inbox.receive(GENERATION, 2, &new, okay)?;
    inbox.process(&h.key, okay)?;
    assert_eq!(inbox.entries()?[0].status, "applied");
    assert_eq!(inbox.entries()?[1].status, "fork");
    Ok(())
}

#[test]
fn both_present_and_absent_closing_heads_are_signature_bound() -> TestResult<()> {
    let h = Harness::new();
    for closing in [None, Some([5; 48])] {
        let bytes = handoff(&h, 73, 71, closing);
        let parsed = parse(&bytes)?;
        let mut changed = parsed.message.clone();
        if let Body::Handoff { closing, .. } = &mut changed.body {
            *closing = if closing.is_some() {
                None
            } else {
                Some([5; 48])
            };
        }
        let mut authority = h.config.authority()?;
        assert_eq!(
            authority.admit_writer(&package(&changed, parsed.signature)?, DOC),
            Err(Error::Signature)
        );
        assert!(authority.admit_writer(&bytes, DOC)?);
    }
    Ok(())
}
