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
