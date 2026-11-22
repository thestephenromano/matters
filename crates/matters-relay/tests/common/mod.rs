//! Shared fixtures for the relay's tests: a fresh PostgreSQL database per
//! test, a collection and its records. See store_tests.rs for the
//! environment variables.
#![allow(dead_code)]

use matters_relay::Relay;
use matters_sync::authority::{
    self, genesis, key_commitment, sign, Authority, Body, DeviceKeys, Membership, Message,
    RecoverySecret, Route,
};
use matters_sync::{seal, wire, ContentKey};
use sqlx::postgres::PgPool;

pub struct TestDb {
    pub admin: PgPool,
    pub name: String,
    pub relay: Relay,
}

impl TestDb {
    pub async fn start() -> Option<Self> {
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

    /// The connection URL of this test's database.
    pub fn url(&self) -> String {
        Self::url_of(&self.name)
    }

    /// The connection URL of database `name` on the test server.
    pub fn url_of(name: &str) -> String {
        let url = std::env::var("MATTERS_RELAY_TEST_DATABASE_URL").unwrap();
        let base = url
            .rsplit_once('/')
            .map_or(url.as_str(), |(b, _)| b)
            .to_owned();
        format!("{base}/{name}")
    }

    /// Another connection to the same database, as after a restart.
    pub async fn reconnect(&self) -> Relay {
        let url = std::env::var("MATTERS_RELAY_TEST_DATABASE_URL").unwrap();
        let base = url
            .rsplit_once('/')
            .map_or(url.as_str(), |(b, _)| b)
            .to_owned();
        Relay::connect(&format!("{base}/{}", self.name))
            .await
            .unwrap()
    }

    pub async fn finish(self) {
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

pub fn route() -> Route {
    Route {
        application: [1; 16],
        tenant: [2; 16],
        collection: [3; 16],
    }
}

pub fn fresh() -> [u8; 32] {
    let mut b = [0; 32];
    b[..16].copy_from_slice(uuid::Uuid::new_v4().as_bytes());
    b[16..].copy_from_slice(uuid::Uuid::new_v4().as_bytes());
    b
}

pub fn device() -> DeviceKeys {
    DeviceKeys::generate(fresh(), fresh()).unwrap()
}

/// A route whose genesis makes `admin` its sole member.
pub struct Collection {
    pub keys: authority::RecoveryKeys,
    pub admin: DeviceKeys,
    pub genesis: Vec<u8>,
    pub authority: Authority,
}

pub fn collection() -> Collection {
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

/// A `snapshot` record (a checkpoint part) sealed by `author`.
pub fn snapshot_record(
    author: &DeviceKeys,
    record_id: [u8; 32],
    plaintext: &[u8],
    route: &Route,
) -> Vec<u8> {
    let header = wire::Header {
        context: route.content(&[4; 16]),
        kind: wire::Kind::Snapshot,
        epoch: 0,
        manifest: &[5; 48],
        author: &author.id(),
        incarnation: &[7; 32],
        record_id: &record_id,
        logical_batch: &[8; 32],
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

/// A record sealed by `author` under `record_id`, with `plaintext`.
pub fn record(
    author: &DeviceKeys,
    record_id: [u8; 32],
    plaintext: &[u8],
    route: &Route,
) -> Vec<u8> {
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
pub fn transition(c: &Collection, roster: Vec<authority::Member>) -> Vec<u8> {
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
                frontier: vec![],
                key_commitment: key_commitment(&route(), 0, epoch, &ContentKey::new([8; 32]))
                    .unwrap(),
            }),
        },
        c.admin.signing_key(),
        fresh(),
    )
    .unwrap()
}
