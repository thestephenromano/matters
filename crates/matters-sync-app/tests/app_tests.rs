//! The app's sync session (#42) through the real relay on PostgreSQL, as
//! the frontends drive it.
//!
//! The scenario:
//! 1. A device creates a collection, and its operator provisions the relay
//!    from the bundle.
//! 2. Another device joins with the invitation's text.
//! 3. Both edit.
//! 4. The admin removes it.
//! 5. A leaves, refused first while its edits are unsent.
//! 6. A third device recovers everything from the recovery kit.
//!
//! Set MATTERS_RELAY_TEST_DATABASE_URL as for the relay's tests.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use matters_core::storage::membership::MemoryKeyStore;
use matters_core::{Database, Task};
use matters_relay::api::{router, system_clock};
use matters_relay::Relay;
use matters_sync::record::VERIFICATION_KEY_BYTES;
use matters_sync::VerificationKey;
use matters_sync_app::{
    collection_id, parse_collection_id, parse_invitation, Health, LeaveRefused, Phase, SyncApp,
    MATTERS_APPLICATION,
};
use sqlx::postgres::PgPool;

const WAIT: Duration = Duration::from_secs(30);

fn id(n: u128) -> String {
    uuid::Uuid::from_u128(n).to_string()
}

struct Relayed {
    admin: PgPool,
    name: String,
    relay: Relay,
    endpoint: String,
    runtime: tokio::runtime::Runtime,
}

fn relay() -> Option<Relayed> {
    let Ok(url) = std::env::var("MATTERS_RELAY_TEST_DATABASE_URL") else {
        assert!(std::env::var_os("MATTERS_RELAY_REQUIRE_DB").is_none());
        eprintln!("skipped: no MATTERS_RELAY_TEST_DATABASE_URL");
        return None;
    };
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let (admin, name, relay, endpoint) = runtime.block_on(async {
        let admin = PgPool::connect(&url).await.unwrap();
        let name = format!("app_test_{}", uuid::Uuid::new_v4().simple());
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
        (admin, name, relay, endpoint)
    });
    Some(Relayed {
        admin,
        name,
        relay,
        endpoint,
        runtime,
    })
}

impl Relayed {
    fn finish(self) {
        self.runtime.block_on(async {
            self.relay.pool().close().await;
            sqlx::query(sqlx::AssertSqlSafe(format!(
                "DROP DATABASE {} WITH (FORCE)",
                self.name
            )))
            .execute(&self.admin)
            .await
            .unwrap();
        });
    }
}

/// A session over a fresh database, counting its change notices and
/// remembering the endpoint it asked to keep.
struct Device {
    app: SyncApp,
    changes: Arc<AtomicUsize>,
    saved: Arc<Mutex<Option<String>>>,
}

fn device(dir: &std::path::Path, name: &str) -> Device {
    let db = Arc::new(Database::open(dir.join(format!("{name}.db"))).unwrap());
    let changes = Arc::new(AtomicUsize::new(0));
    let saved = Arc::new(Mutex::new(None));
    let (c, s) = (changes.clone(), saved.clone());
    let app = SyncApp::open(
        db,
        Arc::new(MemoryKeyStore::default()),
        None,
        dir.join(name),
        move || {
            c.fetch_add(1, Ordering::SeqCst);
        },
        move |endpoint| *s.lock().unwrap() = endpoint,
    )
    .unwrap();
    Device {
        app,
        changes,
        saved,
    }
}

fn title(app: &SyncApp, n: u128) -> Option<String> {
    app.database().get_task(&id(n)).unwrap().map(|t| t.title)
}

