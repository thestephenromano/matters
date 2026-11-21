//! The sync client between two Matters databases and the real relay on a
//! real PostgreSQL (#39): convergence, idempotent re-sends after lost
//! acknowledgements, a relay restore, removal, and hint-driven syncing; and,
//! without PostgreSQL, that a stalled relay never holds up local edits.
//!
//! Set MATTERS_RELAY_TEST_DATABASE_URL (crates/matters-relay/compose.yaml);
//! without it these tests are skipped, unless MATTERS_RELAY_REQUIRE_DB is
//! set, as in CI.

use std::sync::Arc;
use std::time::{Duration, Instant};

use matters_core::storage::membership::{MemoryKeyStore, SyncStanding};
use matters_core::{Database, Task};
use matters_relay::api::{router_with, system_clock, Options};
use matters_relay::hints::Hints;
use matters_relay::Relay;
use matters_sync::authority::{parse, Body, Route};
use matters_sync_client::{Attention, Client, Status, SyncError};
use sqlx::postgres::PgPool;

fn id(n: u128) -> String {
    uuid::Uuid::from_u128(n).to_string()
}

fn route() -> Route {
    Route {
        application: [1; 16],
        tenant: [2; 16],
        collection: [3; 16],
    }
}

struct Relayed {
    admin: PgPool,
    name: String,
    relay: Relay,
    endpoint: String,
}

