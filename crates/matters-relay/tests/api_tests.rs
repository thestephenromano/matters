//! The v0 HTTP API against a real PostgreSQL (#38): device sessions,
//! uploads and pages over HTTP, every error in the contract's shape and
//! status, and deterministic CBOR bodies. See store_tests.rs for the
//! environment variables.

mod common;

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use axum::Router;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use common::*;
use matters_relay::api::{router, Clock};
use matters_sync::authority::{sign_session, Authority, DeviceKeys, Route};
use matters_sync::record::record_hash;
use minicbor::{Decoder, Encoder};
use tower::ServiceExt;

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn route_hex(route: &Route) -> String {
    hex(&[route.application, route.tenant, route.collection].concat())
}

/// A test clock at `start` ms, advanced by the test.
fn clock(start: u64) -> (Clock, Arc<AtomicU64>) {
    let now = Arc::new(AtomicU64::new(start));
    let read = now.clone();
    (Arc::new(move || read.load(Ordering::SeqCst)), now)
}

struct Response {
    status: StatusCode,
    body: Vec<u8>,
}

impl Response {
    /// The body's map, keys in their encoded order, values as raw CBOR.
    fn map(&self) -> Vec<(String, Vec<u8>)> {
        let mut d = Decoder::new(&self.body);
        let n = d.map().unwrap().unwrap();
        (0..n)
            .map(|_| {
                let key = d.str().unwrap().to_owned();
                let start = d.position();
                d.skip().unwrap();
                (key, self.body[start..d.position()].to_vec())
            })
            .collect()
    }

    fn keys(&self) -> Vec<String> {
        self.map().into_iter().map(|(k, _)| k).collect()
    }

    fn bytes(&self, key: &str) -> Vec<u8> {
        let (_, raw) = self.map().into_iter().find(|(k, _)| k == key).unwrap();
        Decoder::new(&raw).bytes().unwrap().to_vec()
    }

    fn text(&self, key: &str) -> String {
        let (_, raw) = self.map().into_iter().find(|(k, _)| k == key).unwrap();
        Decoder::new(&raw).str().unwrap().to_owned()
    }

    fn uint(&self, key: &str) -> u64 {
        let (_, raw) = self.map().into_iter().find(|(k, _)| k == key).unwrap();
        Decoder::new(&raw).u64().unwrap()
    }

    fn code(&self) -> String {
        self.text("code")
    }
}

