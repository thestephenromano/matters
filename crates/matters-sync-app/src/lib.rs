//! The Matters app's sync session (#42), shared by the Qt and GTK
//! frontends.
//!
//! [`SyncApp`] holds the device's database and key store, and runs the sync
//! client on its own runtime, away from the interface's thread. A frontend:
//! - calls its operations, which return at once;
//! - shows its [`View`];
//! - is told through the `changed` callback when the view changed. It must
//!   then read the view again on its own main thread.
//!
//! The operations:
//! - **Start a new collection:** [`SyncApp::create`]. The database's
//!   existing tasks become its first records (#41). The view then carries
//!   the recovery kit, to show once, and the provisioning bundle for the
//!   relay's operator.
//! - **Add a device:** [`SyncApp::invite`] on a device already syncing, and
//!   [`SyncApp::join`] on the new one, through the relay's enrollment
//!   mailbox.
//! - **Recover everything** from the recovery kit after every device is
//!   lost: [`SyncApp::recover`].
//! - **Remove a device:** [`SyncApp::remove_device`], as the admin.
//! - **Return to local-only use:** [`SyncApp::leave`], which never strands
//!   unsent edits without an explicit choice.
//!
//! Local editing never waits for any of this: the database is written
//! immediately, and the client sends what it can when it can.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use matters_core::storage::membership::KeyStore;
use matters_core::storage::{Enrolling, Recovering};
use matters_core::{Database, MattersError};
use matters_sync::authority::{parse, Body, Hash, Invitation, Route};
use matters_sync_client::{check_endpoint, Attention, Client, Joiner, Recoverer, Status};
use tokio::sync::watch;
use zeroize::Zeroizing;

/// The application ID every Matters collection is routed under.
pub const MATTERS_APPLICATION: [u8; 16] = *b"io.matters.tasks";
/// How often a joining device, and an inviting one, look again.
const POLL: Duration = Duration::from_secs(2);
/// How long an invitation is shown: the relay keeps a request an hour.
const INVITATION_TTL: Duration = Duration::from_secs(3600);
const INVITATION_PREFIX: &str = "matters-invite:";
/// Why sync retries when the relay's roster is behind this device's.
pub const RELAY_BEHIND: &str =
    "the relay's list of devices is behind this one's; sync resumes once another device updates it";

/// Where this device stands with sync.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Phase {
    /// Local only.
    #[default]
    Off,
    /// Waiting for the admin's device to add this one.
    Joining,
    /// Recovering the collection from its recovery kit.
    Recovering,
    /// A member of a collection.
    On,
}

/// How syncing is going, in terms an interface can explain.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum Health {
    #[default]
    Idle,
    Syncing,
    /// Everything here is on the relay, and everything there is here.
    UpToDate,
    /// The relay could not be reached or was busy: sync tries again by
    /// itself, and edits wait here meanwhile.
    Retrying(String),
    /// The relay does not know this collection yet: its operator has to
    /// provision it with the bundle.
    WaitingForRelay,
    /// This device was removed from the collection. Its tasks stay here.
    Removed,
    /// The relay's storage for this collection is full. Edits wait here.
    StorageFull,
    /// The relay refused an upload as conflicting with one it holds.
    Conflict,
    /// The relay speaks another version, or is misconfigured.
    Unsupported(String),
}

/// What restores the collection when every device is lost. Shown once.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecoveryKit {
    pub endpoint: String,
    /// The collection's ID, in groups.
    pub collection: String,
    /// The 24 recovery words, wiped from memory when dropped.
    pub words: Zeroizing<String>,
}

/// What the relay's operator needs to accept a new collection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Provisioning {
    pub route: String,
    pub root_key: PathBuf,
    pub genesis: PathBuf,
    /// The operator's command.
    pub command: String,
}

/// A member device.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Device {
    pub id: Hash,
    /// A short form to show and compare: the first 8 bytes, in hex.
    pub short: String,
    pub this_device: bool,
    pub admin: bool,
}

