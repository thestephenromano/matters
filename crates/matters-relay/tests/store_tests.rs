//! The relay's store against a real PostgreSQL (#38): durable, idempotent
//! appends, permanent conflicts, routing checks, membership-authorized
//! uploads, gapless sequences under concurrency, cursor pages and
//! generations, and quotas.
//!
//! Set MATTERS_RELAY_TEST_DATABASE_URL to an administrative connection
//! (crates/matters-relay/compose.yaml starts one); each test creates and
//! drops its own database. Without it these tests are skipped, unless
//! MATTERS_RELAY_REQUIRE_DB is set, as in CI, where that is a failure.

use std::collections::BTreeSet;

mod common;

use common::*;
use matters_relay::{Appended, Cursor, RelayError};
use matters_sync::authority::{self, sign, DeviceKeys, Hash, Message, RecoverySecret};
use matters_sync::record::record_hash;
use matters_sync::ContentKey;

#[tokio::test]
async fn provisioning_verifies_the_genesis_against_the_root() {
    let Some(db) = TestDb::start().await else {
        return;
    };
    let c = collection();
    let other = RecoverySecret::new(fresh()).keys(&route()).unwrap();
    assert!(matches!(
        db.relay
            .provision(&route(), other.root_key(), &c.genesis)
            .await,
        Err(RelayError::Authority(authority::Error::Identity))
    ));
    db.relay
        .provision(&route(), c.keys.root_key(), &c.genesis)
        .await
        .unwrap();
    assert!(matches!(
        db.relay
            .provision(&route(), c.keys.root_key(), &c.genesis)
            .await,
        Err(RelayError::RouteExists)
    ));
    assert!(db.relay.is_member(&route(), &c.admin.id()).await.unwrap());
    db.finish().await;
}

#[tokio::test]
async fn appends_are_idempotent_conflicts_permanent_and_routing_checked() {
    let Some(db) = TestDb::start().await else {
        return;
    };
    let c = collection();
    db.relay
        .provision(&route(), c.keys.root_key(), &c.genesis)
        .await
        .unwrap();
    let id = fresh();
    let bytes = record(&c.admin, id, b"one", &route());
    let first = db
        .relay
        .append(&route(), &c.admin.id(), &id, &bytes)
        .await
        .unwrap();
    let Appended::Stored(receipt) = &first else {
        panic!("{first:?}")
    };
    assert_eq!(receipt.sequence, 1);
    assert_eq!(receipt.digest, record_hash(&bytes));
    // The same bytes again: the original receipt, no second append.
    assert_eq!(
        db.relay
            .append(&route(), &c.admin.id(), &id, &bytes)
            .await
            .unwrap(),
        Appended::Existing(receipt.clone())
    );
    // Other bytes under the same ID: a permanent conflict naming the stored digest.
    let other = record(&c.admin, id, b"two", &route());
    match db.relay.append(&route(), &c.admin.id(), &id, &other).await {
        Err(e @ RelayError::RecordConflict { digest }) => {
            assert_eq!(digest, receipt.digest);
            assert_eq!(e.code(), "record-conflict");
            assert!(!e.retry());
        }
        other => panic!("{other:?}"),
    }
    // Routing: the path's ID, the session's device, the route.
    let next = fresh();
    let next_bytes = record(&c.admin, next, b"three", &route());
    for (name, result, code) in [
        (
            "path ID",
            db.relay
                .append(&route(), &c.admin.id(), &fresh(), &next_bytes)
                .await,
            "context-mismatch",
        ),
        (
            "session device",
            db.relay
                .append(&route(), &device().id(), &next, &next_bytes)
                .await,
            "context-mismatch",
        ),
        (
            "malformed",
            db.relay
                .append(&route(), &c.admin.id(), &next, &[0xff; 40])
                .await,
            "malformed",
        ),
    ] {
        assert_eq!(result.unwrap_err().code(), code, "{name}");
    }
    let mut elsewhere = route();
    elsewhere.collection = [9; 16];
    assert_eq!(
        db.relay
            .append(&elsewhere, &c.admin.id(), &next, &next_bytes)
            .await
            .unwrap_err()
            .code(),
        "context-mismatch"
    );
    let foreign = record(&c.admin, next, b"x", &elsewhere);
    assert_eq!(
        db.relay
            .append(&elsewhere, &c.admin.id(), &next, &foreign)
            .await
            .unwrap_err()
            .code(),
        "unknown-route"
    );
    // A signed record from a device outside the roster.
    let outsider = device();
    let theirs = record(&outsider, next, b"x", &route());
    assert_eq!(
        db.relay
            .append(&route(), &outsider.id(), &next, &theirs)
            .await
            .unwrap_err()
            .code(),
        "not-a-member"
    );
    // Nothing refused was stored; a restart keeps what was.
    let page = db
        .reconnect()
        .await
        .records(&route(), None, 10)
        .await
        .unwrap();
    assert_eq!(page.high_water, 1);
    assert_eq!(page.entries, vec![(1, bytes)]);
    db.finish().await;
}

