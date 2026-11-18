use super::{
    encrypted_outbox::{Binding, Outbox, Point},
    *,
};
use matters_sync_prototype::Result as TestResult;
use rusqlite::Connection;
use std::path::Path;

fn rng() -> TestResult<Id> {
    random32().map_err(|_| std::io::Error::other("entropy failed").into())
}
fn okay(_: Point) -> TestResult<()> {
    Ok(())
}
fn fail() -> Box<dyn std::error::Error + Send + Sync> {
    std::io::Error::other("injected fault").into()
}
fn at(target: Point) -> impl FnMut(Point) -> TestResult<()> {
    move |point| if point == target { Err(fail()) } else { Ok(()) }
}
fn binding(s: &Setup) -> Binding {
    Binding {
        application: route().app,
        tenant: route().tenant,
        collection: route().collection,
        document: DOC,
        epoch: s.authority.epoch().unwrap(),
        manifest: s.authority.manifest().to_vec(),
        author: s.a.id().unwrap().to_vec(),
    }
}
fn seal(h: wire::Header<'_>, cek: &Id, plain: &[u8], device: &Device) -> TestResult<Vec<u8>> {
    seal_fixture(h, cek, plain, device).map_err(|_| std::io::Error::other("sealing failed").into())
}
fn attempts(path: &Path) -> Vec<(Vec<u8>, String)> {
    let conn = Connection::open(path).unwrap();
    let mut q = conn
        .prepare("SELECT id,status FROM attempts ORDER BY seq")
        .unwrap();
    q.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap()
}
fn plaintext(path: &Path, batch: Id) -> Option<Vec<u8>> {
    use rusqlite::OptionalExtension;
    Connection::open(path)
        .unwrap()
        .query_row(
            "SELECT plaintext FROM intents WHERE batch=?1",
            [batch.as_slice()],
            |r| r.get(0),
        )
        .optional()
        .unwrap()
}
fn open_record(bytes: &[u8], binding: &Binding, cek: &Id, device: &Device) -> Vec<u8> {
    use hpke_rs_crypto::{types::AeadAlgorithm, HpkeCrypto};
    use hpke_rs_libcrux::{HpkeLibcrux, HpkeLibcruxPrng};
    use libcrux_ml_dsa::ml_dsa_65;
    let p = wire::parse(bytes, binding.context()).unwrap();
    ml_dsa_65::verify(
        &device.signing.verification_key,
        p.signed_bytes,
        p.header.kind.signature_context(),
        &ml_dsa_65::MLDSA65Signature::new(*p.signature),
    )
    .unwrap();
    let key =
        record_crypto::content_key(p.header.record_id, cek, &p.record_key_info().unwrap()).unwrap();
    HpkeLibcrux::<HpkeLibcruxPrng>::aead_open(
        AeadAlgorithm::Aes256Gcm,
        &key,
        &[0; 12],
        p.header_bytes(),
        p.ciphertext,
    )
    .unwrap()
}

