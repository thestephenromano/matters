//! The HTTP API of docs/SYNC_SERVICE_CONTRACT.md, v0, on axum.
//!
//! Bodies are deterministic CBOR (`application/cbor`) except record and
//! authority bytes, which travel exactly as signed. Every error is a CBOR
//! `{"code", "retry", ...}` with the contract's status. A request that
//! needs a session is authenticated from its headers before its body is
//! read. Bodies are read up to the operation's limit, and a larger one is
//! `too-large` before anything else happens to it.

use std::sync::Arc;

use axum::body::{Body, Bytes};
use axum::extract::{Path, RawQuery, Request, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{post, put};
use axum::Router;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use matters_sync::authority::{Hash, Route};
use matters_sync::wire::{MAX_WIRE_BYTES, SIGNATURE_BYTES};

use crate::cbor::{self, Value};
use crate::{Appended, Cursor, Page, Relay, RelayError, MAX_PAGE};

/// Session request bodies are small; this bounds them generously.
const MAX_SESSION_BODY: usize = 8 * 1024;
const MAX_AUTHORITY_BODY: usize = matters_sync::authority::MAX_CONTROL_BYTES;
pub const CBOR: &str = "application/cbor";
pub const RECORD: &str = "application/vnd.matters.sync-record";
pub const AUTHORITY: &str = "application/vnd.matters.sync-authority";

/// Milliseconds since the Unix epoch; replaceable for tests.
pub type Clock = Arc<dyn Fn() -> u64 + Send + Sync>;

pub fn system_clock() -> Clock {
    Arc::new(|| {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
            .unwrap_or(0)
    })
}

/// A rate limit per route and device: `per_second` sustained, `burst`
/// at once.
#[derive(Debug, Clone, Copy)]
pub struct RateLimit {
    pub per_second: u32,
    pub burst: u32,
}

/// The API's operator configuration.
#[derive(Clone, Default)]
pub struct Options {
    /// Commit hints; without them the hints endpoint answers `unavailable`.
    pub hints: Option<crate::hints::Hints>,
    /// Authenticated requests, per route and device.
    pub rate_limit: Option<RateLimit>,
    /// Requests without a session, per route: session challenges and
    /// answers, mailbox uploads and session-free transitions. They are
    /// limited by route because nothing else identifies them: behind a
    /// proxy every peer address is the proxy's, and the relay trusts no
    /// forwarded header.
    pub public_rate_limit: Option<RateLimit>,
    /// Session challenges, per route and claimed device, beside the route's
    /// own limit.
    pub challenge_rate_limit: Option<RateLimit>,
    /// The operator credential for export, compared as its SHA-384.
    pub operator_token: Option<[u8; 32]>,
    /// Where requests are counted, when metrics are served.
    pub metrics: Option<Arc<crate::metrics::Metrics>>,
}

/// Token buckets by key: what is limited (a tag, the route, maybe a
/// device), with each bucket's tokens, last refill and limit.
type Buckets = Arc<std::sync::Mutex<std::collections::HashMap<Vec<u8>, Bucket>>>;

#[derive(Clone, Copy)]
struct Bucket {
    tokens: f64,
    last: u64,
    limit: RateLimit,
}

impl Bucket {
    fn refill(&mut self, now: u64) {
        let burst = f64::from(self.limit.burst.max(1));
        let rate = f64::from(self.limit.per_second.max(1));
        let elapsed = now.saturating_sub(self.last) as f64 / 1000.0;
        self.tokens = (self.tokens + elapsed * rate).min(burst);
        self.last = now;
    }

    fn full(&self) -> bool {
        self.tokens >= f64::from(self.limit.burst.max(1))
    }
}

/// The most buckets kept at once. Keys without a session name any route,
/// provisioned or not, so idle buckets (refilled to their burst, the same
/// as no bucket) are dropped when the map is full, and while it stays full
/// of active ones, new keys wait.
const MAX_BUCKETS: usize = 65_536;

/// What a bucket limits.
#[derive(Clone, Copy)]
enum Limited {
    Session,
    Public,
    Challenge,
}

fn bucket_key(what: Limited, route: &Route, device: Option<&Hash>) -> Vec<u8> {
    let tag = match what {
        Limited::Session => 0,
        Limited::Public => 1,
        Limited::Challenge => 2,
    };
    let mut key = Vec::with_capacity(1 + 48 + 48);
    key.push(tag);
    key.extend_from_slice(&route.application);
    key.extend_from_slice(&route.tenant);
    key.extend_from_slice(&route.collection);
    if let Some(device) = device {
        key.extend_from_slice(device);
    }
    key
}

/// Open hint sockets by route row and device.
type Sockets = Arc<std::sync::Mutex<std::collections::HashMap<(i64, Hash), usize>>>;

/// The most hint sockets one device keeps open on one route. A client
/// needs one; the rest allow for reconnects the relay has not yet seen
/// close.
pub const MAX_HINT_SOCKETS: usize = 4;
/// How often an idle hint socket checks its session and pings its client.
const HINT_CHECK: std::time::Duration = std::time::Duration::from_secs(30);

#[derive(Clone)]
struct Api {
    relay: Relay,
    clock: Clock,
    options: Options,
    buckets: Buckets,
    sockets: Sockets,
}

/// A device's place among a route's hint sockets, given back on drop.
struct SocketSlot {
    sockets: Sockets,
    key: (i64, Hash),
}

impl SocketSlot {
    fn take(sockets: &Sockets, key: (i64, Hash)) -> Option<Self> {
        let mut open = sockets.lock().unwrap_or_else(|e| e.into_inner());
        let count = open.entry(key).or_insert(0);
        if *count >= MAX_HINT_SOCKETS {
            return None;
        }
        *count += 1;
        Some(Self {
            sockets: sockets.clone(),
            key,
        })
    }
}

impl Drop for SocketSlot {
    fn drop(&mut self) {
        let mut open = self.sockets.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(count) = open.get_mut(&self.key) {
            *count -= 1;
            if *count == 0 {
                open.remove(&self.key);
            }
        }
    }
}

/// The v0 API over `relay`, telling time with `clock`, with no hints, rate
/// limit or operator access.
pub fn router(relay: Relay, clock: Clock) -> Router {
    router_with(relay, clock, Options::default())
}

/// The v0 API with `options`.
pub fn router_with(relay: Relay, clock: Clock, options: Options) -> Router {
    let metrics = options.metrics.clone();
    let router = Router::new()
        .route("/v0/session/challenge", post(challenge))
        .route("/v0/session", post(session))
        .route("/v0/routes/{route}/records/{record}", put(put_record))
        .route(
            "/v0/routes/{route}/records",
            axum::routing::get(get_records),
        )
        .route("/v0/routes/{route}/authority/{digest}", put(put_authority))
        .route(
            "/v0/routes/{route}/authority",
            axum::routing::get(get_authority),
        )
        .route(
            "/v0/routes/{route}/enrollments/{lookup}",
            put(put_enrollment).get(get_enrollment),
        )
        .route(
            "/v0/routes/{route}/invitations/{lookup}",
            put(put_invitation),
        )
        .route(
            "/v0/routes/{route}/snapshots",
            axum::routing::get(get_snapshots),
        )
        .route("/v0/routes/{route}/hints", axum::routing::get(hints))
        .route("/v0/routes/{route}/export", axum::routing::get(export))
        .route("/healthz", axum::routing::get(healthz))
        .route("/readyz", axum::routing::get(readyz))
        .fallback(unknown)
        .with_state(Api {
            relay,
            clock,
            options,
            buckets: Arc::default(),
            sockets: Arc::default(),
        });
    match metrics {
        Some(metrics) => router.layer(axum::middleware::from_fn_with_state(
            metrics,
            crate::metrics::track,
        )),
        None => router,
    }
}

/// `GET /healthz`: the process is up and answering.
async fn healthz() -> &'static str {
    "ok"
}

