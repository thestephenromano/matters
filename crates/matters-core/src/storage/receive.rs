//! Receiving other devices' sealed records (#39, docs/SYNC_RECORD_CONTRACT.md
//! "Receiver and relay obligations").
//!
//! [`Database::receive_record`] checks each record in this order before it
//! changes anything:
//! 1. its route and document;
//! 2. that it is not already applied (by record ID, this device's own
//!    included);
//! 3. that this device holds the key of the epoch it was sealed in;
//! 4. its signature, its author's membership in that epoch and its writer's
//!    admission (`Authority::authenticate_at`);
//! 5. its place in its writer's signed chain, and that it is not past the
//!    writer's closing head;
//! 6. that its engine update carries only the writer's admitted actor.
//!
//! The record then applies in one transaction: journalled as received,
//! merged, the rows rebuilt and the writer's chain head advanced.
//!
//! A record that cannot apply yet is kept in `sync_pending` and retried
//! whenever something it may have waited for arrives: a record, or a
//! writer claim, transition or envelope through `apply_control`. One that can never apply (past its writer's close,
//! or under a frozen lineage) is kept there too, never applied and never
//! discarded. Forged or malformed bytes are refused and not kept.

use std::collections::BTreeSet;

use matters_sync::authority::{self, Boundary, Hash};
use matters_sync::record::record_hash;
use matters_sync::wire;
use rusqlite::{params, Connection, OptionalExtension};

use super::db::Database;
use super::membership::{Joined, HOME_DOCUMENT};
use super::sync::{load_engine, rebuild_rows};
use crate::crdt::Document;
use crate::error::{MattersError, Result};

/// What receiving a record did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Received {
    Applied,
    /// Already applied, or this device's own.
    Duplicate,
    /// Kept, waiting.
    Pending(Waiting),
}

/// Why a record waits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Waiting {
    /// For this device's envelope of the record's epoch key.
    Key,
    /// For its writer's admission.
    Writer,
    /// For the record before it in its writer's chain.
    Predecessor,
    /// Never: it follows its writer's signed closing head.
    AfterClose,
    /// Never, unless recovered: its writer's lineage froze on a fork.
    Frozen,
}

impl Waiting {
    fn as_str(self) -> &'static str {
        match self {
            Self::Key => "key",
            Self::Writer => "writer",
            Self::Predecessor => "predecessor",
            Self::AfterClose => "after-close",
            Self::Frozen => "frozen",
        }
    }
}

fn refused(what: &str) -> MattersError {
    MattersError::Validation(format!("record refused: {what}"))
}

/// How one attempt ended, before anything was written.
enum Attempt {
    Duplicate,
    Wait(Waiting),
    /// Authenticated, in order, opened and checked: ready to apply.
    Ready {
        record_id: [u8; 32],
        incarnation: [u8; 32],
        update: Vec<u8>,
    },
}

fn known(conn: &Connection, record_id: &[u8; 32]) -> Result<bool> {
    let own: Option<i64> = conn
        .query_row(
            "SELECT 1 FROM sync_records WHERE record_id = ?1",
            params![record_id.as_slice()],
            |r| r.get(0),
        )
        .optional()?;
    let received: Option<i64> = conn
        .query_row(
            "SELECT 1 FROM sync_received WHERE record_id = ?1",
            params![record_id.as_slice()],
            |r| r.get(0),
        )
        .optional()?;
    Ok(own.is_some() || received.is_some())
}

