//! The relay's HTTP server (#45): the API router on hyper, with the limits
//! `axum::serve` does not set. It sets no timer, so a connection could take
//! as long as it liked to send its headers, and nothing bounds how many
//! requests run at once or for how long.
//!
//! - **Headers.** A connection must send its first bytes, and each
//!   request's headers, within [`Limits::header_timeout`] (HTTP/1.1; HTTP/2
//!   connections are kept alive by pings instead).
//! - **Requests.** A request must be answered, its body read, within
//!   [`Limits::request_timeout`], or it gets `503 unavailable`. A WebSocket
//!   counts until its upgrade.
//! - **Concurrency.** At most [`Limits::max_requests`] requests run at once.
//!   Past that a request is shed at once with `503 unavailable`, never
//!   queued; health checks are exempt.

use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use axum::extract::{Request, State};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::Router;
use hyper_util::rt::{TokioExecutor, TokioIo, TokioTimer};
use hyper_util::server::conn::auto::Builder;
use hyper_util::service::TowerToHyperService;
use tokio::net::TcpListener;
use tokio::sync::{watch, Semaphore};

use crate::api::ApiError;
use crate::RelayError;

/// The server's limits.
#[derive(Debug, Clone, Copy)]
pub struct Limits {
    /// How long a connection may take to send its first bytes, and each
    /// HTTP/1.1 request's headers.
    pub header_timeout: Duration,
    /// How long a request may take, its body included, to be answered.
    pub request_timeout: Duration,
    /// The most requests handled at once.
    pub max_requests: usize,
}

impl Default for Limits {
    /// 10 s for headers, 60 s for a request (a full record over a slow
    /// link), 256 requests at once (the database pool serves 16).
    fn default() -> Self {
        Self {
            header_timeout: Duration::from_secs(10),
            request_timeout: Duration::from_secs(60),
            max_requests: 256,
        }
    }
}

fn unavailable() -> Response {
    ApiError::from(RelayError::Unavailable(sqlx::Error::PoolTimedOut)).into_response()
}

fn exempt(request: &Request) -> bool {
    matches!(request.uri().path(), "/healthz" | "/readyz")
}

async fn shed(State(permits): State<Arc<Semaphore>>, request: Request, next: Next) -> Response {
    if exempt(&request) {
        return next.run(request).await;
    }
    match permits.try_acquire_owned() {
        Ok(_permit) => next.run(request).await,
        Err(_) => unavailable(),
    }
}

async fn deadline(State(limit): State<Duration>, request: Request, next: Next) -> Response {
    tokio::time::timeout(limit, next.run(request))
        .await
        .unwrap_or_else(|_| unavailable())
}

/// `app` with the request timeout and the concurrency limit.
pub fn limited(app: Router, limits: &Limits) -> Router {
    app.layer(axum::middleware::from_fn_with_state(
        limits.request_timeout,
        deadline,
    ))
    .layer(axum::middleware::from_fn_with_state(
        Arc::new(Semaphore::new(limits.max_requests.max(1))),
        shed,
    ))
}

/// Serve `app` on `listener` with `limits` until `shutdown`, then let open
/// connections finish their requests.
pub async fn serve(
    listener: TcpListener,
    app: Router,
    limits: Limits,
    shutdown: impl Future<Output = ()>,
) -> std::io::Result<()> {
    let app = limited(app, &limits);
    let (signal, signalled) = watch::channel(());
    let (closed, open) = watch::channel(());
    let mut shutdown = std::pin::pin!(shutdown);
    loop {
        let stream = tokio::select! {
            accepted = listener.accept() => match accepted {
                Ok((stream, _)) => stream,
                // Out of descriptors or a reset: let others finish first.
                Err(_) => {
                    tokio::time::sleep(Duration::from_millis(50)).await;
                    continue;
                }
            },
            () = &mut shutdown => break,
        };
        let (app, mut signalled, open) = (app.clone(), signalled.clone(), open.clone());
        tokio::spawn(async move {
            let _open = open;
            // The protocol is told from the first bytes, before hyper's own
            // timers start: a peer that sends none is dropped.
            let mut first = [0u8; 1];
            match tokio::time::timeout(limits.header_timeout, stream.peek(&mut first)).await {
                Ok(Ok(n)) if n > 0 => {}
                _ => return,
            }
            let mut builder = Builder::new(TokioExecutor::new());
            builder
                .http1()
                .timer(TokioTimer::new())
                .header_read_timeout(limits.header_timeout);
            builder
                .http2()
                .timer(TokioTimer::new())
                // WebSockets over HTTP/2.
                .enable_connect_protocol()
                .keep_alive_interval(Duration::from_secs(30))
                .keep_alive_timeout(Duration::from_secs(20));
            let connection = builder.serve_connection_with_upgrades(
                TokioIo::new(stream),
                TowerToHyperService::new(app),
            );
            let mut connection = std::pin::pin!(connection);
            tokio::select! {
                _ = connection.as_mut() => return,
                _ = signalled.changed() => connection.as_mut().graceful_shutdown(),
            }
            let _ = connection.await;
        });
    }
    drop(listener);
    let _ = signal.send(());
    drop(open);
    closed.closed().await;
    Ok(())
}