#[test]
fn durable_bytes_round_trip_through_admitted_writer_and_survive_restart() -> TestResult<()> {
    let mut s = setup();
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("queue.db");
    let bind = binding(&s);
    let cek = rng()?;
    let mut queue = Outbox::open(&path, bind.clone(), rng)?;
    eprintln!("encrypted-outbox SQLite runtime: {}", rusqlite::version());
    assert_eq!(
        Connection::open(&path)?
            .pragma_query_value::<String, _>(None, "journal_mode", |r| r.get(0))?,
        "delete"
    );
    let writer = queue.writer();
    let start = sign(
        &super::writer(&s.authority, &s.a, writer.incarnation, writer.actor, None),
        &s.a,
    )
    .unwrap();
    s.authority.admit_writer(&start, DOC).unwrap();
    let envelope = s.authority.wrap_key(&s.a, s.b.id().unwrap(), &cek).unwrap();
    let key = s.authority.unwrap_key(&envelope, &s.b).unwrap();
    let batch = rng()?;
    let payload = update(writer.actor);
    queue.enqueue(batch, &payload, okay)?;
    let first = queue.seal(
        batch,
        rng,
        |h, p| {
            // A separate connection must observe the committed reservation before
            // the crypto callback can begin, with no uploadable record yet.
            let observer = Connection::open(&path)?;
            let (reserved, status): (Vec<u8>, String) = observer.query_row(
                "SELECT header,status FROM attempts WHERE id=?1",
                [h.record_id.as_slice()],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )?;
            assert_eq!(reserved, wire::header_bytes(h)?);
            assert_eq!(status, "reserved");
            assert_eq!(
                observer.query_row("SELECT count(*) FROM sealed", [], |r| r.get::<_, i64>(0))?,
                0
            );
            seal(h, &cek, p, &s.a)
        },
        okay,
    )?;
    assert_eq!(
        s.authority
            .open_first_data(&first.bytes, &key, &DOC)
            .unwrap(),
        payload
    );
    assert_eq!(queue.pending()?, vec![first.clone()]);
    drop(queue);
    let mut queue = Outbox::open(&path, bind, rng)?;
    assert_ne!(queue.writer().incarnation, writer.incarnation);
    assert_ne!(queue.writer().actor, writer.actor);
    queue.enqueue(batch, &payload, okay)?;
    let retry = queue.seal(
        batch,
        || panic!("retry must not use RNG"),
        |_, _| panic!("retry must not seal"),
        okay,
    )?;
    assert_eq!(retry, first);
    assert_eq!(attempts(&path).len(), 1);
    Ok(())
}

#[test]
fn fifo_and_hash_chain_advance_only_with_committed_ciphertext() -> TestResult<()> {
    let s = setup();
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("chain.db");
    let bind = binding(&s);
    let cek = rng()?;
    let mut queue = Outbox::open(&path, bind.clone(), rng)?;
    let doc = LoroDoc::new();
    doc.set_peer_id(queue.writer().actor)?;
    doc.get_text("notes").insert(0, "one")?;
    let first = doc.export(ExportMode::updates(&VersionVector::default()))?;
    let vv = doc.oplog_vv();
    doc.get_text("notes").insert(3, " two")?;
    let second = doc.export(ExportMode::updates(&vv))?;
    queue.enqueue([1; 32], &first, okay)?;
    queue.enqueue([2; 32], &second, okay)?;
    assert!(queue
        .seal([2; 32], rng, |_, _| panic!("out of order"), okay)
        .is_err());
    let one = queue.seal([1; 32], rng, |h, p| seal(h, &cek, p, &s.a), okay)?;
    assert!(queue
        .seal(
            [2; 32],
            rng,
            |h, p| seal(h, &cek, p, &s.a),
            at(Point::BeforeOutboxCommit)
        )
        .is_err());
    assert_eq!(queue.pending()?, vec![one.clone()]);
    let two = queue.seal([2; 32], rng, |h, p| seal(h, &cek, p, &s.a), okay)?;
    assert_eq!(
        wire::parse(&two.bytes, bind.context())?.header.previous,
        Some(&hash(&one.bytes))
    );
    let restored = LoroDoc::new();
    restored.import(&open_record(&one.bytes, &bind, &cek, &s.a))?;
    restored.import(&open_record(&two.bytes, &bind, &cek, &s.a))?;
    assert_eq!(restored.get_text("notes").to_string(), "one two");
    assert_eq!(
        attempts(&path)
            .iter()
            .map(|x| x.1.as_str())
            .collect::<Vec<_>>(),
        vec!["sealed", "abandoned", "sealed"]
    );
    Ok(())
}

