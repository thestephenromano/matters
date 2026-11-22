//! Commit hints (docs/SYNC_SERVICE_CONTRACT.md "Commit hints"): an append
//! notifies `CHANNEL` inside its transaction, which PostgreSQL delivers
//! only on commit, so a hint never precedes its record. One listener per
//! relay process fans hints out to WebSocket subscribers. Hints may be
//! late, coalesced or lost (a lagging subscriber skips them); clients
//! always fetch by cursor, so nothing depends on them.

use sqlx::postgres::PgListener;
use tokio::sync::broadcast;

use crate::{Relay, RelayError};

pub(crate) const CHANNEL: &str = "matters_relay_hints";

/// A committed record's place: route row, sequence, generation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Hint {
    pub(crate) route: i64,
    pub sequence: u64,
    pub generation: [u8; 16],
}

pub(crate) fn payload(route: i64, sequence: u64, generation: &[u8; 16]) -> String {
    let generation: String = generation.iter().map(|b| format!("{b:02x}")).collect();
    format!("{route}:{sequence}:{generation}")
}

pub(crate) fn parse(payload: &str) -> Option<Hint> {
    let mut parts = payload.split(':');
    let route = parts.next()?.parse().ok()?;
    let sequence = parts.next()?.parse().ok()?;
    let hex = parts.next()?;
    // ASCII hex only: the length is in bytes, and slicing a multibyte
    // character would panic the listener task.
    if parts.next().is_some() || hex.len() != 32 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let mut generation = [0; 16];
    for (i, byte) in generation.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&hex[2 * i..2 * i + 2], 16).ok()?;
    }
    Some(Hint {
        route,
        sequence,
        generation,
    })
}

/// The relay process's hint fan-out.
#[derive(Clone)]
pub struct Hints {
    sender: broadcast::Sender<Hint>,
}

impl Hints {
    /// Listen for committed appends and fan them out. The listener holds
    /// one of the pool's connections, reconnects by itself (hints sent while
    /// it is away are lost) and ends when the pool closes.
    pub async fn start(relay: &Relay) -> Result<Self, RelayError> {
        let mut listener = PgListener::connect_with(relay.pool()).await?;
        listener.listen(CHANNEL).await?;
        let (sender, _) = broadcast::channel(1024);
        let fanout = sender.clone();
        tokio::spawn(async move {
            loop {
                match listener.recv().await {
                    Ok(notification) => {
                        if let Some(hint) = parse(notification.payload()) {
                            // No subscribers is not an error.
                            let _ = fanout.send(hint);
                        }
                    }
                    // The relay is shutting down: return the connection.
                    Err(sqlx::Error::PoolClosed) => return,
                    Err(_) => tokio::time::sleep(std::time::Duration::from_millis(500)).await,
                }
            }
        });
        Ok(Self { sender })
    }

    pub(crate) fn subscribe(&self) -> broadcast::Receiver<Hint> {
        self.sender.subscribe()
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn payloads_round_trip_and_refuse_garbage() {
        let hint = super::parse(&super::payload(7, 42, &[0xab; 16])).unwrap();
        assert_eq!(
            (hint.route, hint.sequence, hint.generation),
            (7, 42, [0xab; 16])
        );
        for bad in [
            "",
            "7:42",
            "7:42:ab",
            "x:42:abababababababababababababababab",
            "7:42:abababababababababababababababab:1",
            // 32 bytes, but not 32 ASCII digits: never sliced mid-character.
            "7:42:éééééééééééééééé",
            "7:42:abababababababababababababababé",
            "7:42:+bababababababababababababababab",
        ] {
            assert!(super::parse(bad).is_none(), "{bad}");
        }
    }
}
