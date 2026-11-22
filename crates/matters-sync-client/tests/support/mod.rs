//! A scripted relay for the client's tests (#45): it keeps the contract's
//! shapes (sessions, paged logs, receipts) in memory, and misbehaves as a
//! test asks, which the real relay never does.

#![allow(dead_code)]

use std::convert::Infallible;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use axum::body::{Body, Bytes};
use axum::extract::{Path, RawQuery, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post, put};
use axum::Router;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use matters_core::storage::membership::{MemoryKeyStore, SyncStanding};
use matters_core::Database;
use matters_sync::authority::{parse, Body as Control, Route};
use minicbor::{Decoder, Encoder};

pub const TOKEN: [u8; 32] = [9; 32];

pub fn route() -> Route {
    Route {
        application: [1; 16],
        tenant: [2; 16],
        collection: [3; 16],
    }
}

/// How the challenge is answered.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Challenge {
    #[default]
    Open,
    /// `403 not-a-member`, always.
    Forbidden,
    /// `403 not-a-member` until a transition is published without a
    /// session: a relay restored from a backup older than the device.
    ForbiddenUntilPublished,
}

/// What a record page's body is.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum RecordsBody {
    #[default]
    Page,
    /// Zeros streamed without end, and no length.
    Endless,
    /// A body of this many bytes, with its length declared.
    Declared(usize),
}

#[derive(Debug, Clone, Default)]
pub struct Behaviour {
    pub challenge: Challenge,
    /// `429` with this `retry-after-ms` for every challenge.
    pub rate_limited_ms: Option<u64>,
    /// The status for an authority PUT without a session (default `401`,
    /// as a relay that takes none answers).
    pub unauthenticated_put: Option<u16>,
    /// `400 malformed` for every authenticated authority PUT.
    pub refuse_controls: bool,
    /// The authority log answers `complete: false` and the same cursor.
    pub stuck_authority: bool,
    pub records_body: RecordsBody,
    /// The hints WebSocket's upgrade is never answered.
    pub stall_hints: bool,
}

#[derive(Default)]
pub struct Mock {
    pub generation: Mutex<[u8; 16]>,
    pub authority: Mutex<Vec<Vec<u8>>>,
    pub records: Mutex<Vec<Vec<u8>>>,
    pub behaviour: Mutex<Behaviour>,
    /// `"GET authority"`, `"GET records"`, ... in order.
    pub requests: Mutex<Vec<String>>,
    pub unauthenticated_puts: Mutex<Vec<Vec<u8>>>,
    pub control_puts: Mutex<Vec<Vec<u8>>>,
    pub record_puts: AtomicUsize,
    published: AtomicBool,
}

impl Mock {
    pub fn behave(&self, f: impl FnOnce(&mut Behaviour)) {
        f(&mut self.behaviour.lock().unwrap());
    }

    fn behaviour(&self) -> Behaviour {
        self.behaviour.lock().unwrap().clone()
    }

    pub fn count(&self, request: &str) -> usize {
        self.requests
            .lock()
            .unwrap()
            .iter()
            .filter(|r| *r == request)
            .count()
    }

    fn note(&self, request: &str) {
        self.requests.lock().unwrap().push(request.to_owned());
    }

    /// A restore: a new generation, so old cursors get `410`.
    pub fn new_generation(&self) {
        let mut generation = self.generation.lock().unwrap();
        generation[0] = generation[0].wrapping_add(1);
    }
}

fn cbor(build: impl FnOnce(&mut Encoder<&mut Vec<u8>>)) -> Vec<u8> {
    let mut out = Vec::new();
    build(&mut Encoder::new(&mut out));
    out
}

fn respond(status: StatusCode, body: Vec<u8>) -> Response {
    (status, [("content-type", "application/cbor")], body).into_response()
}

fn error(status: StatusCode, code: &str) -> Response {
    respond(
        status,
        cbor(|e| {
            e.map(2).unwrap().str("code").unwrap().str(code).unwrap();
            e.str("retry").unwrap().bool(false).unwrap();
        }),
    )
}

fn authenticated(headers: &HeaderMap) -> bool {
    let expected = format!(
        "Bearer {}",
        TOKEN.iter().map(|b| format!("{b:02x}")).collect::<String>()
    );
    headers
        .get("authorization")
        .is_some_and(|v| v.as_bytes() == expected.as_bytes())
}

pub fn cursor(generation: &[u8; 16], last: u64) -> Vec<u8> {
    cbor(|e| {
        e.array(2)
            .unwrap()
            .bytes(generation)
            .unwrap()
            .u64(last)
            .unwrap();
    })
}