#[test]
fn same_process_uncertain_seals_and_sql_rejection_consume_new_ids() -> TestResult<()> {
    for point in [
        Point::BeforeReservationCommit,
        Point::AfterReservationCommit,
        Point::AfterSeal,
        Point::BeforeOutboxCommit,
        Point::AfterOutboxCommit,
    ] {
        let s = setup();
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("fault.db");
        let cek = rng()?;
        let mut q = Outbox::open(&path, binding(&s), rng)?;
        q.enqueue([1; 32], &update(q.writer().actor), okay)?;
        let mut calls = 0;
        assert!(q
            .seal(
                [1; 32],
                rng,
                |h, p| {
                    calls += 1;
                    seal(h, &cek, p, &s.a)
                },
                at(point)
            )
            .is_err());
        let before = attempts(&path);
        let committed = q.record([1; 32])?;
        if !before.is_empty() && committed.is_none() {
            let consumed: Id = before[0].0.clone().try_into().unwrap();
            assert!(q
                .seal(
                    [1; 32],
                    || Ok(consumed),
                    |_, _| panic!("abandoned/reserved ID cannot be reused"),
                    okay
                )
                .is_err());
        }
        let record = q.seal(
            [1; 32],
            rng,
            |h, p| {
                calls += 1;
                seal(h, &cek, p, &s.a)
            },
            okay,
        )?;
        if point == Point::AfterOutboxCommit {
            assert_eq!(Some(record), committed);
            assert_eq!(calls, 1);
        } else {
            assert!(committed.is_none());
            if !before.is_empty() {
                assert_ne!(record.id.as_slice(), before[0].0);
                assert_eq!(attempts(&path)[0].1, "abandoned");
            }
        }
    }
    let s = setup();
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("sql.db");
    let cek = rng()?;
    let mut q = Outbox::open(&path, binding(&s), rng)?;
    q.enqueue([1; 32], &update(q.writer().actor), okay)?;
    let sql = Connection::open(&path)?;
    sql.execute_batch("CREATE TRIGGER deny_state BEFORE UPDATE OF status ON attempts WHEN NEW.status='sealed' BEGIN SELECT RAISE(ABORT,'injected persistence failure'); END;")?;
    assert!(q
        .seal([1; 32], rng, |h, p| seal(h, &cek, p, &s.a), okay)
        .is_err());
    assert!(q.pending()?.is_empty());
    assert_eq!(attempts(&path)[0].1, "reserved");
    sql.execute_batch("DROP TRIGGER deny_state;")?;
    let record = q.seal([1; 32], rng, |h, p| seal(h, &cek, p, &s.a), okay)?;
    assert!(wire::parse(&record.bytes, binding(&s).context())?
        .header
        .previous
        .is_none());
    assert_eq!(attempts(&path)[0].1, "abandoned");
    Ok(())
}

#[test]
fn batch_conflicts_binding_and_foreign_database_fail_without_losing_work() -> TestResult<()> {
    let s = setup();
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("queue.db");
    let bind = binding(&s);
    let mut q = Outbox::open(&path, bind.clone(), rng)?;
    let payload = update(q.writer().actor);
    assert!(q
        .enqueue([1; 32], &payload, at(Point::BeforeIntentCommit))
        .is_err());
    assert!(plaintext(&path, [1; 32]).is_none());
    assert!(q
        .enqueue([1; 32], &payload, at(Point::AfterIntentCommit))
        .is_err());
    assert_eq!(plaintext(&path, [1; 32]), Some(payload.clone()));
    assert!(q.enqueue([1; 32], b"different", okay).is_err());
    q.enqueue([1; 32], &payload, okay)?;
    for field in 0..7 {
        let mut wrong = bind.clone();
        match field {
            0 => wrong.application[0] ^= 1,
            1 => wrong.tenant[0] ^= 1,
            2 => wrong.collection[0] ^= 1,
            3 => wrong.document[0] ^= 1,
            4 => wrong.epoch += 1,
            5 => wrong.manifest[0] ^= 1,
            _ => wrong.author[0] ^= 1,
        }
        assert!(Outbox::open(&path, wrong, || panic!(
            "mismatched binding must fail before entropy"
        ))
        .is_err());
    }
    q.enqueue([2; 32], b"still active", okay)?;
    let foreign = dir.path().join("app.db");
    let app = Connection::open(&foreign)?;
    app.execute_batch("CREATE TABLE tasks(id TEXT); INSERT INTO tasks VALUES ('keep');")?;
    assert!(Outbox::open(&foreign, binding(&s), rng).is_err());
    assert_eq!(
        app.query_row(
            "SELECT count(*) FROM sqlite_master WHERE type='table'",
            [],
            |r| r.get::<_, i64>(0)
        )?,
        1
    );
    Ok(())
}