#[test]
fn a_collection_from_creation_to_recovery() {
    let Some(relayed) = relay() else { return };
    let dir = tempfile::tempdir().unwrap();
    // A already has a task, from before sync.
    let a = device(dir.path(), "a");
    a.app
        .database()
        .add_task(&mut Task::new(id(1), "from before sync", 0.0))
        .unwrap();
    // A creates the collection. Until the relay is provisioned, it waits.
    assert!(a.app.create("ftp://wrong").is_err());
    a.app.create(&relayed.endpoint).unwrap();
    let view = a.app.view();
    assert_eq!(view.phase, Phase::On);
    assert_eq!(*a.saved.lock().unwrap(), Some(relayed.endpoint.clone()));
    let kit = view.kit.clone().unwrap();
    assert_eq!(kit.words.split_whitespace().count(), 24);
    assert!(a
        .app
        .wait_for(WAIT, |v| v.health == Health::WaitingForRelay));
    // The operator provisions it from the bundle.
    let bundle = view.provisioning.clone().unwrap();
    assert!(bundle.command.starts_with("matters-relay provision "));
    let key = std::fs::read(&bundle.root_key).unwrap();
    let genesis = std::fs::read(&bundle.genesis).unwrap();
    let route = parse_collection_id(&kit.collection).unwrap();
    assert_eq!(route.application, MATTERS_APPLICATION);
    assert_eq!(collection_id(&route), kit.collection);
    relayed
        .runtime
        .block_on(relayed.relay.provision(
            &route,
            &VerificationKey::from_bytes(<[u8; VERIFICATION_KEY_BYTES]>::try_from(key).unwrap()),
            &genesis,
        ))
        .unwrap();
    a.app.sync_now();
    assert!(a
        .app
        .wait_for(WAIT, |v| v.health == Health::UpToDate && v.unsent == 0));
    a.app.kit_kept();
    assert!(a.app.view().kit.is_none());
    assert!(a.app.view().admin);
    assert!(
        a.changes.load(Ordering::SeqCst) > 0,
        "the interface was told"
    );

    // B joins with the invitation's text, shown once the relay has it.
    a.app.invite().unwrap();
    assert!(
        a.app
            .wait_for(WAIT, |v| v.invitation.is_some() && !v.inviting),
        "{:?}",
        a.app.view()
    );
    let text = a.app.view().invitation.unwrap();
    assert!(parse_invitation("not an invitation").is_err());
    let b = device(dir.path(), "b");
    b.app.join(&relayed.endpoint, &text).unwrap();
    assert!(
        b.app.wait_for(WAIT, |v| v.phase == Phase::On),
        "{:?}",
        b.app.view()
    );
    assert!(a
        .app
        .wait_for(WAIT, |v| v.invitation.is_none() && v.devices.len() == 2));
    assert!(b.app.wait_for(WAIT, |_| title(&b.app, 1).is_some()));
    assert!(!b.app.view().admin);
    // Both edit.
    b.app
        .database()
        .add_task(&mut Task::new(id(2), "from B", 1.0))
        .unwrap();
    b.app.sync_now();
    assert!(b
        .app
        .wait_for(WAIT, |v| v.unsent == 0 && v.health == Health::UpToDate));
    a.app.sync_now();
    assert!(a.app.wait_for(WAIT, |_| title(&a.app, 2).is_some()));

    // A removes B. B keeps its tasks and is told.
    let b_id = b
        .app
        .view()
        .devices
        .iter()
        .find(|d| d.this_device)
        .unwrap()
        .id;
    a.app.remove_device(&b_id).unwrap();
    assert!(a
        .app
        .wait_for(WAIT, |v| v.devices.len() == 1 && v.unsent == 0));
    b.app.sync_now();
    assert!(
        b.app.wait_for(WAIT, |v| v.health == Health::Removed),
        "{:?}",
        b.app.view()
    );
    assert_eq!(title(&b.app, 2).as_deref(), Some("from B"));

    // A leaves. With an edit not yet sent (nothing woke the client, whose
    // next round is a minute away), only an explicit choice does it.
    a.app
        .database()
        .add_task(&mut Task::new(id(3), "never sent", 2.0))
        .unwrap();
    assert_eq!(a.app.leave(false), Err(LeaveRefused::Unsent(1)));
    assert_eq!(a.app.view().phase, Phase::On, "nothing changed");
    assert_eq!(a.app.view().unsent, 1, "the count shown is the one refused");
    a.app.leave(true).unwrap();
    assert_eq!(a.app.view().phase, Phase::Off);
    assert_eq!(*a.saved.lock().unwrap(), None, "the endpoint is forgotten");
    assert_eq!(title(&a.app, 1).as_deref(), Some("from before sync"));
    assert!(!a.app.database().sync_enabled());

    // Every device is lost; C recovers from A's kit.
    let c = device(dir.path(), "c");
    c.app
        .recover(&relayed.endpoint, &kit.collection, "wrong words")
        .unwrap();
    assert!(c
        .app
        .wait_for(WAIT, |v| v.phase == Phase::Off && v.error.is_some()));
    let c = device(dir.path(), "c2");
    c.app
        .recover(&relayed.endpoint, &kit.collection, &kit.words)
        .unwrap();
    assert!(
        c.app.wait_for(WAIT, |v| v.phase == Phase::On),
        "{:?}",
        c.app.view()
    );
    assert!(c.app.wait_for(WAIT, |_| title(&c.app, 2).is_some()));
    assert_eq!(title(&c.app, 1).as_deref(), Some("from before sync"));
    assert!(c.app.view().admin);
    relayed.finish();
}