#[tokio::test]
async fn concurrent_appends_get_gapless_sequences_and_duplicates_one_record() {
    let Some(db) = TestDb::start().await else {
        return;
    };
    let c = collection();
    db.relay
        .provision(&route(), c.keys.root_key(), &c.genesis)
        .await
        .unwrap();
    let records: Vec<([u8; 32], Vec<u8>)> = (0..24)
        .map(|i| {
            let id = fresh();
            (
                id,
                record(&c.admin, id, format!("r{i}").as_bytes(), &route()),
            )
        })
        .collect();
    let mut tasks = Vec::new();
    // Every record twice, concurrently.
    for (id, bytes) in records.iter().chain(records.iter()) {
        let (relay, id, bytes, device) = (db.relay.clone(), *id, bytes.clone(), c.admin.id());
        tasks.push(tokio::spawn(async move {
            relay.append(&route(), &device, &id, &bytes).await.unwrap()
        }));
    }
    let mut stored = Vec::new();
    for t in tasks {
        if let Appended::Stored(r) = t.await.unwrap() {
            stored.push(r.sequence);
        }
    }
    stored.sort();
    assert_eq!(
        stored,
        (1..=24).collect::<Vec<_>>(),
        "one append each, gapless"
    );
    let page = db.relay.records(&route(), None, 256).await.unwrap();
    let ids: BTreeSet<_> = page.entries.iter().map(|(_, b)| record_hash(b)).collect();
    assert_eq!(ids.len(), 24);
    db.finish().await;
}

#[tokio::test]
async fn pages_resume_by_cursor_stop_at_their_high_water_and_refuse_old_generations() {
    let Some(db) = TestDb::start().await else {
        return;
    };
    let c = collection();
    db.relay
        .provision(&route(), c.keys.root_key(), &c.genesis)
        .await
        .unwrap();
    let append = |n: usize| {
        let relay = db.relay.clone();
        let admin = c.admin.id();
        let bytes: Vec<_> = (0..n)
            .map(|i| {
                let id = fresh();
                (
                    id,
                    record(&c.admin, id, format!("p{i}").as_bytes(), &route()),
                )
            })
            .collect();
        async move {
            for (id, b) in bytes {
                relay.append(&route(), &admin, &id, &b).await.unwrap();
            }
        }
    };
    append(5).await;
    let first = db.relay.records(&route(), None, 2).await.unwrap();
    assert_eq!(
        first.entries.iter().map(|e| e.0).collect::<Vec<_>>(),
        vec![1, 2]
    );
    assert!(!first.complete);
    // Records appended after a page started belong to a later page.
    append(3).await;
    let second = db
        .relay
        .records(&route(), Some(&first.next), 256)
        .await
        .unwrap();
    assert_eq!(second.high_water, 8);
    assert_eq!(second.entries.first().unwrap().0, 3);
    assert!(second.complete);
    let end = db
        .relay
        .records(&route(), Some(&second.next), 256)
        .await
        .unwrap();
    assert!(end.entries.is_empty() && end.complete);
    // The cursor round-trips through its opaque bytes, canonically.
    assert_eq!(Cursor::decode(&second.next.encode()).unwrap(), second.next);
    assert!(Cursor::decode(&[0x82, 0x40, 0x00]).is_err());
    // A page limit outside 1..=256.
    for limit in [0, 257] {
        assert_eq!(
            db.relay
                .records(&route(), None, limit)
                .await
                .unwrap_err()
                .code(),
            "malformed"
        );
    }
    // A cursor past the high-water mark under the same generation is a
    // restore that kept the generation: refused as a change, not answered
    // with an empty, complete page.
    let beyond = Cursor {
        last: second.high_water + 1,
        ..second.next
    };
    for log in [true, false] {
        let page = if log {
            db.relay.records(&route(), Some(&beyond), 10).await
        } else {
            db.relay
                .authority_objects(&route(), Some(&beyond), 10)
                .await
        };
        match page {
            Err(RelayError::GenerationChanged { generation: g }) => {
                assert_eq!(g, second.generation)
            }
            other => panic!("{other:?}"),
        }
    }
    // After a restore installs a new generation, old cursors are refused
    // and name the new generation.
    let generation = db.relay.new_generation(&route()).await.unwrap();
    match db.relay.records(&route(), Some(&first.next), 10).await {
        Err(RelayError::GenerationChanged { generation: g }) => assert_eq!(g, generation),
        other => panic!("{other:?}"),
    }
    assert_eq!(
        db.relay
            .records(&route(), None, 1)
            .await
            .unwrap()
            .generation,
        generation
    );
    db.finish().await;
}