fn after(query: Option<&str>) -> Option<([u8; 16], u64)> {
    let value = query?
        .split('&')
        .find_map(|pair| pair.strip_prefix("after="))?;
    let bytes = URL_SAFE_NO_PAD.decode(value).ok()?;
    let mut d = Decoder::new(&bytes);
    d.array().ok()?;
    let generation = d.bytes().ok()?.try_into().ok()?;
    Some((generation, d.u64().ok()?))
}

fn page(log: &[Vec<u8>], generation: [u8; 16], query: Option<&str>) -> Response {
    let after = after(query);
    if after.is_some_and(|(g, _)| g != generation) {
        return error(StatusCode::GONE, "generation-changed");
    }
    let last = after.map_or(0, |(_, l)| l);
    let high = log.len() as u64;
    let entries: Vec<(u64, &[u8])> = (last + 1..=high)
        .take(256)
        .map(|s| (s, log[s as usize - 1].as_slice()))
        .collect();
    // As the relay does: a cursor beyond the log reaches no further.
    let reached = entries.last().map_or(last, |(s, _)| *s);
    page_body(
        generation,
        high,
        &entries,
        &cursor(&generation, reached),
        reached >= high,
    )
}

fn page_body(
    generation: [u8; 16],
    high_water: u64,
    entries: &[(u64, &[u8])],
    next: &[u8],
    complete: bool,
) -> Response {
    respond(
        StatusCode::OK,
        cbor(|e| {
            e.map(5).unwrap();
            e.str("generation").unwrap().bytes(&generation).unwrap();
            e.str("high-water").unwrap().u64(high_water).unwrap();
            e.str("records")
                .unwrap()
                .array(entries.len() as u64)
                .unwrap();
            for (sequence, bytes) in entries {
                e.array(2)
                    .unwrap()
                    .u64(*sequence)
                    .unwrap()
                    .bytes(bytes)
                    .unwrap();
            }
            e.str("next").unwrap().bytes(next).unwrap();
            e.str("complete").unwrap().bool(complete).unwrap();
        }),
    )
}

type Shared = State<Arc<Mock>>;

async fn challenge(State(mock): Shared) -> Response {
    mock.note("POST challenge");
    let behaviour = mock.behaviour();
    if let Some(ms) = behaviour.rate_limited_ms {
        return respond(
            StatusCode::TOO_MANY_REQUESTS,
            cbor(|e| {
                e.map(3)
                    .unwrap()
                    .str("code")
                    .unwrap()
                    .str("rate-limited")
                    .unwrap();
                e.str("retry").unwrap().bool(true).unwrap();
                e.str("retry-after-ms").unwrap().u64(ms).unwrap();
            }),
        );
    }
    let forbidden = match behaviour.challenge {
        Challenge::Open => false,
        Challenge::Forbidden => true,
        Challenge::ForbiddenUntilPublished => !mock.published.load(Ordering::SeqCst),
    };
    if forbidden {
        return error(StatusCode::FORBIDDEN, "not-a-member");
    }
    respond(
        StatusCode::OK,
        cbor(|e| {
            e.map(1)
                .unwrap()
                .str("challenge")
                .unwrap()
                .bytes(&[1; 32])
                .unwrap();
        }),
    )
}

async fn session(State(mock): Shared) -> Response {
    mock.note("POST session");
    respond(
        StatusCode::OK,
        cbor(|e| {
            e.map(1)
                .unwrap()
                .str("token")
                .unwrap()
                .bytes(&TOKEN)
                .unwrap();
        }),
    )
}

async fn get_authority(
    State(mock): Shared,
    headers: HeaderMap,
    RawQuery(query): RawQuery,
) -> Response {
    mock.note("GET authority");
    if !authenticated(&headers) {
        return error(StatusCode::UNAUTHORIZED, "unauthenticated");
    }
    let generation = *mock.generation.lock().unwrap();
    if mock.behaviour().stuck_authority {
        let at = after(query.as_deref()).map_or(0, |(_, l)| l);
        return page_body(generation, 1000, &[], &cursor(&generation, at), false);
    }
    let log = mock.authority.lock().unwrap().clone();
    page(&log, generation, query.as_deref())
}

async fn get_records(
    State(mock): Shared,
    headers: HeaderMap,
    RawQuery(query): RawQuery,
) -> Response {
    mock.note("GET records");
    if !authenticated(&headers) {
        return error(StatusCode::UNAUTHORIZED, "unauthenticated");
    }
    match mock.behaviour().records_body {
        RecordsBody::Page => {}
        RecordsBody::Endless => {
            let zeros = futures_util::stream::repeat_with(|| {
                Ok::<_, Infallible>(Bytes::from(vec![0u8; 64 * 1024]))
            });
            return Response::new(Body::from_stream(zeros));
        }
        RecordsBody::Declared(len) => return respond(StatusCode::OK, vec![0; len]),
    }
    let generation = *mock.generation.lock().unwrap();
    let log = mock.records.lock().unwrap().clone();
    page(&log, generation, query.as_deref())
}