#[test]
fn entropy_failure_and_known_record_or_writer_collisions_never_seal() -> TestResult<()> {
    let s = setup();
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("rng.db");
    let bind = binding(&s);
    let cek = rng()?;
    assert!(Outbox::open(&path, bind.clone(), || Err(fail())).is_err());
    let mut q = Outbox::open(&path, bind.clone(), rng)?;
    let current = q.writer();
    q.enqueue([1; 32], &update(current.actor), okay)?;
    assert!(q
        .seal(
            [1; 32],
            || Err(fail()),
            |_, _| panic!("entropy failure must precede sealing"),
            okay
        )
        .is_err());
    assert!(attempts(&path).is_empty());
    let first = q.seal(
        [1; 32],
        || Ok([42; 32]),
        |h, p| seal(h, &cek, p, &s.a),
        okay,
    )?;
    q.enqueue([2; 32], b"second", okay)?;
    assert!(q
        .seal(
            [2; 32],
            || Ok(first.id),
            |_, _| panic!("record collision must precede sealing"),
            okay
        )
        .is_err());
    assert_eq!(attempts(&path).len(), 1);
    assert!(Outbox::open(&path, bind.clone(), || Ok(current.incarnation)).is_err());
    let mut n = 0;
    assert!(Outbox::open(&path, bind, || {
        n += 1;
        if n == 1 {
            rng()
        } else {
            let mut v = [0; 32];
            v[..8].copy_from_slice(&current.actor.to_le_bytes());
            Ok(v)
        }
    })
    .is_err());
    q.enqueue([3; 32], b"owner unchanged", okay)?;
    assert_eq!(q.record([1; 32])?, Some(first));
    Ok(())
}

#[test]
fn reopening_fences_old_handles_even_during_a_seal() -> TestResult<()> {
    let s = setup();
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("fence.db");
    let bind = binding(&s);
    let cek = rng()?;
    let mut old = Outbox::open(&path, bind.clone(), rng)?;
    old.enqueue([1; 32], &update(old.writer().actor), okay)?;
    let mut replacement = None;
    assert!(old
        .seal(
            [1; 32],
            rng,
            |h, p| {
                let bytes = seal(h, &cek, p, &s.a)?;
                replacement = Some(Outbox::open(&path, bind.clone(), rng)?);
                Ok(bytes)
            },
            okay
        )
        .is_err());
    assert!(old.pending()?.is_empty());
    assert_eq!(attempts(&path)[0].1, "abandoned");
    assert!(old.enqueue([2; 32], b"stale", okay).is_err());
    assert!(old
        .seal(
            [1; 32],
            rng,
            |_, _| panic!("fenced handle must not seal"),
            okay
        )
        .is_err());
    let mut new = replacement.unwrap();
    new.seal([1; 32], rng, |h, p| seal(h, &cek, p, &s.a), okay)?;
    assert!(old.acknowledge(new.pending()?[0].id, okay).is_err());
    Ok(())
}