/// `GET /readyz`: the relay can serve, its database answering.
async fn readyz(State(api): State<Api>) -> (StatusCode, &'static str) {
    if api.relay.ready().await {
        (StatusCode::OK, "ready")
    } else {
        (StatusCode::SERVICE_UNAVAILABLE, "database unavailable")
    }
}

/// An error response in the contract's shape.
pub struct ApiError(RelayError);

impl From<RelayError> for ApiError {
    fn from(error: RelayError) -> Self {
        Self(error)
    }
}

pub fn status(error: &RelayError) -> StatusCode {
    match error {
        RelayError::Malformed | RelayError::Authority(_) => StatusCode::BAD_REQUEST,
        RelayError::Unauthenticated | RelayError::SessionExpired => StatusCode::UNAUTHORIZED,
        RelayError::NotAMember | RelayError::Revoked => StatusCode::FORBIDDEN,
        RelayError::UnknownRoute | RelayError::UnknownInvitation => StatusCode::NOT_FOUND,
        RelayError::RecordConflict { .. } | RelayError::RouteExists => StatusCode::CONFLICT,
        RelayError::GenerationChanged { .. } => StatusCode::GONE,
        RelayError::TooLarge => StatusCode::PAYLOAD_TOO_LARGE,
        RelayError::ContextMismatch | RelayError::UnsupportedSuite => {
            StatusCode::UNPROCESSABLE_ENTITY
        }
        RelayError::QuotaExhausted => StatusCode::INSUFFICIENT_STORAGE,
        RelayError::RateLimited { .. } => StatusCode::TOO_MANY_REQUESTS,
        RelayError::Unavailable(_) | RelayError::Entropy => StatusCode::SERVICE_UNAVAILABLE,
    }
}