async fn put_authority(
    State(mock): Shared,
    Path((_, _)): Path<(String, String)>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let body = body.to_vec();
    let behaviour = mock.behaviour();
    if !authenticated(&headers) {
        mock.note("PUT authority without a session");
        mock.unauthenticated_puts.lock().unwrap().push(body.clone());
        let status = behaviour.unauthenticated_put.unwrap_or(401);
        if !(200..300).contains(&status) {
            return error(StatusCode::from_u16(status).unwrap(), "unauthenticated");
        }
        mock.published.store(true, Ordering::SeqCst);
    } else {
        mock.note("PUT authority");
        mock.control_puts.lock().unwrap().push(body.clone());
        if behaviour.refuse_controls {
            return error(StatusCode::BAD_REQUEST, "malformed");
        }
    }
    let mut log = mock.authority.lock().unwrap();
    if !log.contains(&body) {
        log.push(body);
    }
    respond(StatusCode::CREATED, cbor(|e| drop(e.map(0))))
}

async fn put_record(
    State(mock): Shared,
    Path((_, _)): Path<(String, String)>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    mock.note("PUT record");
    if !authenticated(&headers) {
        return error(StatusCode::UNAUTHORIZED, "unauthenticated");
    }
    mock.record_puts.fetch_add(1, Ordering::SeqCst);
    let mut log = mock.records.lock().unwrap();
    let body = body.to_vec();
    if !log.contains(&body) {
        log.push(body);
    }
    respond(StatusCode::CREATED, cbor(|e| drop(e.map(0))))
}

async fn snapshots(State(mock): Shared) -> Response {
    let generation = *mock.generation.lock().unwrap();
    respond(
        StatusCode::OK,
        cbor(|e| {
            e.map(2)
                .unwrap()
                .str("generation")
                .unwrap()
                .bytes(&generation)
                .unwrap();
            e.str("records").unwrap().array(0).unwrap();
        }),
    )
}

async fn hints(State(mock): Shared) -> Response {
    mock.note("GET hints");
    if mock.behaviour().stall_hints {
        std::future::pending::<()>().await;
    }
    StatusCode::NOT_FOUND.into_response()
}

/// Serve `mock` on a local port; its `http://` endpoint.
pub async fn serve(mock: Arc<Mock>) -> String {
    let app = Router::new()
        .route("/v0/session/challenge", post(challenge))
        .route("/v0/session", post(session))
        .route("/v0/routes/{route}/authority", get(get_authority))
        .route("/v0/routes/{route}/authority/{digest}", put(put_authority))
        .route("/v0/routes/{route}/records", get(get_records))
        .route("/v0/routes/{route}/records/{id}", put(put_record))
        .route("/v0/routes/{route}/snapshots", get(snapshots))
        .route("/v0/routes/{route}/hints", get(hints))
        .with_state(mock);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    endpoint
}

/// A, the admin, and B, joined out of band (as an invitation is).
pub struct Pair {
    pub dir: tempfile::TempDir,
    pub a: Arc<Database>,
    pub b: Arc<Database>,
}

pub fn pair() -> Pair {
    let dir = tempfile::tempdir().unwrap();
    let a = Database::open(dir.path().join("a.db")).unwrap();
    a.create_collection(&MemoryKeyStore::default(), route())
        .unwrap();
    let b = Database::open(dir.path().join("b.db")).unwrap();
    admit(&a, &b);
    Pair {
        dir,
        a: Arc::new(a),
        b: Arc::new(b),
    }
}

/// `admin` admits `joining` through an invitation, handed over directly.
pub fn admit(admin: &Database, joining: &Database) {
    let keys = MemoryKeyStore::default();
    let shown = admin.invite().unwrap();
    let request = joining.start_enrollment(&keys, &shown).unwrap();
    let id = parse(&request).unwrap().message.signer;
    admin.admit(&request, &shown).unwrap();
    let envelopes: Vec<Vec<u8>> = admin
        .outgoing_controls()
        .unwrap()
        .into_iter()
        .filter(|o| {
            matches!(parse(&o.bytes).unwrap().message.body,
                Control::KeyEnvelope(ref k) if k.recipient == id)
        })
        .map(|o| o.bytes)
        .collect();
    assert_eq!(
        joining
            .finish_enrollment(&keys, &admin.sync_controls().unwrap(), &envelopes)
            .unwrap(),
        SyncStanding::Active
    );
}
