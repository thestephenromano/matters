//! The resumable encrypted sync client (#39, docs/SYNC_SERVICE_CONTRACT.md).
//!
//! A [`Client`] moves a device's sealed records and signed controls
//! between its [`LocalStore`] and a relay, and never sees plaintext or
//! keys. One round ([`Client::sync_once`]):
//! 1. pushes the device's controls, then its records, acknowledging each
//!    in the store only on the relay's durable `200`/`201`. A control the
//!    relay refuses for good is set aside, so the round still pulls;
//! 2. pulls the authority log page by page, pairing each transition with
//!    this device's envelopes of the new epoch's key;
//! 3. pulls the record log.
//!
//! A page's cursor is saved only after its entries are applied. Applying is
//! idempotent, so a page interrupted halfway is fetched and replayed whole
//! next time. A lost acknowledgement re-sends identical bytes, and the relay
//! answers with the original receipt.
//!
//! The relay is not trusted with more than availability:
//! - it is reached over TLS only (cleartext only to this machine, for
//!   tests and a relay on the same host);
//! - a response body is read up to [`MAX_RESPONSE_BYTES`];
//! - a page must keep the contract: no more entries than asked for, in
//!   ascending sequence, and a cursor that advances until the page says it
//!   is complete. A round reads at most [`MAX_PAGES_PER_ROUND`] pages of a
//!   log. A relay that breaks this is retried later, never followed;
//! - a page whose high-water mark is below this device's cursor means the
//!   relay's log was reset, and is handled as a new generation;
//! - a key envelope that does not open is not taken for the transition's
//!   fault: every envelope addressed to this device for it is tried.
//!
//! [`Client::run`] repeats rounds:
//! - when the relay hints at new records, the application signals a local
//!   edit, or a minute passes;
//! - after a temporary failure, with exponential backoff and jitter, and
//!   at most [`MAX_RETRY_AFTER`] when the relay asks for longer;
//! - not at all when the device needs attention (removed, conflict, quota),
//!   until the application wakes it.
//!
//! [`Status`] says which, in terms an interface can show without
//! cryptographic detail. Store calls run on a blocking thread, never across
//! a network wait, so local editing is never held up by the network.

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
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

#[cfg(feature = "fuzzing")]
pub mod fuzzing;

/// Entries asked for per page: the contract's largest page.
const PAGE: u32 = 256;
const PERIOD: Duration = Duration::from_secs(60);
const MAX_BACKOFF: Duration = Duration::from_secs(300);
/// The most bytes read of one relay response. The relay keeps a page under
/// 16 MiB; twice that leaves room for an encoding's overhead, and bounds
/// what a misbehaving relay can make a device hold.
pub const MAX_RESPONSE_BYTES: usize = 32 * 1024 * 1024;
/// The most pages of one log a round reads: about four million entries,
/// far beyond any collection's catch-up between checkpoints. A record
/// page's cursor is saved as it is applied, so a longer log goes on in the
/// next round.
pub const MAX_PAGES_PER_ROUND: usize = 16_384;
/// The most bytes of authority log held at once: while a transition waits
/// for its envelope on a later page, and when a joining or recovering
/// device reads the whole log.
const MAX_HELD_AUTHORITY_BYTES: usize = 128 * 1024 * 1024;
/// The longest wait a relay's `retry-after-ms` is followed for.
pub const MAX_RETRY_AFTER: Duration = Duration::from_secs(600);
/// How long the hints WebSocket may take to connect.
const HINTS_CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
/// The retry reason of a relay whose storage generation changed.
const GENERATION_CHANGED: &str = "generation-changed";

/// Why the device needs the user's or operator's attention. Sync stops
/// until the application wakes the client; local work is untouched.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Attention {
    /// This device is not, or no longer, a member of the collection: its
    /// own authority removed it, or the relay holds everything this device
    /// knows of the collection's membership and a later removal besides.
    NotAMember,
    /// The relay does not know this device as a member, but this device's
    /// own authority does, and the relay would not take the transitions
    /// that show it: its roster is behind (a restore from an older backup).
    /// The client keeps trying, with backoff; another device bringing the
    /// relay up to date resolves it.
    RelayBehind,
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