fn error_body(code: &str, retry: bool, error: Option<&RelayError>) -> Vec<u8> {
    let mut fields = vec![("code", Value::Text(code)), ("retry", Value::Bool(retry))];
    match error {
        Some(RelayError::RecordConflict { digest }) => {
            fields.push(("digest", Value::Bytes(digest)))
        }
        Some(RelayError::GenerationChanged { generation }) => {
            fields.push(("generation", Value::Bytes(generation)))
        }
        Some(RelayError::RateLimited { retry_after_ms }) => {
            fields.push(("retry-after-ms", Value::Uint(*retry_after_ms)))
        }
        _ => {}
    }
    cbor::map(fields)
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let body = error_body(self.0.code(), self.0.retry(), Some(&self.0));
        (status(&self.0), [(header::CONTENT_TYPE, CBOR)], body).into_response()
    }
}

type Result<T> = std::result::Result<T, ApiError>;

fn malformed() -> ApiError {
    ApiError(RelayError::Malformed)
}

async fn read(body: Body, max: usize) -> Result<Bytes> {
    axum::body::to_bytes(body, max)
        .await
        .map_err(|_| ApiError(RelayError::TooLarge))
}

fn cbor_response(status: StatusCode, body: Vec<u8>) -> Response {
    (status, [(header::CONTENT_TYPE, CBOR)], body).into_response()
}

fn hex<const N: usize>(text: &str) -> Result<[u8; N]> {
    if text.len() != 2 * N
        || !text
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err(malformed());
    }
    let mut out = [0; N];
    for (i, byte) in out.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&text[2 * i..2 * i + 2], 16).map_err(|_| malformed())?;
    }
    Ok(out)
}

/// A route in a path: 96 lowercase hex digits.
fn path_route(text: &str) -> Result<Route> {
    let bytes: [u8; 48] = hex(text)?;
    let mut route = Route {
        application: [0; 16],
        tenant: [0; 16],
        collection: [0; 16],
    };
    route.application.copy_from_slice(&bytes[..16]);
    route.tenant.copy_from_slice(&bytes[16..32]);
    route.collection.copy_from_slice(&bytes[32..]);
    Ok(route)
}

fn body_route(parts: [[u8; 16]; 3]) -> Route {
    Route {
        application: parts[0],
        tenant: parts[1],
        collection: parts[2],
    }
}

async fn authenticated(api: &Api, route: &Route, headers: &HeaderMap) -> Result<Hash> {
    authenticated_as(api, route, headers, false).await
}

/// The bearer token of a request.
fn bearer(headers: &HeaderMap) -> Result<[u8; 32]> {
    headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .and_then(|t| hex::<32>(t).ok())
        .ok_or(ApiError(RelayError::Unauthenticated))
}

/// As [`authenticated`], also accepting a recovering device's root session.
async fn authenticated_as(
    api: &Api,
    route: &Route,
    headers: &HeaderMap,
    root_allowed: bool,
) -> Result<Hash> {
    let token = bearer(headers)?;
    let now = (api.clock)();
    let device = api
        .relay
        .authenticate_as(route, &token, now, root_allowed)
        .await?;
    if let Some(limit) = api.options.rate_limit {
        admit(
            &api.buckets,
            limit,
            bucket_key(Limited::Session, route, Some(&device)),
            now,
        )?;
    }
    Ok(device)
}

