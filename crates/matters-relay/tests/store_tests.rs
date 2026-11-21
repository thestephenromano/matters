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

use matters_relay::{Appended, Cursor, Relay, RelayError};
use matters_sync::authority::{
    self, genesis, key_commitment, sign, Authority, Body, DeviceKeys, Hash, Membership, Message,
    RecoverySecret, Route,
};
use matters_sync::record::record_hash;
use matters_sync::{seal, wire, ContentKey};
use sqlx::postgres::PgPool;

struct TestDb {
    admin: PgPool,
    name: String,
    relay: Relay,
}

impl TestDb {
    async fn start() -> Option<Self> {
        let Ok(url) = std::env::var("MATTERS_RELAY_TEST_DATABASE_URL") else {
            assert!(
                std::env::var_os("MATTERS_RELAY_REQUIRE_DB").is_none(),
                "MATTERS_RELAY_REQUIRE_DB is set but MATTERS_RELAY_TEST_DATABASE_URL is not"
            );
            eprintln!("skipped: no MATTERS_RELAY_TEST_DATABASE_URL");
            return None;
        };
        let admin = PgPool::connect(&url).await.expect("the test database");
        let name = format!("relay_test_{}", uuid::Uuid::new_v4().simple());
        sqlx::query(sqlx::AssertSqlSafe(format!("CREATE DATABASE {name}")))
            .execute(&admin)
            .await
            .unwrap();
        let base = url.rsplit_once('/').map_or(url.as_str(), |(b, _)| b);
        let relay = Relay::connect(&format!("{base}/{name}")).await.unwrap();
        Some(Self { admin, name, relay })
    }

    /// Another connection to the same database, as after a restart.
    async fn reconnect(&self) -> Relay {
        let url = std::env::var("MATTERS_RELAY_TEST_DATABASE_URL").unwrap();
        let base = url
            .rsplit_once('/')
            .map_or(url.as_str(), |(b, _)| b)
            .to_owned();
        Relay::connect(&format!("{base}/{}", self.name))
            .await
            .unwrap()
    }

    async fn finish(self) {
        self.relay.pool().close().await;
        sqlx::query(sqlx::AssertSqlSafe(format!(
            "DROP DATABASE {} WITH (FORCE)",
            self.name
        )))
        .execute(&self.admin)
        .await
        .unwrap();
    }
}

fn route() -> Route {
    Route {
        application: [1; 16],
        tenant: [2; 16],
        collection: [3; 16],
    }
}

fn fresh() -> [u8; 32] {
    let mut b = [0; 32];
    b[..16].copy_from_slice(uuid::Uuid::new_v4().as_bytes());
    b[16..].copy_from_slice(uuid::Uuid::new_v4().as_bytes());
    b
}

fn device() -> DeviceKeys {
    DeviceKeys::generate(fresh(), fresh()).unwrap()
}

/// A route whose genesis makes `admin` its sole member.
struct Collection {
    keys: authority::RecoveryKeys,
    admin: DeviceKeys,
    genesis: Vec<u8>,
    authority: Authority,
}

fn collection() -> Collection {
    let keys = RecoverySecret::new(fresh()).keys(&route()).unwrap();
    let admin = device();
    let genesis = genesis(&route(), &keys, &admin, &ContentKey::new([4; 32]), fresh()).unwrap();
    let authority = Authority::bootstrap(&genesis, route(), keys.root_key()).unwrap();
    Collection {
        keys,
        admin,
        genesis,
        authority,
    }
}

/// A record sealed by `author` under `record_id`, with `plaintext`.
fn record(author: &DeviceKeys, record_id: [u8; 32], plaintext: &[u8], route: &Route) -> Vec<u8> {
    let header = wire::Header {
        context: route.content(&[4; 16]),
        kind: wire::Kind::Data,
        epoch: 0,
        manifest: &[5; 48],
        author: &author.id(),
        incarnation: &[7; 32],
        record_id: &record_id,
        logical_batch: &[9; 32],
        previous: None,
    };
    seal(
        header,
        &ContentKey::new([4; 32]),
        plaintext,
        author.signing_key(),
        fresh(),
    )
    .unwrap()
}

/// The admin's next transition with `roster`.
fn transition(c: &Collection, roster: Vec<authority::Member>) -> Vec<u8> {
    let a = &c.authority;
    let epoch = a.epoch().unwrap() + 1;
    let mut roster = roster;
    roster.sort_by_key(|m| m.id);
    sign(
        &Message {
            route: route(),
            signer: c.admin.id(),
            body: Body::Membership(Membership {
                generation: 0,
                sequence: a.sequence().unwrap() + 1,
                previous: a.manifest(),
                roster,
                admin: c.admin.id(),
                epoch,
                recovery_key: a.recovery().unwrap(),
                key_commitment: key_commitment(&route(), 0, epoch, &ContentKey::new([8; 32]))
                    .unwrap(),
            }),
        },
        c.admin.signing_key(),
        fresh(),
    )
    .unwrap()
}

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
