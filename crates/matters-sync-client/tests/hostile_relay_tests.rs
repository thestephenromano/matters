//! The client against a relay that misbehaves (#45), scripted by
//! `support::Mock`: no PostgreSQL needed. The relay is trusted with
//! availability only, so each misbehaviour ends in a retry or a status the
//! interface can show, never in a skipped transition, a loop without end,
//! unbounded memory or a stalled shutdown.

mod support;

use std::sync::Arc;
use std::time::Duration;

use matters_core::storage::membership::SyncStanding;
use matters_core::{Database, Task};
use matters_sync::authority::{parse, Body};
use matters_sync::record::record_hash;
use matters_sync_client::{
    check_endpoint, Attention, Client, LocalStore, Log, Status, SyncError, MAX_RETRY_AFTER,
};
use support::{admit, pair, serve, Challenge, Mock, RecordsBody};

const WAIT: Duration = Duration::from_secs(20);

fn id(n: u128) -> String {
    uuid::Uuid::from_u128(n).to_string()
}

fn retry_reason(result: Result<impl std::fmt::Debug, SyncError>) -> String {
    match result {
        Err(SyncError::Retry { reason, .. }) => reason,
        other => panic!("expected a retry, got {other:?}"),
    }
}

/// The authority log as A published it.
fn published(a: &Database) -> Vec<Vec<u8>> {
    a.outgoing_controls()
        .unwrap()
        .into_iter()
        .map(|o| o.bytes)
        .collect()
}

#[test]
fn only_https_reaches_a_relay_off_this_machine() {
    let dir = tempfile::tempdir().unwrap();
    let db = Arc::new(Database::open(dir.path().join("a.db")).unwrap());
    for refused in [
        "http://relay.example",
        "http://10.0.0.1:8080",
        "http://127.0.0.1.relay.example",
        "ws://127.0.0.1:1",
        "relay.example",
        "",
    ] {
        assert!(check_endpoint(refused).is_err(), "{refused}");
        assert!(Client::new(refused, db.clone()).is_err(), "{refused}");
    }
    for accepted in [
        "https://relay.example",
        "https://relay.example:8443/",
        "http://127.0.0.1:1",
        "http://127.8.9.10:1",
        "http://[::1]:1",
        "http://localhost:1",
    ] {
        assert_eq!(check_endpoint(accepted), Ok(()), "{accepted}");
        assert!(Client::new(accepted, db.clone()).is_ok(), "{accepted}");
    }
}

/// A relay that answers `complete: false` with the same cursor forever is
/// left after one page, not followed.
#[tokio::test(flavor = "multi_thread")]
async fn a_cursor_that_never_advances_is_a_relay_error() {
    let p = pair();
    let mock = Arc::new(Mock::default());
    mock.behave(|b| b.stuck_authority = true);
    let client = Client::new(&serve(mock.clone()).await, p.b.clone()).unwrap();
    let reason = retry_reason(
        tokio::time::timeout(WAIT, client.sync_once())
            .await
            .unwrap(),
    );
    assert!(reason.contains("does not advance"), "{reason}");
    assert_eq!(mock.count("GET authority"), 1);
}

/// A response body is read up to 32 MiB, whether its length is declared or
/// it streams without end.
#[tokio::test(flavor = "multi_thread")]
async fn an_oversized_body_is_a_relay_error() {
    let p = pair();
    for body in [
        RecordsBody::Endless,
        RecordsBody::Declared(matters_sync_client::MAX_RESPONSE_BYTES + 1),
    ] {
        let mock = Arc::new(Mock::default());
        mock.behave(|b| b.records_body = body);
        let client = Client::new(&serve(mock.clone()).await, p.b.clone()).unwrap();
        let reason = retry_reason(
            tokio::time::timeout(WAIT, client.sync_once())
                .await
                .unwrap(),
        );
        assert!(reason.contains("too large"), "{body:?}: {reason}");
        assert_eq!(p.b.sync_cursor(Log::Records).unwrap(), None);
    }
}