/// Take one request without a session for `route` (and, for a challenge,
/// for the device it names) from its buckets, or say when to retry.
fn admit_public(api: &Api, route: &Route, challenged: Option<&Hash>) -> Result<()> {
    let now = (api.clock)();
    if let Some(limit) = api.options.public_rate_limit {
        admit(
            &api.buckets,
            limit,
            bucket_key(Limited::Public, route, None),
            now,
        )?;
    }
    if let (Some(limit), Some(device)) = (api.options.challenge_rate_limit, challenged) {
        admit(
            &api.buckets,
            limit,
            bucket_key(Limited::Challenge, route, Some(device)),
            now,
        )?;
    }
    Ok(())
}

/// Take one request from the token bucket under `key`, or say when to
/// retry.
fn admit(buckets: &Buckets, limit: RateLimit, key: Vec<u8>, now: u64) -> Result<()> {
    let mut buckets = buckets.lock().unwrap_or_else(|e| e.into_inner());
    if buckets.len() >= MAX_BUCKETS && !buckets.contains_key(&key) {
        buckets.retain(|_, b| {
            b.refill(now);
            !b.full()
        });
        if buckets.len() >= MAX_BUCKETS {
            return Err(ApiError(RelayError::RateLimited {
                retry_after_ms: 1000,
            }));
        }
    }
    let bucket = buckets.entry(key).or_insert(Bucket {
        tokens: f64::from(limit.burst.max(1)),
        last: now,
        limit,
    });
    bucket.limit = limit;
    bucket.refill(now);
    if bucket.tokens >= 1.0 {
        bucket.tokens -= 1.0;
        return Ok(());
    }
    let rate = f64::from(limit.per_second.max(1));
    let retry_after_ms = ((1.0 - bucket.tokens) / rate * 1000.0).ceil() as u64;
    Err(ApiError(RelayError::RateLimited {
        retry_after_ms: retry_after_ms.max(1),
    }))
}

async fn challenge(State(api): State<Api>, request: Request) -> Result<Response> {
    let body = read(request.into_body(), MAX_SESSION_BODY).await?;
    let req = cbor::request(&body, &["route", "device"]).ok_or_else(malformed)?;
    let route = body_route(req.route("route").ok_or_else(malformed)?);
    let device: Hash = req.bytes("device").ok_or_else(malformed)?;
    admit_public(&api, &route, Some(&device))?;
    let (challenge, expires) = api.relay.challenge(&route, &device, (api.clock)()).await?;
    Ok(cbor_response(
        StatusCode::OK,
        cbor::map(vec![
            ("challenge", Value::Bytes(&challenge)),
            ("expires", Value::Uint(expires)),
        ]),
    ))
}

async fn session(State(api): State<Api>, request: Request) -> Result<Response> {
    let body = read(request.into_body(), MAX_SESSION_BODY).await?;
    let req = cbor::request(&body, &["route", "device", "challenge", "signature"])
        .ok_or_else(malformed)?;
    let route = body_route(req.route("route").ok_or_else(malformed)?);
    let device: Hash = req.bytes("device").ok_or_else(malformed)?;
    let challenge: [u8; 32] = req.bytes("challenge").ok_or_else(malformed)?;
    let signature: [u8; SIGNATURE_BYTES] = req.bytes("signature").ok_or_else(malformed)?;
    admit_public(&api, &route, None)?;
    let s = api
        .relay
        .open_session(&route, &device, &challenge, &signature, (api.clock)())
        .await?;
    Ok(cbor_response(
        StatusCode::OK,
        cbor::map(vec![
            ("token", Value::Bytes(&s.token)),
            ("expires", Value::Uint(s.expires)),
            ("generation", Value::Bytes(&s.generation)),
        ]),
    ))
}

fn receipt(appended: Appended) -> Response {
    let (status, r) = match &appended {
        Appended::Stored(r) => (StatusCode::CREATED, r),
        Appended::Existing(r) => (StatusCode::OK, r),
    };
    cbor_response(
        status,
        cbor::map(vec![
            ("sequence", Value::Uint(r.sequence)),
            ("generation", Value::Bytes(&r.generation)),
            ("digest", Value::Bytes(&r.digest)),
        ]),
    )
}

async fn put_record(
    State(api): State<Api>,
    Path((route, record)): Path<(String, String)>,
    headers: HeaderMap,
    request: Request,
) -> Result<Response> {
    let route = path_route(&route)?;
    let record: [u8; 32] = hex(&record)?;
    // The session first: nobody without one makes the relay read a body.
    let device = authenticated(&api, &route, &headers).await?;
    let body = read(request.into_body(), MAX_WIRE_BYTES).await?;
    Ok(receipt(
        api.relay.append(&route, &device, &record, &body).await?,
    ))
}