#[tokio::test]
async fn membership_transitions_authorize_uploads_and_are_verified_first() {
    let Some(db) = TestDb::start().await else {
        return;
    };
    let mut c = collection();
    db.relay
        .provision(&route(), c.keys.root_key(), &c.genesis)
        .await
        .unwrap();
    let b = device();
    let id = fresh();
    let bs = record(&b, id, b"from b", &route());
    assert_eq!(
        db.relay
            .append(&route(), &b.id(), &id, &bs)
            .await
            .unwrap_err()
            .code(),
        "not-a-member"
    );
    // An admission makes B a member.
    let add = transition(&c, vec![c.admin.member(true), b.member(false)]);
    let digest = record_hash(&add);
    assert!(matches!(
        db.relay
            .put_authority(&route(), &digest, &add)
            .await
            .unwrap(),
        Appended::Stored(_)
    ));
    assert!(matches!(
        db.relay
            .put_authority(&route(), &digest, &add)
            .await
            .unwrap(),
        Appended::Existing(_)
    ));
    c.authority.apply_membership(&add).unwrap();
    db.relay.append(&route(), &b.id(), &id, &bs).await.unwrap();
    // The path digest must match, and a transition must verify.
    assert_eq!(
        db.relay
            .put_authority(&route(), &[0; 48], &add)
            .await
            .unwrap_err()
            .code(),
        "context-mismatch"
    );
    let mut forged = parse_message(&transition(&c, vec![c.admin.member(true)]));
    forged.signer = b.id();
    let forged = sign(&forged, b.signing_key(), fresh()).unwrap();
    assert!(matches!(
        db.relay
            .put_authority(&route(), &record_hash(&forged), &forged)
            .await,
        Err(RelayError::Authority(authority::Error::Unauthorized))
    ));
    assert!(matches!(
        db.relay
            .put_authority(&route(), &record_hash(&c.genesis), &c.genesis)
            .await,
        Ok(Appended::Existing(_))
    ));
    // A removal ends B's uploads.
    let remove = transition(&c, vec![c.admin.member(true)]);
    db.relay
        .put_authority(&route(), &record_hash(&remove), &remove)
        .await
        .unwrap();
    let next = fresh();
    assert_eq!(
        db.relay
            .append(
                &route(),
                &b.id(),
                &next,
                &record(&b, next, b"late", &route())
            )
            .await
            .unwrap_err()
            .code(),
        "not-a-member"
    );
    // Other kinds are stored for clients to verify, in acceptance order.
    let envelope = c
        .authority
        .wrap_key(&c.admin, c.admin.id(), &ContentKey::new([8; 32]), fresh())
        .unwrap();
    db.relay
        .put_authority_as(
            &route(),
            Some(&c.admin.id()),
            &record_hash(&envelope),
            &envelope,
        )
        .await
        .unwrap();
    let page = db
        .relay
        .authority_objects(&route(), None, 256)
        .await
        .unwrap();
    assert_eq!(
        page.entries
            .iter()
            .map(|(_, b)| b.clone())
            .collect::<Vec<_>>(),
        vec![c.genesis.clone(), add, remove, envelope]
    );
    db.finish().await;
}