fn network(error: reqwest::Error) -> SyncError {
    retry(format!("network: {error}"))
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
    /// Controls the relay refused for good, set aside so the round could go
    /// on: a transition made stale by one this device had not received, or
    /// a malformed one.
    pub set_aside_controls: usize,
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
                "retry-after-ms" => {
                    after = d
                        .u64()
                        .ok()
                        .map(|ms| Duration::from_millis(ms).min(MAX_RETRY_AFTER))
                }
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

/// A fetched page: its entries with their sequence numbers, and its cursor.
struct Page {
    generation: [u8; 16],
    high_water: u64,
    entries: Vec<(u64, Vec<u8>)>,
    next: Vec<u8>,
    complete: bool,
}

/// `[[sequence, bytes], ...]`, as pages and snapshot lists carry entries.
fn parse_entries(d: &mut Decoder<'_>) -> Option<Vec<(u64, Vec<u8>)>> {
    let count = d.array().ok()??;
    let mut list = Vec::new();
    for _ in 0..count {
        if d.array().ok()? != Some(2) {
            return None;
        }
        let sequence = d.u64().ok()?;
        list.push((sequence, d.bytes().ok()?.to_vec()));
    }
    Some(list)
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
            "records" => entries = Some(parse_entries(&mut d)?),
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
            "records" => entries = Some(parse_entries(&mut d)?),
            _ => d.skip().ok()?,
        }
    }
    let entries = entries?.into_iter().map(|(_, bytes)| bytes).collect();
    Some((generation?, entries))
}

/// A relay cursor's generation and last sequence: `CBOR([generation,
/// last-sequence])` (sync-relay-v0.cddl), exactly.
fn decode_cursor(bytes: &[u8]) -> Option<([u8; 16], u64)> {
    let mut d = Decoder::new(bytes);
    if d.array().ok()? != Some(2) {
        return None;
    }
    let generation = d.bytes().ok()?.try_into().ok()?;
    let last = d.u64().ok()?;
    (d.position() == bytes.len()).then_some((generation, last))
}

/// What is wrong with a page, judged against the cursor it was fetched
/// after.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PageFault {
    /// The relay's log is not the one the cursor was read from: another
    /// generation, or a high-water mark below the cursor (a reset the relay
    /// did not announce). Handled as a restore.
    GenerationChanged,
    /// The page breaks the contract.
    Invalid(&'static str),
}

/// Check a page against the contract and the cursor `after` it answers:
/// - at most [`PAGE`] entries, in strictly ascending sequence after the
///   cursor;
/// - a next cursor of the page's generation, at or past its last entry, at
///   most the high-water mark;
/// - complete exactly when the next cursor reached the high-water mark,
///   and otherwise a next cursor past `after`, so paging always ends.
fn check_page(page: &Page, after: Option<&[u8]>) -> std::result::Result<(), PageFault> {
    let mut last = 0;
    if let Some(after) = after {
        match decode_cursor(after) {
            Some((generation, at)) if generation == page.generation => {
                if page.high_water < at {
                    return Err(PageFault::GenerationChanged);
                }
                last = at;
            }
            // Another generation (the relay should have answered 410), or a
            // cursor no relay of this contract made: start over.
            _ => return Err(PageFault::GenerationChanged),
        }
    }
    let Some((generation, next)) = decode_cursor(&page.next) else {
        return Err(PageFault::Invalid("cursor"));
    };
    if generation != page.generation {
        return Err(PageFault::Invalid("cursor generation"));
    }
    if page.entries.len() > PAGE as usize {
        return Err(PageFault::Invalid("more entries than asked for"));
    }
    let mut previous = last;
    for (sequence, _) in &page.entries {
        if *sequence <= previous {
            return Err(PageFault::Invalid("entries out of order"));
        }
        previous = *sequence;
    }
    if next < previous || next > page.high_water {
        return Err(PageFault::Invalid("cursor out of range"));
    }
    if page.complete != (next == page.high_water) {
        return Err(PageFault::Invalid("completeness"));
    }
    if !page.complete && next <= last {
        return Err(PageFault::Invalid("cursor does not advance"));
    }
    Ok(())
}