/// Session tokens and sync traffic never travel in the clear (#45): a relay
/// is reached over https, or over http only on this machine. Each way in
/// refuses another address before it changes anything.
#[test]
fn a_relay_is_reached_over_https_or_on_this_machine_only() {
    let dir = tempfile::tempdir().unwrap();
    let a = device(dir.path(), "a");
    let refused = Err("the relay's address starts with https://".to_owned());
    for endpoint in [
        "http://relay.example",
        "http://127.0.0.1.relay.example",
        "http://127.0.0.1@relay.example",
        "ws://relay.example",
        "relay.example",
    ] {
        assert_eq!(a.app.create(endpoint), refused, "{endpoint}");
        assert_eq!(
            a.app.join(endpoint, "matters-invite:x"),
            refused,
            "{endpoint}"
        );
        assert_eq!(
            a.app.recover(endpoint, &"0".repeat(64), "words"),
            refused,
            "{endpoint}"
        );
    }
    let db = a.app.database();
    assert!(!db.sync_enabled() && db.enrolling().unwrap().is_none());
    assert_eq!(a.app.view().phase, Phase::Off);
    assert_eq!(*a.saved.lock().unwrap(), None);
    // On this machine, plain http is fine: tests, and a relay on the host.
    for endpoint in [
        "http://127.0.0.1:9",
        "http://[::1]:9",
        "http://localhost:9",
        "https://relay.example",
    ] {
        assert_eq!(matters_sync_client::check_endpoint(endpoint), Ok(()));
    }
    a.app.create("http://127.0.0.1:9").unwrap();
    assert_eq!(a.app.view().phase, Phase::On);
    assert!(a.app.view().kit.is_some());
}

/// A session on `db` with `keys`, its changes and saved address dropped.
fn session(db: Database, keys: Arc<MemoryKeyStore>, dir: &std::path::Path) -> SyncApp {
    SyncApp::open(Arc::new(db), keys, None, dir, || {}, |_| {}).unwrap()
}

/// A relay that takes connections and never answers.
fn silent_relay() -> String {
    let silent = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let endpoint = format!("http://{}", silent.local_addr().unwrap());
    std::thread::spawn(move || {
        let mut held = Vec::new();
        for stream in silent.incoming() {
            held.push(stream);
        }
    });
    endpoint
}

fn retrying(v: &matters_sync_app::View) -> bool {
    matches!(v.health, Health::Retrying(_))
}

/// Stopping sync while the client retries against a relay it cannot reach
/// (the Qt tests' setup) leaves at once, whenever it is asked, for a
/// database in memory or on disk.
#[test]
fn leaving_while_the_client_retries_always_leaves() {
    for (n, wait) in [0, 1500, 0, 1500].into_iter().enumerate() {
        let dir = tempfile::tempdir().unwrap();
        let db = if n < 2 {
            Database::open_in_memory().unwrap()
        } else {
            Database::open(dir.path().join("a.db")).unwrap()
        };
        db.add_task(&mut Task::new(id(1), "kept", 0.0)).unwrap();
        let app = session(db, Arc::default(), dir.path());
        app.create("http://127.0.0.1:9").unwrap();
        assert!(app.wait_for(WAIT, retrying), "{:?}", app.view());
        std::thread::sleep(Duration::from_millis(wait));
        assert_eq!(app.leave(true), Ok(()), "{n}");
        assert_eq!(app.view().phase, Phase::Off);
        assert_eq!(title(&app, 1).as_deref(), Some("kept"));
        // And it can start a collection again, as the Qt tests do.
        app.create("http://127.0.0.1:9").unwrap();
        assert_eq!(app.leave(true), Ok(()), "{n}, again");
    }
}

/// A refusal to leave says what is unsent now, not what was shown before
/// the latest edit: the interface asks again with the right count.
#[test]
fn a_refused_leave_shows_what_is_unsent_now() {
    let dir = tempfile::tempdir().unwrap();
    let app = session(
        Database::open_in_memory().unwrap(),
        Arc::default(),
        dir.path(),
    );
    app.create("http://127.0.0.1:9").unwrap();
    assert!(app.wait_for(WAIT, retrying));
    let shown = app.view().unsent;
    app.database()
        .add_task(&mut Task::new(id(1), "just now", 0.0))
        .unwrap();
    assert!(matches!(app.leave(false), Err(LeaveRefused::Unsent(_))));
    // The edit, and the writer claim its first seal made with it.
    let db = app.database();
    let now = db.outgoing_records().unwrap().len() + db.outgoing_controls().unwrap().len();
    assert!(now > shown);
    assert_eq!(app.view().unsent, now);
    assert_eq!(app.view().phase, Phase::On);
}

/// An edit counts as unsent as soon as sync is asked for, whatever the
/// last round left the view saying: waiting for nothing unsent after an
/// edit waits for the relay to have it. Here the client is held in a round
/// by a relay that never answers, so nothing else refreshes the view.
#[test]
fn an_edit_is_unsent_as_soon_as_sync_is_asked_for() {
    let dir = tempfile::tempdir().unwrap();
    let app = session(
        Database::open_in_memory().unwrap(),
        Arc::default(),
        dir.path(),
    );
    app.create(&silent_relay()).unwrap();
    assert!(app.wait_for(WAIT, |v| v.health == Health::Syncing));
    let shown = app.view().unsent;
    app.database()
        .add_task(&mut Task::new(id(1), "just now", 0.0))
        .unwrap();
    app.sync_now();
    let db = app.database();
    let now = db.outgoing_records().unwrap().len() + db.outgoing_controls().unwrap().len();
    assert!(now > shown);
    assert_eq!(app.view().unsent, now);
    assert_eq!(app.view().health, Health::Syncing);
}

