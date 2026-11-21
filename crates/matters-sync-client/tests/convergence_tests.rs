//! Convergence under recorded schedules (#39): three Matters devices edit
//! offline and sync through the real relay in fixed, seeded interleavings.
//! The schedules include concurrent edits of one task, lost
//! acknowledgements and a relay restore. After a final round of syncing,
//! every device must hold the same tasks, field for field, and nothing
//! may be left pending.
//!
//! Each seed is a recorded schedule: a failure names its seed and step, and
//! rerunning that seed replays it exactly. See client_tests.rs for the
//! environment.

use std::sync::Arc;

use matters_core::storage::membership::{MemoryKeyStore, SyncStanding};
use matters_core::{Database, Task};
use matters_relay::api::{router, system_clock};
use matters_relay::Relay;
use matters_sync::authority::{parse, Body, Route};
use matters_sync_client::Client;
use sqlx::postgres::PgPool;

fn route() -> Route {
    Route {
        application: [1; 16],
        tenant: [2; 16],
        collection: [3; 16],
    }
}

fn id(n: u128) -> String {
    uuid::Uuid::from_u128(n).to_string()
}

/// xorshift64*: a schedule is a seed.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }

    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

struct World {
    _dir: tempfile::TempDir,
    admin: PgPool,
    name: String,
    relay: Relay,
    devices: Vec<Arc<Database>>,
    clients: Vec<Client<Database>>,
}

async fn world() -> Option<World> {
    let Ok(url) = std::env::var("MATTERS_RELAY_TEST_DATABASE_URL") else {
        assert!(std::env::var_os("MATTERS_RELAY_REQUIRE_DB").is_none());
        eprintln!("skipped: no MATTERS_RELAY_TEST_DATABASE_URL");
        return None;
    };
    let admin = PgPool::connect(&url).await.unwrap();
    let name = format!("converge_test_{}", uuid::Uuid::new_v4().simple());
    sqlx::query(sqlx::AssertSqlSafe(format!("CREATE DATABASE {name}")))
        .execute(&admin)
        .await
        .unwrap();
    let relay = Relay::connect(&format!("{}/{name}", url.rsplit_once('/').unwrap().0))
        .await
        .unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let app = router(relay.clone(), system_clock());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    // A, then B and C joined out of band.
    let dir = tempfile::tempdir().unwrap();
    let a = Database::open(dir.path().join("a.db")).unwrap();
    a.create_collection(&MemoryKeyStore::default(), route())
        .unwrap();
    let mut devices = vec![Arc::new(a)];
    for name in ["b", "c"] {
        let a = &devices[0];
        let joiner = Database::open(dir.path().join(format!("{name}.db"))).unwrap();
        let keys = MemoryKeyStore::default();
        let shown = a.invite().unwrap();
        let request = joiner.start_enrollment(&keys, &shown).unwrap();
        let joiner_id = parse(&request).unwrap().message.signer;
        a.admit(&request, &shown).unwrap();
        let envelopes: Vec<Vec<u8>> = a
            .outgoing_controls()
            .unwrap()
            .into_iter()
            .filter(|o| {
                matches!(parse(&o.bytes).unwrap().message.body,
                    Body::KeyEnvelope(ref k) if k.recipient == joiner_id)
            })
            .map(|o| o.bytes)
            .collect();
        assert_eq!(
            joiner
                .finish_enrollment(&keys, &a.sync_controls().unwrap(), &envelopes)
                .unwrap(),
            SyncStanding::Active
        );
        devices.push(Arc::new(joiner));
    }
    let genesis = devices[0].sync_controls().unwrap().remove(0);
    let Body::Genesis(g) = parse(&genesis).unwrap().message.body else {
        unreachable!()
    };
    relay
        .provision(&route(), &g.root_key, &genesis)
        .await
        .unwrap();
    let clients = devices
        .iter()
        .map(|d| Client::new(&endpoint, d.clone()).unwrap())
        .collect();
    Some(World {
        _dir: dir,
        admin,
        name,
        relay,
        devices,
        clients,
    })
}