fn attempt(conn: &Connection, joined: &Joined, bytes: &[u8]) -> Result<Attempt> {
    let route = &joined.route;
    let parsed =
        wire::parse(bytes, route.content(&HOME_DOCUMENT)).map_err(|e| refused(&e.to_string()))?;
    let h = parsed.header;
    if known(conn, h.record_id)? {
        return Ok(Attempt::Duplicate);
    }
    let envelope: Option<Vec<u8>> = conn
        .query_row(
            "SELECT bytes FROM sync_envelopes WHERE manifest = ?1",
            params![h.manifest.as_slice()],
            |r| r.get(0),
        )
        .optional()?;
    let Some(envelope) = envelope else {
        return Ok(Attempt::Wait(Waiting::Key));
    };
    let key = joined
        .authority
        .unwrap_history_key(&envelope, &joined.device)?;
    let record_id = *h.record_id;
    let incarnation = *h.incarnation;
    let previous = h.previous.copied();
    let authenticated = match joined
        .authority
        .authenticate_at(bytes, &key, &HOME_DOCUMENT)
    {
        Ok(a) => a,
        Err(authority::Error::MissingWriter) => return Ok(Attempt::Wait(Waiting::Writer)),
        Err(authority::Error::Fork) => return Ok(Attempt::Wait(Waiting::Frozen)),
        Err(error) => return Err(refused(&error.to_string())),
    };
    let head: Option<Vec<u8>> = conn
        .query_row(
            "SELECT head FROM sync_receive_heads WHERE incarnation = ?1",
            params![incarnation.as_slice()],
            |r| r.get(0),
        )
        .optional()?;
    let head: Option<Hash> = head
        .map(|h| h.try_into())
        .transpose()
        .map_err(|_| refused("stored chain head"))?;
    // A retired writer's records end at its signed closing head.
    if let Ok(history) = joined.authority.writer_history(HOME_DOCUMENT, incarnation) {
        if let Some(Boundary::Closed(closing)) = history.closed {
            if head == closing {
                return Ok(Attempt::Wait(Waiting::AfterClose));
            }
        }
    }
    if previous != head {
        return Ok(Attempt::Wait(Waiting::Predecessor));
    }
    let update = authenticated.open().map_err(|e| refused(&e.to_string()))?;
    let peers = crate::crdt::update_peers(&update)?;
    if peers != BTreeSet::from([authenticated.actor()]) {
        return Err(refused(
            "its update carries another actor than its writer's",
        ));
    }
    Ok(Attempt::Ready {
        record_id,
        incarnation,
        update,
    })
}

/// Apply a ready record in one transaction.
fn apply(
    conn: &mut Connection,
    engine: &mut Option<Document>,
    bytes: &[u8],
    record_id: [u8; 32],
    incarnation: [u8; 32],
    update: &[u8],
) -> Result<()> {
    let Some(doc) = engine.as_mut() else {
        return Err(MattersError::Validation("sync is off".into()));
    };
    let tx = conn.transaction()?;
    let applied = (|| {
        tx.execute(
            "INSERT INTO sync_journal (bytes, outgoing) VALUES (?1, 0)",
            params![update],
        )?;
        doc.merge(update)?;
        rebuild_rows(&tx, &doc.project()?)?;
        tx.execute(
            "INSERT INTO sync_received (record_id, incarnation, digest) VALUES (?1, ?2, ?3)",
            params![
                record_id.as_slice(),
                incarnation.as_slice(),
                record_hash(bytes).as_slice()
            ],
        )?;
        tx.execute(
            "INSERT INTO sync_receive_heads (incarnation, head) VALUES (?1, ?2)
             ON CONFLICT (incarnation) DO UPDATE SET head = excluded.head",
            params![incarnation.as_slice(), record_hash(bytes).as_slice()],
        )?;
        tx.execute(
            "DELETE FROM sync_pending WHERE record_id = ?1",
            params![record_id.as_slice()],
        )?;
        super::sync::crash_point("receive-before-commit");
        Ok::<_, MattersError>(tx.commit()?)
    })();
    if let Err(error) = applied {
        *engine = load_engine(conn)?;
        return Err(error);
    }
    Ok(())
}

/// The record ID of bytes that parse for this collection's document.
fn record_id(joined: &Joined, bytes: &[u8]) -> Result<[u8; 32]> {
    let parsed = wire::parse(bytes, joined.route.content(&HOME_DOCUMENT))
        .map_err(|e| refused(&e.to_string()))?;
    Ok(*parsed.header.record_id)
}