/// Registering an invitation never holds up the caller (the interface's
/// thread), however long the relay takes: the view says it is pending,
/// and cancelling it drops whatever the relay answers later.
#[test]
fn an_invitation_is_registered_off_the_callers_thread() {
    let endpoint = silent_relay();
    let dir = tempfile::tempdir().unwrap();
    let app = session(
        Database::open_in_memory().unwrap(),
        Arc::default(),
        dir.path(),
    );
    app.create(&endpoint).unwrap();
    let started = std::time::Instant::now();
    app.invite().unwrap();
    assert!(started.elapsed() < Duration::from_secs(2));
    let view = app.view();
    assert!(view.inviting && view.invitation.is_none() && view.error.is_none());
    app.cancel_invitation();
    assert!(!app.view().inviting);
    assert_eq!(app.view().invitation, None);
}

/// A relay that refuses the registration leaves no invitation to show,
/// and says why.
#[test]
fn an_invitation_the_relay_cannot_take_is_reported() {
    let dir = tempfile::tempdir().unwrap();
    let app = session(
        Database::open_in_memory().unwrap(),
        Arc::default(),
        dir.path(),
    );
    assert!(app.invite().is_err(), "not syncing");
    app.create("http://127.0.0.1:9").unwrap();
    app.invite().unwrap();
    assert!(
        app.wait_for(WAIT, |v| !v.inviting && v.error.is_some()),
        "{:?}",
        app.view()
    );
    assert_eq!(app.view().invitation, None);
}

/// Another open of the database (matters-syncd, or another Matters) took
/// over as this device's writer: the app's writes are refused until it
/// reloads, which reads in the other open's work, keeps the recovery kit
/// still to be shown, and makes the app the writer again.
#[test]
fn a_superseded_session_reloads_and_writes_again() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("a.db");
    let keys = Arc::new(MemoryKeyStore::default());
    let app = session(Database::open(&path).unwrap(), keys.clone(), dir.path());
    app.create("http://127.0.0.1:9").unwrap();
    let kit = app.view().kit.clone();
    assert!(kit.is_some());
    let db = app.database().clone();
    db.add_task(&mut Task::new(id(1), "before", 0.0)).unwrap();
    let other = Database::open(&path).unwrap();
    other.resume_sync(keys.as_ref()).unwrap();
    other
        .add_task(&mut Task::new(id(2), "elsewhere", 1.0))
        .unwrap();
    assert!(matches!(
        db.add_task(&mut Task::new(id(3), "refused", 2.0)),
        Err(matters_core::MattersError::Superseded)
    ));
    app.reload().unwrap();
    let view = app.view();
    assert_eq!(view.phase, Phase::On);
    assert_eq!(view.endpoint.as_deref(), Some("http://127.0.0.1:9"));
    assert_eq!(view.kit, kit, "the kit is still to be shown");
    assert_eq!(view.error, None);
    db.add_task(&mut Task::new(id(4), "after reload", 3.0))
        .unwrap();
    assert_eq!(title(&app, 2).as_deref(), Some("elsewhere"));
    assert_eq!(title(&app, 3), None);
    assert!(matches!(
        other.add_task(&mut Task::new(id(5), "stale", 4.0)),
        Err(matters_core::MattersError::Superseded)
    ));
    assert!(
        app.wait_for(WAIT, retrying),
        "syncing again: {:?}",
        app.view()
    );
}

/// A database a newer Matters wrote since is not reloaded: the session
/// says why and goes on as it was.
#[test]
fn a_reload_refused_changes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("a.db");
    let app = session(Database::open(&path).unwrap(), Arc::default(), dir.path());
    app.create("http://127.0.0.1:9").unwrap();
    // A newer Matters opens it too, and migrates it past this version.
    Database::open(&path)
        .unwrap()
        .with_raw_conn(|c| {
            c.execute(
                "INSERT INTO schema_migrations (version, applied_at) VALUES (?1, 0)",
                [matters_core::storage::db::LATEST_SCHEMA + 1],
            )?;
            Ok(())
        })
        .unwrap();
    assert!(app.reload().is_err());
    let view = app.view();
    assert_eq!(view.phase, Phase::On);
    assert!(view.error.is_some());
    app.database()
        .add_task(&mut Task::new(id(1), "still writes", 0.0))
        .unwrap();
}
