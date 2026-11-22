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
//!
//! A `snapshot` record is a checkpoint part (#40). It is authenticated and
//! opened the same way, outside any chain, and handed to the checkpoint
//! module (`checkpoint.rs`), which keeps it until its checkpoint is
//! complete.

use std::collections::BTreeSet;

use matters_sync::authority::{self, Boundary, Hash};
use matters_sync::checkpoint::Sealed;
use matters_sync::record::record_hash;
use matters_sync::wire;
use rusqlite::{params, Connection, OptionalExtension};

use super::checkpoint::{self, Checkpointed};
use super::db::Database;
use super::membership::{Joined, HOME_DOCUMENT};
use super::sync::{load_engine, rebuild_rows};
use crate::crdt::Document;
use crate::error::{MattersError, Result};

/// What receiving a record did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Received {
    Applied,
    /// Already applied, or this device's own.
    Duplicate,
    /// Kept, waiting.
    Pending(Waiting),
    /// A checkpoint part, and what became of its checkpoint.
    Checkpoint(Checkpointed),
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
    /// A checkpoint part, authenticated and opened.
    Part {
        record_id: [u8; 32],
        sealed: Sealed,
        plaintext: Vec<u8>,
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
            "SELECT 1 FROM sync_received WHERE record_id = ?1
             UNION ALL SELECT 1 FROM sync_covered WHERE record_id = ?1
             UNION ALL SELECT 1 FROM sync_checkpoint_out WHERE record_id = ?1
             UNION ALL SELECT 1 FROM sync_checkpoint_parts WHERE record_id = ?1",
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
    if h.kind == wire::Kind::Snapshot {
        let sealed = Sealed {
            author: *h.author,
            incarnation,
            epoch: h.epoch,
        };
        let authenticated =
            match joined
                .authority
                .authenticate_snapshot_at(bytes, &key, &HOME_DOCUMENT)
            {
                Ok(a) => a,
                Err(authority::Error::MissingWriter) => return Ok(Attempt::Wait(Waiting::Writer)),
                Err(authority::Error::Fork) => return Ok(Attempt::Wait(Waiting::Frozen)),
                Err(error) => return Err(refused(&error.to_string())),
            };
        let plaintext = authenticated.open().map_err(|e| refused(&e.to_string()))?;
        return Ok(Attempt::Part {
            record_id,
            sealed,
            plaintext,
        });
    }
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

/// A checkpoint part's record ID, the header facts of its record, and its
/// plaintext.
pub(super) type OpenedPart = ([u8; 32], Sealed, Vec<u8>);

/// Authenticate and open a checkpoint part without keeping it: `None` when
/// it must wait for its key or its writer.
pub(super) fn open_part(
    conn: &Connection,
    joined: &Joined,
    bytes: &[u8],
) -> Result<Option<OpenedPart>> {
    let parsed = wire::parse(bytes, joined.route.content(&HOME_DOCUMENT))
        .map_err(|e| refused(&e.to_string()))?;
    if parsed.header.kind != wire::Kind::Snapshot {
        return Err(refused("not a checkpoint part"));
    }
    let id = *parsed.header.record_id;
    // Not known yet on a device that holds nothing; checked anyway.
    let _ = known(conn, &id)?;
    let envelope: Option<Vec<u8>> = conn
        .query_row(
            "SELECT bytes FROM sync_envelopes WHERE manifest = ?1",
            params![parsed.header.manifest.as_slice()],
            |r| r.get(0),
        )
        .optional()?;
    let Some(envelope) = envelope else {
        return Ok(None);
    };
    let key = joined
        .authority
        .unwrap_history_key(&envelope, &joined.device)?;
    let sealed = Sealed {
        author: *parsed.header.author,
        incarnation: *parsed.header.incarnation,
        epoch: parsed.header.epoch,
    };
    let authenticated = match joined
        .authority
        .authenticate_snapshot_at(bytes, &key, &HOME_DOCUMENT)
    {
        Ok(a) => a,
        Err(authority::Error::MissingWriter) => return Ok(None),
        Err(error) => return Err(refused(&error.to_string())),
    };
    let plaintext = authenticated.open().map_err(|e| refused(&e.to_string()))?;
    Ok(Some((id, sealed, plaintext)))
}

/// Apply a ready record in the caller's transaction: journalled, merged
/// into the engine, recorded as received and its chain head advanced. The
/// rows are rebuilt once, before the commit, by the caller (#241).
fn apply(
    tx: &Connection,
    doc: &mut Document,
    bytes: &[u8],
    record_id: [u8; 32],
    incarnation: [u8; 32],
    update: &[u8],
) -> Result<()> {
    tx.execute(
        "INSERT INTO sync_journal (bytes, outgoing) VALUES (?1, 0)",
        params![update],
    )?;
    doc.merge_unindexed(update)?;
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
    Ok(())
}

/// Act on one attempt in the caller's transaction. `changed` is set when
/// the engine took an update, so the rows must be rebuilt.
fn settle(
    tx: &Connection,
    engine: &mut Option<Document>,
    joined: &Joined,
    bytes: &[u8],
    attempt: Attempt,
    changed: &mut bool,
) -> Result<Received> {
    Ok(match attempt {
        Attempt::Duplicate => {
            let id = record_id(joined, bytes)?;
            tx.execute(
                "DELETE FROM sync_pending WHERE record_id = ?1",
                params![id.as_slice()],
            )?;
            Received::Duplicate
        }
        Attempt::Wait(waiting) => {
            keep(tx, joined, bytes, waiting)?;
            Received::Pending(waiting)
        }
        Attempt::Ready {
            record_id,
            incarnation,
            update,
        } => {
            let doc = engine
                .as_mut()
                .ok_or_else(|| MattersError::Validation("sync is off".into()))?;
            apply(tx, doc, bytes, record_id, incarnation, &update)?;
            *changed = true;
            Received::Applied
        }
        Attempt::Part {
            record_id,
            sealed,
            plaintext,
        } => Received::Checkpoint(checkpoint::keep_part(
            tx, engine, joined, &record_id, &sealed, &plaintext,
        )?),
    })
}

/// Receive `records` in order, then retry what was pending, all in one
/// transaction: the rows are rebuilt once, however many records applied
/// (#241). A refused record is that record's error; the others go on. Any
/// other failure rolls the whole batch back, and the engine is reloaded.
fn receive_all(
    conn: &mut Connection,
    engine: &mut Option<Document>,
    joined: &Joined,
    records: &[&[u8]],
) -> Result<Vec<Result<Received>>> {
    let tx = conn.transaction()?;
    let received = (|| {
        let mut changed = false;
        let mut outcomes = Vec::with_capacity(records.len());
        for bytes in records {
            outcomes.push(match attempt(&tx, joined, bytes) {
                Ok(a) => Ok(settle(&tx, engine, joined, bytes, a, &mut changed)?),
                Err(refused) => Err(refused),
            });
        }
        drain_in(&tx, engine, joined, &mut changed)?;
        if changed {
            let doc = engine
                .as_ref()
                .ok_or_else(|| MattersError::Validation("sync is off".into()))?;
            rebuild_rows(&tx, &doc.project()?)?;
        }
        Ok::<_, MattersError>(outcomes)
    })();
    match received {
        Ok(outcomes) => {
            super::sync::crash_point("receive-before-commit");
            match tx.commit() {
                Ok(()) => Ok(outcomes),
                Err(error) => {
                    *engine = load_engine(conn)?;
                    Err(error.into())
                }
            }
        }
        Err(error) => {
            drop(tx);
            *engine = load_engine(conn)?;
            Err(error)
        }
    }
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

/// Retry the pending records until a pass applies none, in the caller's
/// transaction. Each pass that applies something shortens the chains it
/// waited on, so at most one pass per pending record, and one more, can
/// make progress.
fn drain_in(
    tx: &Connection,
    engine: &mut Option<Document>,
    joined: &Joined,
    changed: &mut bool,
) -> Result<()> {
    checkpoint::walk_covered(tx, joined)?;
    let passes: i64 = tx.query_row("SELECT COUNT(*) FROM sync_pending", [], |r| r.get(0))?;
    for _ in 0..=passes {
        let pending: Vec<(Vec<u8>, String)> = {
            let mut stmt = tx.prepare("SELECT bytes, reason FROM sync_pending ORDER BY seq")?;
            let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
            rows.collect::<rusqlite::Result<_>>()?
        };
        let mut progress = false;
        for (bytes, reason) in pending {
            if reason == Waiting::AfterClose.as_str() {
                continue;
            }
            let attempt = attempt(tx, joined, &bytes)?;
            let applied = matches!(attempt, Attempt::Ready { .. });
            settle(tx, engine, joined, &bytes, attempt, changed)?;
            progress |= applied;
        }
        if !progress {
            break;
        }
    }
    checkpoint::walk_covered(tx, joined)?;
    checkpoint::finish_waiting(tx, engine, joined)?;
    Ok(())
}

/// Retry what is pending, as after a control or a start from a checkpoint,
/// in one transaction.
pub(super) fn drain(
    conn: &mut Connection,
    engine: &mut Option<Document>,
    joined: &Joined,
) -> Result<()> {
    receive_all(conn, engine, joined, &[]).map(|_| ())
}

impl Database {
    /// Receive one sealed record fetched from the relay. Applies it if it
    /// can, keeps it pending if it must wait, and refuses forged or
    /// malformed bytes. Then retries whatever was pending.
    pub fn receive_record(&self, bytes: &[u8]) -> Result<Received> {
        self.receive_records(&[bytes])?
            .pop()
            .expect("one outcome for one record")
    }

    /// Receive a page of records, in order, as `receive_record` does each,
    /// in one transaction with one rebuild of the rows (#241). Each record
    /// has its own outcome: a refused one is an error, and the others go on.
    pub fn receive_records(&self, records: &[&[u8]]) -> Result<Vec<Result<Received>>> {
        self.with_sealing(|conn, engine, sync| {
            let joined = sync
                .membership
                .as_ref()
                .ok_or_else(|| MattersError::Validation("sync is not resumed".into()))?;
            if engine.is_none() {
                return Err(MattersError::Validation("sync is off".into()));
            }
            receive_all(conn, engine, joined, records)
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
