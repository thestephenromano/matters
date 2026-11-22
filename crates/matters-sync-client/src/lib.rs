//! The resumable encrypted sync client (#39, docs/SYNC_SERVICE_CONTRACT.md).
//!
//! A [`Client`] moves a device's sealed records and signed controls
//! between its [`LocalStore`] and a relay, and never sees plaintext or
//! keys. One round ([`Client::sync_once`]):
//! 1. pushes the device's controls, then its records, acknowledging each
//!    in the store only on the relay's durable `200`/`201`;
//! 2. pulls the authority log, pairing each transition with this device's
//!    envelope of the new epoch's key;
//! 3. pulls the record log.
//!
//! A page's cursor is saved only after its entries are applied. Applying is
//! idempotent, so a page interrupted halfway is fetched and replayed whole
//! next time. A lost acknowledgement re-sends identical bytes, and the relay
//! answers with the original receipt.
//!
//! [`Client::run`] repeats rounds:
//! - when the relay hints at new records, the application signals a local
//!   edit, or a minute passes;
//! - after a temporary failure, with exponential backoff and jitter;
//! - not at all when the device needs attention (removed, conflict, quota),
//!   until the application wakes it.
//!
//! [`Status`] says which, in terms an interface can show without
//! cryptographic detail. Store calls run on a blocking thread, never across
//! a network wait, so local editing is never held up by the network.

use std::sync::Arc;
use std::time::Duration;

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use futures_util::StreamExt;
use matters_sync::authority::{self, Body, Hash, Route};
use matters_sync::checkpoint::Cursor;
use matters_sync::record::record_hash;
pub use matters_sync::store::{
    ControlOutcome, JoiningStore, LocalStore, Log, RecoveringStore, StoreError,
};
use matters_sync::wire;
use matters_sync::wire::SIGNATURE_BYTES;
use minicbor::Decoder;
use tokio::sync::{watch, Mutex, Notify};

const PAGE: u32 = 256;
const PERIOD: Duration = Duration::from_secs(60);
const MAX_BACKOFF: Duration = Duration::from_secs(300);

/// Why the device needs the user's or operator's attention. Sync stops
/// until the application wakes the client; local work is untouched.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Attention {
    /// This device is not, or no longer, a member of the collection.
    NotAMember,
    /// The relay refused an upload that conflicts with a stored record.
    Conflict,
    /// The collection's storage quota on the relay is used up.
    QuotaExhausted,
    /// The relay speaks another protocol version or does not know the route.
    Configuration(String),
}

/// What the client is doing, for an interface to show.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Status {
    Idle,
    Syncing,
    /// Everything this device holds is on the relay, and everything on the
    /// relay was received, as of the last round.
    UpToDate,
    /// A temporary failure; the next round starts after the delay.
    Retrying {
        after: Duration,
        reason: String,
    },
    NeedsAttention(Attention),
}

/// Why a round stopped.
#[derive(Debug, thiserror::Error)]
pub enum SyncError {
    #[error("temporarily unavailable: {reason}")]
    Retry {
        reason: String,
        after: Option<Duration>,
    },
    #[error("needs attention: {0:?}")]
    Attention(Attention),
    #[error("local store: {0}")]
    Store(StoreError),
}

type Result<T> = std::result::Result<T, SyncError>;

fn retry(reason: impl Into<String>) -> SyncError {
    SyncError::Retry {
        reason: reason.into(),
        after: None,
    }
}

/// What one round moved.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Report {
    pub pushed_controls: usize,
    pub pushed_records: usize,
    pub applied_controls: usize,
    pub received_records: usize,
    /// Records the relay served that the store refused (forged or
    /// malformed): evidence of a misbehaving relay or device.
    pub refused_records: usize,
    /// Controls the authority refused (forged, malformed or made stale by
    /// a later transition).
    pub refused_controls: usize,
    /// This round started the device from the relay's newest checkpoint.
    pub started_from_checkpoint: bool,
    /// This round made a checkpoint and pushed its parts.
    pub made_checkpoint: bool,
}

/// A relay error body: `{"code", "retry", ? "retry-after-ms", ...}`.
struct Refusal {
    status: u16,
    code: String,
    after: Option<Duration>,
}

fn refusal(status: u16, body: &[u8]) -> Refusal {
    let mut code = String::new();
    let mut after = None;
    let mut d = Decoder::new(body);
    if let Ok(Some(n)) = d.map() {
        for _ in 0..n {
            let Ok(key) = d.str() else { break };
            match key {
                "code" => code = d.str().unwrap_or("").to_owned(),
                "retry-after-ms" => after = d.u64().ok().map(Duration::from_millis),
                _ => {
                    if d.skip().is_err() {
                        break;
                    }
                }
            }
        }
    }
    Refusal {
        status,
        code,
        after,
    }
}

