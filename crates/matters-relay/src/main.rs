//! The relay server: the v0 API over HTTP/1.1 and cleartext HTTP/2. TLS
//! terminates at a proxy in front (self-hosting, #44).
//!
//! - `MATTERS_RELAY_DATABASE_URL`: the PostgreSQL connection (required).
//! - `MATTERS_RELAY_LISTEN`: the address to listen on (default
//!   127.0.0.1:8484).

#[tokio::main]
async fn main() -> std::process::ExitCode {
    let Ok(url) = std::env::var("MATTERS_RELAY_DATABASE_URL") else {
        eprintln!("matters-relay: set MATTERS_RELAY_DATABASE_URL");
        return std::process::ExitCode::from(2);
    };
    let listen = std::env::var("MATTERS_RELAY_LISTEN").unwrap_or_else(|_| "127.0.0.1:8484".into());
    let relay = match matters_relay::Relay::connect(&url).await {
        Ok(relay) => relay,
        Err(error) => {
            eprintln!("matters-relay: {error}");
            return std::process::ExitCode::from(1);
        }
    };
    let listener = match tokio::net::TcpListener::bind(&listen).await {
        Ok(listener) => listener,
        Err(error) => {
            eprintln!("matters-relay: cannot listen on {listen}: {error}");
            return std::process::ExitCode::from(1);
        }
    };
    eprintln!("matters-relay: listening on {listen}");
    let app = matters_relay::api::router(relay, matters_relay::api::system_clock());
    let shutdown = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    match axum::serve(listener, app)
        .with_graceful_shutdown(shutdown)
        .await
    {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("matters-relay: {error}");
            std::process::ExitCode::from(1)
        }
    }
}