/// Read a response's status and body, refusing a body over
/// [`MAX_RESPONSE_BYTES`] as it streams in.
async fn read_body(mut response: reqwest::Response) -> Result<(u16, Vec<u8>)> {
    let too_large = || retry("the relay's response is too large");
    let status = response.status().as_u16();
    if response
        .content_length()
        .is_some_and(|n| n > MAX_RESPONSE_BYTES as u64)
    {
        return Err(too_large());
    }
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(network)? {
        if body.len() + chunk.len() > MAX_RESPONSE_BYTES {
            return Err(too_large());
        }
        body.extend_from_slice(&chunk);
    }
    Ok((status, body))
}

/// How a client reaches its relay.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Transport {
    /// HTTPS, and WSS for hints.
    Tls,
    /// Cleartext to this machine only.
    Loopback,
}

const NOT_HTTPS: &str = "the relay's address starts with https://";

/// Check a relay's address: `https://`, or `http://` to this machine only
/// (`127.0.0.1`, `::1` or `localhost`), for tests and a relay on the same
/// host. Anything else would carry session tokens in the clear.
pub fn check_endpoint(endpoint: &str) -> std::result::Result<(), String> {
    transport(endpoint).map(|_| ())
}

fn transport(endpoint: &str) -> std::result::Result<Transport, String> {
    let url = reqwest::Url::parse(endpoint.trim()).map_err(|_| NOT_HTTPS.to_owned())?;
    let host = url.host_str().ok_or_else(|| NOT_HTTPS.to_owned())?;
    match url.scheme() {
        "https" => Ok(Transport::Tls),
        "http" if is_loopback(host) => Ok(Transport::Loopback),
        _ => Err(NOT_HTTPS.to_owned()),
    }
}

fn is_loopback(host: &str) -> bool {
    let bare = host.trim_start_matches('[').trim_end_matches(']');
    host.eq_ignore_ascii_case("localhost")
        || bare
            .parse::<std::net::IpAddr>()
            .is_ok_and(|ip| ip.is_loopback())
}

/// Engine updates between the checkpoints a device makes, by default.
pub const CHECKPOINT_EVERY: u64 = 1000;

/// The sync client for one device's store and one relay.
pub struct Client<S: LocalStore + 'static> {
    base: String,
    transport: Transport,
    checkpoint_every: u64,
    http: reqwest::Client,
    store: Arc<S>,
    token: Mutex<Option<[u8; 32]>>,
    /// Set by a generation change: the next round publishes every control
    /// this device knows, not only those it signed.
    republish: AtomicBool,
    status: watch::Sender<Status>,
    wake: Arc<Notify>,
}

/// rustls' process-wide crypto provider: ring (decided for #39).
fn install_provider() {
    // Fails only if a provider is already installed, which is fine.
    let _ = rustls::crypto::ring::default_provider().install_default();
}

/// How a control PUT ended.
enum Put {
    Stored,
    /// Refused for good: these bytes will never be stored.
    Refused,
}

/// Whether `bytes` is a membership transition or root recovery: what makes
/// a device a member on the relay.
fn is_transition(bytes: &[u8]) -> bool {
    matches!(
        authority::parse(bytes).map(|p| p.message.body),
        Ok(Body::Membership(_) | Body::RootRecovery(_))
    )
}