/// A relay with hints on a fresh database, serving on a local port.
async fn relay() -> Option<Relayed> {
    let Ok(url) = std::env::var("MATTERS_RELAY_TEST_DATABASE_URL") else {
        assert!(std::env::var_os("MATTERS_RELAY_REQUIRE_DB").is_none());
        eprintln!("skipped: no MATTERS_RELAY_TEST_DATABASE_URL");
        return None;
    };
    let admin = PgPool::connect(&url).await.unwrap();
    let name = format!("client_test_{}", uuid::Uuid::new_v4().simple());
    sqlx::query(sqlx::AssertSqlSafe(format!("CREATE DATABASE {name}")))
        .execute(&admin)
        .await
        .unwrap();
    let base = url.rsplit_once('/').unwrap().0;
    let relay = Relay::connect(&format!("{base}/{name}")).await.unwrap();
    let hints = Hints::start(&relay).await.unwrap();
    let app = router_with(
        relay.clone(),
        system_clock(),
        Options {
            hints: Some(hints),
            ..Options::default()
        },
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    Some(Relayed {
        admin,
        name,
        relay,
        endpoint,
    })
}

impl Relayed {
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

/// A with two tasks, B joined out of band (as an invitation is), and the
/// route provisioned on the relay with A's genesis.
struct Devices {
    _dir: tempfile::TempDir,
    a: Arc<Database>,
    b: Arc<Database>,
}

async fn devices(relayed: &Relayed) -> Devices {
    let dir = tempfile::tempdir().unwrap();
    let a = Database::open(dir.path().join("a.db")).unwrap();
    a.create_collection(&MemoryKeyStore::default(), route())
        .unwrap();
    a.add_task(&mut Task::new(id(1), "from A, one", 0.0))
        .unwrap();
    a.add_task(&mut Task::new(id(2), "from A, two", 1.0))
        .unwrap();
    let b = Database::open(dir.path().join("b.db")).unwrap();
    let b_keys = MemoryKeyStore::default();
    let shown = a.invite().unwrap();
    let request = b.start_enrollment(&b_keys, &shown).unwrap();
    let b_id = parse(&request).unwrap().message.signer;
    a.admit(&request, &shown).unwrap();
    let envelopes: Vec<Vec<u8>> = a
        .outgoing_controls()
        .unwrap()
        .into_iter()
        .filter(|o| {
            matches!(parse(&o.bytes).unwrap().message.body,
                Body::KeyEnvelope(ref k) if k.recipient == b_id)
        })
        .map(|o| o.bytes)
        .collect();
    assert_eq!(
        b.finish_enrollment(&b_keys, &a.sync_controls().unwrap(), &envelopes)
            .unwrap(),
        SyncStanding::Active
    );
    let genesis = a.sync_controls().unwrap().remove(0);
    let Body::Genesis(g) = parse(&genesis).unwrap().message.body else {
        unreachable!()
    };
    relayed
        .relay
        .provision(&route(), &g.root_key, &genesis)
        .await
        .unwrap();
    Devices {
        _dir: dir,
        a: Arc::new(a),
        b: Arc::new(b),
    }
}

fn titles(db: &Database) -> Vec<String> {
    let mut t: Vec<String> = (1..=9)
        .filter_map(|n| db.get_task(&id(n)).unwrap())
        .map(|t| t.title)
        .collect();
    t.sort();
    t
}

#[tokio::test(flavor = "multi_thread")]
async fn two_devices_converge_through_the_relay() {
    let Some(relayed) = relay().await else { return };
    let d = devices(&relayed).await;
    let a = Client::new(&relayed.endpoint, d.a.clone()).unwrap();
    let b = Client::new(&relayed.endpoint, d.b.clone()).unwrap();
    let pushed = a.sync_once().await.unwrap();
    assert_eq!(pushed.pushed_records, 2);
    assert!(pushed.pushed_controls >= 3, "{pushed:?}");
    let pulled = b.sync_once().await.unwrap();
    assert_eq!(pulled.received_records, 2);
    assert_eq!(titles(&d.b), ["from A, one", "from A, two"]);
    // B writes; it reaches A.
    d.b.add_task(&mut Task::new(id(3), "from B", 2.0)).unwrap();
    b.sync_once().await.unwrap();
    a.sync_once().await.unwrap();
    assert_eq!(titles(&d.a), titles(&d.b));
    assert_eq!(titles(&d.a).len(), 3);
    // Nothing left to send, and nothing new to receive.
    let idle = a.sync_once().await.unwrap();
    assert_eq!((idle.pushed_records, idle.pushed_controls), (0, 0));
    assert!(d.a.pending_records().unwrap().is_empty());
    relayed.finish().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn lost_acknowledgements_and_a_relay_restore_duplicate_nothing() {
    let Some(relayed) = relay().await else { return };
    let d = devices(&relayed).await;
    let a = Client::new(&relayed.endpoint, d.a.clone()).unwrap();
    let b = Client::new(&relayed.endpoint, d.b.clone()).unwrap();
    a.sync_once().await.unwrap();
    b.sync_once().await.unwrap();
    // A's acknowledgements were lost: it sends everything again, the relay
    // answers with the original receipts, and nothing is stored twice.
    d.a.sync_generation_changed().unwrap();
    let again = a.sync_once().await.unwrap();
    assert_eq!(again.pushed_records, 2);
    let page = relayed.relay.records(&route(), None, 256).await.unwrap();
    assert_eq!(page.high_water, 2, "one record each");
    // The relay is restored to a new generation: both clients start over
    // from sequence 1, re-upload, and stay converged.
    relayed.relay.new_generation(&route()).await.unwrap();
    d.b.add_task(&mut Task::new(id(4), "after the restore", 3.0))
        .unwrap();
    b.sync_once().await.unwrap();
    a.sync_once().await.unwrap();
    assert_eq!(titles(&d.a), titles(&d.b));
    assert!(titles(&d.a).contains(&"after the restore".to_owned()));
    relayed.finish().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_removed_device_is_told_and_keeps_working_locally() {
    let Some(relayed) = relay().await else { return };
    let d = devices(&relayed).await;
    let a = Client::new(&relayed.endpoint, d.a.clone()).unwrap();
    let b = Client::new(&relayed.endpoint, d.b.clone()).unwrap();
    a.sync_once().await.unwrap();
    b.sync_once().await.unwrap();
    let b_id = d.b.sync_identity().unwrap().1;
    d.a.remove_member(&b_id).unwrap();
    a.sync_once().await.unwrap();
    match b.sync_once().await {
        Err(SyncError::Attention(Attention::NotAMember)) => {}
        other => panic!("{other:?}"),
    }
    // The relay gives a removed device no session, so it cannot fetch the
    // signed removal: it is told to stop (the contract's 403), and keeps
    // its work locally for recovery.
    d.b.add_task(&mut Task::new(id(5), "still local", 4.0))
        .unwrap();
    assert!(titles(&d.b).contains(&"still local".to_owned()));
    relayed.finish().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn running_clients_follow_each_other_through_hints() {
    let Some(relayed) = relay().await else { return };
    let d = devices(&relayed).await;
    let a = Arc::new(Client::new(&relayed.endpoint, d.a.clone()).unwrap());
    let b = Arc::new(Client::new(&relayed.endpoint, d.b.clone()).unwrap());
    let (stop, stopped) = tokio::sync::watch::channel(false);
    for client in [a.clone(), b.clone()] {
        let mut stopped = stopped.clone();
        tokio::spawn(async move {
            client
                .run(async move {
                    let _ = stopped.changed().await;
                })
                .await
        });
    }
    let mut status = a.status();
    tokio::time::timeout(Duration::from_secs(20), async {
        while *status.borrow() != Status::UpToDate {
            status.changed().await.unwrap();
        }
    })
    .await
    .expect("A comes up to date");
    // B edits and signals; A hears the hint and receives it.
    tokio::time::sleep(Duration::from_millis(500)).await;
    d.b.add_task(&mut Task::new(id(6), "hinted", 5.0)).unwrap();
    b.waker().notify_one();
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            if titles(&d.a).contains(&"hinted".to_owned()) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .expect("A receives B's edit through the hint, well before the minute");
    let _ = stop.send(true);
    relayed.finish().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_stalled_relay_never_blocks_local_edits() {
    let dir = tempfile::tempdir().unwrap();
    let db = Database::open(dir.path().join("a.db")).unwrap();
    db.create_collection(&MemoryKeyStore::default(), route())
        .unwrap();
    let db = Arc::new(db);
    // A relay that accepts connections and never answers.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let held = tokio::spawn(async move {
        let mut open = Vec::new();
        loop {
            open.push(listener.accept().await.unwrap());
        }
    });
    let client = Client::new(&endpoint, db.clone()).unwrap();
    let round = tokio::spawn(async move { client.sync_once().await });
    tokio::time::sleep(Duration::from_millis(300)).await;
    // While the round waits on the network, the device keeps working.
    let started = Instant::now();
    let writer = db.clone();
    tokio::task::spawn_blocking(move || {
        for n in 1..=20 {
            writer
                .add_task(&mut Task::new(id(n), format!("offline {n}"), n as f64))
                .unwrap();
        }
    })
    .await
    .unwrap();
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "local edits waited {:?} on a stalled round",
        started.elapsed()
    );
    assert!(
        !round.is_finished(),
        "the round is still waiting on the relay"
    );
    assert_eq!(db.outgoing_records().unwrap().len(), 20, "each edit queued");
    round.abort();
    held.abort();
}

#[tokio::test(flavor = "multi_thread")]
async fn a_device_joins_through_the_relay_mailbox() {
    let Some(relayed) = relay().await else { return };
    let dir = tempfile::tempdir().unwrap();
    let a = Arc::new(Database::open(dir.path().join("a.db")).unwrap());
    a.create_collection(&MemoryKeyStore::default(), route())
        .unwrap();
    a.add_task(&mut Task::new(id(1), "before B joined", 0.0))
        .unwrap();
    let genesis = a.sync_controls().unwrap().remove(0);
    let Body::Genesis(g) = parse(&genesis).unwrap().message.body else {
        unreachable!()
    };
    relayed
        .relay
        .provision(&route(), &g.root_key, &genesis)
        .await
        .unwrap();
    let a_client = Client::new(&relayed.endpoint, a.clone()).unwrap();
    a_client.sync_once().await.unwrap();
    // A shows an invitation; B, holding it, signs a request and leaves it.
    let invitation = a.invite().unwrap();
    let lookup = invitation.secret.lookup(&invitation.route).unwrap();
    let b = Arc::new(Database::open(dir.path().join("b.db")).unwrap());
    let b_keys = Arc::new(MemoryKeyStore::default());
    let request = b.start_enrollment(b_keys.as_ref(), &invitation).unwrap();
    let joining = Arc::new(matters_core::storage::Enrolling {
        db: b.clone(),
        keys: b_keys.clone(),
    });
    let joiner = matters_sync_client::Joiner::new(&relayed.endpoint, joining).unwrap();
    assert!(!joiner.try_finish().await.unwrap(), "not admitted yet");
    joiner.send_request(&lookup, &request).await.unwrap();
    joiner.send_request(&lookup, &request).await.unwrap();
    // A collects it, admits B and publishes.
    let collected = a_client.fetch_enrollment(&lookup).await.unwrap().unwrap();
    assert_eq!(collected, request);
    a.admit(&collected, &invitation).unwrap();
    a_client.sync_once().await.unwrap();
    // B is a member now: it finishes, and syncs as any device.
    assert!(joiner.try_finish().await.unwrap());
    let b_client = Client::new(&relayed.endpoint, b.clone()).unwrap();
    b_client.sync_once().await.unwrap();
    assert_eq!(titles(&b), ["before B joined"]);
    b.add_task(&mut Task::new(id(2), "from B", 1.0)).unwrap();
    b_client.sync_once().await.unwrap();
    a_client.sync_once().await.unwrap();
    assert_eq!(titles(&a), titles(&b));
    // Someone else's lookup holds nothing for A.
    assert_eq!(a_client.fetch_enrollment(&[0; 16]).await.unwrap(), None);
    relayed.finish().await;
}