impl Refusal {
    /// The round's outcome for a refusal that is not handled in place.
    fn into_error(self) -> SyncError {
        match (self.status, self.code.as_str()) {
            (403, _) => SyncError::Attention(Attention::NotAMember),
            (409, _) => SyncError::Attention(Attention::Conflict),
            (507, _) => SyncError::Attention(Attention::QuotaExhausted),
            (404, code) => SyncError::Attention(Attention::Configuration(code.to_owned())),
            (429 | 503, code) => SyncError::Retry {
                reason: code.to_owned(),
                after: self.after,
            },
            (status, code) if status >= 500 => retry(format!("{status} {code}")),
            (status, code) => {
                SyncError::Attention(Attention::Configuration(format!("{status} {code}")))
            }
        }
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn route_hex(route: &Route) -> String {
    hex(&[route.application, route.tenant, route.collection].concat())
}

fn session_body(route: &Route, device: &Hash, answer: Option<(&[u8; 32], &[u8])>) -> Vec<u8> {
    let mut e = minicbor::Encoder::new(Vec::new());
    let built = (|| {
        e.map(if answer.is_some() { 4 } else { 2 })?;
        e.str("route")?
            .array(3)?
            .bytes(&route.application)?
            .bytes(&route.tenant)?
            .bytes(&route.collection)?;
        e.str("device")?.bytes(device)?;
        if let Some((challenge, signature)) = answer {
            e.str("challenge")?.bytes(challenge)?;
            e.str("signature")?.bytes(signature)?;
        }
        Ok::<_, minicbor::encode::Error<std::convert::Infallible>>(())
    })();
    // Writing to a Vec cannot fail.
    let _ = built;
    e.into_writer()
}

/// Read one byte-string field of a CBOR map body.
fn field_bytes(body: &[u8], name: &str) -> Option<Vec<u8>> {
    let mut d = Decoder::new(body);
    let n = d.map().ok()??;
    for _ in 0..n {
        let key = d.str().ok()?;
        if key == name {
            return d.bytes().ok().map(<[u8]>::to_vec);
        }
        d.skip().ok()?;
    }
    None
}

/// A fetched page: its entries and its cursor.
struct Page {
    generation: [u8; 16],
    high_water: u64,
    entries: Vec<Vec<u8>>,
    next: Vec<u8>,
    complete: bool,
}

fn parse_page(body: &[u8]) -> Option<Page> {
    let mut d = Decoder::new(body);
    let n = d.map().ok()??;
    let (mut entries, mut next, mut complete) = (None, None, None);
    let (mut generation, mut high_water) = (None, None);
    for _ in 0..n {
        match d.str().ok()? {
            "generation" => generation = d.bytes().ok()?.try_into().ok(),
            "high-water" => high_water = Some(d.u64().ok()?),
            "records" => {
                let count = d.array().ok()??;
                let mut list = Vec::new();
                for _ in 0..count {
                    d.array().ok()?;
                    d.u64().ok()?;
                    list.push(d.bytes().ok()?.to_vec());
                }
                entries = Some(list);
            }
            "next" => next = Some(d.bytes().ok()?.to_vec()),
            "complete" => complete = Some(d.bool().ok()?),
            _ => d.skip().ok()?,
        }
    }
    Some(Page {
        generation: generation?,
        high_water: high_water?,
        entries: entries?,
        next: next?,
        complete: complete?,
    })
}

/// The relay's newest snapshot records, newest first, and its generation.
fn parse_snapshots(body: &[u8]) -> Option<([u8; 16], Vec<Vec<u8>>)> {
    let mut d = Decoder::new(body);
    let n = d.map().ok()??;
    let (mut generation, mut entries) = (None, None);
    for _ in 0..n {
        match d.str().ok()? {
            "generation" => generation = d.bytes().ok()?.try_into().ok(),
            "records" => {
                let count = d.array().ok()??;
                let mut list = Vec::new();
                for _ in 0..count {
                    d.array().ok()?;
                    d.u64().ok()?;
                    list.push(d.bytes().ok()?.to_vec());
                }
                entries = Some(list);
            }
            _ => d.skip().ok()?,
        }
    }
    Some((generation?, entries?))
}

/// Engine updates between the checkpoints a device makes, by default.
pub const CHECKPOINT_EVERY: u64 = 1000;

/// The sync client for one device's store and one relay.
pub struct Client<S: LocalStore + 'static> {
    base: String,
    checkpoint_every: u64,
    http: reqwest::Client,
    store: Arc<S>,
    token: Mutex<Option<[u8; 32]>>,
    status: watch::Sender<Status>,
    wake: Arc<Notify>,
}

/// rustls' process-wide crypto provider: ring (decided for #39).
fn install_provider() {
    // Fails only if a provider is already installed, which is fine.
    let _ = rustls::crypto::ring::default_provider().install_default();
}

impl<S: LocalStore + 'static> Client<S> {
    /// A client for `endpoint` (e.g. `https://relay.example`), trusting the
    /// operating system's certificate store.
    pub fn new(endpoint: &str, store: Arc<S>) -> std::result::Result<Self, String> {
        install_provider();
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(60))
            .connect_timeout(Duration::from_secs(15))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|e| e.to_string())?;
        Ok(Self {
            base: endpoint.trim_end_matches('/').to_owned(),
            checkpoint_every: CHECKPOINT_EVERY,
            http,
            store,
            token: Mutex::new(None),
            status: watch::channel(Status::Idle).0,
            wake: Arc::new(Notify::new()),
        })
    }

    /// Make a checkpoint every `every` engine updates (#40) instead of
    /// every [`CHECKPOINT_EVERY`].
    pub fn with_checkpoint_interval(mut self, every: u64) -> Self {
        self.checkpoint_every = every.max(1);
        self
    }

    pub fn status(&self) -> watch::Receiver<Status> {
        self.status.subscribe()
    }

    /// A handle the application signals after local edits, to sync soon.
    pub fn waker(&self) -> Arc<Notify> {
        self.wake.clone()
    }

    async fn on_store<T: Send + 'static>(
        &self,
        f: impl FnOnce(&S) -> std::result::Result<T, StoreError> + Send + 'static,
    ) -> Result<T> {
        let store = self.store.clone();
        tokio::task::spawn_blocking(move || f(&store))
            .await
            .map_err(|e| SyncError::Store(e.to_string().into()))?
            .map_err(SyncError::Store)
    }

    /// A CBOR POST without a session, as signing in needs.
    async fn post(&self, path: &str, body: Vec<u8>) -> Result<(u16, Vec<u8>)> {
        let request = self
            .http
            .post(format!("{}{path}", self.base))
            .header("content-type", "application/cbor")
            .body(body);
        let response = request.send().await.map_err(|e| retry(e.to_string()))?;
        let status = response.status().as_u16();
        let bytes = response.bytes().await.map_err(|e| retry(e.to_string()))?;
        Ok((status, bytes.to_vec()))
    }

    async fn request(
        &self,
        method: reqwest::Method,
        path: &str,
        body: Option<Vec<u8>>,
        content_type: &str,
    ) -> Result<(u16, Vec<u8>)> {
        let mut request = self.http.request(method, format!("{}{path}", self.base));
        if let Some(body) = body {
            request = request.header("content-type", content_type).body(body);
        }
        let token = self.session().await?;
        request = request.bearer_auth(hex(&token));
        let response = request.send().await.map_err(|e| retry(e.to_string()))?;
        let status = response.status().as_u16();
        let bytes = response.bytes().await.map_err(|e| retry(e.to_string()))?;
        Ok((status, bytes.to_vec()))
    }

    /// An authenticated request, signing in again once if the session ended.
    async fn call(
        &self,
        method: reqwest::Method,
        path: &str,
        body: Option<Vec<u8>>,
        content_type: &str,
    ) -> Result<(u16, Vec<u8>)> {
        let first = self
            .request(method.clone(), path, body.clone(), content_type)
            .await?;
        if first.0 != 401 {
            return Ok(first);
        }
        *self.token.lock().await = None;
        self.request(method, path, body, content_type).await
    }

    /// The current session token, signing in if there is none.
    async fn session(&self) -> Result<[u8; 32]> {
        let mut token = self.token.lock().await;
        if let Some(t) = *token {
            return Ok(t);
        }
        let route = self.on_store(|s| s.route()).await?;
        let device = self.on_store(|s| s.device()).await?;
        let (status, body) = self
            .post("/v0/session/challenge", session_body(&route, &device, None))
            .await?;
        if status != 200 {
            return Err(refusal(status, &body).into_error());
        }
        let challenge: [u8; 32] = field_bytes(&body, "challenge")
            .and_then(|c| c.try_into().ok())
            .ok_or_else(|| retry("malformed challenge"))?;
        let signature = self.on_store(move |s| s.sign_session(&challenge)).await?;
        let (status, body) = self
            .post(
                "/v0/session",
                session_body(&route, &device, Some((&challenge, &signature))),
            )
            .await?;
        if status != 200 {
            return Err(refusal(status, &body).into_error());
        }
        let new: [u8; 32] = field_bytes(&body, "token")
            .and_then(|t| t.try_into().ok())
            .ok_or_else(|| retry("malformed session"))?;
        *token = Some(new);
        Ok(new)
    }

    /// One round: push, then pull. A changed relay generation is handled in
    /// place, by re-queueing everything and starting the round again once.
    pub async fn sync_once(&self) -> Result<Report> {
        match self.round().await {
            Err(SyncError::Retry { reason, .. }) if reason == "generation-changed" => {
                self.on_store(|s| s.generation_changed()).await?;
                self.round().await
            }
            other => other,
        }
    }

    async fn round(&self) -> Result<Report> {
        let route = self.on_store(|s| s.route()).await?;
        let r = route_hex(&route);
        let mut report = Report::default();
        // Push: controls first, so receivers know a writer before its records.
        for (seq, bytes) in self.on_store(|s| s.outgoing_controls()).await? {
            let path = format!("/v0/routes/{r}/authority/{}", hex(&record_hash(&bytes)));
            let (status, body) = self
                .call(
                    reqwest::Method::PUT,
                    &path,
                    Some(bytes),
                    "application/vnd.matters.sync-authority",
                )
                .await?;
            match status {
                200 | 201 => {
                    self.on_store(move |s| s.acknowledge_control(seq)).await?;
                    report.pushed_controls += 1;
                }
                _ => return Err(refusal(status, &body).into_error()),
            }
        }
        self.push_records(&r, &mut report).await?;
        // Pull the authority log whole first, so a transition finds this
        // device's envelope even on a later page.
        let device = self.on_store(|s| s.device()).await?;
        let mut pages = Vec::new();
        let mut cursor = self.on_store(|s| s.cursor(Log::Authority)).await?;
        loop {
            let page = self.fetch(&r, "authority", cursor.as_deref()).await?;
            let complete = page.complete;
            cursor = Some(page.next.clone());
            pages.push(page);
            if complete {
                break;
            }
        }
        let all: Vec<Vec<u8>> = pages.iter().flat_map(|p| p.entries.clone()).collect();
        'pages: for page in &pages {
            for control in &page.entries {
                let envelope = envelope_for(&all, control, &device);
                let control = control.clone();
                match self
                    .on_store(move |s| s.receive_control(&control, envelope.as_deref()))
                    .await?
                {
                    ControlOutcome::Done => report.applied_controls += 1,
                    ControlOutcome::Refused => report.refused_controls += 1,
                    // Its envelope is not on the relay yet: stop here, so
                    // this page is fetched again next round.
                    ControlOutcome::NeedsEnvelope => break 'pages,
                    ControlOutcome::Stopped => {
                        let next = page.next.clone();
                        self.on_store(move |s| s.save_cursor(Log::Authority, &next))
                            .await?;
                        return Err(SyncError::Attention(Attention::NotAMember));
                    }
                }
            }
            let next = page.next.clone();
            self.on_store(move |s| s.save_cursor(Log::Authority, &next))
                .await?;
        }
        // A device that holds nothing yet starts from the newest
        // checkpoint, when the relay has one it accepts.
        let mut cursor = self.on_store(|s| s.cursor(Log::Records)).await?;
        if cursor.is_none()
            && self.on_store(|s| s.wants_checkpoint()).await?
            && self.start_from_newest(&r).await?
        {
            report.started_from_checkpoint = true;
            cursor = self.on_store(|s| s.cursor(Log::Records)).await?;
        }
        // Pull records, saving each page's cursor once it is received.
        loop {
            let page = self.fetch(&r, "records", cursor.as_deref()).await?;
            // The whole page at once: the store applies it in one go (#241).
            let records = page.entries;
            for outcome in self.on_store(move |s| s.receive_records(&records)).await? {
                match outcome {
                    Ok(()) => report.received_records += 1,
                    Err(_) => report.refused_records += 1,
                }
            }
            let next = page.next.clone();
            self.on_store(move |s| s.save_cursor(Log::Records, &next))
                .await?;
            cursor = Some(page.next);
            if page.complete {
                // Up to date with the relay: a checkpoint may be due.
                let at = Cursor {
                    generation: page.generation,
                    sequence: page.high_water,
                };
                let every = self.checkpoint_every;
                if self.on_store(move |s| s.make_checkpoint(at, every)).await? {
                    report.made_checkpoint = true;
                    self.push_records(&r, &mut report).await?;
                }
                break;
            }
        }
        Ok(report)
    }

    /// Push every outgoing record, acknowledging each on the relay's
    /// durable receipt.
    async fn push_records(&self, r: &str, report: &mut Report) -> Result<()> {
        for (id, bytes) in self.on_store(|s| s.outgoing_records()).await? {
            let path = format!("/v0/routes/{r}/records/{}", hex(&id));
            let (status, body) = self
                .call(
                    reqwest::Method::PUT,
                    &path,
                    Some(bytes),
                    "application/vnd.matters.sync-record",
                )
                .await?;
            match status {
                200 | 201 => {
                    self.on_store(move |s| s.acknowledge_record(&id)).await?;
                    report.pushed_records += 1;
                }
                _ => return Err(refusal(status, &body).into_error()),
            }
        }
        Ok(())
    }

    /// Fetch the relay's newest snapshot records and hand the newest
    /// checkpoint's parts (one logical batch) to the store. A checkpoint
    /// the store cannot use leaves the device to read the whole log.
    async fn start_from_newest(&self, r: &str) -> Result<bool> {
        let path = format!("/v0/routes/{r}/snapshots?limit={PAGE}");
        let (status, body) = self
            .call(reqwest::Method::GET, &path, None, "application/cbor")
            .await?;
        if status != 200 {
            return Err(refusal(status, &body).into_error());
        }
        let (generation, records) =
            parse_snapshots(&body).ok_or_else(|| retry("malformed snapshots"))?;
        let route = self.on_store(|s| s.route()).await?;
        let batch_of = |bytes: &[u8]| {
            wire::parse_for_route(bytes, &route.application, &route.tenant, &route.collection)
                .ok()
                .map(|p| *p.header.logical_batch)
        };
        let Some(newest) = records.first().and_then(|b| batch_of(b)) else {
            return Ok(false);
        };
        let parts: Vec<Vec<u8>> = records
            .into_iter()
            .filter(|b| batch_of(b) == Some(newest))
            .collect();
        match self
            .on_store(move |s| s.start_from_checkpoint(&parts, generation))
            .await
        {
            Ok(started) => Ok(started),
            Err(SyncError::Store(_)) => Ok(false),
            Err(other) => Err(other),
        }
    }

    async fn fetch(&self, route: &str, log: &str, after: Option<&[u8]>) -> Result<Page> {
        let mut path = format!("/v0/routes/{route}/{log}?limit={PAGE}");
        if let Some(after) = after {
            path.push_str(&format!("&after={}", URL_SAFE_NO_PAD.encode(after)));
        }
        let (status, body) = self
            .call(reqwest::Method::GET, &path, None, "application/cbor")
            .await?;
        match status {
            200 => parse_page(&body).ok_or_else(|| retry("malformed page")),
            410 => Err(retry("generation-changed")),
            _ => Err(refusal(status, &body).into_error()),
        }
    }

    /// Run rounds until `shutdown` resolves: on hints, local edits, every
    /// minute, and after temporary failures with backoff. A round that needs
    /// attention waits for the application to wake the client.
    pub async fn run(&self, shutdown: impl std::future::Future<Output = ()>) {
        tokio::pin!(shutdown);
        let hints = Arc::new(Notify::new());
        let mut backoff = Duration::from_secs(1);
        let mut hint_task: Option<tokio::task::JoinHandle<()>> = None;
        loop {
            let _ = self.status.send(Status::Syncing);
            let wait = match self.sync_once().await {
                Ok(_) => {
                    backoff = Duration::from_secs(1);
                    let _ = self.status.send(Status::UpToDate);
                    if hint_task.as_ref().is_none_or(|t| t.is_finished()) {
                        hint_task = self.listen(hints.clone()).await;
                    }
                    Some(PERIOD)
                }
                Err(SyncError::Attention(attention)) => {
                    let _ = self.status.send(Status::NeedsAttention(attention));
                    None
                }
                Err(SyncError::Retry { reason, after }) => {
                    let delay = after.unwrap_or(backoff).max(backoff / 2);
                    backoff = (backoff * 2).min(MAX_BACKOFF);
                    let _ = self.status.send(Status::Retrying {
                        after: delay,
                        reason,
                    });
                    Some(jitter(delay))
                }
                Err(SyncError::Store(error)) => {
                    let _ = self.status.send(Status::Retrying {
                        after: backoff,
                        reason: error.to_string(),
                    });
                    Some(jitter(backoff))
                }
            };
            let sleep = async {
                match wait {
                    Some(d) => tokio::time::sleep(d).await,
                    None => std::future::pending().await,
                }
            };
            tokio::select! {
                _ = &mut shutdown => break,
                _ = self.wake.notified() => {}
                _ = hints.notified() => {}
                _ = sleep => {}
            }
        }
        if let Some(task) = hint_task {
            task.abort();
        }
        let _ = self.status.send(Status::Idle);
    }

    /// Hold the hints WebSocket open, notifying on each hint; ends when the
    /// connection does (the next round reconnects).
    async fn listen(&self, hints: Arc<Notify>) -> Option<tokio::task::JoinHandle<()>> {
        let route = self.on_store(|s| s.route()).await.ok()?;
        let token = self.session().await.ok()?;
        let url = format!("{}/v0/routes/{}/hints", self.base, route_hex(&route))
            .replacen("https://", "wss://", 1)
            .replacen("http://", "ws://", 1);
        use tokio_tungstenite::tungstenite::client::IntoClientRequest;
        let mut request = url.into_client_request().ok()?;
        request.headers_mut().insert(
            "authorization",
            format!("Bearer {}", hex(&token)).parse().ok()?,
        );
        let connector = tls_config().map(|c| tokio_tungstenite::Connector::Rustls(Arc::new(c)));
        let (mut socket, _) =
            tokio_tungstenite::connect_async_tls_with_config(request, None, false, connector)
                .await
                .ok()?;
        Some(tokio::spawn(async move {
            while let Some(Ok(_)) = socket.next().await {
                hints.notify_one();
            }
        }))
    }
}