/// A relay serving a forged envelope for this device ahead of the genuine
/// one cannot make it skip the transition.
#[tokio::test(flavor = "multi_thread")]
async fn a_forged_envelope_cannot_make_a_device_skip_a_transition() {
    let p = pair();
    let b_id = p.b.sync_identity().unwrap().1;
    // A admits C: a new epoch, with an envelope of its key for B.
    let c = Database::open(p.dir.path().join("c.db")).unwrap();
    admit(&p.a, &c);
    let mut log = published(&p.a);
    let transition = log
        .iter()
        .rev()
        .find(|bytes| matches!(parse(bytes).unwrap().message.body, Body::Membership(_)))
        .unwrap()
        .clone();
    let manifest = record_hash(&transition);
    let genuine = log
        .iter()
        .position(|bytes| {
            matches!(parse(bytes).unwrap().message.body,
                Body::KeyEnvelope(ref k) if k.recipient == b_id && k.manifest == manifest)
        })
        .unwrap();
    // Damaged inside the signed envelope: still addressed to B, for this
    // transition, but it does not open.
    let mut forged = log[genuine].clone();
    let at = forged.len() - 3400;
    forged[at] ^= 1;
    assert!(matches!(parse(&forged).unwrap().message.body,
        Body::KeyEnvelope(ref k) if k.recipient == b_id && k.manifest == manifest));
    assert_ne!(forged, log[genuine]);
    let (a, behind) = (
        p.a.sync_membership().unwrap(),
        p.b.sync_membership().unwrap(),
    );
    assert_ne!(behind.manifest, a.manifest);
    // Served the forgery alone, B waits for a genuine envelope: the
    // transition is neither applied nor skipped past.
    let mut only_forged = log.clone();
    only_forged[genuine] = forged.clone();
    let mock = Arc::new(Mock::default());
    *mock.authority.lock().unwrap() = only_forged;
    let client = Client::new(&serve(mock.clone()).await, p.b.clone()).unwrap();
    client.sync_once().await.unwrap();
    assert_eq!(p.b.sync_membership().unwrap().manifest, behind.manifest);
    assert_eq!(
        p.b.sync_cursor(Log::Authority).unwrap(),
        None,
        "not past it"
    );
    // Served the forgery ahead of the genuine envelope, B tries both.
    log.insert(genuine, forged);
    *mock.authority.lock().unwrap() = log;
    client.sync_once().await.unwrap();
    let b = p.b.sync_membership().unwrap();
    assert_eq!((b.epoch, b.manifest), (a.epoch, a.manifest));
    assert_eq!(b.standing, SyncStanding::Active);
    assert_eq!(b.members.len(), 3);
}

/// A refused control of this device's own is set aside, and the round goes
/// on to pull: how an old admin whose queued change went stale learns what
/// made it so.
#[tokio::test(flavor = "multi_thread")]
async fn a_refused_control_is_set_aside_and_the_round_pulls() {
    let p = pair();
    let queued = p.a.outgoing_controls().unwrap().len();
    assert!(queued > 0);
    p.a.add_task(&mut Task::new(id(1), "an edit", 0.0)).unwrap();
    let mock = Arc::new(Mock::default());
    mock.behave(|b| b.refuse_controls = true);
    let client = Client::new(&serve(mock.clone()).await, p.a.clone()).unwrap();
    let report = client.sync_once().await.unwrap();
    assert_eq!(report.set_aside_controls, queued, "{report:?}");
    assert_eq!(report.pushed_controls, 0);
    assert!(report.pushed_records > 0, "{report:?}");
    assert!(mock.count("GET authority") > 0 && mock.count("GET records") > 0);
    assert!(p.a.outgoing_controls().unwrap().is_empty(), "set aside");
    // A restore queues them again.
    p.a.sync_generation_changed().unwrap();
    assert_eq!(p.a.outgoing_controls().unwrap().len(), queued);
}

/// A relay restored from a backup older than this device's membership
/// takes its transitions without a session, and lets it in.
#[tokio::test(flavor = "multi_thread")]
async fn a_device_the_restored_relay_forgot_publishes_its_membership_and_signs_in() {
    let p = pair();
    let mock = Arc::new(Mock::default());
    mock.behave(|b| {
        b.challenge = Challenge::ForbiddenUntilPublished;
        b.unauthenticated_put = Some(201);
    });
    let client = Client::new(&serve(mock.clone()).await, p.b.clone()).unwrap();
    client.sync_once().await.unwrap();
    let republished = mock.unauthenticated_puts.lock().unwrap().clone();
    let transitions: Vec<Vec<u8>> =
        p.b.sync_controls()
            .unwrap()
            .into_iter()
            .filter(|c| matches!(parse(c).unwrap().message.body, Body::Membership(_)))
            .collect();
    assert!(!transitions.is_empty());
    assert_eq!(
        republished, transitions,
        "every transition B knows, in order"
    );
}

/// Still refused after publishing: the relay is behind if it would not take
/// the transitions, and this device was removed if it took them all.
#[tokio::test(flavor = "multi_thread")]
async fn a_relay_behind_is_told_apart_from_a_removal() {
    for (put, expected) in [
        (None, Attention::RelayBehind),
        (Some(503), Attention::RelayBehind),
        (Some(200), Attention::NotAMember),
    ] {
        let p = pair();
        let mock = Arc::new(Mock::default());
        mock.behave(|b| {
            b.challenge = Challenge::Forbidden;
            b.unauthenticated_put = put;
        });
        let client = Client::new(&serve(mock.clone()).await, p.b.clone()).unwrap();
        match client.sync_once().await {
            Err(SyncError::Attention(attention)) => assert_eq!(attention, expected, "{put:?}"),
            other => panic!("{put:?}: {other:?}"),
        }
        assert_eq!(mock.count("POST challenge"), 2, "signed in once more");
    }
}