async fn put_authority(
    State(api): State<Api>,
    Path((route, digest)): Path<(String, String)>,
    headers: HeaderMap,
    request: Request,
) -> Result<Response> {
    let route = path_route(&route)?;
    let digest: Hash = hex(&digest)?;
    // With a session, it is checked before the body is read. Without one,
    // only membership transitions and root recoveries are taken: their
    // signatures say who may publish them, and a device a restored relay no
    // longer knows re-publishes the transition that admitted it (#45).
    let device = if headers.contains_key(header::AUTHORIZATION) {
        Some(authenticated_as(&api, &route, &headers, true).await?)
    } else {
        admit_public(&api, &route, None)?;
        None
    };
    let body = read(request.into_body(), MAX_AUTHORITY_BODY).await?;
    // Who may publish what is checked with the object, in the transaction
    // that stores it (Relay::put_authority_as).
    Ok(receipt(
        api.relay
            .put_authority_as(&route, device.as_ref(), &digest, &body)
            .await?,
    ))
}

/// `PUT /v0/routes/{route}/invitations/{lookup}`: the route's admin
/// registers an invitation before showing it (#45), so the mailbox takes a
/// request under its lookup. No body.
async fn put_invitation(
    State(api): State<Api>,
    Path((route, lookup)): Path<(String, String)>,
    headers: HeaderMap,
) -> Result<Response> {
    let route = path_route(&route)?;
    let lookup: [u8; 16] = hex(&lookup)?;
    let device = authenticated(&api, &route, &headers).await?;
    let created = api
        .relay
        .register_invitation(&route, &device, &lookup)
        .await?;
    let status = if created {
        StatusCode::CREATED
    } else {
        StatusCode::OK
    };
    Ok(status.into_response())
}

/// `PUT /v0/routes/{route}/enrollments/{lookup}`: a joining device leaves
/// its enrollment request (#42). No session: it is not a member yet, but
/// the lookup must be a registered invitation's.
async fn put_enrollment(
    State(api): State<Api>,
    Path((route, lookup)): Path<(String, String)>,
    request: Request,
) -> Result<Response> {
    let route = path_route(&route)?;
    let lookup: [u8; 16] = hex(&lookup)?;
    admit_public(&api, &route, None)?;
    let body = read(request.into_body(), MAX_AUTHORITY_BODY).await?;
    let created = api.relay.put_enrollment(&route, &lookup, &body).await?;
    let status = if created {
        StatusCode::CREATED
    } else {
        StatusCode::OK
    };
    Ok(status.into_response())
}

/// `GET /v0/routes/{route}/enrollments/{lookup}`: the route's admin
/// collects a request. Anyone else is told there is none.
async fn get_enrollment(
    State(api): State<Api>,
    Path((route, lookup)): Path<(String, String)>,
    headers: HeaderMap,
) -> Result<Response> {
    let route = path_route(&route)?;
    let lookup: [u8; 16] = hex(&lookup)?;
    let device = authenticated(&api, &route, &headers).await?;
    match api.relay.enrollment(&route, &device, &lookup).await? {
        Some(bytes) => Ok((
            StatusCode::OK,
            [(
                header::CONTENT_TYPE,
                "application/vnd.matters.sync-authority",
            )],
            bytes,
        )
            .into_response()),
        None => Ok(cbor_response(
            StatusCode::NOT_FOUND,
            error_body("malformed", false, None),
        )),
    }
}

/// `after` (base64url of the cursor) and `limit` (1..=256) from a query.
fn page_query(query: Option<String>) -> Result<(Option<Cursor>, u32)> {
    let mut after = None;
    let mut limit = None;
    for pair in query
        .as_deref()
        .unwrap_or("")
        .split('&')
        .filter(|p| !p.is_empty())
    {
        let (key, value) = pair.split_once('=').ok_or_else(malformed)?;
        match key {
            "after" if after.is_none() => {
                let bytes = URL_SAFE_NO_PAD.decode(value).map_err(|_| malformed())?;
                after = Some(Cursor::decode(&bytes)?);
            }
            "limit" if limit.is_none() => {
                limit = Some(value.parse::<u32>().map_err(|_| malformed())?);
            }
            _ => return Err(malformed()),
        }
    }
    let limit = limit.unwrap_or(MAX_PAGE);
    Ok((after, limit))
}