impl World {
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

/// What a device shows of the schedule's tasks: every field an edit or a
/// sync can change.
fn view(db: &Database, created: u128) -> Vec<String> {
    (1..=created)
        .map(|n| {
            db.get_task(&id(n)).unwrap().map_or("absent".into(), |t| {
                format!(
                    "{} | {} | {:?} | {}",
                    t.title,
                    t.notes,
                    t.status,
                    t.completed_at.is_some()
                )
            })
        })
        .collect()
}

async fn run_schedule(seed: u64, steps: usize) {
    let Some(w) = world().await else { return };
    let mut rng = Rng(seed);
    let mut created: u128 = 0;
    for step in 0..steps {
        let d = rng.below(3) as usize;
        let db = &w.devices[d];
        let context = format!("seed {seed} step {step} device {d}");
        match rng.below(100) {
            // Edits, offline.
            0..=24 => {
                created += 1;
                db.add_task(&mut Task::new(
                    id(created),
                    format!("t{created} by {d}"),
                    step as f64,
                ))
                .unwrap_or_else(|e| panic!("{context}: add: {e}"));
            }
            25..=44 if created > 0 => {
                let n = 1 + rng.below(created as u64) as u128;
                if db.get_task(&id(n)).unwrap().is_some() {
                    db.rename_task(&id(n), &format!("t{n} renamed by {d} at {step}"))
                        .unwrap_or_else(|e| panic!("{context}: rename: {e}"));
                }
            }
            45..=54 if created > 0 => {
                let n = 1 + rng.below(created as u64) as u128;
                if db.get_task(&id(n)).unwrap().is_some() {
                    db.set_task_notes(&id(n), &format!("notes by {d} at {step}"))
                        .unwrap_or_else(|e| panic!("{context}: notes: {e}"));
                }
            }
            55..=62 if created > 0 => {
                let n = 1 + rng.below(created as u64) as u128;
                if let Some(t) = db.get_task(&id(n)).unwrap() {
                    if t.completed_at.is_some() {
                        db.uncomplete_task(&id(n))
                    } else {
                        db.complete_task(&id(n))
                    }
                    .unwrap_or_else(|e| panic!("{context}: complete: {e}"));
                }
            }
            // Coming online.
            63..=92 => match w.clients[d].sync_once().await {
                Ok(_) => {}
                // Before the admin's device first comes online, the relay
                // has not seen this device's admission: it is refused a
                // session, and tries again later.
                Err(matters_sync_client::SyncError::Attention(
                    matters_sync_client::Attention::NotAMember,
                )) => {}
                Err(e) => panic!("{context}: sync: {e}"),
            },
            // An acknowledgement was lost: everything is sent again.
            93..=97 => db.sync_generation_changed().unwrap(),
            // The relay is restored from a backup into a new generation.
            _ => {
                w.relay.new_generation(&route()).await.unwrap();
            }
        }
    }
    // Everyone comes online until nothing moves.
    for _ in 0..3 {
        for (d, client) in w.clients.iter().enumerate() {
            client
                .sync_once()
                .await
                .unwrap_or_else(|e| panic!("seed {seed} final sync of device {d}: {e}"));
        }
    }
    let views: Vec<Vec<String>> = w.devices.iter().map(|db| view(db, created)).collect();
    for (d, v) in views.iter().enumerate().skip(1) {
        assert_eq!(
            v, &views[0],
            "seed {seed}: device {d} differs from device 0"
        );
    }
    assert!(
        views[0].iter().all(|line| line != "absent"),
        "seed {seed}: a created task is missing everywhere"
    );
    for (d, db) in w.devices.iter().enumerate() {
        assert!(
            db.pending_records().unwrap().is_empty(),
            "seed {seed}: device {d} has pending records"
        );
    }
    w.finish().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn recorded_schedules_converge() {
    for seed in [1, 2, 3, 5, 8, 13, 21, 34] {
        run_schedule(seed, 60).await;
    }
}