/// After a restore, every control the device knows goes back to the relay,
/// whoever signed it: here A's transition admitting B, re-sent by B.
#[tokio::test(flavor = "multi_thread")]
async fn a_generation_change_republishes_every_known_control() {
    let p = pair();
    let mock = Arc::new(Mock::default());
    *mock.authority.lock().unwrap() = published(&p.a);
    let client = Client::new(&serve(mock.clone()).await, p.b.clone()).unwrap();
    client.sync_once().await.unwrap();
    mock.control_puts.lock().unwrap().clear();
    mock.new_generation();
    client.sync_once().await.unwrap();
    let a_signed: Vec<Vec<u8>> =
        p.b.sync_controls()
            .unwrap()
            .into_iter()
            .filter(|c| matches!(parse(c).unwrap().message.body, Body::Membership(_)))
            .collect();
    let a_id = p.a.sync_identity().unwrap().1;
    assert!(a_signed
        .iter()
        .all(|c| parse(c).unwrap().message.signer == a_id));
    let puts = mock.control_puts.lock().unwrap().clone();
    for control in &a_signed {
        assert!(puts.contains(control), "A's transition re-sent by B");
    }
    assert!(!mock.unauthenticated_puts.lock().unwrap().is_empty());
}

/// A relay whose log was reset within its generation (high-water below the
/// device's cursor) is handled as a restore: the device re-sends.
#[tokio::test(flavor = "multi_thread")]
async fn a_high_water_below_the_cursor_is_a_generation_change() {
    let p = pair();
    p.b.add_task(&mut Task::new(id(1), "from B", 0.0)).unwrap();
    let mock = Arc::new(Mock::default());
    let client = Client::new(&serve(mock.clone()).await, p.b.clone()).unwrap();
    let first = client.sync_once().await.unwrap();
    assert!(first.pushed_records > 0);
    let sent = mock.record_puts.load(std::sync::atomic::Ordering::SeqCst);
    assert!(p.b.sync_cursor(Log::Records).unwrap().is_some());
    // The relay loses its records, and keeps its generation.
    mock.records.lock().unwrap().clear();
    let again = client.sync_once().await.unwrap();
    assert_eq!(again.pushed_records, first.pushed_records, "{again:?}");
    assert_eq!(
        mock.record_puts.load(std::sync::atomic::Ordering::SeqCst),
        2 * sent
    );
    assert_eq!(mock.records.lock().unwrap().len(), first.pushed_records);
}

/// A relay asking to wait for ever is followed for ten minutes at most.
#[tokio::test(flavor = "multi_thread")]
async fn a_relay_asking_to_wait_for_ever_is_retried_within_ten_minutes() {
    let p = pair();
    let mock = Arc::new(Mock::default());
    mock.behave(|b| b.rate_limited_ms = Some(u64::MAX));
    let client = Arc::new(Client::new(&serve(mock.clone()).await, p.b.clone()).unwrap());
    let mut status = client.status();
    let (stop, mut stopped) = tokio::sync::watch::channel(false);
    let runner = client.clone();
    let run = tokio::spawn(async move {
        runner
            .run(async move {
                let _ = stopped.changed().await;
            })
            .await
    });
    let after = tokio::time::timeout(WAIT, async {
        loop {
            if let Status::Retrying { after, .. } = *status.borrow_and_update() {
                return after;
            }
            status.changed().await.unwrap();
        }
    })
    .await
    .unwrap();
    assert!(after <= MAX_RETRY_AFTER, "{after:?}");
    assert_eq!(MAX_RETRY_AFTER, Duration::from_secs(600));
    stop.send(true).unwrap();
    tokio::time::timeout(WAIT, run).await.unwrap().unwrap();
}