async fn call(
    app: &Router,
    method: Method,
    uri: &str,
    token: Option<&[u8; 32]>,
    body: Vec<u8>,
) -> Response {
    let mut request = Request::builder().method(method).uri(uri);
    if let Some(token) = token {
        request = request.header("authorization", format!("Bearer {}", hex(token)));
    }
    let response = app
        .clone()
        .oneshot(request.body(Body::from(body)).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap()
        .to_vec();
    Response { status, body }
}

fn challenge_body(route: &Route, device: &[u8; 48]) -> Vec<u8> {
    let mut e = Encoder::new(Vec::new());
    e.map(2).unwrap();
    e.str("route").unwrap();
    e.array(3)
        .unwrap()
        .bytes(&route.application)
        .unwrap()
        .bytes(&route.tenant)
        .unwrap()
        .bytes(&route.collection)
        .unwrap();
    e.str("device").unwrap().bytes(device).unwrap();
    e.into_writer()
}

fn session_body(route: &Route, device: &[u8; 48], challenge: &[u8], signature: &[u8]) -> Vec<u8> {
    let mut e = Encoder::new(Vec::new());
    e.map(4).unwrap();
    e.str("route").unwrap();
    e.array(3)
        .unwrap()
        .bytes(&route.application)
        .unwrap()
        .bytes(&route.tenant)
        .unwrap()
        .bytes(&route.collection)
        .unwrap();
    e.str("device").unwrap().bytes(device).unwrap();
    e.str("challenge").unwrap().bytes(challenge).unwrap();
    e.str("signature").unwrap().bytes(signature).unwrap();
    e.into_writer()
}

/// Open a session for `device` through the API.
async fn login(app: &Router, device: &DeviceKeys) -> [u8; 32] {
    let c = call(
        app,
        Method::POST,
        "/v0/session/challenge",
        None,
        challenge_body(&route(), &device.id()),
    )
    .await;
    assert_eq!(c.status, StatusCode::OK, "{:?}", c.body);
    let challenge: [u8; 32] = c.bytes("challenge").try_into().unwrap();
    let signature = sign_session(device, &route(), &challenge, fresh()).unwrap();
    let s = call(
        app,
        Method::POST,
        "/v0/session",
        None,
        session_body(&route(), &device.id(), &challenge, &signature),
    )
    .await;
    assert_eq!(s.status, StatusCode::OK);
    assert_eq!(
        s.keys(),
        ["token", "expires", "generation"],
        "deterministic order"
    );
    s.bytes("token").try_into().unwrap()
}

async fn setup() -> Option<(TestDb, Collection, Router, Arc<AtomicU64>)> {
    let db = TestDb::start().await?;
    let c = collection();
    db.relay
        .provision(&route(), c.keys.root_key(), &c.genesis)
        .await
        .unwrap();
    let (clock, now) = clock(1_000_000);
    let app = router(db.relay.clone(), clock);
    Some((db, c, app, now))
}

#[tokio::test]
async fn a_device_signs_in_uploads_and_pages_over_http() {
    let Some((db, c, app, _)) = setup().await else {
        return;
    };
    let token = login(&app, &c.admin).await;
    let r = route_hex(&route());
    let id = fresh();
    let bytes = record(&c.admin, id, b"over http", &route());
    let uri = format!("/v0/routes/{r}/records/{}", hex(&id));
    let created = call(&app, Method::PUT, &uri, Some(&token), bytes.clone()).await;
    assert_eq!(created.status, StatusCode::CREATED);
    assert_eq!(created.keys(), ["digest", "sequence", "generation"]);
    assert_eq!(created.uint("sequence"), 1);
    assert_eq!(created.bytes("digest"), record_hash(&bytes));
    let again = call(&app, Method::PUT, &uri, Some(&token), bytes.clone()).await;
    assert_eq!(again.status, StatusCode::OK);
    assert_eq!(again.body, created.body, "the original receipt");
    let conflict = call(
        &app,
        Method::PUT,
        &uri,
        Some(&token),
        record(&c.admin, id, b"other", &route()),
    )
    .await;
    assert_eq!(conflict.status, StatusCode::CONFLICT);
    assert_eq!(conflict.code(), "record-conflict");
    assert_eq!(conflict.bytes("digest"), record_hash(&bytes));
    assert_eq!(conflict.keys(), ["code", "retry", "digest"]);
    // A page, then resuming from its cursor.
    let page = call(
        &app,
        Method::GET,
        &format!("/v0/routes/{r}/records?limit=10"),
        Some(&token),
        vec![],
    )
    .await;
    assert_eq!(page.status, StatusCode::OK);
    assert_eq!(
        page.keys(),
        ["next", "records", "complete", "generation", "high-water"]
    );
    assert_eq!(page.uint("high-water"), 1);
    let next = URL_SAFE_NO_PAD.encode(page.bytes("next"));
    let rest = call(
        &app,
        Method::GET,
        &format!("/v0/routes/{r}/records?after={next}&limit=10"),
        Some(&token),
        vec![],
    )
    .await;
    assert_eq!(rest.status, StatusCode::OK);
    // The authority log holds the genesis.
    let authority = call(
        &app,
        Method::GET,
        &format!("/v0/routes/{r}/authority"),
        Some(&token),
        vec![],
    )
    .await;
    assert_eq!(authority.status, StatusCode::OK);
    assert_eq!(authority.uint("high-water"), 1);
    db.finish().await;
}

#[tokio::test]
async fn sessions_are_single_use_short_lived_and_end_with_membership() {
    let Some((db, mut c, app, now)) = setup().await else {
        return;
    };
    let r = route_hex(&route());
    let records = format!("/v0/routes/{r}/records");
    // No token, a wrong token.
    let none = call(&app, Method::GET, &records, None, vec![]).await;
    assert_eq!(
        (none.status, none.code().as_str()),
        (StatusCode::UNAUTHORIZED, "unauthenticated")
    );
    let wrong = call(&app, Method::GET, &records, Some(&[7; 32]), vec![]).await;
    assert_eq!(wrong.code(), "unauthenticated");
    // A challenge answers once, within 60 s, and only with the device's key.
    let c1 = call(
        &app,
        Method::POST,
        "/v0/session/challenge",
        None,
        challenge_body(&route(), &c.admin.id()),
    )
    .await;
    assert_eq!(c1.keys(), ["expires", "challenge"]);
    let challenge: [u8; 32] = c1.bytes("challenge").try_into().unwrap();
    let forged = sign_session(&device(), &route(), &challenge, fresh()).unwrap();
    let refused = call(
        &app,
        Method::POST,
        "/v0/session",
        None,
        session_body(&route(), &c.admin.id(), &challenge, &forged),
    )
    .await;
    assert_eq!(refused.code(), "unauthenticated");
    let signature = sign_session(&c.admin, &route(), &challenge, fresh()).unwrap();
    let reused = call(
        &app,
        Method::POST,
        "/v0/session",
        None,
        session_body(&route(), &c.admin.id(), &challenge, &signature),
    )
    .await;
    assert_eq!(
        reused.code(),
        "unauthenticated",
        "the failed answer used it up"
    );
    let c2 = call(
        &app,
        Method::POST,
        "/v0/session/challenge",
        None,
        challenge_body(&route(), &c.admin.id()),
    )
    .await;
    let late: [u8; 32] = c2.bytes("challenge").try_into().unwrap();
    now.fetch_add(60_001, Ordering::SeqCst);
    let signature = sign_session(&c.admin, &route(), &late, fresh()).unwrap();
    let expired = call(
        &app,
        Method::POST,
        "/v0/session",
        None,
        session_body(&route(), &c.admin.id(), &late, &signature),
    )
    .await;
    assert_eq!(expired.code(), "unauthenticated");
    // A device outside the roster gets no challenge.
    let outsider = device();
    let not_member = call(
        &app,
        Method::POST,
        "/v0/session/challenge",
        None,
        challenge_body(&route(), &outsider.id()),
    )
    .await;
    assert_eq!(
        (not_member.status, not_member.code().as_str()),
        (StatusCode::FORBIDDEN, "not-a-member")
    );
    // A token lasts 15 minutes.
    let token = login(&app, &c.admin).await;
    assert_eq!(
        call(&app, Method::GET, &records, Some(&token), vec![])
            .await
            .status,
        StatusCode::OK
    );
    now.fetch_add(15 * 60_000 + 1, Ordering::SeqCst);
    let stale = call(&app, Method::GET, &records, Some(&token), vec![]).await;
    assert_eq!(
        (stale.status, stale.code().as_str()),
        (StatusCode::UNAUTHORIZED, "session-expired")
    );
    // A member's session ends when a transition removes it.
    let b = device();
    let add = transition(&c, vec![c.admin.member(true), b.member(false)]);
    let admin_token = login(&app, &c.admin).await;
    let put = call(
        &app,
        Method::PUT,
        &format!("/v0/routes/{r}/authority/{}", hex(&record_hash(&add))),
        Some(&admin_token),
        add.clone(),
    )
    .await;
    assert_eq!(put.status, StatusCode::CREATED);
    let b_token = login(&app, &b).await;
    c.authority.apply_membership(&add).unwrap();
    let remove = transition(&c, vec![c.admin.member(true)]);
    call(
        &app,
        Method::PUT,
        &format!("/v0/routes/{r}/authority/{}", hex(&record_hash(&remove))),
        Some(&admin_token),
        remove,
    )
    .await;
    let revoked = call(&app, Method::GET, &records, Some(&b_token), vec![]).await;
    assert_eq!(
        (revoked.status, revoked.code().as_str()),
        (StatusCode::FORBIDDEN, "revoked")
    );
    db.finish().await;
}

#[tokio::test]
async fn errors_carry_the_contracts_codes_and_statuses() {
    let Some((db, c, app, _)) = setup().await else {
        return;
    };
    let token = login(&app, &c.admin).await;
    let r = route_hex(&route());
    let id = fresh();
    let bytes = record(&c.admin, id, b"e", &route());
    let cases = [
        (
            Method::GET,
            "/v1/routes".to_owned(),
            vec![],
            StatusCode::NOT_FOUND,
            "unsupported-version",
        ),
        (
            Method::GET,
            "/v0/routes/zz/records".to_owned(),
            vec![],
            StatusCode::BAD_REQUEST,
            "malformed",
        ),
        (
            Method::GET,
            format!("/v0/routes/{r}/records?limit=0"),
            vec![],
            StatusCode::BAD_REQUEST,
            "malformed",
        ),
        (
            Method::GET,
            format!("/v0/routes/{r}/records?after=!!"),
            vec![],
            StatusCode::BAD_REQUEST,
            "malformed",
        ),
        (
            Method::PUT,
            format!("/v0/routes/{r}/records/{}", hex(&fresh())),
            bytes.clone(),
            StatusCode::UNPROCESSABLE_ENTITY,
            "context-mismatch",
        ),
        (
            Method::PUT,
            format!("/v0/routes/{r}/records/{}", hex(&id)),
            vec![0; matters_sync::wire::MAX_WIRE_BYTES + 1],
            StatusCode::PAYLOAD_TOO_LARGE,
            "too-large",
        ),
        (
            Method::PUT,
            format!("/v0/routes/{}/records/{}", "ab".repeat(48), hex(&id)),
            bytes.clone(),
            StatusCode::NOT_FOUND,
            "unknown-route",
        ),
        (
            Method::POST,
            "/v0/session/challenge".to_owned(),
            vec![0xa0],
            StatusCode::BAD_REQUEST,
            "malformed",
        ),
    ];
    for (method, uri, body, status, code) in cases {
        let response = call(&app, method, &uri, Some(&token), body).await;
        assert_eq!(
            (response.status, response.code()),
            (status, code.to_owned()),
            "{uri}"
        );
        assert_eq!(response.keys()[..2], ["code", "retry"]);
    }
    // A cursor from before a restore names the new generation.
    let page = call(
        &app,
        Method::GET,
        &format!("/v0/routes/{r}/records"),
        Some(&token),
        vec![],
    )
    .await;
    let next = URL_SAFE_NO_PAD.encode(page.bytes("next"));
    let generation = db.relay.new_generation(&route()).await.unwrap();
    let gone = call(
        &app,
        Method::GET,
        &format!("/v0/routes/{r}/records?after={next}"),
        Some(&token),
        vec![],
    )
    .await;
    assert_eq!(
        (gone.status, gone.code().as_str()),
        (StatusCode::GONE, "generation-changed")
    );
    assert_eq!(gone.bytes("generation"), generation);
    // A quota.
    db.relay.set_quota(&route(), Some(1)).await.unwrap();
    let full = call(
        &app,
        Method::PUT,
        &format!("/v0/routes/{r}/records/{}", hex(&id)),
        Some(&token),
        bytes,
    )
    .await;
    assert_eq!(
        (full.status, full.code().as_str()),
        (StatusCode::INSUFFICIENT_STORAGE, "quota-exhausted")
    );
    // Authority objects count toward it too.
    let envelope = c
        .authority
        .wrap_key(
            &c.admin,
            c.admin.id(),
            &matters_sync::ContentKey::new([4; 32]),
            fresh(),
        )
        .unwrap();
    let full = call(
        &app,
        Method::PUT,
        &format!("/v0/routes/{r}/authority/{}", hex(&record_hash(&envelope))),
        Some(&token),
        envelope,
    )
    .await;
    assert_eq!(
        (full.status, full.code().as_str()),
        (StatusCode::INSUFFICIENT_STORAGE, "quota-exhausted")
    );
    db.finish().await;
}

#[tokio::test]
async fn health_readiness_and_redacted_metrics() {
    let Some(db) = TestDb::start().await else {
        return;
    };
    let c = collection();
    db.relay
        .provision(&route(), c.keys.root_key(), &c.genesis)
        .await
        .unwrap();
    let metrics = Arc::new(matters_relay::metrics::Metrics::new());
    let (clock, _) = clock(1_000_000);
    let app = matters_relay::api::router_with(
        db.relay.clone(),
        clock,
        matters_relay::api::Options {
            metrics: Some(metrics.clone()),
            ..Default::default()
        },
    );
    let health = call(&app, Method::GET, "/healthz", None, vec![]).await;
    assert_eq!(
        (health.status, health.body.as_slice()),
        (StatusCode::OK, &b"ok"[..])
    );
    let ready = call(&app, Method::GET, "/readyz", None, vec![]).await;
    assert_eq!(ready.status, StatusCode::OK);
    // A durable acknowledgement, a replay and a refusal.
    let token = login(&app, &c.admin).await;
    let r = route_hex(&route());
    let id = fresh();
    let bytes = record(&c.admin, id, b"counted", &route());
    let uri = format!("/v0/routes/{r}/records/{}", hex(&id));
    for _ in 0..2 {
        call(&app, Method::PUT, &uri, Some(&token), bytes.clone()).await;
    }
    call(
        &app,
        Method::GET,
        &format!("/v0/routes/{r}/records?limit=0"),
        Some(&token),
        vec![],
    )
    .await;
    metrics.refresh(&db.relay).await.unwrap();
    let text = metrics.encode();
    for expected in [
        r#"matters_relay_requests_total{endpoint="/v0/routes/{route}/records/{record}",status="201"} 1"#,
        r#"matters_relay_requests_total{endpoint="/v0/routes/{route}/records/{record}",status="200"} 1"#,
        r#"matters_relay_requests_total{endpoint="/v0/routes/{route}/records",status="400"} 1"#,
        r#"matters_relay_requests_total{endpoint="/healthz",status="200"} 1"#,
        "matters_relay_routes 1",
        "matters_relay_stored_records 1",
        "matters_relay_request_duration_seconds_count{endpoint=\"/v0/session\"} 1",
    ] {
        assert!(text.contains(expected), "missing {expected} in\n{text}");
    }
    // Nothing that identifies a route, device, record or content.
    for secret in [
        r.clone(),
        hex(&id),
        hex(&c.admin.id()),
        hex(&record_hash(&bytes)),
        "counted".to_string(),
    ] {
        assert!(!text.contains(&secret), "metrics carry {secret}");
    }
    // The database goes away: not ready, though still alive.
    db.relay.pool().close().await;
    let ready = call(&app, Method::GET, "/readyz", None, vec![]).await;
    assert_eq!(ready.status, StatusCode::SERVICE_UNAVAILABLE);
    let health = call(&app, Method::GET, "/healthz", None, vec![]).await;
    assert_eq!(health.status, StatusCode::OK);
    db.finish().await;
}

#[tokio::test]
async fn the_enrollment_mailbox_holds_requests_for_the_admin_only() {
    let Some((db, c, app, _)) = setup().await else {
        return;
    };
    let r = route_hex(&route());
    // A request under an invitation into a collection on `route`.
    let request_for = |route: &Route| {
        let keys = matters_sync::authority::RecoverySecret::new(fresh())
            .keys(route)
            .unwrap();
        let admin = device();
        let genesis = matters_sync::authority::genesis(
            route,
            &keys,
            &admin,
            &matters_sync::ContentKey::new([4; 32]),
            fresh(),
        )
        .unwrap();
        let invitation = Authority::bootstrap(&genesis, route.clone(), keys.root_key())
            .unwrap()
            .invite(matters_sync::authority::InvitationSecret::new([1; 16]))
            .unwrap();
        matters_sync::authority::request_enrollment(&invitation, &device(), fresh()).unwrap()
    };
    let request = request_for(&route());
    let lookup = hex(&[3; 16]);
    let uri = format!("/v0/routes/{r}/enrollments/{lookup}");
    // Left without a session; again with the same bytes; never replaced.
    let put = |bytes: Vec<u8>, uri: String| {
        let app = app.clone();
        async move { call(&app, Method::PUT, &uri, None, bytes).await }
    };
    assert_eq!(
        put(request.clone(), uri.clone()).await.status,
        StatusCode::CREATED
    );
    assert_eq!(
        put(request.clone(), uri.clone()).await.status,
        StatusCode::OK
    );
    let other = request_for(&route());
    assert_eq!(put(other, uri.clone()).await.status, StatusCode::CONFLICT);
    // Only enrollment requests, and only for this route.
    let malformed = put(
        c.genesis.clone(),
        format!("/v0/routes/{r}/enrollments/{}", hex(&[4; 16])),
    )
    .await;
    assert_eq!(malformed.code(), "malformed");
    let elsewhere = Route {
        collection: [7; 16],
        ..route()
    };
    let foreign = put(
        request_for(&elsewhere),
        format!("/v0/routes/{r}/enrollments/{}", hex(&[5; 16])),
    )
    .await;
    assert_eq!(foreign.code(), "context-mismatch");
    // The admin collects it; anyone without a session cannot.
    let token = login(&app, &c.admin).await;
    let got = call(&app, Method::GET, &uri, Some(&token), vec![]).await;
    assert_eq!(
        (got.status, got.body.clone()),
        (StatusCode::OK, request.clone())
    );
    let anonymous = call(&app, Method::GET, &uri, None, vec![]).await;
    assert_eq!(anonymous.status, StatusCode::UNAUTHORIZED);
    let absent = call(
        &app,
        Method::GET,
        &format!("/v0/routes/{r}/enrollments/{}", hex(&[9; 16])),
        Some(&token),
        vec![],
    )
    .await;
    assert_eq!(absent.status, StatusCode::NOT_FOUND);
    // A route keeps a bounded number at once.
    for n in 0..(matters_relay::MAX_ENROLLMENTS - 1) {
        let uri = format!("/v0/routes/{r}/enrollments/{}", hex(&[10 + n as u8; 16]));
        assert_eq!(
            put(request_for(&route()), uri).await.status,
            StatusCode::CREATED
        );
    }
    let full = put(
        request_for(&route()),
        format!("/v0/routes/{r}/enrollments/{}", hex(&[200; 16])),
    )
    .await;
    assert_eq!(full.code(), "quota-exhausted");
    // An expired request is gone, and makes room.
    sqlx::query("UPDATE enrollments SET received_at = now() - interval '2 hours'")
        .execute(db.relay.pool())
        .await
        .unwrap();
    let expired = call(&app, Method::GET, &uri, Some(&token), vec![]).await;
    assert_eq!(expired.status, StatusCode::NOT_FOUND);
    let room = put(
        request_for(&route()),
        format!("/v0/routes/{r}/enrollments/{}", hex(&[201; 16])),
    )
    .await;
    assert_eq!(room.status, StatusCode::CREATED);
    db.finish().await;
}

#[tokio::test]
async fn the_roots_session_reads_the_authority_log_and_publishes_only_a_recovery() {
    let Some((db, c, app, _)) = setup().await else {
        return;
    };
    let root = c.keys.root_id().unwrap();
    let ch = call(
        &app,
        Method::POST,
        "/v0/session/challenge",
        None,
        challenge_body(&route(), &root),
    )
    .await;
    assert_eq!(ch.status, StatusCode::OK);
    let challenge: [u8; 32] = ch.bytes("challenge").try_into().unwrap();
    let signature =
        matters_sync::authority::sign_root_session(&c.keys, &route(), &challenge, fresh()).unwrap();
    let s = call(
        &app,
        Method::POST,
        "/v0/session",
        None,
        session_body(&route(), &root, &challenge, &signature),
    )
    .await;
    assert_eq!(s.status, StatusCode::OK);
    let token: [u8; 32] = s.bytes("token").try_into().unwrap();
    let r = route_hex(&route());
    // It reads the authority log.
    let log = call(
        &app,
        Method::GET,
        &format!("/v0/routes/{r}/authority"),
        Some(&token),
        vec![],
    )
    .await;
    assert_eq!(log.status, StatusCode::OK);
    // Nothing else: no records or mailbox, and no other kind of authority
    // object.
    for path in [
        format!("/v0/routes/{r}/records"),
        format!("/v0/routes/{r}/enrollments/{}", hex(&[1; 16])),
    ] {
        let refused = call(&app, Method::GET, &path, Some(&token), vec![]).await;
        assert_eq!(refused.code(), "not-a-member", "{path}");
    }
    let id = fresh();
    let put = call(
        &app,
        Method::PUT,
        &format!("/v0/routes/{r}/records/{}", hex(&id)),
        Some(&token),
        record(&c.admin, id, b"as the root", &route()),
    )
    .await;
    assert_eq!(put.code(), "not-a-member");
    let claim = matters_sync::authority::sign(
        &matters_sync::authority::Message {
            route: route(),
            signer: c.admin.id(),
            body: matters_sync::authority::Body::WriterStart(
                matters_sync::authority::WriterStart {
                    manifest: c.authority.manifest(),
                    document: [4; 16],
                    device: c.admin.id(),
                    incarnation: fresh(),
                    actor: 1,
                    previous: None,
                },
            ),
        },
        c.admin.signing_key(),
        fresh(),
    )
    .unwrap();
    let writer = call(
        &app,
        Method::PUT,
        &format!("/v0/routes/{r}/authority/{}", hex(&record_hash(&claim))),
        Some(&token),
        claim,
    )
    .await;
    assert_eq!(writer.code(), "not-a-member");
    // A session signed with other words is refused.
    let other = matters_sync::authority::RecoverySecret::new(fresh())
        .keys(&route())
        .unwrap();
    let refused = call(
        &app,
        Method::POST,
        "/v0/session/challenge",
        None,
        challenge_body(&route(), &other.root_id().unwrap()),
    )
    .await;
    assert_eq!(refused.code(), "not-a-member");
    db.finish().await;
}

#[tokio::test]
async fn the_newest_checkpoint_parts_are_found_without_reading_the_log() {
    let Some((db, c, app, _)) = setup().await else {
        return;
    };
    let token = login(&app, &c.admin).await;
    let r = route_hex(&route());
    let mut snapshots = Vec::new();
    for n in 0..4u8 {
        let id = fresh();
        let bytes = if n % 2 == 1 {
            let bytes = snapshot_record(&c.admin, id, &[n; 20], &route());
            snapshots.push(bytes.clone());
            bytes
        } else {
            record(&c.admin, id, &[n; 20], &route())
        };
        let uri = format!("/v0/routes/{r}/records/{}", hex(&id));
        let stored = call(&app, Method::PUT, &uri, Some(&token), bytes).await;
        assert_eq!(stored.status, StatusCode::CREATED);
    }
    let entries = |response: &Response| -> Vec<(u64, Vec<u8>)> {
        let (_, raw) = response
            .map()
            .into_iter()
            .find(|(k, _)| k == "records")
            .unwrap();
        let mut d = Decoder::new(&raw);
        let n = d.array().unwrap().unwrap();
        (0..n)
            .map(|_| {
                d.array().unwrap();
                (d.u64().unwrap(), d.bytes().unwrap().to_vec())
            })
            .collect()
    };
    let found = call(
        &app,
        Method::GET,
        &format!("/v0/routes/{r}/snapshots"),
        Some(&token),
        vec![],
    )
    .await;
    assert_eq!(found.status, StatusCode::OK);
    assert_eq!(found.keys(), ["records", "generation"]);
    assert_eq!(
        entries(&found),
        [(4, snapshots[1].clone()), (2, snapshots[0].clone())],
        "snapshot records only, newest first"
    );
    let newest = call(
        &app,
        Method::GET,
        &format!("/v0/routes/{r}/snapshots?limit=1"),
        Some(&token),
        vec![],
    )
    .await;
    assert_eq!(entries(&newest), [(4, snapshots[1].clone())]);
    for bad in ["limit=0", "limit=257", "after=AA", "limit=1&limit=2"] {
        let refused = call(
            &app,
            Method::GET,
            &format!("/v0/routes/{r}/snapshots?{bad}"),
            Some(&token),
            vec![],
        )
        .await;
        assert_eq!(refused.code(), "malformed", "{bad}");
    }
    let anonymous = call(
        &app,
        Method::GET,
        &format!("/v0/routes/{r}/snapshots"),
        None,
        vec![],
    )
    .await;
    assert_eq!(anonymous.status, StatusCode::UNAUTHORIZED);
    db.finish().await;
}
