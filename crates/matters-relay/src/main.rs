//! The relay server and its operator commands (#38, #44).
//!
//! ```text
//! matters-relay [serve]
//! matters-relay provision <route-hex> <root-key-file> <genesis-file>
//! matters-relay export <route-hex> <file>
//! matters-relay import <file>
//! matters-relay new-generation (<route-hex> | --all)
//! matters-relay quota <route-hex> (bytes | records) (<n> | none)
//! matters-relay probe [<host:port>]
//! ```
//!
//! - `MATTERS_RELAY_DATABASE_URL`: the PostgreSQL connection (required).
//! - `MATTERS_RELAY_LISTEN`: where `serve` listens (default 127.0.0.1:8484).
//!   It speaks HTTP/1.1 and cleartext HTTP/2; TLS terminates at a proxy.
//! - `MATTERS_RELAY_RATE`: `<per-second>/<burst>` per route and device,
//!   for authenticated requests; off when unset.
//! - `MATTERS_RELAY_PUBLIC_RATE`: the same per route, for requests without
//!   a session (default `10/60`; `off` turns it off).
//! - `MATTERS_RELAY_CHALLENGE_RATE`: session challenges per route and
//!   device (default `1/10`; `off`).
//! - `MATTERS_RELAY_HEADER_TIMEOUT`, `MATTERS_RELAY_REQUEST_TIMEOUT`:
//!   seconds a connection may take to send a request's headers (default
//!   10), and a request to be answered (default 60).
//! - `MATTERS_RELAY_MAX_REQUESTS`: requests handled at once (default 256);
//!   past it a request gets `503 unavailable` at once.
//! - `MATTERS_RELAY_OPERATOR_TOKEN`: 64 hex digits, the export credential.
//! - `MATTERS_RELAY_METRICS_LISTEN`: where to serve `GET /metrics`
//!   (OpenMetrics text), on its own listener; off when unset. Keep it on a
//!   private interface.
//!
//! `GET /healthz` answers while the process is up, and `GET /readyz` while
//! its database answers too. `probe` asks a running relay's `/readyz` and
//! exits 0 if it is ready (default 127.0.0.1:8484), for container health
//! checks. It needs no database URL.
//!
//! A route is a 96-digit lowercase hex of application, tenant and
//! collection. `new-generation --all` is the last step of restoring the
//! database from a backup.

use std::process::ExitCode;

use matters_relay::api::{self, Options, RateLimit};
use matters_relay::hints::Hints;
use matters_relay::server::Limits;
use matters_relay::Relay;
use matters_sync::authority::Route;
use matters_sync::record::VERIFICATION_KEY_BYTES;
use matters_sync::VerificationKey;

fn fail(message: impl std::fmt::Display) -> ExitCode {
    eprintln!("matters-relay: {message}");
    ExitCode::from(1)
}

fn usage() -> ExitCode {
    eprintln!(
        "usage: matters-relay [serve] | provision <route> <root-key> <genesis> | \
         export <route> <file> | import <file> | new-generation (<route> | --all) | \
         quota <route> (bytes | records) (<n> | none) | probe [<host:port>]"
    );
    ExitCode::from(2)
}

fn hex_bytes(text: &str) -> Option<Vec<u8>> {
    if !text.len().is_multiple_of(2) {
        return None;
    }
    (0..text.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(text.get(i..i + 2)?, 16).ok())
        .collect()
}

fn route(text: &str) -> Option<Route> {
    let bytes = hex_bytes(text)?;
    if bytes.len() != 48 {
        return None;
    }
    Some(Route {
        application: bytes[..16].try_into().ok()?,
        tenant: bytes[16..32].try_into().ok()?,
        collection: bytes[32..].try_into().ok()?,
    })
}