fn parse_message(bytes: &[u8]) -> Message {
    authority::parse(bytes).unwrap().message
}

#[tokio::test]
async fn a_quota_refuses_before_storing_anything() {
    let Some(db) = TestDb::start().await else {
        return;
    };
    let c = collection();
    db.relay
        .provision(&route(), c.keys.root_key(), &c.genesis)
        .await
        .unwrap();
    let id = fresh();
    let bytes = record(&c.admin, id, b"q", &route());
    db.relay
        .set_quota(&route(), Some(bytes.len() as u64 - 1))
        .await
        .unwrap();
    let refused = db
        .relay
        .append(&route(), &c.admin.id(), &id, &bytes)
        .await
        .unwrap_err();
    assert_eq!(refused.code(), "quota-exhausted");
    assert_eq!(
        db.relay
            .records(&route(), None, 1)
            .await
            .unwrap()
            .high_water,
        0
    );
    db.relay.set_quota(&route(), None).await.unwrap();
    db.relay
        .append(&route(), &c.admin.id(), &id, &bytes)
        .await
        .unwrap();
    let mut unknown = route();
    unknown.tenant = [9; 16];
    assert_eq!(
        db.relay
            .set_quota(&unknown, Some(1))
            .await
            .unwrap_err()
            .code(),
        "unknown-route"
    );
    let _: Hash = record_hash(&bytes);
    db.finish().await;
}

/// Rolling back the relay (#44, docs/SELF_HOSTING.md "Upgrades"): a relay
/// binary older than its database's schema refuses to start, rather than
/// serve tables it does not know. Starting the current one again migrates
/// nothing twice.
#[tokio::test]
async fn an_older_relay_refuses_a_database_a_newer_one_migrated() {
    let Some(db) = TestDb::start().await else {
        return;
    };
    // Started again on the same database: nothing to migrate.
    let again = matters_relay::Relay::connect(&db.url()).await.unwrap();
    again.pool().close().await;
    // A newer relay applied a migration this one does not have.
    sqlx::query(
        "INSERT INTO _sqlx_migrations (version, description, success, checksum, execution_time)
         VALUES (9999, 'from a newer relay', true, '\\x00', 0)",
    )
    .execute(db.relay.pool())
    .await
    .unwrap();
    match matters_relay::Relay::connect(&db.url()).await {
        Err(error) => assert!(error.to_string().contains("migrat"), "{error}"),
        Ok(_) => panic!("an older relay served a newer schema"),
    }
    db.finish().await;
}

#[tokio::test]
async fn pages_hold_a_bounded_number_of_bytes_and_always_one_entry() {
    let Some(db) = TestDb::start().await else {
        return;
    };
    let c = collection();
    db.relay
        .provision(&route(), c.keys.root_key(), &c.genesis)
        .await
        .unwrap();
    // Seventeen checkpoint parts of about a megabyte each: more than one
    // page's byte budget, far fewer than its entry limit.
    let plaintext = vec![0x5a; 1_000_000];
    for _ in 0..17 {
        let id = fresh();
        let bytes = snapshot_record(&c.admin, id, &plaintext, &route());
        db.relay
            .append(&route(), &c.admin.id(), &id, &bytes)
            .await
            .unwrap();
    }
    let size = |entries: &[(u64, Vec<u8>)]| entries.iter().map(|(_, b)| b.len()).sum::<usize>();
    let budget = usize::try_from(matters_relay::MAX_PAGE_BYTES).unwrap();
    let first = db.relay.records(&route(), None, 256).await.unwrap();
    assert!(first.entries.len() < 17 && !first.entries.is_empty());
    assert!(size(&first.entries) <= budget);
    assert!(!first.complete, "the rest is on the next page");
    let rest = db
        .relay
        .records(&route(), Some(&first.next), 256)
        .await
        .unwrap();
    assert!(rest.complete);
    assert_eq!(first.entries.len() + rest.entries.len(), 17);
    let snapshots = db.relay.snapshots(&route(), 256).await.unwrap();
    assert!(snapshots.entries.len() < 17 && !snapshots.entries.is_empty());
    assert!(size(&snapshots.entries) <= budget);
    assert_eq!(snapshots.entries[0].0, 17, "newest first");
    db.finish().await;
}