/// Everything an interface shows about sync.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct View {
    pub phase: Phase,
    pub endpoint: Option<String>,
    pub health: Health,
    /// Edits saved here, and membership changes signed here, that the
    /// relay has not acknowledged yet.
    pub unsent: usize,
    /// The invitation to show the new device, while adding one.
    pub invitation: Option<String>,
    /// An invitation is being registered with the relay, before it is
    /// shown.
    pub inviting: bool,
    /// Shown once after a collection is created, until acknowledged.
    pub kit: Option<RecoveryKit>,
    pub provisioning: Option<Provisioning>,
    pub devices: Vec<Device>,
    /// Whether this device is the collection's admin.
    pub admin: bool,
    /// The last operation's failure, in plain words.
    pub error: Option<String>,
}

/// Why leaving was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LeaveRefused {
    /// These many edits have not reached the relay. Leaving anyway keeps
    /// them on this device only.
    Unsent(usize),
    Failed(String),
}

type Changed = Box<dyn Fn() + Send + Sync>;
type EndpointChanged = Box<dyn Fn(Option<String>) + Send + Sync>;

struct Running {
    client: Arc<Client<Database>>,
    stop: watch::Sender<bool>,
}

struct Inner {
    runtime: tokio::runtime::Runtime,
    db: Arc<Database>,
    keys: Arc<dyn KeyStore + Send + Sync>,
    data_dir: PathBuf,
    view: Mutex<View>,
    /// Held from reading the database to showing what it read, so a
    /// refresh never replaces a later one's counts with older ones.
    refreshing: Mutex<()>,
    running: Mutex<Option<Running>>,
    /// Bumped to stop an invitation's or a join's polling.
    attempt: Mutex<u64>,
    changed: Changed,
    endpoint_changed: EndpointChanged,
}

/// The app's sync session. Cheap to clone.
#[derive(Clone)]
pub struct SyncApp {
    inner: Arc<Inner>,
}

/// The text form of an invitation: a prefix and its payload in base64url.
pub fn invitation_text(invitation: &Invitation) -> Result<String, String> {
    let bytes = invitation.encode().map_err(|e| e.to_string())?;
    Ok(format!(
        "{INVITATION_PREFIX}{}",
        URL_SAFE_NO_PAD.encode(bytes)
    ))
}

/// Read an invitation's text form, ignoring surrounding whitespace.
pub fn parse_invitation(text: &str) -> Result<Invitation, String> {
    let body = text
        .trim()
        .strip_prefix(INVITATION_PREFIX)
        .ok_or("this is not a Matters invitation")?;
    let bytes = URL_SAFE_NO_PAD
        .decode(body.trim())
        .map_err(|_| "this invitation is damaged")?;
    Invitation::parse(&bytes).map_err(|_| "this invitation is damaged".into())
}

/// A collection's ID as shown in its recovery kit: tenant and collection,
/// 64 hex digits in groups of eight.
pub fn collection_id(route: &Route) -> String {
    let hex: String = [route.tenant, route.collection]
        .concat()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    hex.as_bytes()
        .chunks(8)
        .map(|c| std::str::from_utf8(c).expect("hex is ASCII"))
        .collect::<Vec<_>>()
        .join("-")
}

/// The route a recovery kit's collection ID names, accepting any case,
/// spaces and dashes.
pub fn parse_collection_id(text: &str) -> Result<Route, String> {
    let hex: String = text
        .chars()
        .filter(|c| !c.is_whitespace() && *c != '-')
        .collect::<String>()
        .to_lowercase();
    if hex.len() != 64 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err("a collection ID is 64 hexadecimal digits".into());
    }
    let bytes: Vec<u8> = (0..64)
        .step_by(2)
        .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).expect("checked hex"))
        .collect();
    Ok(Route {
        application: MATTERS_APPLICATION,
        tenant: bytes[..16].try_into().expect("16 bytes"),
        collection: bytes[16..].try_into().expect("16 bytes"),
    })
}

