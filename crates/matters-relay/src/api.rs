//! The HTTP API of docs/SYNC_SERVICE_CONTRACT.md, v0, on axum.
//!
//! Bodies are deterministic CBOR (`application/cbor`) except record and
//! authority bytes, which travel exactly as signed. Every error is a CBOR
//! `{"code", "retry", ...}` with the contract's status. Bodies are read
//! up to the operation's limit, and a larger one is `too-large` before
//! anything else happens.

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

#[derive(Clone)]
struct Api {
    relay: Relay,
    clock: Clock,
}

/// The v0 API over `relay`, telling time with `clock`.
pub fn router(relay: Relay, clock: Clock) -> Router {
    Router::new()
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
        .fallback(unknown)
        .with_state(Api { relay, clock })
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
        RelayError::UnknownRoute => StatusCode::NOT_FOUND,
        RelayError::RecordConflict { .. } | RelayError::RouteExists => StatusCode::CONFLICT,
        RelayError::GenerationChanged { .. } => StatusCode::GONE,
        RelayError::TooLarge => StatusCode::PAYLOAD_TOO_LARGE,
        RelayError::ContextMismatch | RelayError::UnsupportedSuite => {
            StatusCode::UNPROCESSABLE_ENTITY
        }
        RelayError::QuotaExhausted => StatusCode::INSUFFICIENT_STORAGE,
        RelayError::Unavailable(_) => StatusCode::SERVICE_UNAVAILABLE,
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
    let token = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .and_then(|t| hex::<32>(t).ok())
        .ok_or(ApiError(RelayError::Unauthenticated))?;
    Ok(api.relay.authenticate(route, &token, (api.clock)()).await?)
}

async fn challenge(State(api): State<Api>, request: Request) -> Result<Response> {
    let body = read(request.into_body(), MAX_SESSION_BODY).await?;
    let req = cbor::request(&body, &["route", "device"]).ok_or_else(malformed)?;
    let route = body_route(req.route("route").ok_or_else(malformed)?);
    let device: Hash = req.bytes("device").ok_or_else(malformed)?;
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
    let body = read(request.into_body(), MAX_WIRE_BYTES).await?;
    let device = authenticated(&api, &route, &headers).await?;
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
    let body = read(request.into_body(), MAX_AUTHORITY_BODY).await?;
    authenticated(&api, &route, &headers).await?;
    Ok(receipt(
        api.relay.put_authority(&route, &digest, &body).await?,
    ))
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
    authenticated(&api, &route, &headers).await?;
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
    authenticated(&api, &route, &headers).await?;
    Ok(page_response(
        api.relay
            .authority_objects(&route, after.as_ref(), limit)
            .await?,
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
