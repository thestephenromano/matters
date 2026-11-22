//! Commit hints, rate limits, record quotas, and export and import (#38),
//! against a real PostgreSQL. See store_tests.rs for the environment.

mod common;

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use common::*;
use futures_util::StreamExt;
use matters_relay::api::{router_with, Options, RateLimit};
use matters_relay::hints::Hints;
use matters_relay::{Relay, RelayError};
use matters_sync::authority::{sign_session, DeviceKeys};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tower::ServiceExt;

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn route_hex() -> String {
    let r = route();
    hex(&[r.application, r.tenant, r.collection].concat())
}

/// A session token for `device`, through the store.
async fn token(relay: &Relay, device: &DeviceKeys, now: u64) -> [u8; 32] {
    let (challenge, _) = relay.challenge(&route(), &device.id(), now).await.unwrap();
    let signature = sign_session(device, &route(), &challenge, fresh()).unwrap();
    relay
        .open_session(&route(), &device.id(), &challenge, &signature, now)
        .await
        .unwrap()
        .token
}

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
}

async fn provisioned() -> Option<(TestDb, Collection)> {
    let db = TestDb::start().await?;
    let c = collection();
    db.relay
        .provision(&route(), c.keys.root_key(), &c.genesis)
        .await
        .unwrap();
    Some((db, c))
}