fn route_hex(route: &Route) -> String {
    [route.application, route.tenant, route.collection]
        .concat()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn fresh16() -> [u8; 16] {
    *uuid::Uuid::new_v4().as_bytes()
}

fn plain(error: impl std::fmt::Display) -> String {
    error.to_string()
}

fn health(status: &Status) -> Health {
    match status {
        Status::Idle => Health::Idle,
        Status::Syncing => Health::Syncing,
        Status::UpToDate => Health::UpToDate,
        Status::Retrying { reason, .. } => Health::Retrying(reason.clone()),
        Status::NeedsAttention(Attention::NotAMember) => Health::Removed,
        // Not removed: the relay lost this device's membership (a restore
        // from an older backup), and the client keeps trying.
        Status::NeedsAttention(Attention::RelayBehind) => Health::Retrying(RELAY_BEHIND.into()),
        Status::NeedsAttention(Attention::QuotaExhausted) => Health::StorageFull,
        Status::NeedsAttention(Attention::Conflict) => Health::Conflict,
        Status::NeedsAttention(Attention::Configuration(code)) if code == "unknown-route" => {
            Health::WaitingForRelay
        }
        Status::NeedsAttention(Attention::Configuration(code)) => Health::Unsupported(code.clone()),
    }
}

impl SyncApp {
    /// The session for `db`, whose keys are in `keys`, syncing with
    /// `endpoint` when it has joined a collection. Provisioning bundles are
    /// written under `data_dir`. `changed` is called, from any thread,
    /// whenever the view changes. `endpoint_changed` is called when the
    /// relay this device syncs with should be saved, or forgotten (`None`).
    pub fn open(
        db: Arc<Database>,
        keys: Arc<dyn KeyStore + Send + Sync>,
        endpoint: Option<String>,
        data_dir: impl Into<PathBuf>,
        changed: impl Fn() + Send + Sync + 'static,
        endpoint_changed: impl Fn(Option<String>) + Send + Sync + 'static,
    ) -> Result<Self, String> {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .thread_name("matters-sync")
            .enable_all()
            .build()
            .map_err(plain)?;
        let app = Self {
            inner: Arc::new(Inner {
                runtime,
                db,
                keys,
                data_dir: data_dir.into(),
                view: Mutex::new(View {
                    endpoint: endpoint.clone(),
                    ..View::default()
                }),
                refreshing: Mutex::new(()),
                running: Mutex::new(None),
                attempt: Mutex::new(0),
                changed: Box::new(changed),
                endpoint_changed: Box::new(endpoint_changed),
            }),
        };
        app.resume(endpoint)?;
        Ok(app)
    }

    /// Carry on as the database stands: syncing with `endpoint` when it
    /// has joined a collection, or waiting to join.
    fn resume(&self, endpoint: Option<String>) -> Result<(), String> {
        let db = &self.inner.db;
        match endpoint {
            Some(endpoint) if db.sync_enabled() => match db.resume_sync(self.inner.keys.as_ref()) {
                // A saved address the client refuses (plain http) leaves the
                // app working locally, with the reason shown.
                Ok(_) => {
                    if let Err(error) = self.start(&endpoint) {
                        self.update(|v| v.error = Some(error));
                    }
                }
                Err(error) => self.update(|v| v.error = Some(plain(error))),
            },
            Some(endpoint) if db.enrolling().map_err(plain)?.is_some() => {
                self.update(|v| {
                    v.phase = Phase::Joining;
                    v.endpoint = Some(endpoint.clone());
                });
                self.wait_to_join(endpoint);
            }
            _ => {}
        }
        Ok(())
    }

    /// Open the database again in this process (#45), after another open
    /// of it (matters-syncd, or another Matters) took over as this
    /// device's writer and this one's writes were refused
    /// ([`MattersError::Superseded`]). It reads in what that open wrote and
    /// becomes the writer again; the other open's writes are refused from
    /// then on. Nothing is lost: refused writes changed nothing, and edits
    /// not yet on the relay stay queued. A recovery kit still to be shown
    /// is kept; an invitation being shown is withdrawn.
    pub fn reload(&self) -> Result<(), String> {
        // Refused (a newer Matters wrote the database), nothing changes.
        self.inner.db.reopen().map_err(|e| self.fail(e))?;
        self.next_attempt();
        self.stop();
        let endpoint = {
            let mut view = self.inner.view.lock().unwrap();
            let endpoint = view.endpoint.clone();
            *view = View {
                endpoint: endpoint.clone(),
                kit: view.kit.take(),
                provisioning: view.provisioning.take(),
                ..View::default()
            };
            endpoint
        };
        (self.inner.changed)();
        self.resume(endpoint).map_err(|e| self.fail(e))?;
        self.refresh();
        Ok(())
    }

    /// What to show now.
    pub fn view(&self) -> View {
        self.inner.view.lock().unwrap().clone()
    }

    fn update(&self, f: impl FnOnce(&mut View)) {
        f(&mut self.inner.view.lock().unwrap());
        (self.inner.changed)();
    }

    fn fail(&self, error: impl std::fmt::Display) -> String {
        let message = plain(error);
        let copy = message.clone();
        self.update(|v| v.error = Some(copy));
        message
    }

    /// The unsent count, device list and admin flag, from the database.
    fn refresh(&self) {
        self.show(None);
    }

    /// Show the database as it is now, with the client's `health` when it
    /// changed: in one change of the view, so it never shows a new health
    /// with counts read before it.
    fn show(&self, health: Option<Health>) {
        let _refreshing = self.inner.refreshing.lock().unwrap();
        let db = &self.inner.db;
        // Edits, and membership changes signed here, not yet on the relay.
        let unsent = db.outgoing_records().map(|r| r.len()).unwrap_or(0)
            + db.outgoing_controls().map(|c| c.len()).unwrap_or(0);
        let membership = db.sync_membership().ok();
        let admin = membership.as_ref().and_then(|m| {
            db.sync_controls().ok().and_then(|controls| {
                let mut admin = None;
                for c in controls {
                    if let Ok(p) = parse(&c) {
                        match p.message.body {
                            Body::Genesis(g) => admin = Some(g.admin),
                            Body::Membership(m) | Body::RootRecovery(m) => admin = Some(m.admin),
                            _ => {}
                        }
                    }
                }
                admin.map(|a| (a, m.device))
            })
        });
        self.update(|v| {
            if let Some(health) = health {
                v.health = health;
            }
            v.unsent = unsent;
            match &membership {
                Some(m) => {
                    v.devices = m
                        .members
                        .iter()
                        .map(|id| Device {
                            id: *id,
                            short: id[..8].iter().map(|b| format!("{b:02x}")).collect(),
                            this_device: *id == m.device,
                            admin: admin.is_some_and(|(a, _)| a == *id),
                        })
                        .collect();
                    v.admin = admin.is_some_and(|(a, d)| a == d);
                }
                None => {
                    v.devices.clear();
                    v.admin = false;
                }
            }
        });
    }

    /// Start the client for `endpoint` and follow its status.
    fn start(&self, endpoint: &str) -> Result<(), String> {
        let client = Arc::new(Client::new(endpoint, self.inner.db.clone())?);
        let (stop, mut stopped) = watch::channel(false);
        let mut status = client.status();
        let app = self.clone();
        self.inner.runtime.spawn(async move {
            loop {
                let now = health(&status.borrow());
                app.show(Some(now));
                if status.changed().await.is_err() {
                    break;
                }
            }
        });
        let runner = client.clone();
        self.inner.runtime.spawn(async move {
            runner
                .run(async move {
                    let _ = stopped.changed().await;
                })
                .await;
        });
        *self.inner.running.lock().unwrap() = Some(Running { client, stop });
        let endpoint = endpoint.to_owned();
        (self.inner.endpoint_changed)(Some(endpoint.clone()));
        self.update(|v| {
            v.phase = Phase::On;
            v.endpoint = Some(endpoint);
            v.error = None;
        });
        self.refresh();
        Ok(())
    }

    fn stop(&self) {
        if let Some(running) = self.inner.running.lock().unwrap().take() {
            let _ = running.stop.send(true);
        }
    }

    fn client(&self) -> Option<Arc<Client<Database>>> {
        self.inner
            .running
            .lock()
            .unwrap()
            .as_ref()
            .map(|r| r.client.clone())
    }

    /// Sync now, as after an edit. The edit counts as unsent from here:
    /// until a round has sent it, the view says so, whatever the last round
    /// left it saying.
    pub fn sync_now(&self) {
        self.refresh();
        if let Some(client) = self.client() {
            client.waker().notify_one();
        }
    }

    /// Start a new collection that syncs with `endpoint`, this device its
    /// first member and admin. Existing tasks become its first records.
    /// The view then holds the recovery kit and the provisioning bundle.
    pub fn create(&self, endpoint: &str) -> Result<(), String> {
        let endpoint = endpoint.trim().trim_end_matches('/').to_owned();
        check_endpoint(&endpoint).map_err(|e| self.fail(e))?;
        let route = Route {
            application: MATTERS_APPLICATION,
            tenant: fresh16(),
            collection: fresh16(),
        };
        let db = &self.inner.db;
        let words = db
            .create_collection(self.inner.keys.as_ref(), route.clone())
            .map_err(|e| self.fail(e))?;
        let provisioning = self.write_bundle(&route).map_err(|e| self.fail(e))?;
        self.update(|v| {
            v.kit = Some(RecoveryKit {
                endpoint: endpoint.clone(),
                collection: collection_id(&route),
                words: Zeroizing::new(words),
            });
            v.provisioning = Some(provisioning);
        });
        self.start(&endpoint)
    }

    /// The root key and genesis the operator provisions the route with.
    fn write_bundle(&self, route: &Route) -> Result<Provisioning, String> {
        let genesis = self
            .inner
            .db
            .sync_controls()
            .map_err(plain)?
            .into_iter()
            .next()
            .ok_or("no genesis")?;
        let Body::Genesis(g) = parse(&genesis).map_err(plain)?.message.body else {
            return Err("the first control is not a genesis".into());
        };
        let hex = route_hex(route);
        let dir = self
            .inner
            .data_dir
            .join(format!("provision-{}", &hex[64..80]));
        std::fs::create_dir_all(&dir).map_err(plain)?;
        let root_key = dir.join("root.key");
        let genesis_path = dir.join("genesis.bin");
        std::fs::write(&root_key, g.root_key.as_bytes()).map_err(plain)?;
        std::fs::write(&genesis_path, &genesis).map_err(plain)?;
        Ok(Provisioning {
            command: format!(
                "matters-relay provision {hex} {} {}",
                path_text(&root_key),
                path_text(&genesis_path)
            ),
            route: hex,
            root_key,
            genesis: genesis_path,
        })
    }

    /// The recovery kit was written down: stop showing it.
    pub fn kit_kept(&self) {
        self.update(|v| v.kit = None);
    }

    /// Make an invitation for a new device, and add the device when its
    /// request arrives through the relay. Admin only.
    ///
    /// The relay takes a joining device's request only under an invitation
    /// its admin registered, so the invitation is registered first, on the
    /// session's runtime: this returns at once, with the view's `inviting`
    /// set. The view then shows the invitation, or the error that kept it
    /// from being registered.
    pub fn invite(&self) -> Result<(), String> {
        let client = self
            .client()
            .ok_or_else(|| self.fail("this device does not sync"))?;
        let invitation = self.inner.db.invite().map_err(|e| self.fail(e))?;
        let text = invitation_text(&invitation).map_err(|e| self.fail(e))?;
        let lookup = invitation
            .secret
            .lookup(&invitation.route)
            .map_err(|e| self.fail(e))?;
        let attempt = self.next_attempt();
        self.update(|v| {
            v.invitation = None;
            v.inviting = true;
            v.error = None;
        });
        let app = self.clone();
        self.inner.runtime.spawn(async move {
            let registered = client.register_invitation(&lookup).await;
            // Checked under the view's lock: a cancel in between wins.
            let mut shown = false;
            app.update(|v| {
                if !app.current(attempt) {
                    return;
                }
                v.inviting = false;
                match &registered {
                    Ok(()) => {
                        v.invitation = Some(text);
                        shown = true;
                    }
                    Err(error) => v.error = Some(plain(error)),
                }
            });
            if !shown {
                return;
            }
            let deadline = tokio::time::Instant::now() + INVITATION_TTL;
            while app.current(attempt) && tokio::time::Instant::now() < deadline {
                if let Ok(Some(request)) = client.fetch_enrollment(&lookup).await {
                    let admitted = app.inner.db.admit(&request, &invitation);
                    app.update(|v| {
                        v.invitation = None;
                        if let Err(error) = &admitted {
                            v.error = Some(plain(error));
                        }
                    });
                    let _ = client.sync_once().await;
                    app.refresh();
                    return;
                }
                tokio::time::sleep(POLL).await;
            }
            if app.current(attempt) {
                app.update(|v| v.invitation = None);
            }
        });
        Ok(())
    }

    /// Stop showing the invitation, or registering it; a request made with
    /// it is not added.
    pub fn cancel_invitation(&self) {
        self.next_attempt();
        self.update(|v| {
            v.invitation = None;
            v.inviting = false;
        });
    }

    fn next_attempt(&self) -> u64 {
        let mut attempt = self.inner.attempt.lock().unwrap();
        *attempt += 1;
        *attempt
    }

    fn current(&self, attempt: u64) -> bool {
        *self.inner.attempt.lock().unwrap() == attempt
    }

    /// Join the collection an invitation shown on another device names,
    /// through the relay at `endpoint`. The database must hold no tasks yet,
    /// or have set them aside (#41).
    pub fn join(&self, endpoint: &str, invitation: &str) -> Result<(), String> {
        let endpoint = endpoint.trim().trim_end_matches('/').to_owned();
        check_endpoint(&endpoint).map_err(|e| self.fail(e))?;
        let invitation = parse_invitation(invitation).map_err(|e| self.fail(e))?;
        let request = self
            .inner
            .db
            .start_enrollment(self.inner.keys.as_ref(), &invitation)
            .map_err(|e| self.fail(e))?;
        let lookup = invitation
            .secret
            .lookup(&invitation.route)
            .map_err(|e| self.fail(e))?;
        let joiner = self.joiner(&endpoint).map_err(|e| self.fail(e))?;
        self.update(|v| {
            v.phase = Phase::Joining;
            v.endpoint = Some(endpoint.clone());
            v.error = None;
        });
        let app = self.clone();
        self.inner.runtime.spawn(async move {
            if let Err(error) = joiner.send_request(&lookup, &request).await {
                app.update(|v| v.error = Some(plain(error)));
            }
            app.wait_to_join(endpoint);
        });
        Ok(())
    }

    fn joiner(
        &self,
        endpoint: &str,
    ) -> Result<Joiner<Enrolling<Arc<dyn KeyStore + Send + Sync>>>, String> {
        Joiner::new(
            endpoint,
            Arc::new(Enrolling {
                db: self.inner.db.clone(),
                keys: self.inner.keys.clone(),
            }),
        )
    }

    /// Poll until the admin has added this device, then start syncing.
    fn wait_to_join(&self, endpoint: String) {
        let Ok(joiner) = self.joiner(&endpoint) else {
            return;
        };
        let attempt = self.next_attempt();
        let app = self.clone();
        self.inner.runtime.spawn(async move {
            while app.current(attempt) {
                match joiner.try_finish().await {
                    Ok(true) => {
                        if let Err(error) = app.start(&endpoint) {
                            app.update(|v| v.error = Some(error));
                        }
                        return;
                    }
                    Ok(false) => {}
                    Err(error) => app.update(|v| v.error = Some(plain(error))),
                }
                tokio::time::sleep(POLL).await;
            }
        });
    }

    /// Recover the collection from its recovery kit after every device was
    /// lost. This device becomes its only member and admin. Other devices
    /// join again through invitations.
    pub fn recover(&self, endpoint: &str, collection: &str, words: &str) -> Result<(), String> {
        let endpoint = endpoint.trim().trim_end_matches('/').to_owned();
        check_endpoint(&endpoint).map_err(|e| self.fail(e))?;
        let route = parse_collection_id(collection).map_err(|e| self.fail(e))?;
        let recoverer = Recoverer::new(
            &endpoint,
            Arc::new(Recovering {
                db: self.inner.db.clone(),
                keys: self.inner.keys.clone(),
                words: Zeroizing::new(words.to_owned()),
                route,
            }),
        )
        .map_err(|e| self.fail(e))?;
        self.update(|v| {
            v.phase = Phase::Recovering;
            v.error = None;
        });
        let app = self.clone();
        self.inner.runtime.spawn(async move {
            match recoverer.recover().await {
                Ok(()) => {
                    if let Err(error) = app.start(&endpoint) {
                        app.update(|v| v.error = Some(error));
                    }
                }
                Err(error) => app.update(|v| {
                    v.phase = Phase::Off;
                    v.error = Some(format!("recovery failed: {error}"));
                }),
            }
        });
        Ok(())
    }

    /// Remove a device from the collection, as its admin. It keeps its
    /// tasks but receives nothing new.
    pub fn remove_device(&self, device: &Hash) -> Result<(), String> {
        self.inner
            .db
            .remove_member(device)
            .map_err(|e| self.fail(e))?;
        self.sync_now();
        Ok(())
    }

    /// Stop syncing and keep every task on this device only. While edits
    /// have not reached the relay, this refuses unless `discard_unsent`
    /// says the user chose to keep them here only.
    pub fn leave(&self, discard_unsent: bool) -> Result<(), LeaveRefused> {
        let db = &self.inner.db;
        if db.sync_enabled() {
            match db.leave_collection(discard_unsent) {
                Ok(_) => {}
                Err(MattersError::UnsentWork(n)) => {
                    // The count shown may be behind: show what is unsent now.
                    self.refresh();
                    return Err(LeaveRefused::Unsent(n));
                }
                Err(error) => return Err(LeaveRefused::Failed(self.fail(error))),
            }
        }
        // Refused, nothing changed; leaving, stop any invitation or join.
        self.next_attempt();
        self.stop();
        (self.inner.endpoint_changed)(None);
        self.update(|v| *v = View::default());
        Ok(())
    }

    /// The database this session syncs.
    pub fn database(&self) -> &Arc<Database> {
        &self.inner.db
    }

    /// Wait until `pred` holds for the view, for tests and scripted use.
    pub fn wait_for(&self, timeout: Duration, pred: impl Fn(&View) -> bool) -> bool {
        let deadline = std::time::Instant::now() + timeout;
        while std::time::Instant::now() < deadline {
            if pred(&self.view()) {
                return true;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        pred(&self.view())
    }
}

impl Drop for Inner {
    fn drop(&mut self) {
        if let Some(running) = self.running.get_mut().unwrap().take() {
            let _ = running.stop.send(true);
        }
    }
}

fn path_text(path: &Path) -> String {
    path.display().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A relay that lost this device's membership is not a removal: the
    /// interface says the relay is behind, and that sync carries on.
    #[test]
    fn a_relay_behind_is_told_apart_from_a_removal() {
        assert_eq!(
            health(&Status::NeedsAttention(Attention::NotAMember)),
            Health::Removed
        );
        assert_eq!(
            health(&Status::NeedsAttention(Attention::RelayBehind)),
            Health::Retrying(RELAY_BEHIND.into())
        );
    }
}
