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
use matters_sync::record::record_hash;
pub use matters_sync::store::{ControlOutcome, LocalStore, Log, StoreError};
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
    entries: Vec<Vec<u8>>,
    next: Vec<u8>,
    complete: bool,
}

fn parse_page(body: &[u8]) -> Option<Page> {
    let mut d = Decoder::new(body);
    let n = d.map().ok()??;
    let (mut entries, mut next, mut complete) = (None, None, None);
    for _ in 0..n {
        match d.str().ok()? {
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
        entries: entries?,
        next: next?,
        complete: complete?,
    })
}

/// The sync client for one device's store and one relay.
pub struct Client<S: LocalStore + 'static> {
    base: String,
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
            http,
            store,
            token: Mutex::new(None),
            status: watch::channel(Status::Idle).0,
            wake: Arc::new(Notify::new()),
        })
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
        // Pull records, saving each page's cursor once it is received.
        let mut cursor = self.on_store(|s| s.cursor(Log::Records)).await?;
        loop {
            let page = self.fetch(&r, "records", cursor.as_deref()).await?;
            for record in page.entries {
                match self.on_store(move |s| s.receive_record(&record)).await {
                    Ok(()) => report.received_records += 1,
                    Err(SyncError::Store(_)) => report.refused_records += 1,
                    Err(other) => return Err(other),
                }
            }
            let next = page.next.clone();
            self.on_store(move |s| s.save_cursor(Log::Records, &next))
                .await?;
            cursor = Some(page.next);
            if page.complete {
                break;
            }
        }
        Ok(report)
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