fn page_response(page: Page) -> Response {
    // The body carries the cursor's bytes; a query carries their base64url.
    let next = page.next.encode();
    cbor_response(
        StatusCode::OK,
        cbor::map(vec![
            ("generation", Value::Bytes(&page.generation)),
            ("high-water", Value::Uint(page.high_water)),
            ("records", Value::Entries(&page.entries)),
            ("next", Value::Bytes(&next)),
            ("complete", Value::Bool(page.complete)),
        ]),
    )
}

async fn get_records(
    State(api): State<Api>,
    Path(route): Path<String>,
    RawQuery(query): RawQuery,
    headers: HeaderMap,
) -> Result<Response> {
    let route = path_route(&route)?;
    let (after, limit) = page_query(query)?;
    // The root's session reads records too: a recovery closes earlier
    // generations' writers at what the recovering device holds. Whoever
    // holds it has the recovery words, and so every key anyway.
    authenticated_as(&api, &route, &headers, true).await?;
    Ok(page_response(
        api.relay.records(&route, after.as_ref(), limit).await?,
    ))
}

async fn get_authority(
    State(api): State<Api>,
    Path(route): Path<String>,
    RawQuery(query): RawQuery,
    headers: HeaderMap,
) -> Result<Response> {
    let route = path_route(&route)?;
    let (after, limit) = page_query(query)?;
    authenticated_as(&api, &route, &headers, true).await?;
    Ok(page_response(
        api.relay
            .authority_objects(&route, after.as_ref(), limit)
            .await?,
    ))
}

/// `GET /v0/routes/{route}/snapshots?limit=`: the newest checkpoint parts,
/// newest first, so a new device can start from a checkpoint (#40).
async fn get_snapshots(
    State(api): State<Api>,
    Path(route): Path<String>,
    RawQuery(query): RawQuery,
    headers: HeaderMap,
) -> Result<Response> {
    let route = path_route(&route)?;
    let mut limit = None;
    for pair in query
        .as_deref()
        .unwrap_or("")
        .split('&')
        .filter(|p| !p.is_empty())
    {
        match pair.split_once('=') {
            Some(("limit", value)) if limit.is_none() => {
                limit = Some(value.parse::<u32>().map_err(|_| malformed())?);
            }
            _ => return Err(malformed()),
        }
    }
    // As for records, the root's session may read them.
    authenticated_as(&api, &route, &headers, true).await?;
    let found = api
        .relay
        .snapshots(&route, limit.unwrap_or(MAX_PAGE))
        .await?;
    Ok(cbor_response(
        StatusCode::OK,
        cbor::map(vec![
            ("generation", Value::Bytes(&found.generation)),
            ("records", Value::Entries(&found.entries)),
        ]),
    ))
}

async fn unknown(request: Request) -> Response {
    let code = if request.uri().path().starts_with("/v0/") {
        "malformed"
    } else {
        "unsupported-version"
    };
    cbor_response(StatusCode::NOT_FOUND, error_body(code, false, None))
}