#[tokio::test]
async fn a_hint_follows_each_committed_record_and_never_a_duplicate() {
    let Some((db, c)) = provisioned().await else {
        return;
    };
    let hints = Hints::start(&db.relay).await.unwrap();
    let app = router_with(
        db.relay.clone(),
        matters_relay::api::system_clock(),
        Options {
            hints: Some(hints),
            ..Options::default()
        },
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let url = format!("ws://{addr}/v0/routes/{}/hints", route_hex());
    // Without a session the upgrade is refused.
    match tokio_tungstenite::connect_async(url.as_str()).await {
        Err(tokio_tungstenite::tungstenite::Error::Http(response)) => {
            assert_eq!(response.status(), StatusCode::UNAUTHORIZED)
        }
        other => panic!("{:?}", other.map(|_| ())),
    }
    let b_token = token(&db.relay, &c.admin, now()).await;
    let mut request = url.as_str().into_client_request().unwrap();
    request.headers_mut().insert(
        "authorization",
        format!("Bearer {}", hex(&b_token)).parse().unwrap(),
    );
    let (mut socket, _) = tokio_tungstenite::connect_async(request).await.unwrap();
    let id = fresh();
    let bytes = record(&c.admin, id, b"hinted", &route());
    let receipt = db
        .relay
        .append(&route(), &c.admin.id(), &id, &bytes)
        .await
        .unwrap();
    let message = tokio::time::timeout(Duration::from_secs(5), socket.next())
        .await
        .expect("a hint")
        .unwrap()
        .unwrap();
    let body = message.into_data();
    let mut d = minicbor::Decoder::new(&body);
    assert_eq!(d.map().unwrap(), Some(2));
    assert_eq!(d.str().unwrap(), "sequence");
    assert_eq!(d.u64().unwrap(), receipt.receipt().sequence);
    assert_eq!(d.str().unwrap(), "generation");
    assert_eq!(d.bytes().unwrap(), receipt.receipt().generation);
    // The same bytes again store nothing, so no second hint.
    db.relay
        .append(&route(), &c.admin.id(), &id, &bytes)
        .await
        .unwrap();
    assert!(
        tokio::time::timeout(Duration::from_millis(700), socket.next())
            .await
            .is_err(),
        "no hint for a duplicate"
    );
    db.finish().await;
}

/// A relay with hints on a local port, for WebSocket clients.
async fn serve_hints(db: &TestDb) -> std::net::SocketAddr {
    let hints = Hints::start(&db.relay).await.unwrap();
    let app = router_with(
        db.relay.clone(),
        matters_relay::api::system_clock(),
        Options {
            hints: Some(hints),
            ..Options::default()
        },
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    addr
}

type Socket =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

async fn subscribe(
    addr: std::net::SocketAddr,
    token: &[u8; 32],
) -> Result<Socket, tokio_tungstenite::tungstenite::Error> {
    let url = format!("ws://{addr}/v0/routes/{}/hints", route_hex());
    let mut request = url.as_str().into_client_request().unwrap();
    request.headers_mut().insert(
        "authorization",
        format!("Bearer {}", hex(token)).parse().unwrap(),
    );
    tokio_tungstenite::connect_async(request)
        .await
        .map(|(socket, _)| socket)
}

/// What the socket gets next: a hint's sequence, or the close it ends with.
async fn next_event(socket: &mut Socket) -> Result<u64, Option<String>> {
    use tokio_tungstenite::tungstenite::Message;
    let message = tokio::time::timeout(Duration::from_secs(5), socket.next())
        .await
        .expect("an event within 5 s");
    match message {
        Some(Ok(Message::Binary(body))) => {
            let mut d = minicbor::Decoder::new(&body);
            d.map().unwrap();
            assert_eq!(d.str().unwrap(), "sequence");
            Ok(d.u64().unwrap())
        }
        Some(Ok(Message::Close(frame))) => Err(frame.map(|f| f.reason.to_string())),
        None | Some(Err(_)) => Err(None),
        Some(Ok(other)) => panic!("{other:?}"),
    }
}

#[tokio::test]
async fn a_hint_socket_ends_with_its_devices_membership_or_session() {
    let Some((db, mut c)) = provisioned().await else {
        return;
    };
    let b = device();
    let add = transition(&c, vec![c.admin.member(true), b.member(false)]);
    db.relay
        .put_authority(&route(), &matters_sync::record::record_hash(&add), &add)
        .await
        .unwrap();
    c.authority.apply_membership(&add).unwrap();
    let addr = serve_hints(&db).await;
    let append = |n: u8| {
        let (relay, admin) = (db.relay.clone(), &c.admin);
        let id = fresh();
        let bytes = record(admin, id, &[n], &route());
        async move {
            relay
                .append(&route(), &admin.id(), &id, &bytes)
                .await
                .unwrap()
        }
    };
    // A's session ends: its socket closes at the next hint, which it never
    // gets.
    let a_token = token(&db.relay, &c.admin, now()).await;
    let mut a_socket = subscribe(addr, &a_token).await.unwrap();
    let b_token = token(&db.relay, &b, now()).await;
    let mut b_socket = subscribe(addr, &b_token).await.unwrap();
    append(1).await;
    assert_eq!(next_event(&mut a_socket).await, Ok(1));
    assert_eq!(next_event(&mut b_socket).await, Ok(1));
    sqlx::query("UPDATE sessions SET expires_at = 0 WHERE token_hash = $1")
        .bind(matters_sync::record::record_hash(&a_token).as_slice())
        .execute(db.relay.pool())
        .await
        .unwrap();
    append(2).await;
    assert_eq!(
        next_event(&mut a_socket).await,
        Err(Some("session-expired".to_owned()))
    );
    assert_eq!(next_event(&mut b_socket).await, Ok(2));
    // B is removed: its socket closes before the next hint reaches it.
    let remove = transition(&c, vec![c.admin.member(true)]);
    db.relay
        .put_authority(
            &route(),
            &matters_sync::record::record_hash(&remove),
            &remove,
        )
        .await
        .unwrap();
    append(3).await;
    assert_eq!(
        next_event(&mut b_socket).await,
        Err(Some("revoked".to_owned()))
    );
    // A device keeps a bounded number of sockets.
    let a_token = token(&db.relay, &c.admin, now()).await;
    let mut open = Vec::new();
    for _ in 0..matters_relay::api::MAX_HINT_SOCKETS {
        open.push(subscribe(addr, &a_token).await.unwrap());
    }
    match subscribe(addr, &a_token).await {
        Err(tokio_tungstenite::tungstenite::Error::Http(response)) => {
            assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS)
        }
        other => panic!("{:?}", other.map(|_| ())),
    }
    // Closing one gives its place back.
    open.pop().unwrap().close(None).await.unwrap();
    let mut again = None;
    for _ in 0..20 {
        if let Ok(socket) = subscribe(addr, &a_token).await {
            again = Some(socket);
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(again.is_some(), "the closed socket's place is free again");
    db.finish().await;
}

#[tokio::test]
async fn a_rate_limit_answers_429_with_a_retry_time_and_refills() {
    let Some((db, c)) = provisioned().await else {
        return;
    };
    let clock_at = Arc::new(AtomicU64::new(now()));
    let read = clock_at.clone();
    let app = router_with(
        db.relay.clone(),
        Arc::new(move || read.load(Ordering::SeqCst)),
        Options {
            rate_limit: Some(RateLimit {
                per_second: 1,
                burst: 2,
            }),
            ..Options::default()
        },
    );
    let t = token(&db.relay, &c.admin, clock_at.load(Ordering::SeqCst)).await;
    let get = |app: axum::Router| async move {
        app.oneshot(
            Request::get(format!("/v0/routes/{}/records", route_hex()))
                .header("authorization", format!("Bearer {}", hex(&t)))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap()
    };
    assert_eq!(get(app.clone()).await.status(), StatusCode::OK);
    assert_eq!(get(app.clone()).await.status(), StatusCode::OK);
    let limited = get(app.clone()).await;
    assert_eq!(limited.status(), StatusCode::TOO_MANY_REQUESTS);
    let body = axum::body::to_bytes(limited.into_body(), usize::MAX)
        .await
        .unwrap();
    let mut d = minicbor::Decoder::new(&body);
    assert_eq!(d.map().unwrap(), Some(3));
    assert_eq!(
        (d.str().unwrap(), d.str().unwrap()),
        ("code", "rate-limited")
    );
    assert_eq!((d.str().unwrap(), d.bool().unwrap()), ("retry", true));
    assert_eq!(d.str().unwrap(), "retry-after-ms");
    assert!(d.u64().unwrap() > 0);
    clock_at.fetch_add(1_000, Ordering::SeqCst);
    assert_eq!(get(app).await.status(), StatusCode::OK, "refilled");
    db.finish().await;
}

fn challenge_body(route: &matters_sync::authority::Route, device: &[u8; 48]) -> Vec<u8> {
    let mut e = minicbor::Encoder::new(Vec::new());
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

#[tokio::test]
async fn requests_without_a_session_are_limited_by_route_and_device() {
    let Some((db, c)) = provisioned().await else {
        return;
    };
    let clock_at = Arc::new(AtomicU64::new(now()));
    let read = clock_at.clone();
    let app = router_with(
        db.relay.clone(),
        Arc::new(move || read.load(Ordering::SeqCst)),
        Options {
            public_rate_limit: Some(RateLimit {
                per_second: 1,
                burst: 3,
            }),
            challenge_rate_limit: Some(RateLimit {
                per_second: 1,
                burst: 1,
            }),
            ..Options::default()
        },
    );
    let post = |app: axum::Router, uri: String, body: Vec<u8>, put: bool| async move {
        let request = if put {
            Request::put(uri)
        } else {
            Request::post(uri)
        };
        app.oneshot(request.body(Body::from(body)).unwrap())
            .await
            .unwrap()
            .status()
    };
    let challenge = |device: [u8; 48]| {
        (
            "/v0/session/challenge".to_owned(),
            challenge_body(&route(), &device),
            false,
        )
    };
    let root = c.keys.root_id().unwrap();
    let (uri, body, put) = challenge(c.admin.id());
    assert_eq!(post(app.clone(), uri, body, put).await, StatusCode::OK);
    // The device's challenges have their own, smaller allowance.
    let (uri, body, put) = challenge(c.admin.id());
    assert_eq!(
        post(app.clone(), uri, body, put).await,
        StatusCode::TOO_MANY_REQUESTS
    );
    let (uri, body, put) = challenge(root);
    assert_eq!(post(app.clone(), uri, body, put).await, StatusCode::OK);
    // The route's allowance covers every request without a session.
    let mailbox = format!("/v0/routes/{}/enrollments/{}", route_hex(), hex(&[1; 16]));
    assert_eq!(
        post(app.clone(), mailbox, vec![], true).await,
        StatusCode::TOO_MANY_REQUESTS
    );
    // Another route has its own.
    let elsewhere = matters_sync::authority::Route {
        collection: [8; 16],
        ..route()
    };
    let other = post(
        app.clone(),
        "/v0/session/challenge".to_owned(),
        challenge_body(&elsewhere, &c.admin.id()),
        false,
    )
    .await;
    assert_eq!(other, StatusCode::NOT_FOUND, "unknown-route, not limited");
    clock_at.fetch_add(1_000, Ordering::SeqCst);
    let (uri, body, put) = challenge(c.admin.id());
    assert_eq!(post(app, uri, body, put).await, StatusCode::OK, "refilled");
    db.finish().await;
}

#[tokio::test]
async fn a_record_quota_refuses_before_storing() {
    let Some((db, c)) = provisioned().await else {
        return;
    };
    db.relay.set_record_quota(&route(), Some(1)).await.unwrap();
    for (i, expect) in [(0, None), (1, Some("quota-exhausted"))] {
        let id = fresh();
        let result = db
            .relay
            .append(
                &route(),
                &c.admin.id(),
                &id,
                &record(&c.admin, id, &[i], &route()),
            )
            .await;
        assert_eq!(result.err().map(|e| e.code()), expect);
    }
    assert_eq!(
        db.relay
            .records(&route(), None, 10)
            .await
            .unwrap()
            .high_water,
        1
    );
    db.relay.set_record_quota(&route(), None).await.unwrap();
    db.finish().await;
}

#[tokio::test]
async fn export_and_import_keep_every_byte_under_a_new_generation() {
    let Some((source, c)) = provisioned().await else {
        return;
    };
    let Some(target) = TestDb::start().await else {
        return;
    };
    let mut sent = Vec::new();
    for i in 0..3u8 {
        let id = fresh();
        // The middle one is a checkpoint part.
        let bytes = if i == 1 {
            snapshot_record(&c.admin, id, &[i; 9], &route())
        } else {
            record(&c.admin, id, &[i; 9], &route())
        };
        source
            .relay
            .append(&route(), &c.admin.id(), &id, &bytes)
            .await
            .unwrap();
        sent.push(bytes);
    }
    let b = device();
    let add = transition(&c, vec![c.admin.member(true), b.member(false)]);
    source
        .relay
        .put_authority(&route(), &matters_sync::record::record_hash(&add), &add)
        .await
        .unwrap();
    let export = source.relay.export(&route()).await.unwrap();
    let before = source.relay.records(&route(), None, 10).await.unwrap();
    let (imported, generation) = target.relay.import(&export).await.unwrap();
    assert_eq!(imported, route());
    assert_ne!(generation, before.generation, "a new generation");
    let after = target.relay.records(&route(), None, 10).await.unwrap();
    assert_eq!(after.generation, generation);
    assert_eq!(
        after
            .entries
            .iter()
            .map(|(_, b)| b.clone())
            .collect::<Vec<_>>(),
        sent,
        "byte for byte, in order"
    );
    assert_eq!(
        target
            .relay
            .authority_objects(&route(), None, 10)
            .await
            .unwrap()
            .entries
            .iter()
            .map(|(_, b)| b.clone())
            .collect::<Vec<_>>(),
        vec![c.genesis.clone(), add]
    );
    assert!(
        target.relay.is_member(&route(), &b.id()).await.unwrap(),
        "membership travels"
    );
    let snapshots = target.relay.snapshots(&route(), 10).await.unwrap();
    assert_eq!(snapshots.generation, generation);
    assert_eq!(
        snapshots.entries,
        [(2, sent[1].clone())],
        "the snapshot index travels"
    );
    // Twice, truncated, or for a route that exists: refused, nothing changed.
    assert!(matches!(
        target.relay.import(&export).await,
        Err(RelayError::RouteExists)
    ));
    assert!(matches!(
        target.relay.import(&export[..export.len() - 1]).await,
        Err(RelayError::Malformed)
    ));
    // The operator endpoint needs the operator credential.
    let operator = [0x0f; 32];
    let app = router_with(
        source.relay.clone(),
        matters_relay::api::system_clock(),
        Options {
            operator_token: Some(operator),
            ..Options::default()
        },
    );
    let get = |token: Option<[u8; 32]>| {
        let app = app.clone();
        async move {
            let mut request = Request::get(format!("/v0/routes/{}/export", route_hex()));
            if let Some(t) = token {
                request = request.header("authorization", format!("Bearer {}", hex(&t)));
            }
            app.oneshot(request.body(Body::empty()).unwrap())
                .await
                .unwrap()
        }
    };
    assert_eq!(get(None).await.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(
        get(Some([0x1f; 32])).await.status(),
        StatusCode::UNAUTHORIZED
    );
    let ok = get(Some(operator)).await;
    assert_eq!(ok.status(), StatusCode::OK);
    let body = axum::body::to_bytes(ok.into_body(), usize::MAX)
        .await
        .unwrap();
    assert_eq!(body.as_ref(), export.as_slice());
    // A whole-database restore: every route gets a new generation.
    assert_eq!(source.relay.new_generation_everywhere().await.unwrap(), 1);
    assert_ne!(
        source
            .relay
            .records(&route(), None, 1)
            .await
            .unwrap()
            .generation,
        before.generation
    );
    source.finish().await;
    target.finish().await;
}