#[test]
fn lost_ack_resends_exact_bytes_and_ack_never_deletes_history() -> TestResult<()> {
    let s = setup();
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("ack.db");
    let cek = rng()?;
    let mut q = Outbox::open(&path, binding(&s), rng)?;
    q.enqueue([1; 32], &update(q.writer().actor), okay)?;
    let record = q.seal([1; 32], rng, |h, p| seal(h, &cek, p, &s.a), okay)?;
    // Simulated durable relay receipt followed by lost ACK; upload queue remains.
    let mut relay = std::collections::BTreeMap::new();
    for _ in 0..2 {
        for r in q.pending()? {
            if let Some(old) = relay.insert(r.id, r.bytes.clone()) {
                assert_eq!(old, r.bytes);
            }
        }
    }
    assert_eq!(relay.len(), 1);
    assert!(q
        .acknowledge(record.id, at(Point::BeforeAckCommit))
        .is_err());
    assert_eq!(q.pending()?, vec![record.clone()]);
    assert!(q.acknowledge(record.id, at(Point::AfterAckCommit)).is_err());
    assert!(q.pending()?.is_empty());
    q.acknowledge(record.id, okay)?;
    assert_eq!(q.record([1; 32])?, Some(record));
    assert!(q.acknowledge([0; 32], okay).is_err());
    Ok(())
}

#[test]
fn clone_preserves_bytes_and_pending_intent_but_allocates_a_new_writer() -> TestResult<()> {
    let s = setup();
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("original.db");
    let clone = dir.path().join("clone.db");
    let bind = binding(&s);
    let cek = rng()?;
    let mut original = Outbox::open(&path, bind.clone(), rng)?;
    let old_writer = original.writer();
    original.enqueue([1; 32], &update(old_writer.actor), okay)?;
    let first = original.seal([1; 32], rng, |h, p| seal(h, &cek, p, &s.a), okay)?;
    original.enqueue([2; 32], b"retained pending intent", okay)?;
    assert!(original
        .seal(
            [2; 32],
            rng,
            |h, p| seal(h, &cek, p, &s.a),
            at(Point::AfterSeal)
        )
        .is_err());
    let consumed = attempts(&path)[1].0.clone();
    // SQLite makes a consistent live backup; never copy an active DB file directly.
    Connection::open(&path)?.execute("VACUUM INTO ?1", [clone.to_str().unwrap()])?;
    let mut restored = Outbox::open(&clone, bind.clone(), rng)?;
    assert_ne!(restored.writer(), old_writer);
    assert_ne!(restored.writer().actor, old_writer.actor);
    assert_eq!(restored.record([1; 32])?, Some(first));
    assert_eq!(plaintext(&clone, [2; 32]), plaintext(&path, [2; 32]));
    let a = original.seal([2; 32], rng, |h, p| seal(h, &cek, p, &s.a), okay)?;
    let b = restored.seal([2; 32], rng, |h, p| seal(h, &cek, p, &s.a), okay)?;
    assert_ne!(a.id, b.id);
    assert_ne!(a.id.as_slice(), consumed);
    assert_ne!(b.id.as_slice(), consumed);
    for q in [&mut original, &mut restored] {
        let writer = q.writer();
        let plain = update(writer.actor);
        q.enqueue([3; 32], &plain, okay)?;
        let r = q.seal([3; 32], rng, |h, p| seal(h, &cek, p, &s.a), okay)?;
        assert_eq!(
            *wire::parse(&r.bytes, bind.context())?.header.incarnation,
            writer.incarnation
        );
        assert_eq!(open_record(&r.bytes, &bind, &cek, &s.a), plain);
    }
    Ok(())
}

