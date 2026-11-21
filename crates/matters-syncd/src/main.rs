//! Headless Matters sync (#39): syncs the per-user database with a relay,
//! with no interface.
//!
//! ```text
//! MATTERS_SYNC_ENDPOINT=https://relay.example matters-syncd
//! ```
//!
//! It opens the database at `Database::default_path()` and loads this
//! device's keys from the system keyring (oo7). It resumes sync, then runs
//! rounds until interrupted, printing each status change. The device must
//! already have joined a collection. Exits non-zero, changing nothing, if
//! the database has not joined one or its keys are missing.

use std::process::ExitCode;
use std::sync::Arc;

use matters_core::Database;
use matters_keyring::Oo7KeyStore;
use matters_sync_client::{Client, Status};

fn fail(message: impl std::fmt::Display) -> ExitCode {
    eprintln!("matters-syncd: {message}");
    ExitCode::from(1)
}

#[tokio::main]
async fn main() -> ExitCode {
    let Ok(endpoint) = std::env::var("MATTERS_SYNC_ENDPOINT") else {
        eprintln!("matters-syncd: set MATTERS_SYNC_ENDPOINT to the relay's URL");
        return ExitCode::from(2);
    };
    let db = match Database::open_default() {
        Ok(db) => db,
        Err(error) => return fail(error),
    };
    let keys = match Oo7KeyStore::open() {
        Ok(keys) => keys,
        Err(error) => return fail(error),
    };
    if let Err(error) = db.resume_sync(&keys) {
        return fail(error);
    }
    let client = match Client::new(&endpoint, Arc::new(db)) {
        Ok(client) => Arc::new(client),
        Err(error) => return fail(error),
    };
    let mut status = client.status();
    tokio::spawn(async move {
        while status.changed().await.is_ok() {
            let now = status.borrow().clone();
            match now {
                Status::Syncing => {}
                other => eprintln!("matters-syncd: {other:?}"),
            }
        }
    });
    client
        .run(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await;
    ExitCode::SUCCESS
}