/// The TLS configuration for the hints WebSocket: rustls with ring and the
/// operating system's trust store, as reqwest uses for requests.
fn tls_config() -> Option<rustls::ClientConfig> {
    use rustls_platform_verifier::BuilderVerifierExt;
    install_provider();
    rustls::ClientConfig::builder()
        .with_platform_verifier()
        .ok()
        .map(|b| b.with_no_client_auth())
}

/// This device's envelope of the key of the epoch `control` starts, if it
/// is a transition and the log holds one.
fn envelope_for(log: &[Vec<u8>], control: &[u8], device: &Hash) -> Option<Vec<u8>> {
    let parsed = authority::parse(control).ok()?;
    if !matches!(
        parsed.message.body,
        Body::Membership(_) | Body::RootRecovery(_)
    ) {
        return None;
    }
    let manifest = record_hash(control);
    log.iter()
        .find(|bytes| {
            matches!(authority::parse(bytes).map(|p| p.message.body),
                Ok(Body::KeyEnvelope(k)) if k.recipient == *device && k.manifest == manifest)
        })
        .cloned()
}

/// Up to a quarter more, so devices that failed together retry apart.
fn jitter(delay: Duration) -> Duration {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    delay + delay.mul_f64(f64::from(nanos % 1000) / 4000.0)
}