impl<S: LocalStore + 'static> Client<S> {
    /// A client for `endpoint` (e.g. `https://relay.example`), trusting the
    /// operating system's certificate store. Plain `http://` is refused
    /// except to this machine ([`check_endpoint`]).
    pub fn new(endpoint: &str, store: Arc<S>) -> std::result::Result<Self, String> {
        let transport = transport(endpoint)?;
        install_provider();
        let http = reqwest::Client::builder()
            .https_only(transport == Transport::Tls)
            .timeout(Duration::from_secs(60))
            .connect_timeout(Duration::from_secs(15))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|e| e.to_string())?;
        Ok(Self {
            base: endpoint.trim().trim_end_matches('/').to_owned(),
            transport,
            checkpoint_every: CHECKPOINT_EVERY,
            http,
            store,
            token: Mutex::new(None),
            republish: AtomicBool::new(false),
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
        read_body(request.send().await.map_err(network)?).await
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
        read_body(request.send().await.map_err(network)?).await
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
        let new = match self.sign_in().await {
            Err(SyncError::Attention(Attention::NotAMember)) => self.sign_in_again().await?,
            other => other?,
        };
        *token = Some(new);
        Ok(new)
    }

    /// Answer the relay's challenge for a session token.
    async fn sign_in(&self) -> Result<[u8; 32]> {
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
        field_bytes(&body, "token")
            .and_then(|t| t.try_into().ok())
            .ok_or_else(|| retry("malformed session"))
    }

    /// The relay does not know this device as a member. If this device's
    /// own authority does, the relay may have been restored from a backup
    /// older than its membership: publish the transitions this device
    /// knows, and sign in once more. Still refused, the relay is behind if
    /// it would not take them; if it took them all, it holds a later
    /// removal this device has not received.
    async fn sign_in_again(&self) -> Result<[u8; 32]> {
        if !self.on_store(|s| s.is_current_member()).await? {
            return Err(SyncError::Attention(Attention::NotAMember));
        }
        let Some(all_taken) = self.republish_without_session().await? else {
            return Err(SyncError::Attention(Attention::NotAMember));
        };
        match self.sign_in().await {
            Err(SyncError::Attention(Attention::NotAMember)) if !all_taken => {
                Err(SyncError::Attention(Attention::RelayBehind))
            }
            other => other,
        }
    }

    /// Publish the membership transitions and root recoveries this device
    /// knows, oldest first, without a session: they are what makes a device
    /// a member, so a relay that lost them must take them from anyone.
    /// `Some(true)` if the relay took every one, `Some(false)` if it
    /// refused one (the rest depend on it, so they are not sent), and
    /// `None` when there is nothing to publish.
    async fn republish_without_session(&self) -> Result<Option<bool>> {
        let route = self.on_store(|s| s.route()).await?;
        let r = route_hex(&route);
        let transitions: Vec<Vec<u8>> = self
            .on_store(|s| s.known_controls())
            .await?
            .into_iter()
            .filter(|bytes| is_transition(bytes))
            .collect();
        if transitions.is_empty() {
            return Ok(None);
        }
        for bytes in transitions {
            let path = format!("/v0/routes/{r}/authority/{}", hex(&record_hash(&bytes)));
            let response = self
                .http
                .put(format!("{}{path}", self.base))
                .header("content-type", "application/vnd.matters.sync-authority")
                .body(bytes)
                .send()
                .await
                .map_err(network)?;
            let (status, _) = read_body(response).await?;
            if !matches!(status, 200 | 201) {
                return Ok(Some(false));
            }
        }
        Ok(Some(true))
    }

    /// One round: push, then pull. A changed relay generation is handled in
    /// place, by re-queueing everything and starting the round again once.
    pub async fn sync_once(&self) -> Result<Report> {
        match self.round().await {
            Err(SyncError::Retry { reason, .. }) if reason == GENERATION_CHANGED => {
                self.generation_changed().await?;
                self.round().await
            }
            other => other,
        }
    }

    /// The relay's log is not the one this device read: forget the cursors
    /// and queue everything again. Every known control goes back too,
    /// whoever signed it, since the restore may predate any of them; the
    /// transitions first and without a session, so a device the restored
    /// relay does not know yet can sign in.
    async fn generation_changed(&self) -> Result<()> {
        self.on_store(|s| s.generation_changed()).await?;
        self.republish.store(true, Ordering::SeqCst);
        self.republish_without_session().await?;
        Ok(())
    }

    async fn round(&self) -> Result<Report> {
        let route = self.on_store(|s| s.route()).await?;
        let r = route_hex(&route);
        let mut report = Report::default();
        // Push: controls first, so receivers know a writer before its records.
        self.push_controls(&r, &mut report).await?;
        self.push_records(&r, &mut report).await?;
        self.pull_authority(&r, &mut report).await?;
        self.pull_records(&r, &mut report).await?;
        Ok(report)
    }

    async fn put_control(&self, r: &str, bytes: Vec<u8>) -> Result<Put> {
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
            200 | 201 => Ok(Put::Stored),
            // Malformed, too large, or refused by the relay's authority
            // (made stale by a transition this device has not received).
            400 | 413 | 422 => Ok(Put::Refused),
            _ => Err(refusal(status, &body).into_error()),
        }
    }

    /// Push this device's outgoing controls, then, after a generation
    /// change, every control it knows. A control the relay refuses for
    /// good is set aside: the round goes on to pull, which is how this
    /// device learns what made it stale.
    async fn push_controls(&self, r: &str, report: &mut Report) -> Result<()> {
        for (seq, bytes) in self.on_store(|s| s.outgoing_controls()).await? {
            match self.put_control(r, bytes).await? {
                Put::Stored => {
                    self.on_store(move |s| s.acknowledge_control(seq)).await?;
                    report.pushed_controls += 1;
                }
                Put::Refused => {
                    self.on_store(move |s| s.set_aside_control(seq)).await?;
                    report.set_aside_controls += 1;
                }
            }
        }
        if self.republish.load(Ordering::SeqCst) {
            for bytes in self.on_store(|s| s.known_controls()).await? {
                // The genesis is the route's, provisioned by its operator.
                if matches!(
                    authority::parse(&bytes).map(|p| p.message.body),
                    Ok(Body::Genesis(_))
                ) {
                    continue;
                }
                match self.put_control(r, bytes).await? {
                    Put::Stored => report.pushed_controls += 1,
                    Put::Refused => report.set_aside_controls += 1,
                }
            }
            self.republish.store(false, Ordering::SeqCst);
        }
        Ok(())
    }

    /// Pull the authority log and apply it page by page. A transition is
    /// tried with each envelope this device has seen for it; when none
    /// opens, later pages are read for more (the admin publishes them right
    /// after the transition), up to [`MAX_HELD_AUTHORITY_BYTES`] held. If
    /// none comes, applying stops there, and the page is read again next
    /// round.
    async fn pull_authority(&self, r: &str, report: &mut Report) -> Result<()> {
        let device = self.on_store(|s| s.device()).await?;
        let cursor = self.on_store(|s| s.cursor(Log::Authority)).await?;
        let mut reader = LogReader::new(self, r, "authority", cursor);
        // Pages read and not yet wholly applied, oldest first; `at` is the
        // next entry to apply of the first.
        let mut window: VecDeque<Page> = VecDeque::new();
        let (mut at, mut held) = (0, 0);
        // This device's envelopes seen so far, by the manifest they name.
        let mut mine: HashMap<Hash, Vec<Vec<u8>>> = HashMap::new();
        let mut read_more = true;
        loop {
            if read_more {
                let Some(page) = reader.next().await? else {
                    // The log is read; a transition still lacks its envelope.
                    return Ok(());
                };
                for (_, bytes) in &page.entries {
                    match authority::parse(bytes).map(|p| p.message.body) {
                        Ok(Body::KeyEnvelope(k)) if k.recipient == device => {
                            let seen = mine.entry(k.manifest).or_default();
                            if !seen.contains(bytes) {
                                seen.push(bytes.clone());
                            }
                        }
                        _ => {}
                    }
                }
                held += page.entries.iter().map(|(_, b)| b.len()).sum::<usize>();
                window.push_back(page);
            }
            let Some(front) = window.front() else {
                return Ok(());
            };
            let mut waiting = false;
            while at < front.entries.len() {
                let control = front.entries[at].1.clone();
                let candidates = if is_transition(&control) {
                    mine.get(&record_hash(&control))
                        .cloned()
                        .unwrap_or_default()
                } else {
                    Vec::new()
                };
                match self.apply_control(control, candidates).await? {
                    ControlOutcome::Done => report.applied_controls += 1,
                    ControlOutcome::Refused => report.refused_controls += 1,
                    ControlOutcome::NeedsEnvelope => {
                        waiting = true;
                        break;
                    }
                    ControlOutcome::Stopped => {
                        let next = front.next.clone();
                        self.on_store(move |s| s.save_cursor(Log::Authority, &next))
                            .await?;
                        return Err(SyncError::Attention(Attention::NotAMember));
                    }
                }
                at += 1;
            }
            if waiting {
                // Its envelope is not on the relay yet, or lies further on.
                if reader.complete || held > MAX_HELD_AUTHORITY_BYTES {
                    return Ok(());
                }
                read_more = true;
                continue;
            }
            let next = front.next.clone();
            self.on_store(move |s| s.save_cursor(Log::Authority, &next))
                .await?;
            if let Some(done) = window.pop_front() {
                held -= done.entries.iter().map(|(_, b)| b.len()).sum::<usize>();
            }
            at = 0;
            read_more = window.is_empty();
        }
    }

    /// Apply one control, with each candidate envelope in turn until one
    /// opens. With none that opens, a transition that keeps this device a
    /// member waits for one: it is never skipped.
    async fn apply_control(
        &self,
        control: Vec<u8>,
        candidates: Vec<Vec<u8>>,
    ) -> Result<ControlOutcome> {
        let control = Arc::new(control);
        if candidates.is_empty() {
            let control = control.clone();
            return self
                .on_store(move |s| s.receive_control(&control, None))
                .await;
        }
        for envelope in candidates {
            let control = control.clone();
            match self
                .on_store(move |s| s.receive_control(&control, Some(&envelope)))
                .await?
            {
                ControlOutcome::NeedsEnvelope => continue,
                outcome => return Ok(outcome),
            }
        }
        Ok(ControlOutcome::NeedsEnvelope)
    }

    /// Pull the record log, saving each page's cursor once it is received.
    /// A device that holds nothing yet starts from the newest checkpoint,
    /// when the relay has one it accepts.
    async fn pull_records(&self, r: &str, report: &mut Report) -> Result<()> {
        let mut cursor = self.on_store(|s| s.cursor(Log::Records)).await?;
        if cursor.is_none()
            && self.on_store(|s| s.wants_checkpoint()).await?
            && self.start_from_newest(r).await?
        {
            report.started_from_checkpoint = true;
            cursor = self.on_store(|s| s.cursor(Log::Records)).await?;
        }
        let mut reader = LogReader::new(self, r, "records", cursor);
        while let Some(page) = reader.next().await? {
            // The whole page at once: the store applies it in one go (#241).
            let records: Vec<Vec<u8>> = page.entries.into_iter().map(|(_, b)| b).collect();
            for outcome in self.on_store(move |s| s.receive_records(&records)).await? {
                match outcome {
                    Ok(()) => report.received_records += 1,
                    Err(_) => report.refused_records += 1,
                }
            }
            let next = page.next;
            self.on_store(move |s| s.save_cursor(Log::Records, &next))
                .await?;
            if page.complete {
                // Up to date with the relay: a checkpoint may be due.
                let at = Cursor {
                    generation: page.generation,
                    sequence: page.high_water,
                };
                let every = self.checkpoint_every;
                if self.on_store(move |s| s.make_checkpoint(at, every)).await? {
                    report.made_checkpoint = true;
                    self.push_records(r, report).await?;
                }
            }
        }
        Ok(())
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
        if records.len() > PAGE as usize {
            return Err(retry("the relay served more snapshots than asked for"));
        }
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

    /// Fetch one page of `log` after the cursor `after`, checked against
    /// the contract ([`check_page`]).
    async fn fetch(&self, route: &str, log: &str, after: Option<&[u8]>) -> Result<Page> {
        let mut path = format!("/v0/routes/{route}/{log}?limit={PAGE}");
        if let Some(after) = after {
            path.push_str(&format!("&after={}", URL_SAFE_NO_PAD.encode(after)));
        }
        let (status, body) = self
            .call(reqwest::Method::GET, &path, None, "application/cbor")
            .await?;
        match status {
            200 => {
                let page = parse_page(&body).ok_or_else(|| retry("malformed page"))?;
                match check_page(&page, after) {
                    Ok(()) => Ok(page),
                    Err(PageFault::GenerationChanged) => Err(retry(GENERATION_CHANGED)),
                    Err(PageFault::Invalid(why)) => {
                        Err(retry(format!("the relay served a bad {log} page: {why}")))
                    }
                }
            }
            410 => Err(retry(GENERATION_CHANGED)),
            _ => Err(refusal(status, &body).into_error()),
        }
    }

    /// The whole authority log from its start, keeping the entries `keep`
    /// selects, up to [`MAX_HELD_AUTHORITY_BYTES`] of them.
    async fn whole_authority(&self, r: &str, keep: impl Fn(&[u8]) -> bool) -> Result<Vec<Vec<u8>>> {
        let mut reader = LogReader::new(self, r, "authority", None);
        let (mut log, mut held) = (Vec::new(), 0);
        while let Some(page) = reader.next().await? {
            for (_, bytes) in page.entries {
                if !keep(&bytes) {
                    continue;
                }
                held += bytes.len();
                if held > MAX_HELD_AUTHORITY_BYTES {
                    return Err(retry("the relay's authority log is too large"));
                }
                log.push(bytes);
            }
        }
        Ok(log)
    }

    /// Run rounds until `shutdown` resolves: on hints, local edits, every
    /// minute, and after temporary failures with backoff. A round that needs
    /// attention waits for the application to wake the client. Nothing here
    /// waits on the hints connection.
    pub async fn run(&self, shutdown: impl std::future::Future<Output = ()>) {
        tokio::pin!(shutdown);
        let hints = Arc::new(Notify::new());
        let mut backoff = Duration::from_secs(1);
        let mut hint_task: Option<tokio::task::JoinHandle<()>> = None;
        loop {
            let _ = self.status.send(Status::Syncing);
            let round = tokio::select! {
                _ = &mut shutdown => break,
                round = self.sync_once() => round,
            };
            let wait = match round {
                Ok(_) => {
                    backoff = Duration::from_secs(1);
                    let _ = self.status.send(Status::UpToDate);
                    if hint_task.as_ref().is_none_or(|t| t.is_finished()) {
                        hint_task = self.listen(hints.clone()).await;
                    }
                    Some(PERIOD)
                }
                // Another device bringing the relay up to date resolves
                // it, so keep trying, slowly.
                Err(SyncError::Attention(Attention::RelayBehind)) => {
                    let delay = backoff;
                    backoff = (backoff * 2).min(MAX_BACKOFF);
                    let _ = self
                        .status
                        .send(Status::NeedsAttention(Attention::RelayBehind));
                    Some(jitter(delay))
                }
                Err(SyncError::Attention(attention)) => {
                    let _ = self.status.send(Status::NeedsAttention(attention));
                    None
                }
                Err(SyncError::Retry { reason, after }) => {
                    let delay = after
                        .unwrap_or(backoff)
                        .max(backoff / 2)
                        .min(MAX_RETRY_AFTER);
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

    /// Start holding the hints WebSocket open, notifying on each hint. It
    /// connects on its own task, within [`HINTS_CONNECT_TIMEOUT`], and ends
    /// when the connection does (a later round reconnects). It uses the
    /// session the round left, without waiting for one: `None` if there is
    /// none to hand.
    async fn listen(&self, hints: Arc<Notify>) -> Option<tokio::task::JoinHandle<()>> {
        let route = self.on_store(|s| s.route()).await.ok()?;
        let token = (*self.token.try_lock().ok()?)?;
        let url = format!("{}/v0/routes/{}/hints", self.base, route_hex(&route));
        let (url, connector) = match self.transport {
            Transport::Tls => (
                url.replacen("https://", "wss://", 1),
                tls_config().map(|c| tokio_tungstenite::Connector::Rustls(Arc::new(c))),
            ),
            Transport::Loopback => (
                url.replacen("http://", "ws://", 1),
                Some(tokio_tungstenite::Connector::Plain),
            ),
        };
        use tokio_tungstenite::tungstenite::client::IntoClientRequest;
        let mut request = url.into_client_request().ok()?;
        request.headers_mut().insert(
            "authorization",
            format!("Bearer {}", hex(&token)).parse().ok()?,
        );
        Some(tokio::spawn(async move {
            let connect =
                tokio_tungstenite::connect_async_tls_with_config(request, None, false, connector);
            let Ok(Ok((mut socket, _))) =
                tokio::time::timeout(HINTS_CONNECT_TIMEOUT, connect).await
            else {
                return;
            };
            while let Some(Ok(_)) = socket.next().await {
                hints.notify_one();
            }
        }))
    }
}

/// One log read in order from a cursor, each page checked, at most
/// [`MAX_PAGES_PER_ROUND`] pages a round.
struct LogReader<'a, S: LocalStore + 'static> {
    client: &'a Client<S>,
    route: &'a str,
    log: &'static str,
    cursor: Option<Vec<u8>>,
    pages: usize,
    complete: bool,
}

impl<'a, S: LocalStore + 'static> LogReader<'a, S> {
    fn new(
        client: &'a Client<S>,
        route: &'a str,
        log: &'static str,
        cursor: Option<Vec<u8>>,
    ) -> Self {
        Self {
            client,
            route,
            log,
            cursor,
            pages: 0,
            complete: false,
        }
    }

    /// The next page, or `None` once a complete one was read.
    async fn next(&mut self) -> Result<Option<Page>> {
        if self.complete {
            return Ok(None);
        }
        if self.pages == MAX_PAGES_PER_ROUND {
            return Err(retry(format!(
                "the relay's {} log is longer than a round reads",
                self.log
            )));
        }
        self.pages += 1;
        let page = self
            .client
            .fetch(self.route, self.log, self.cursor.as_deref())
            .await?;
        self.cursor = Some(page.next.clone());
        self.complete = page.complete;
        Ok(Some(page))
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
            .map_err(network)?;
        let (status, body) = read_body(response).await?;
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
        // Everything but other devices' envelopes.
        let ours = |bytes: &[u8]| match authority::parse(bytes).map(|p| p.message.body) {
            Ok(Body::KeyEnvelope(k)) => k.recipient == device,
            Ok(_) => true,
            Err(_) => false,
        };
        let log = match self.client.whole_authority(&r, ours).await {
            Ok(log) => log,
            Err(SyncError::Attention(Attention::NotAMember)) => return Ok(false),
            Err(other) => return Err(other),
        };
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
    /// For the collection's admin: register an invitation's `lookup` with
    /// the relay before showing the invitation, so that its mailbox takes a
    /// joining device's request under it (#45). Registering it again is
    /// harmless.
    pub async fn register_invitation(&self, lookup: &[u8; 16]) -> Result<()> {
        let route = self.on_store(|s| s.route()).await?;
        let path = format!(
            "/v0/routes/{}/invitations/{}",
            route_hex(&route),
            hex(lookup)
        );
        let (status, body) = self
            .call(reqwest::Method::PUT, &path, None, "application/cbor")
            .await?;
        match status {
            200 | 201 => Ok(()),
            _ => Err(refusal(status, &body).into_error()),
        }
    }

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
        let log = self
            .client
            .whole_authority(&r, |bytes| authority::parse(bytes).is_ok())
            .await?;
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