/// A writer start for `device`'s own lineage under the current manifest,
/// signed with `key`.
fn writer_start(c: &Collection, device: &DeviceKeys, key: &DeviceKeys) -> Vec<u8> {
    sign(
        &Message {
            route: route(),
            signer: device.id(),
            body: authority::Body::WriterStart(authority::WriterStart {
                manifest: c.authority.manifest(),
                document: [4; 16],
                device: device.id(),
                incarnation: fresh(),
                actor: u64::from_le_bytes(fresh()[..8].try_into().unwrap()),
                previous: None,
            }),
        },
        key.signing_key(),
        fresh(),
    )
    .unwrap()
}

async fn used_bytes(db: &TestDb) -> i64 {
    sqlx::query_scalar("SELECT used_bytes FROM routes WHERE collection = $1")
        .bind(route().collection.as_slice())
        .fetch_one(db.relay.pool())
        .await
        .unwrap()
}

#[tokio::test]
async fn the_authority_log_stores_only_what_its_signers_may_publish() {
    let Some(db) = TestDb::start().await else {
        return;
    };
    let mut c = collection();
    db.relay
        .provision(&route(), c.keys.root_key(), &c.genesis)
        .await
        .unwrap();
    let (a, b, outsider) = (c.admin.id(), device(), device());
    let add = transition(&c, vec![c.admin.member(true), b.member(false)]);
    db.relay
        .put_authority(&route(), &record_hash(&add), &add)
        .await
        .unwrap();
    c.authority.apply_membership(&add).unwrap();
    let put = |session: Option<Hash>, bytes: Vec<u8>| {
        let relay = db.relay.clone();
        async move {
            relay
                .put_authority_as(&route(), session.as_ref(), &record_hash(&bytes), &bytes)
                .await
        }
    };
    let code = |r: Result<Appended, RelayError>| r.map(|_| "stored").unwrap_or_else(|e| e.code());
    // Writer claims: the session's own, signed with its roster key.
    let own = writer_start(&c, &c.admin, &c.admin);
    assert_eq!(code(put(Some(a), own.clone()).await), "stored");
    assert_eq!(code(put(Some(b.id()), own).await), "stored", "a replay");
    let a_claim = writer_start(&c, &c.admin, &c.admin);
    assert_eq!(
        code(put(Some(b.id()), a_claim.clone()).await),
        "context-mismatch"
    );
    assert_eq!(code(put(None, a_claim).await), "unauthenticated");
    let forged = writer_start(&c, &c.admin, &b);
    assert!(matches!(
        put(Some(a), forged).await,
        Err(RelayError::Authority(authority::Error::Signature))
    ));
    let theirs = writer_start(&c, &outsider, &outsider);
    assert_eq!(code(put(Some(outsider.id()), theirs).await), "not-a-member");
    let root = c.keys.root_id().unwrap();
    let as_root = writer_start(&c, &c.admin, &c.admin);
    assert_eq!(code(put(Some(root), as_root).await), "not-a-member");
    // Envelopes: signed by the admin of the manifest they name.
    let envelope = c
        .authority
        .wrap_key(&c.admin, b.id(), &ContentKey::new([8; 32]), fresh())
        .unwrap();
    assert_eq!(code(put(Some(b.id()), envelope.clone()).await), "stored");
    let mut forged = parse_message(&envelope);
    let parsed = authority::parse(&envelope).unwrap();
    let signed_by_b = sign(&forged, b.signing_key(), fresh()).unwrap();
    assert!(matches!(
        put(Some(a), signed_by_b).await,
        Err(RelayError::Authority(authority::Error::Signature))
    ));
    forged.signer = b.id();
    if let authority::Body::KeyEnvelope(k) = &mut forged.body {
        k.authority = b.id();
    }
    let by_a_member = sign(&forged, b.signing_key(), fresh()).unwrap();
    assert!(matches!(
        put(Some(b.id()), by_a_member).await,
        Err(RelayError::Authority(authority::Error::Unauthorized))
    ));
    let mut elsewhere = parsed.message.clone();
    if let authority::Body::KeyEnvelope(k) = &mut elsewhere.body {
        k.manifest = [7; 48];
    }
    let unknown_manifest = sign(&elsewhere, c.admin.signing_key(), fresh()).unwrap();
    assert!(matches!(
        put(Some(a), unknown_manifest).await,
        Err(RelayError::Authority(authority::Error::Stale))
    ));
    // Enrollment requests and new genesis objects do not belong in the log;
    // the stored genesis again is a replay.
    let invitation = c
        .authority
        .invite(authority::InvitationSecret::new([1; 16]))
        .unwrap();
    let request = authority::request_enrollment(&invitation, &outsider, fresh()).unwrap();
    assert!(matches!(
        put(Some(a), request).await,
        Err(RelayError::Authority(authority::Error::Unsupported))
    ));
    assert!(matches!(
        put(None, c.genesis.clone()).await,
        Ok(Appended::Existing(_))
    ));
    // Every stored byte counts toward the quota, refused before storing; a
    // transition is still stored, so a full route can remove a device.
    let used = used_bytes(&db).await;
    let stored: i64 = sqlx::query_scalar("SELECT SUM(octet_length(bytes))::BIGINT FROM authority")
        .fetch_one(db.relay.pool())
        .await
        .unwrap();
    assert_eq!(used, stored);
    db.relay
        .set_quota(&route(), Some(used as u64))
        .await
        .unwrap();
    let full = writer_start(&c, &b, &b);
    assert_eq!(code(put(Some(b.id()), full).await), "quota-exhausted");
    assert_eq!(used_bytes(&db).await, used, "nothing stored");
    let remove = transition(&c, vec![c.admin.member(true)]);
    assert_eq!(code(put(None, remove.clone()).await), "stored");
    c.authority.apply_membership(&remove).unwrap();
    db.finish().await;
}