/// A joining device seen as a local store: its identity and session
/// signatures, and nothing to send or receive yet.
struct AsLocal<J>(Arc<J>);

impl<J: JoiningStore> LocalStore for AsLocal<J> {
    fn route(&self) -> std::result::Result<Route, StoreError> {
        self.0.route()
    }
    fn device(&self) -> std::result::Result<Hash, StoreError> {
        self.0.device()
    }
    fn sign_session(
        &self,
        challenge: &[u8; 32],
    ) -> std::result::Result<[u8; SIGNATURE_BYTES], StoreError> {
        self.0.sign_session(challenge)
    }
    fn outgoing_controls(&self) -> std::result::Result<Vec<(i64, Vec<u8>)>, StoreError> {
        Ok(Vec::new())
    }
    fn acknowledge_control(&self, _: i64) -> std::result::Result<(), StoreError> {
        Ok(())
    }
    fn outgoing_records(&self) -> std::result::Result<Vec<([u8; 32], Vec<u8>)>, StoreError> {
        Ok(Vec::new())
    }
    fn acknowledge_record(&self, _: &[u8; 32]) -> std::result::Result<(), StoreError> {
        Ok(())
    }
    fn receive_control(
        &self,
        _: &[u8],
        _: Option<&[u8]>,
    ) -> std::result::Result<ControlOutcome, StoreError> {
        Err("a joining device applies no controls".into())
    }
    fn receive_record(&self, _: &[u8]) -> std::result::Result<(), StoreError> {
        Err("a joining device receives no records".into())
    }
    fn cursor(&self, _: Log) -> std::result::Result<Option<Vec<u8>>, StoreError> {
        Ok(None)
    }
    fn save_cursor(&self, _: Log, _: &[u8]) -> std::result::Result<(), StoreError> {
        Ok(())
    }
    fn generation_changed(&self) -> std::result::Result<(), StoreError> {
        Ok(())
    }
}

