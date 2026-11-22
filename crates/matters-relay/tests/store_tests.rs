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
use matters_sync::authority::{self, sign, Hash, Message, RecoverySecret};
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
        .put_authority(&route(), &record_hash(&envelope), &envelope)
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