/// `probe [host:port]`: exit 0 if the relay there is ready. For container
/// health checks, where there is no shell or HTTP client.
async fn probe(addr: &str) -> ExitCode {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let attempt = async {
        let mut stream = tokio::net::TcpStream::connect(addr).await.ok()?;
        stream
            .write_all(b"GET /readyz HTTP/1.1\r\nHost: relay\r\nConnection: close\r\n\r\n")
            .await
            .ok()?;
        let mut head = [0u8; 12];
        stream.read_exact(&mut head).await.ok()?;
        Some(&head == b"HTTP/1.1 200")
    };
    match tokio::time::timeout(std::time::Duration::from_secs(3), attempt).await {
        Ok(Some(true)) => ExitCode::SUCCESS,
        _ => fail(format!("{addr} is not ready")),
    }
}

#[tokio::main]
async fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("probe") {
        return probe(args.get(1).map_or("127.0.0.1:8484", String::as_str)).await;
    }
    let Ok(url) = std::env::var("MATTERS_RELAY_DATABASE_URL") else {
        eprintln!("matters-relay: set MATTERS_RELAY_DATABASE_URL");
        return ExitCode::from(2);
    };
    let relay = match Relay::connect(&url).await {
        Ok(relay) => relay,
        Err(error) => return fail(error),
    };
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    match args.as_slice() {
        [] | ["serve"] => serve(relay).await,
        ["provision", r, key, genesis] => {
            let Some(r) = route(r) else { return usage() };
            let (Ok(key), Ok(genesis)) = (std::fs::read(key), std::fs::read(genesis)) else {
                return fail("cannot read the root key or genesis");
            };
            let Ok(key) = <[u8; VERIFICATION_KEY_BYTES]>::try_from(key.as_slice()) else {
                return fail("the root key must be a raw ML-DSA-65 verification key");
            };
            match relay
                .provision(&r, &VerificationKey::from_bytes(key), &genesis)
                .await
            {
                Ok(_) => ExitCode::SUCCESS,
                Err(error) => fail(error),
            }
        }
        ["export", r, file] => {
            let Some(r) = route(r) else { return usage() };
            match relay.export(&r).await {
                Ok(bytes) => match std::fs::write(file, bytes) {
                    Ok(()) => ExitCode::SUCCESS,
                    Err(error) => fail(error),
                },
                Err(error) => fail(error),
            }
        }
        ["import", file] => {
            let Ok(bytes) = std::fs::read(file) else {
                return fail("cannot read the export");
            };
            match relay.import(&bytes).await {
                Ok(_) => ExitCode::SUCCESS,
                Err(error) => fail(error),
            }
        }
        ["new-generation", "--all"] => match relay.new_generation_everywhere().await {
            Ok(n) => {
                eprintln!("matters-relay: new generation on {n} routes");
                ExitCode::SUCCESS
            }
            Err(error) => fail(error),
        },
        ["new-generation", r] => {
            let Some(r) = route(r) else { return usage() };
            match relay.new_generation(&r).await {
                Ok(_) => ExitCode::SUCCESS,
                Err(error) => fail(error),
            }
        }
        ["quota", r, what, n] => {
            let Some(r) = route(r) else { return usage() };
            let n = match *n {
                "none" => None,
                n => match n.parse::<u64>() {
                    Ok(n) => Some(n),
                    Err(_) => return usage(),
                },
            };
            let result = match *what {
                "bytes" => relay.set_quota(&r, n).await,
                "records" => relay.set_record_quota(&r, n).await,
                _ => return usage(),
            };
            match result {
                Ok(()) => ExitCode::SUCCESS,
                Err(error) => fail(error),
            }
        }
        _ => usage(),
    }
}

/// A rate limit from `var`: `<per-second>/<burst>`, or `off`; `default`
/// when unset.
fn rate_from_env(var: &str, default: Option<RateLimit>) -> Result<Option<RateLimit>, ExitCode> {
    let Ok(text) = std::env::var(var) else {
        return Ok(default);
    };
    if text == "off" {
        return Ok(None);
    }
    let parsed = text.split_once('/').and_then(|(rate, burst)| {
        Some(RateLimit {
            per_second: rate.parse().ok()?,
            burst: burst.parse().ok()?,
        })
    });
    match parsed {
        Some(limit) => Ok(Some(limit)),
        None => Err(fail(format!("{var} must be <per-second>/<burst> or off"))),
    }
}