/// A device joining a collection through an invitation (#42).
///
/// 1. It leaves its signed enrollment request in the relay's mailbox under
///    the invitation's lookup ID ([`Joiner::send_request`]).
/// 2. The admin's device collects it ([`Client::fetch_enrollment`]), admits
///    it, and publishes the transition and envelopes.
/// 3. The joining device is then a member. It reads the authority log and
///    finishes ([`Joiner::try_finish`]).
pub struct Joiner<J: JoiningStore + 'static> {
    client: Client<AsLocal<J>>,
    store: Arc<J>,
}

impl<J: JoiningStore + 'static> Joiner<J> {
    pub fn new(endpoint: &str, store: Arc<J>) -> std::result::Result<Self, String> {
        Ok(Self {
            client: Client::new(endpoint, Arc::new(AsLocal(store.clone())))?,
            store,
        })
    }

    /// Leave `request`, the signed enrollment request, under `lookup`. No
    /// session is needed: the device is not a member yet. Sending it again
    /// is harmless.
    pub async fn send_request(&self, lookup: &[u8; 16], request: &[u8]) -> Result<()> {
        let store = self.store.clone();
        let route = tokio::task::spawn_blocking(move || store.route())
            .await
            .map_err(|e| SyncError::Store(e.to_string().into()))?
            .map_err(SyncError::Store)?;
        let path = format!(
            "/v0/routes/{}/enrollments/{}",
            route_hex(&route),
            hex(lookup)
        );
        let response = self
            .client
            .http
            .put(format!("{}{path}", self.client.base))
            .header("content-type", "application/vnd.matters.sync-authority")
            .body(request.to_vec())
            .send()
            .await
            .map_err(|e| retry(format!("network: {e}")))?;
        let status = response.status().as_u16();
        let body = response
            .bytes()
            .await
            .map_err(|e| retry(format!("network: {e}")))?;
        match status {
            200 | 201 => Ok(()),
            _ => Err(refusal(status, &body).into_error()),
        }
    }

    /// Finish joining if the admin has admitted this device: read the whole
    /// authority log and hand it, with this device's envelopes, to the
    /// store. False while the device is not a member yet.
    pub async fn try_finish(&self) -> Result<bool> {
        let route = self.client.on_store(|s| s.route()).await?;
        let device = self.client.on_store(|s| s.device()).await?;
        let r = route_hex(&route);
        let mut log = Vec::new();
        let mut cursor: Option<Vec<u8>> = None;
        loop {
            let page = match self.client.fetch(&r, "authority", cursor.as_deref()).await {
                Ok(page) => page,
                Err(SyncError::Attention(Attention::NotAMember)) => return Ok(false),
                Err(other) => return Err(other),
            };
            log.extend(page.entries);
            cursor = Some(page.next);
            if page.complete {
                break;
            }
        }
        let (mut controls, mut envelopes) = (Vec::new(), Vec::new());
        for bytes in log {
            match authority::parse(&bytes).map(|p| p.message.body) {
                Ok(Body::KeyEnvelope(k)) => {
                    if k.recipient == device {
                        envelopes.push(bytes);
                    }
                }
                Ok(_) => controls.push(bytes),
                Err(_) => {}
            }
        }
        let store = self.store.clone();
        tokio::task::spawn_blocking(move || store.finish(&controls, &envelopes))
            .await
            .map_err(|e| SyncError::Store(e.to_string().into()))?
            .map_err(SyncError::Store)
    }
}