#[tokio::test]
async fn forks_are_kept_as_evidence_once_and_never_unverified() {
    let Some(db) = TestDb::start().await else {
        return;
    };
    let c = collection();
    db.relay
        .provision(&route(), c.keys.root_key(), &c.genesis)
        .await
        .unwrap();
    let put = |bytes: Vec<u8>| {
        let relay = db.relay.clone();
        async move {
            relay
                .put_authority(&route(), &record_hash(&bytes), &bytes)
                .await
        }
    };
    let log = || {
        let relay = db.relay.clone();
        async move {
            relay
                .authority_objects(&route(), None, 256)
                .await
                .unwrap()
                .high_water
        }
    };
    // Two valid successors of the genesis: the second is fork evidence.
    let first = transition(&c, vec![c.admin.member(true), device().member(false)]);
    let second = transition(&c, vec![c.admin.member(true), device().member(false)]);
    put(first).await.unwrap();
    put(second).await.unwrap();
    assert!(db.relay.authority(&route()).await.unwrap().is_forked());
    // Frozen, the authority no longer checks a transition's signature, so
    // the relay takes no more of them, valid or forged.
    let stored = log().await;
    let third = transition(&c, vec![c.admin.member(true), device().member(false)]);
    let mut forged = authority::parse(&third).unwrap().message;
    forged.signer = [9; 48];
    let forged = sign(&forged, device().signing_key(), fresh()).unwrap();
    for bytes in [third, forged] {
        assert!(matches!(
            put(bytes).await,
            Err(RelayError::Authority(authority::Error::Fork))
        ));
    }
    assert_eq!(log().await, stored);
    // The root recovers. A competing recovery of the same generation is
    // kept as evidence, and no further one.
    let recovery = |d: &DeviceKeys| {
        c.authority
            .recover(&c.keys, d, &ContentKey::new([6; 32]), fresh())
            .unwrap()
    };
    let new_admin = device();
    let recovered = recovery(&new_admin);
    assert!(matches!(
        put(recovered.clone()).await,
        Ok(Appended::Stored(_))
    ));
    assert!(matches!(
        put(recovery(&device())).await,
        Ok(Appended::Stored(_))
    ));
    let stored = log().await;
    assert_eq!(
        put(recovery(&device())).await.unwrap_err().code(),
        "quota-exhausted"
    );
    assert_eq!(log().await, stored);
    // The root's session publishes the new admin's envelopes for the
    // generation the recovery started.
    let mut fresh_state = c.authority.clone();
    fresh_state.apply_recovery(&recovered).unwrap();
    let envelope = fresh_state
        .wrap_key(
            &new_admin,
            new_admin.id(),
            &ContentKey::new([6; 32]),
            fresh(),
        )
        .unwrap();
    let root = c.keys.root_id().unwrap();
    assert!(matches!(
        db.relay
            .put_authority_as(&route(), Some(&root), &record_hash(&envelope), &envelope)
            .await,
        Ok(Appended::Stored(_))
    ));
    db.finish().await;
}