#[test]
fn bad_sealer_output_consumes_reservation_and_capacity_limits_preserve_history() -> TestResult<()> {
    let s = setup();
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("bounds.db");
    let bind = binding(&s);
    let cek = rng()?;
    let mut q = Outbox::open(&path, bind.clone(), rng)?;
    assert!(q
        .enqueue([99; 32], &vec![0; wire::MAX_PLAINTEXT_BYTES + 1], okay)
        .is_err());
    q.enqueue([0; 32], b"intent", okay)?;
    assert!(q
        .seal(
            [0; 32],
            rng,
            |mut h, p| {
                h.logical_batch = &[99; 32];
                seal(h, &cek, p, &s.a)
            },
            okay
        )
        .is_err());
    assert!(q.pending()?.is_empty());
    for _ in 1..64 {
        assert!(q
            .seal(
                [0; 32],
                rng,
                |_, _| panic!("stop before encryption"),
                at(Point::AfterReservationCommit)
            )
            .is_err());
    }
    assert_eq!(attempts(&path).len(), 64);
    assert!(q
        .seal(
            [0; 32],
            rng,
            |_, _| panic!("capacity must precede encryption"),
            okay
        )
        .is_err());
    for i in 1..16 {
        q.enqueue([i; 32], b"retained", okay)?;
    }
    assert!(q.enqueue([16; 32], b"over budget", okay).is_err());
    for _ in 1..16 {
        q = Outbox::open(&path, bind.clone(), rng)?;
    }
    assert!(Outbox::open(&path, bind, rng).is_err());
    q.enqueue([0; 32], b"intent", okay)?;
    assert_eq!(plaintext(&path, [0; 32]), Some(b"intent".to_vec()));
    Ok(())
}