impl<S: LocalStore + 'static> Client<S> {
    /// For the collection's admin: collect the enrollment request a joining
    /// device left under `lookup`, if there is one.
    pub async fn fetch_enrollment(&self, lookup: &[u8; 16]) -> Result<Option<Vec<u8>>> {
        let route = self.on_store(|s| s.route()).await?;
        let path = format!(
            "/v0/routes/{}/enrollments/{}",
            route_hex(&route),
            hex(lookup)
        );
        let (status, body) = self
            .call(reqwest::Method::GET, &path, None, "application/cbor")
            .await?;
        match status {
            200 => Ok(Some(body)),
            404 => Ok(None),
            _ => Err(refusal(status, &body).into_error()),
        }
    }
}

/// A recovering device seen as a local store: it signs in as the root, and
/// has nothing of its own to send or receive.
struct RootAsLocal<R>(Arc<R>);

impl<R: RecoveringStore> LocalStore for RootAsLocal<R> {
    fn route(&self) -> std::result::Result<Route, StoreError> {
        self.0.route()
    }
    fn device(&self) -> std::result::Result<Hash, StoreError> {
        self.0.root()
    }
    fn sign_session(
        &self,
        challenge: &[u8; 32],
    ) -> std::result::Result<[u8; SIGNATURE_BYTES], StoreError> {
        self.0.sign_root_session(challenge)
    }
    fn outgoing_controls(&self) -> std::result::Result<Vec<(i64, Vec<u8>)>, StoreError> {
        Ok(Vec::new())
    }
    fn acknowledge_control(&self, _: i64) -> std::result::Result<(), StoreError> {
        Ok(())
    }
    fn outgoing_records(&self) -> std::result::Result<Vec<([u8; 32], Vec<u8>)>, StoreError> {
        Ok(Vec::new())
    }
    fn acknowledge_record(&self, _: &[u8; 32]) -> std::result::Result<(), StoreError> {
        Ok(())
    }
    fn receive_control(
        &self,
        _: &[u8],
        _: Option<&[u8]>,
    ) -> std::result::Result<ControlOutcome, StoreError> {
        Err("a recovering device applies no controls this way".into())
    }
    fn receive_record(&self, _: &[u8]) -> std::result::Result<(), StoreError> {
        Err("a recovering device receives no records this way".into())
    }
    fn cursor(&self, _: Log) -> std::result::Result<Option<Vec<u8>>, StoreError> {
        Ok(None)
    }
    fn save_cursor(&self, _: Log, _: &[u8]) -> std::result::Result<(), StoreError> {
        Ok(())
    }
    fn generation_changed(&self) -> std::result::Result<(), StoreError> {
        Ok(())
    }
}

