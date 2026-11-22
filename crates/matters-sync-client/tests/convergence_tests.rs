//! Convergence under recorded schedules (#39): three Matters devices edit
//! offline and sync through the real relay in fixed, seeded interleavings.
//! The schedules include concurrent edits of one task, lost
//! acknowledgements, process restarts, checkpoints, emptied trash and a relay
//! restore. After a final round of syncing,
//! every device must hold the same tasks, field for field, and nothing
//! may be left pending or have refused an honest checkpoint. A task any device
//! purged stays absent everywhere (ADR 0001, C09); no other task goes missing.
//!
//! Each seed is a recorded schedule: a failure names its seed and step, and
//! rerunning that seed replays it exactly. See client_tests.rs for the
//! environment.

use std::path::PathBuf;
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
    endpoint: String,
    paths: Vec<PathBuf>,
    keys: Vec<MemoryKeyStore>,
    devices: Vec<Arc<Database>>,
    clients: Vec<Client<Database>>,
}

/// A client that makes a checkpoint every few updates, so the schedules
/// mix checkpoints into everything else.
fn client(endpoint: &str, db: Arc<Database>) -> Client<Database> {
    Client::new(endpoint, db)
        .unwrap()
        .with_checkpoint_interval(3)
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
    let paths: Vec<PathBuf> = ["a", "b", "c"]
        .iter()
        .map(|n| dir.path().join(format!("{n}.db")))
        .collect();
    let mut keys = vec![MemoryKeyStore::default()];
    let a = Database::open(&paths[0]).unwrap();
    a.create_collection(&keys[0], route()).unwrap();
    let mut devices = vec![Arc::new(a)];
    for path in &paths[1..] {
        let a = &devices[0];
        let joiner = Database::open(path).unwrap();
        let joiner_keys = MemoryKeyStore::default();
        let shown = a.invite().unwrap();
        let request = joiner.start_enrollment(&joiner_keys, &shown).unwrap();
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
                .finish_enrollment(&joiner_keys, &a.sync_controls().unwrap(), &envelopes)
                .unwrap(),
            SyncStanding::Active
        );
        devices.push(Arc::new(joiner));
        keys.push(joiner_keys);
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
        .map(|d| client(&endpoint, d.clone()))
        .collect();
    Some(World {
        _dir: dir,
        admin,
        name,
        relay,
        endpoint,
        paths,
        keys,
        devices,
        clients,
    })
}

impl World {
    /// The device's process stops and starts again: everything it knows
    /// comes back from its database and its key store.
    fn restart(&mut self, d: usize) {
        drop(self.clients.remove(d));
        drop(self.devices.remove(d));
        let db = Database::open(&self.paths[d]).unwrap();
        db.resume_sync(&self.keys[d]).unwrap();
        let db = Arc::new(db);
        self.devices.insert(d, db.clone());
        self.clients.insert(d, client(&self.endpoint, db));
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
    let Some(mut w) = world().await else { return };
    let mut rng = Rng(seed);
    let mut created: u128 = 0;
    // Tasks some device emptied from its trash: permanently absent
    // everywhere, whatever happened to them concurrently (ADR 0001, C09).
    let mut purged = std::collections::BTreeSet::new();
    for step in 0..steps {
        let d = rng.below(3) as usize;
        let db = &w.devices[d].clone();
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
            // Trashed and the trash emptied, at once.
            60..=62 if created > 0 => {
                let n = 1 + rng.below(created as u64) as u128;
                if db.get_task(&id(n)).unwrap().is_some() {
                    db.trash_task(&id(n))
                        .unwrap_or_else(|e| panic!("{context}: trash: {e}"));
                    db.empty_trash()
                        .unwrap_or_else(|e| panic!("{context}: empty trash: {e}"));
                    purged.insert(n);
                }
            }
            55..=59 if created > 0 => {
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
            63..=89 => match w.clients[d].sync_once().await {
                Ok(_) => {}
                // Before the admin's device first comes online, the relay
                // has not seen this device's admission: it is refused a
                // session, and tries again later. This device's own
                // authority admits it, so it reads as the relay being
                // behind unless the relay takes the admission from it.
                Err(matters_sync_client::SyncError::Attention(
                    matters_sync_client::Attention::NotAMember
                    | matters_sync_client::Attention::RelayBehind,
                )) => {}
                Err(e) => panic!("{context}: sync: {e}"),
            },
            // The device's process restarts.
            90..=92 => w.restart(d),
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
    // Nothing purged comes back, and nothing else goes missing.
    for (i, line) in views[0].iter().enumerate() {
        let n = i as u128 + 1;
        if purged.contains(&n) {
            assert_eq!(line, "absent", "seed {seed}: purged task {n} came back");
        } else {
            assert_ne!(
                line, "absent",
                "seed {seed}: task {n} went missing everywhere"
            );
        }
    }
    for (d, db) in w.devices.iter().enumerate() {
        assert!(
            db.pending_records().unwrap().is_empty(),
            "seed {seed}: device {d} has pending records"
        );
        // Honest checkpoints, in any interleaving, are never refused.
        let refused: Vec<_> = db
            .checkpoints()
            .unwrap()
            .into_iter()
            .filter(|(_, state, _)| state == "refused")
            .collect();
        assert!(
            refused.is_empty(),
            "seed {seed}: device {d} refused {refused:?}"
        );
    }
    let made: usize = w
        .devices
        .iter()
        .map(|db| db.checkpoints().unwrap().len())
        .sum();
    assert!(made > 0, "seed {seed}: the schedule made checkpoints");
    w.finish().await;
}

/// The recorded seeds every change runs. The weekly suite runs many more:
/// `MATTERS_CONVERGENCE_SEEDS=<n>` runs seeds 1 to n, and
/// `MATTERS_CONVERGENCE_STEPS` sets the steps of each (default 60).
#[tokio::test(flavor = "multi_thread")]
async fn recorded_schedules_converge() {
    let seeds: Vec<u64> = match std::env::var("MATTERS_CONVERGENCE_SEEDS") {
        Ok(n) => (1..=n.parse::<u64>().expect("a seed count")).collect(),
        Err(_) => vec![1, 2, 3, 5, 8, 13, 21, 34],
    };
    let steps = std::env::var("MATTERS_CONVERGENCE_STEPS")
        .map(|s| s.parse().expect("a step count"))
        .unwrap_or(60);
    for seed in seeds {
        run_schedule(seed, steps).await;
    }
}