fn keep(conn: &Connection, joined: &Joined, bytes: &[u8], waiting: Waiting) -> Result<()> {
    let id = record_id(joined, bytes)?;
    conn.execute(
        "INSERT INTO sync_pending (record_id, bytes, reason, seq)
         VALUES (?1, ?2, ?3, (SELECT COALESCE(MAX(seq), 0) + 1 FROM sync_pending))
         ON CONFLICT (record_id) DO UPDATE SET reason = excluded.reason",
        params![id.as_slice(), bytes, waiting.as_str()],
    )?;
    Ok(())
}

/// Retry the pending records until a pass applies none. Each pass that
/// applies something shortens the chains it waited on, so at most one pass
/// per pending record, and one more, can make progress.
pub(super) fn drain(
    conn: &mut Connection,
    engine: &mut Option<Document>,
    joined: &Joined,
) -> Result<()> {
    let passes: i64 = conn.query_row("SELECT COUNT(*) FROM sync_pending", [], |r| r.get(0))?;
    for _ in 0..=passes {
        let pending: Vec<(Vec<u8>, String)> = {
            let mut stmt = conn.prepare("SELECT bytes, reason FROM sync_pending ORDER BY seq")?;
            let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
            rows.collect::<rusqlite::Result<_>>()?
        };
        let mut progress = false;
        for (bytes, reason) in pending {
            if reason == Waiting::AfterClose.as_str() {
                continue;
            }
            match attempt(conn, joined, &bytes)? {
                Attempt::Ready {
                    record_id,
                    incarnation,
                    update,
                } => {
                    apply(conn, engine, &bytes, record_id, incarnation, &update)?;
                    progress = true;
                }
                Attempt::Duplicate => {
                    let id = record_id(joined, &bytes)?;
                    conn.execute(
                        "DELETE FROM sync_pending WHERE record_id = ?1",
                        params![id.as_slice()],
                    )?;
                }
                Attempt::Wait(waiting) => keep(conn, joined, &bytes, waiting)?,
            }
        }
        if !progress {
            break;
        }
    }
    Ok(())
}

impl Database {
    /// Receive one sealed record fetched from the relay. Applies it if it
    /// can, keeps it pending if it must wait, and refuses forged or
    /// malformed bytes. Then retries whatever was pending.
    pub fn receive_record(&self, bytes: &[u8]) -> Result<Received> {
        self.with_sealing(|conn, engine, sync| {
            let joined = sync
                .membership
                .as_ref()
                .ok_or_else(|| MattersError::Validation("sync is not resumed".into()))?;
            let outcome = match attempt(conn, joined, bytes)? {
                Attempt::Duplicate => Received::Duplicate,
                Attempt::Wait(waiting) => {
                    keep(conn, joined, bytes, waiting)?;
                    Received::Pending(waiting)
                }
                Attempt::Ready {
                    record_id,
                    incarnation,
                    update,
                } => {
                    apply(conn, engine, bytes, record_id, incarnation, &update)?;
                    Received::Applied
                }
            };
            drain(conn, engine, joined)?;
            Ok(outcome)
        })
    }

    /// The records kept pending, with why, oldest first.
    pub fn pending_records(&self) -> Result<Vec<([u8; 32], String)>> {
        self.with_conn(|conn| {
            let mut stmt =
                conn.prepare("SELECT record_id, reason FROM sync_pending ORDER BY seq")?;
            let rows = stmt.query_map([], |r| Ok((r.get::<_, Vec<u8>>(0)?, r.get(1)?)))?;
            let mut out = Vec::new();
            for row in rows {
                let (id, reason) = row?;
                out.push((
                    id.try_into()
                        .map_err(|_| MattersError::Validation("stored record ID".into()))?,
                    reason,
                ));
            }
            Ok(out)
        })
    }
}