/// A device recovering a collection from its recovery words (#42). It
/// signs in as the collection's root and reads the authority log. It then
/// recovers locally, as the next generation's sole admin, and publishes the
/// root recovery. From then on it syncs with an ordinary [`Client`].
pub struct Recoverer<R: RecoveringStore + 'static> {
    client: Client<RootAsLocal<R>>,
    store: Arc<R>,
}

impl<R: RecoveringStore + 'static> Recoverer<R> {
    pub fn new(endpoint: &str, store: Arc<R>) -> std::result::Result<Self, String> {
        Ok(Self {
            client: Client::new(endpoint, Arc::new(RootAsLocal(store.clone())))?,
            store,
        })
    }

    pub async fn recover(&self) -> Result<()> {
        let route = self.client.on_store(|s| s.route()).await?;
        let r = route_hex(&route);
        let mut log = Vec::new();
        let mut cursor: Option<Vec<u8>> = None;
        loop {
            let page = self
                .client
                .fetch(&r, "authority", cursor.as_deref())
                .await?;
            log.extend(page.entries);
            cursor = Some(page.next);
            if page.complete {
                break;
            }
        }
        let (mut controls, mut envelopes) = (Vec::new(), Vec::new());
        for bytes in log {
            match authority::parse(&bytes).map(|p| p.message.body) {
                Ok(Body::KeyEnvelope(_)) => envelopes.push(bytes),
                Ok(_) => controls.push(bytes),
                Err(_) => {}
            }
        }
        let store = self.store.clone();
        tokio::task::spawn_blocking(move || store.recover(&controls, &envelopes))
            .await
            .map_err(|e| SyncError::Store(e.to_string().into()))?
            .map_err(SyncError::Store)?;
        let store = self.store.clone();
        let outgoing = tokio::task::spawn_blocking(move || store.outgoing())
            .await
            .map_err(|e| SyncError::Store(e.to_string().into()))?
            .map_err(SyncError::Store)?;
        // The root's session publishes only what the recovery signed. The
        // device's own writer claim waits for its ordinary client.
        for (seq, bytes) in outgoing {
            let recovery_kind = matches!(
                authority::parse(&bytes).map(|p| p.message.body),
                Ok(Body::RootRecovery(_) | Body::KeyEnvelope(_))
            );
            if !recovery_kind {
                continue;
            }
            let path = format!("/v0/routes/{r}/authority/{}", hex(&record_hash(&bytes)));
            let (status, body) = self
                .client
                .call(
                    reqwest::Method::PUT,
                    &path,
                    Some(bytes),
                    "application/vnd.matters.sync-authority",
                )
                .await?;
            match status {
                200 | 201 => {
                    let store = self.store.clone();
                    tokio::task::spawn_blocking(move || store.acknowledge(seq))
                        .await
                        .map_err(|e| SyncError::Store(e.to_string().into()))?
                        .map_err(SyncError::Store)?;
                }
                _ => return Err(refusal(status, &body).into_error()),
            }
        }
        Ok(())
    }
}