/// Transitions are exempt from the byte quota, so a full route can still
/// remove a device, and bounded instead by a per-route count (#45): the
/// authority has no lifetime limit on them.
#[tokio::test]
async fn transitions_pass_a_full_quota_but_a_route_stores_a_bounded_number() {
    let Some(db) = TestDb::start().await else {
        return;
    };
    let mut c = collection();
    db.relay
        .provision(&route(), c.keys.root_key(), &c.genesis)
        .await
        .unwrap();
    // A full byte quota refuses records, but still takes a transition.
    db.relay.set_quota(&route(), Some(1)).await.unwrap();
    let id = fresh();
    let bytes = record(&c.admin, id, b"q", &route());
    assert_eq!(
        db.relay
            .append(&route(), &c.admin.id(), &id, &bytes)
            .await
            .unwrap_err()
            .code(),
        "quota-exhausted"
    );
    let first = transition(&c, vec![c.admin.member(true)]);
    db.relay
        .put_authority(&route(), &record_hash(&first), &first)
        .await
        .unwrap();
    c.authority.apply_membership(&first).unwrap();
    // Stand-in rows fill the route to its transition cap, past the
    // replayed high-water mark so the replay never reads them.
    sqlx::query(
        "INSERT INTO authority (route, sequence, digest, kind, bytes)
         SELECT r.id, 1000000 + g, sha384(int8send(g)), 'membership', '\\x00'
         FROM routes r, generate_series(1, $1) g",
    )
    .bind(matters_relay::MAX_STORED_TRANSITIONS - 1)
    .execute(db.relay.pool())
    .await
    .unwrap();
    let next = transition(&c, vec![c.admin.member(true)]);
    assert_eq!(
        db.relay
            .put_authority(&route(), &record_hash(&next), &next)
            .await
            .unwrap_err()
            .code(),
        "quota-exhausted"
    );
    // An exact retry of a stored transition is still answered.
    assert!(matches!(
        db.relay
            .put_authority(&route(), &record_hash(&first), &first)
            .await
            .unwrap(),
        Appended::Existing(_)
    ));
    db.finish().await;
}

/// Writer claims and key envelopes have no count cap: every app open adds a
/// claim, so a count would be a lifetime cap (#45). The byte quota bounds
/// them, as it bounds records.
#[tokio::test]
async fn writer_claims_past_4096_are_stored_and_only_the_byte_quota_refuses_them() {
    let Some(db) = TestDb::start().await else {
        return;
    };
    let c = collection();
    db.relay
        .provision(&route(), c.keys.root_key(), &c.genesis)
        .await
        .unwrap();
    let put = |bytes: Vec<u8>| {
        let relay = db.relay.clone();
        let device = c.admin.id();
        async move {
            relay
                .put_authority_as(&route(), Some(&device), &record_hash(&bytes), &bytes)
                .await
        }
    };
    // 4,096 claims already stored, as years of app opens leave them. Stand-in
    // rows past the replayed high-water mark: the replay never reads claims.
    sqlx::query(
        "INSERT INTO authority (route, sequence, digest, kind, bytes)
         SELECT r.id, 1000000 + g, sha384(int8send(g)), 'writer_start', '\\x00'
         FROM routes r, generate_series(1, 4096) g",
    )
    .execute(db.relay.pool())
    .await
    .unwrap();
    let claim = writer_start(&c, &c.admin, &c.admin);
    assert!(matches!(put(claim).await.unwrap(), Appended::Stored(_)));
    // A full byte quota refuses the next one with 507, storing nothing.
    let used = used_bytes(&db).await;
    db.relay
        .set_quota(&route(), Some(used as u64))
        .await
        .unwrap();
    let refused = put(writer_start(&c, &c.admin, &c.admin)).await.unwrap_err();
    assert_eq!(refused.code(), "quota-exhausted");
    assert_eq!(used_bytes(&db).await, used, "nothing stored");
    db.finish().await;
}