async fn serve(relay: Relay) -> ExitCode {
    let listen = std::env::var("MATTERS_RELAY_LISTEN").unwrap_or_else(|_| "127.0.0.1:8484".into());
    let rate_limit = match rate_from_env("MATTERS_RELAY_RATE", None) {
        Ok(limit) => limit,
        Err(code) => return code,
    };
    // Requests without a session are on by default: anyone can send them.
    let public_rate_limit = match rate_from_env(
        "MATTERS_RELAY_PUBLIC_RATE",
        Some(RateLimit {
            per_second: 10,
            burst: 60,
        }),
    ) {
        Ok(limit) => limit,
        Err(code) => return code,
    };
    let challenge_rate_limit = match rate_from_env(
        "MATTERS_RELAY_CHALLENGE_RATE",
        Some(RateLimit {
            per_second: 1,
            burst: 10,
        }),
    ) {
        Ok(limit) => limit,
        Err(code) => return code,
    };
    let limits = match limits() {
        Ok(limits) => limits,
        Err(code) => return code,
    };
    let operator_token = match std::env::var("MATTERS_RELAY_OPERATOR_TOKEN") {
        Err(_) => None,
        Ok(text) => match hex_bytes(&text).and_then(|b| <[u8; 32]>::try_from(b).ok()) {
            Some(token) => Some(token),
            None => return fail("MATTERS_RELAY_OPERATOR_TOKEN must be 64 hex digits"),
        },
    };
    let hints = match Hints::start(&relay).await {
        Ok(hints) => hints,
        Err(error) => return fail(error),
    };
    let listener = match tokio::net::TcpListener::bind(&listen).await {
        Ok(listener) => listener,
        Err(error) => return fail(format!("cannot listen on {listen}: {error}")),
    };
    // The bound address, for a caller that asked for port 0.
    match listener.local_addr() {
        Ok(addr) => eprintln!("matters-relay: listening on {addr}"),
        Err(_) => eprintln!("matters-relay: listening on {listen}"),
    }
    let metrics = match std::env::var("MATTERS_RELAY_METRICS_LISTEN") {
        Err(_) => None,
        Ok(address) => {
            let metrics = std::sync::Arc::new(matters_relay::metrics::Metrics::new());
            let listener = match tokio::net::TcpListener::bind(&address).await {
                Ok(listener) => listener,
                Err(error) => return fail(format!("cannot listen on {address}: {error}")),
            };
            eprintln!("matters-relay: metrics on {address}");
            let app = matters_relay::metrics::router(relay.clone(), metrics.clone());
            tokio::spawn(matters_relay::server::serve(
                listener,
                app,
                Limits::default(),
                std::future::pending(),
            ));
            Some(metrics)
        }
    };
    let app = api::router_with(
        relay,
        api::system_clock(),
        Options {
            hints: Some(hints),
            rate_limit,
            public_rate_limit,
            challenge_rate_limit,
            operator_token,
            metrics,
        },
    );
    let shutdown = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    match matters_relay::server::serve(listener, app, limits, shutdown).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => fail(error),
    }
}

/// The server's limits from the environment, defaults where unset.
fn limits() -> Result<Limits, ExitCode> {
    let mut limits = Limits::default();
    let number = |var: &str| -> Result<Option<u64>, ExitCode> {
        match std::env::var(var) {
            Err(_) => Ok(None),
            Ok(text) => match text.parse::<u64>() {
                Ok(n) if n > 0 => Ok(Some(n)),
                _ => Err(fail(format!("{var} must be a positive whole number"))),
            },
        }
    };
    if let Some(s) = number("MATTERS_RELAY_HEADER_TIMEOUT")? {
        limits.header_timeout = std::time::Duration::from_secs(s);
    }
    if let Some(s) = number("MATTERS_RELAY_REQUEST_TIMEOUT")? {
        limits.request_timeout = std::time::Duration::from_secs(s);
    }
    if let Some(n) = number("MATTERS_RELAY_MAX_REQUESTS")? {
        limits.max_requests = usize::try_from(n).unwrap_or(usize::MAX);
    }
    Ok(limits)
}