// Public deterministic fixture credentials ONLY, so an abruptly exiting child
// and its parent can operate on the same synthetic authority without saving keys.
fn crash_device() -> Device {
    use hpke_rs::{Hpke, Mode};
    use hpke_rs_crypto::types::{AeadAlgorithm, KdfAlgorithm, KemAlgorithm};
    use hpke_rs_libcrux::HpkeLibcrux;
    let hpke: Hpke<HpkeLibcrux> = Hpke::try_new(
        Mode::Base,
        KemAlgorithm::MlKem768P256,
        KdfAlgorithm::HkdfSha256,
        AeadAlgorithm::Aes256Gcm,
    )
    .unwrap();
    Device {
        signing: libcrux_ml_dsa::ml_dsa_65::generate_key_pair([17; 32]),
        kem: hpke.derive_key_pair(&[19; 32]).unwrap(),
    }
}
fn crash_binding(device: &Device) -> Binding {
    Binding {
        application: [1; 16],
        tenant: [2; 16],
        collection: [3; 16],
        document: DOC,
        epoch: 0,
        manifest: vec![8; 48],
        author: device.id().unwrap().to_vec(),
    }
}
const CRASH_CEK: Id = [171; 32];
const POINTS: [Point; 9] = [
    Point::BeforeIntentCommit,
    Point::AfterIntentCommit,
    Point::BeforeReservationCommit,
    Point::AfterReservationCommit,
    Point::AfterSeal,
    Point::BeforeOutboxCommit,
    Point::AfterOutboxCommit,
    Point::BeforeAckCommit,
    Point::AfterAckCommit,
];
#[test]
#[ignore = "invoked as a child by abrupt_exit_covers_every_outbox_boundary"]
fn outbox_crash_worker() -> TestResult<()> {
    let path = std::env::var("MATTERS_OUTBOX_CRASH_DB")?;
    let target: usize = std::env::var("MATTERS_OUTBOX_CRASH_POINT")?.parse()?;
    let hook = |p| {
        if p == POINTS[target] {
            std::process::exit(100 + target as i32);
        }
        Ok(())
    };
    let device = crash_device();
    let mut q = Outbox::open(&path, crash_binding(&device), rng)?;
    q.enqueue([1; 32], &update(q.writer().actor), hook)?;
    let r = q.seal(
        [1; 32],
        rng,
        |h, p| {
            let bytes = seal(h, &CRASH_CEK, p, &device)?;
            let mut file = std::fs::File::create(format!("{path}.ciphertext"))?;
            std::io::Write::write_all(&mut file, &bytes)?;
            file.sync_all()?;
            Ok(bytes)
        },
        hook,
    )?;
    q.acknowledge(r.id, hook)?;
    panic!("selected crash point did not terminate process")
}
#[test]
fn abrupt_exit_covers_every_outbox_boundary() -> TestResult<()> {
    for (index, point) in POINTS.iter().enumerate() {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("crash.db");
        let output = std::process::Command::new(std::env::current_exe()?)
            .args(["--exact", "outbox_cases::outbox_crash_worker", "--ignored"])
            .env("MATTERS_OUTBOX_CRASH_DB", &path)
            .env("MATTERS_OUTBOX_CRASH_POINT", index.to_string())
            .output()?;
        assert_eq!(
            output.status.code(),
            Some(100 + index as i32),
            "{point:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let before = attempts(&path);
        let retained = plaintext(&path, [1; 32]);
        let device = crash_device();
        let bind = crash_binding(&device);
        let mut q = Outbox::open(&path, bind.clone(), rng)?;
        if let Some(p) = retained.as_ref() {
            q.enqueue([1; 32], p, okay)?;
        } else {
            assert_eq!(*point, Point::BeforeIntentCommit);
            q.enqueue([1; 32], &update(q.writer().actor), okay)?;
        }
        let durable = index >= 6;
        let mut calls = 0;
        let r = q.seal(
            [1; 32],
            rng,
            |h, p| {
                calls += 1;
                seal(h, &CRASH_CEK, p, &device)
            },
            okay,
        )?;
        assert_eq!(calls, usize::from(!durable));
        if durable {
            assert_eq!(
                r.bytes,
                std::fs::read(format!("{}.ciphertext", path.display()))?
            );
        } else if !before.is_empty() {
            assert_ne!(r.id.as_slice(), before[0].0);
            assert_eq!(attempts(&path)[0].1, "abandoned");
        }
        assert_eq!(
            open_record(&r.bytes, &bind, &CRASH_CEK, &device),
            plaintext(&path, [1; 32]).unwrap()
        );
        assert_eq!(q.pending()?.is_empty(), *point == Point::AfterAckCommit);
        assert_eq!(
            q.seal(
                [1; 32],
                || panic!("retry RNG"),
                |_, _| panic!("retry seal"),
                okay
            )?,
            r
        );
    }
    Ok(())
}

#[test]
fn sqlite_busy_at_commit_abandons_ciphertext_and_preserves_the_reservation() -> TestResult<()> {
    let s = setup();
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("busy.db");
    let cek = rng()?;
    let mut q = Outbox::open(&path, binding(&s), rng)?;
    let blocker = Connection::open(&path)?;
    blocker.execute_batch("BEGIN IMMEDIATE;")?;
    assert!(q.enqueue([1; 32], b"pending", okay).is_err());
    blocker.execute_batch("ROLLBACK;")?;
    assert!(plaintext(&path, [1; 32]).is_none());
    q.enqueue([1; 32], b"pending", okay)?;
    let mut read_lock = None;
    assert!(q
        .seal(
            [1; 32],
            rng,
            |h, p| {
                let bytes = seal(h, &cek, p, &s.a)?;
                let reader = Connection::open(&path)?;
                reader.execute_batch("BEGIN; SELECT * FROM outbox_meta;")?;
                read_lock = Some(reader);
                Ok(bytes)
            },
            okay
        )
        .is_err());
    read_lock.take().unwrap().execute_batch("ROLLBACK;")?;
    assert!(q.pending()?.is_empty());
    let consumed = attempts(&path)[0].0.clone();
    assert_eq!(attempts(&path)[0].1, "reserved");
    let record = q.seal([1; 32], rng, |h, p| seal(h, &cek, p, &s.a), okay)?;
    assert_ne!(record.id.as_slice(), consumed);
    assert_eq!(attempts(&path)[0].1, "abandoned");
    assert!(wire::parse(&record.bytes, binding(&s).context())?
        .header
        .previous
        .is_none());
    Ok(())
}