/// A hints endpoint that never answers the upgrade holds up neither the
/// next round nor shutdown.
#[tokio::test(flavor = "multi_thread")]
async fn a_hints_connection_that_never_opens_blocks_nothing() {
    let p = pair();
    let mock = Arc::new(Mock::default());
    mock.behave(|b| b.stall_hints = true);
    let client = Arc::new(Client::new(&serve(mock.clone()).await, p.b.clone()).unwrap());
    let mut status = client.status();
    let (stop, mut stopped) = tokio::sync::watch::channel(false);
    let runner = client.clone();
    let run = tokio::spawn(async move {
        runner
            .run(async move {
                let _ = stopped.changed().await;
            })
            .await
    });
    tokio::time::timeout(WAIT, async {
        while *status.borrow_and_update() != Status::UpToDate {
            status.changed().await.unwrap();
        }
    })
    .await
    .unwrap();
    tokio::time::timeout(WAIT, async {
        while mock.count("GET hints") == 0 {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("the client asks for hints");
    // A local edit still starts a round at once.
    let rounds = mock.count("GET records");
    client.waker().notify_one();
    tokio::time::timeout(Duration::from_secs(5), async {
        while mock.count("GET records") == rounds {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("a wake starts a round while the hints connection hangs");
    // And shutdown is prompt.
    stop.send(true).unwrap();
    tokio::time::timeout(Duration::from_secs(2), run)
        .await
        .expect("run ends at shutdown")
        .unwrap();
    assert_eq!(*client.status().borrow(), Status::Idle);
}

/// The store's own view backs the classification: a device its own
/// authority removed is told so without asking the relay again.
#[tokio::test(flavor = "multi_thread")]
async fn a_device_its_own_authority_removed_is_not_a_member() {
    let p = pair();
    let b_id = p.b.sync_identity().unwrap().1;
    p.a.remove_member(&b_id).unwrap();
    let removal = published(&p.a)
        .into_iter()
        .rev()
        .find(|c| matches!(parse(c).unwrap().message.body, Body::Membership(_)))
        .unwrap();
    assert_eq!(
        p.b.receive_control(&removal, None).unwrap(),
        matters_sync_client::ControlOutcome::Stopped
    );
    assert!(!p.b.is_current_member().unwrap());
    let mock = Arc::new(Mock::default());
    mock.behave(|b| b.challenge = Challenge::Forbidden);
    let client = Client::new(&serve(mock.clone()).await, p.b.clone()).unwrap();
    match client.sync_once().await {
        Err(SyncError::Attention(Attention::NotAMember)) => {}
        other => panic!("{other:?}"),
    }
    assert!(mock.unauthenticated_puts.lock().unwrap().is_empty());
}

/// A record naming a manifest nobody knows, signed by nobody known.
fn forgery(document: &[u8; 16]) -> Vec<u8> {
    let (sk, _) = matters_sync::generate_signing_key([3; 32]);
    let route = support::route();
    let mut record_id = [0; 32];
    record_id[..16].copy_from_slice(uuid::Uuid::new_v4().as_bytes());
    let header = matters_sync::wire::Header {
        context: route.content(document),
        kind: matters_sync::wire::Kind::Data,
        epoch: 99,
        manifest: &[0xAB; 48],
        author: &[0xCD; 48],
        incarnation: &[0x11; 32],
        record_id: &record_id,
        logical_batch: &[0x22; 32],
        previous: None,
    };
    matters_sync::seal(
        header,
        &matters_sync::ContentKey::new([1; 32]),
        b"junk",
        &sk,
        [4; 32],
    )
    .unwrap()
}

/// A relay that serves a record before the rotation it is sealed under,
/// then a flood of forgeries that pushes it out of the device's budget:
/// once the rotation arrives, the client reads the log again from the
/// record, and it applies (#45).
#[tokio::test(flavor = "multi_thread")]
async fn a_record_a_flood_pushed_out_is_read_again_after_the_rotation() {
    let p = pair();
    p.a.add_task(&mut Task::new(id(1), "before", 0.0)).unwrap();
    let before = published(&p.a);
    p.a.rotate_epoch().unwrap();
    p.a.add_task(&mut Task::new(id(2), "after the rotation", 1.0))
        .unwrap();
    let document = p.b.sync_membership().unwrap().document;
    let mock = Arc::new(Mock::default());
    *mock.authority.lock().unwrap() = before;
    {
        let mut log = mock.records.lock().unwrap();
        log.extend(p.a.outgoing_records().unwrap().into_iter().map(|r| r.bytes));
        for _ in 0..=matters_core::storage::receive::UNAUTHENTICATED_ROWS {
            log.push(forgery(&document));
        }
    }
    let client = Client::new(&serve(mock.clone()).await, p.b.clone()).unwrap();
    client.sync_once().await.unwrap();
    assert!(p.b.get_task(&id(1)).unwrap().is_some());
    assert!(p.b.get_task(&id(2)).unwrap().is_none());
    // The rotation reaches the relay's authority log.
    {
        let mut log = mock.authority.lock().unwrap();
        for control in published(&p.a) {
            if !log.contains(&control) {
                log.push(control);
            }
        }
    }
    client.sync_once().await.unwrap();
    assert_eq!(
        p.b.get_task(&id(2)).unwrap().map(|t| t.title).as_deref(),
        Some("after the rotation")
    );
    // Nothing new arrives: the next round reads on from the end.
    let pages = mock.count("GET records");
    client.sync_once().await.unwrap();
    assert_eq!(mock.count("GET records"), pages + 1);
}