/// `GET /v0/routes/{route}/hints`: a WebSocket on which the relay sends
/// `{"sequence", "generation"}` for each committed record of the route.
///
/// The socket lasts as long as its session: before each hint, every
/// [`HINT_CHECK`] while idle, and when the session expires, the session is
/// checked again (still unexpired, the device still a member), and the
/// socket closes with a policy close once it fails. Hints already waiting
/// are coalesced into the newest, so a burst costs one check. A device
/// keeps at most [`MAX_HINT_SOCKETS`] open on a route.
async fn hints(
    State(api): State<Api>,
    Path(route): Path<String>,
    headers: HeaderMap,
    upgrade: axum::extract::ws::WebSocketUpgrade,
) -> Result<Response> {
    let route = path_route(&route)?;
    let token = bearer(&headers)?;
    authenticated(&api, &route, &headers).await?;
    let (device, expires) = api
        .relay
        .session_of(&route, &token, (api.clock)(), false)
        .await?;
    let Some(hints) = api.options.hints.clone() else {
        return Err(ApiError(RelayError::Unavailable(sqlx::Error::PoolClosed)));
    };
    let id = api.relay.route_id(&route).await?;
    let slot =
        SocketSlot::take(&api.sockets, (id, device)).ok_or(ApiError(RelayError::RateLimited {
            retry_after_ms: HINT_CHECK.as_millis() as u64,
        }))?;
    let mut receiver = hints.subscribe();
    Ok(upgrade.on_upgrade(move |mut socket| async move {
        use axum::extract::ws::{close_code, CloseFrame, Message};
        use tokio::sync::broadcast::error::{RecvError, TryRecvError};
        let _slot = slot;
        // Whether the session still holds; the reason to close if not.
        let check = || async {
            api.relay
                .session_of(&route, &token, (api.clock)(), false)
                .await
                .err()
        };
        let until = |expires: u64| {
            let left = expires.saturating_sub((api.clock)()).max(1000);
            tokio::time::Instant::now() + std::time::Duration::from_millis(left)
        };
        let mut expiry = until(expires);
        let mut idle = tokio::time::interval(HINT_CHECK);
        idle.tick().await;
        let refused = loop {
            tokio::select! {
                received = receiver.recv() => {
                    let mut newest = match received {
                        Ok(hint) if hint.route == id => hint,
                        Ok(_) | Err(RecvError::Lagged(_)) => continue,
                        Err(RecvError::Closed) => break None,
                    };
                    loop {
                        match receiver.try_recv() {
                            Ok(hint) if hint.route == id => newest = hint,
                            Ok(_) | Err(TryRecvError::Lagged(_)) => {}
                            Err(_) => break,
                        }
                    }
                    if let Some(error) = check().await {
                        break Some(error);
                    }
                    let body = cbor::map(vec![
                        ("sequence", Value::Uint(newest.sequence)),
                        ("generation", Value::Bytes(&newest.generation)),
                    ]);
                    if socket.send(Message::Binary(body.into())).await.is_err() {
                        return;
                    }
                }
                incoming = socket.recv() => match incoming {
                    // The client speaks only to close; pings are answered
                    // by the socket itself.
                    Some(Ok(Message::Close(_))) | Some(Err(_)) | None => return,
                    Some(Ok(_)) => {}
                },
                _ = idle.tick() => {
                    if let Some(error) = check().await {
                        break Some(error);
                    }
                    if socket.send(Message::Ping(Vec::new().into())).await.is_err() {
                        return;
                    }
                }
                _ = tokio::time::sleep_until(expiry) => {
                    if let Some(error) = check().await {
                        break Some(error);
                    }
                    expiry = until(expires);
                }
            }
        };
        if let Some(error) = refused {
            let _ = socket
                .send(Message::Close(Some(CloseFrame {
                    code: close_code::POLICY,
                    reason: error.code().into(),
                })))
                .await;
        }
    }))
}

/// `GET /v0/routes/{route}/export`, with the operator credential.
async fn export(
    State(api): State<Api>,
    Path(route): Path<String>,
    headers: HeaderMap,
) -> Result<Response> {
    let route = path_route(&route)?;
    let presented = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .and_then(|t| hex::<32>(t).ok());
    let authorized = match (api.options.operator_token, presented) {
        (Some(expected), Some(presented)) => {
            use matters_sync::record::record_hash;
            record_hash(&expected) == record_hash(&presented)
        }
        _ => false,
    };
    if !authorized {
        return Err(ApiError(RelayError::Unauthenticated));
    }
    let bytes = api.relay.export(&route).await?;
    Ok(cbor_response(StatusCode::OK, bytes))
}

#[cfg(test)]
mod tests {
    use super::{admit, Buckets, RateLimit, MAX_BUCKETS};

    #[test]
    fn buckets_are_bounded_and_idle_ones_make_room() {
        let buckets = Buckets::default();
        let limit = RateLimit {
            per_second: 1,
            burst: 1,
        };
        let key = |n: usize| n.to_le_bytes().to_vec();
        for n in 0..MAX_BUCKETS {
            assert!(admit(&buckets, limit, key(n), 0).is_ok());
        }
        // Full of buckets in use: a new key waits.
        assert!(admit(&buckets, limit, key(MAX_BUCKETS), 0).is_err());
        assert!(admit(&buckets, limit, key(0), 0).is_err(), "still limited");
        // A second later they have refilled, the same as no bucket.
        assert!(admit(&buckets, limit, key(MAX_BUCKETS), 1000).is_ok());
        assert_eq!(buckets.lock().unwrap().len(), 1);
    }
}
