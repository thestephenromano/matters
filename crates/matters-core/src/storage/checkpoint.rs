//! Checkpoints in Matters' database (#40, docs/SYNC_CHECKPOINTS.md).
//!
//! [`Database::create_checkpoint`]:
//! - is called by a fully synced member;
//! - snapshots the document with a coverage statement, carrying any
//!   pending records;
//! - seals the parts as `snapshot` records to publish, in one transaction.
//!
//! Received parts are kept until their checkpoint is complete. It is then
//! judged against what this device holds:
//! 1. the snapshot's own version and frontier must be the statement's;
//! 2. each listed writer must be admitted, under its listed author, and
//!    every actor in the version must be one of theirs;
//! 3. where this device holds a listed chain as far, the head must be the
//!    one it holds there;
//! 4. each writer's counter must agree with the chain lengths.
//!
//! What it is then compared with the latest accepted checkpoint decides:
//! - a contradiction is refused, and kept as evidence;
//! - an older one is kept as superseded and never becomes the latest, so a
//!   relay cannot roll a device back;
//! - a newer one is accepted.
//!
//! Nothing a device already holds is replaced. A device with history
//! imports nothing from a checkpoint.
//!
//! [`Database::start_from_checkpoint`] starts a freshly joined device from
//! one, in one transaction:
//! - the snapshot, journalled;
//! - each covered chain's length and head;
//! - the carried pending records;
//! - the relay cursor.
//!
//! Later records from the relay are its tail. If it reads covered records
//! again after a relay restore, `walk_covered` recognises them by walking
//! each chain back from its covered head.

use std::collections::BTreeMap;

use matters_sync::authority::{Hash, Id};
use matters_sync::checkpoint::{
    self as format, Assembled, Checkpoint, Coverage, Cursor, Sealed, Statement, Writer,
};
use matters_sync::record_hash;
use matters_sync::wire;
use rusqlite::{params, Connection, OptionalExtension};

use super::db::Database;
use super::membership::{Joined, HOME_DOCUMENT};
use super::sealing::seal_snapshot;
use super::sync::{crash_point, load_engine, rebuild_rows};
use crate::crdt::{self, Document};
use crate::error::{MattersError, Result};

/// The snapshot format checkpoints carry.
pub const ENGINE: &str = "loro-1";
/// The document layout checkpoints carry. A new layout bumps it.
pub const DOCUMENT_SCHEMA: u64 = 1;

/// What became of a checkpoint part.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Checkpointed {
    /// Kept: the checkpoint is not complete, or names a writer not yet
    /// admitted here.
    Part,
    Accepted,
    /// Consistent, but not newer than the latest accepted, or from a newer
    /// layout: kept, never used.
    Superseded,
    /// Contradicts this device's history or itself: kept as evidence.
    Refused(String),
}

fn invalid(what: impl Into<String>) -> MattersError {
    MattersError::Validation(what.into())
}

fn stored<const N: usize>(bytes: Vec<u8>) -> Result<[u8; N]> {
    bytes
        .try_into()
        .map_err(|_| MattersError::Engine("stored sync identifier".into()))
}

/// One writer chain as this device holds it.
#[derive(Debug)]
struct Chain {
    records: u64,
    head: Option<Hash>,
    base: u64,
    base_head: Option<Hash>,
    own: bool,
}

fn chain(conn: &Connection, incarnation: &Id) -> Result<Chain> {
    let inc = incarnation.as_slice();
    let own: i64 = conn.query_row(
        "SELECT COUNT(*) FROM sync_records WHERE incarnation = ?1",
        params![inc],
        |r| r.get(0),
    )?;
    if own > 0 {
        let head: Option<Vec<u8>> = conn.query_row(
            "SELECT head FROM sync_writers WHERE incarnation = ?1",
            params![inc],
            |r| r.get(0),
        )?;
        return Ok(Chain {
            records: own as u64,
            head: head.map(stored).transpose()?,
            base: 0,
            base_head: None,
            own: true,
        });
    }
    let received: Option<Vec<u8>> = conn
        .query_row(
            "SELECT head FROM sync_receive_heads WHERE incarnation = ?1",
            params![inc],
            |r| r.get(0),
        )
        .optional()?;
    let (base, base_head): (i64, Option<Vec<u8>>) = conn
        .query_row(
            "SELECT base, base_head FROM sync_receive_base WHERE incarnation = ?1",
            params![inc],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?
        .unwrap_or((0, None));
    let Some(head) = received else {
        return Ok(Chain {
            records: 0,
            head: None,
            base: 0,
            base_head: None,
            own: false,
        });
    };
    let applied: i64 = conn.query_row(
        "SELECT COUNT(*) FROM sync_received WHERE incarnation = ?1",
        params![inc],
        |r| r.get(0),
    )?;
    Ok(Chain {
        records: (base + applied) as u64,
        head: Some(stored(head)?),
        base: base as u64,
        base_head: base_head.map(stored).transpose()?,
        own: false,
    })
}

/// Whether this device holds the record `digest` in chain `incarnation`.
fn holds(conn: &Connection, incarnation: &Id, digest: &Hash, own: bool) -> Result<bool> {
    if own {
        let mut stmt = conn.prepare("SELECT bytes FROM sync_records WHERE incarnation = ?1")?;
        let rows = stmt.query_map(params![incarnation.as_slice()], |r| r.get::<_, Vec<u8>>(0))?;
        for bytes in rows {
            if record_hash(&bytes?) == *digest {
                return Ok(true);
            }
        }
        return Ok(false);
    }
    Ok(conn
        .query_row(
            "SELECT 1 FROM sync_received WHERE incarnation = ?1 AND digest = ?2
             UNION ALL SELECT 1 FROM sync_covered WHERE incarnation = ?1 AND digest = ?2",
            params![incarnation.as_slice(), digest.as_slice()],
            |r| r.get::<_, i64>(0),
        )
        .optional()?
        .is_some())
}

/// Every chain this device holds records of, as a checkpoint lists them.
fn writers(conn: &Connection, joined: &Joined) -> Result<Vec<Writer>> {
    let mut out = Vec::new();
    for (incarnation, history) in joined.authority.writer_histories(HOME_DOCUMENT) {
        let held = chain(conn, &incarnation)?;
        // A chain this device holds no record of has no head.
        if let Some(head) = held.head {
            out.push(Writer {
                author: history.device,
                incarnation,
                records: held.records,
                head,
            });
        }
    }
    Ok(out)
}

fn latest(conn: &Connection) -> Result<Option<Coverage>> {
    let bytes: Option<Vec<u8>> = conn
        .query_row(
            "SELECT coverage FROM sync_checkpoints WHERE state = 'accepted'
             ORDER BY seq DESC LIMIT 1",
            [],
            |r| r.get(0),
        )
        .optional()?;
    bytes
        .map(|b| Coverage::parse(&b).map_err(|e| MattersError::Engine(format!("stored: {e}"))))
        .transpose()
}

/// How a complete checkpoint compares with what this device holds.
enum Verdict {
    Accept,
    Supersede,
    Refuse(String),
    /// It names a writer whose admission has not arrived yet.
    Wait,
}

/// Checks 1 and 2: the parts of the judgement that need no history.
fn consistent(joined: &Joined, c: &Checkpoint) -> Result<std::result::Result<(), Verdict>> {
    let coverage = &c.coverage;
    if coverage.engine != ENGINE {
        return Ok(Err(Verdict::Refuse(format!(
            "unknown engine {}",
            coverage.engine
        ))));
    }
    if coverage.schema > DOCUMENT_SCHEMA {
        return Ok(Err(Verdict::Supersede));
    }
    match crdt::snapshot_version(&c.snapshot) {
        Ok((version, frontiers))
            if version == coverage.version && frontiers == coverage.frontiers => {}
        Ok(_) => {
            return Ok(Err(Verdict::Refuse(
                "the snapshot does not match its coverage".into(),
            )))
        }
        Err(_) => return Ok(Err(Verdict::Refuse("not a full-history snapshot".into()))),
    }
    let histories = joined.authority.writer_histories(HOME_DOCUMENT);
    let mut actors = BTreeMap::new();
    for w in &coverage.writers {
        let Some(history) = histories.get(&w.incarnation) else {
            return Ok(Err(Verdict::Wait));
        };
        if history.device != w.author {
            return Ok(Err(Verdict::Refuse(
                "a writer listed under another author".into(),
            )));
        }
        actors.insert(history.actor, w);
    }
    let version = crdt::decode_version(&coverage.version)?;
    if version.keys().any(|actor| !actors.contains_key(actor)) {
        return Ok(Err(Verdict::Refuse(
            "operations of an actor no listed writer holds".into(),
        )));
    }
    Ok(Ok(()))
}

fn judge(conn: &Connection, joined: &Joined, doc: &Document, c: &Checkpoint) -> Result<Verdict> {
    if let Err(verdict) = consistent(joined, c)? {
        return Ok(verdict);
    }
    let coverage = &c.coverage;
    let histories = joined.authority.writer_histories(HOME_DOCUMENT);
    let theirs = crdt::decode_version(&coverage.version)?;
    let mine = crdt::decode_version(&crdt::encode_version(&doc.version()))?;
    for w in &coverage.writers {
        let actor = histories[&w.incarnation].actor;
        let (t, m) = (
            theirs.get(&actor).copied().unwrap_or(0),
            mine.get(&actor).copied().unwrap_or(0),
        );
        let held = chain(conn, &w.incarnation)?;
        let fork = || Verdict::Refuse("another head at a position this device holds".into());
        let counters = || Verdict::Refuse("a writer's counter disagrees with its chain".into());
        match held.records.cmp(&w.records) {
            std::cmp::Ordering::Equal => {
                if held.head != Some(w.head) {
                    return Ok(fork());
                }
                if t != m {
                    return Ok(counters());
                }
            }
            std::cmp::Ordering::Greater => {
                let known = if w.records > held.base {
                    holds(conn, &w.incarnation, &w.head, held.own)?
                } else if w.records == held.base {
                    held.base_head == Some(w.head)
                } else {
                    // Inside what this device knows only by its length.
                    true
                };
                if !known {
                    return Ok(fork());
                }
                if t >= m {
                    return Ok(counters());
                }
            }
            std::cmp::Ordering::Less => {
                if t <= m {
                    return Ok(counters());
                }
            }
        }
    }
    Ok(match latest(conn)? {
        None => Verdict::Accept,
        Some(known) => match coverage.follows(&known) {
            Ok(()) => Verdict::Accept,
            Err(format::Error::Fork) => {
                Verdict::Refuse("it contradicts an accepted checkpoint".into())
            }
            Err(_) => Verdict::Supersede,
        },
    })
}

fn record(
    tx: &Connection,
    id: &Hash,
    coverage: &[u8],
    state: &str,
    reason: Option<&str>,
) -> Result<()> {
    tx.execute(
        "INSERT INTO sync_checkpoints (id, coverage, state, reason, journal)
         VALUES (?1, ?2, ?3, ?4, (SELECT COUNT(*) FROM sync_journal))
         ON CONFLICT (id) DO NOTHING",
        params![id.as_slice(), coverage, state, reason],
    )?;
    Ok(())
}

fn decided(conn: &Connection, id: &Hash) -> Result<Option<Checkpointed>> {
    let row: Option<(String, Option<String>)> = conn
        .query_row(
            "SELECT state, reason FROM sync_checkpoints WHERE id = ?1",
            params![id.as_slice()],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    Ok(row.map(|(state, reason)| match state.as_str() {
        "accepted" => Checkpointed::Accepted,
        "superseded" => Checkpointed::Superseded,
        _ => Checkpointed::Refused(reason.unwrap_or_default()),
    }))
}

/// Keep one authenticated, opened part, then judge its checkpoint if it
/// is complete, in the caller's transaction.
pub(super) fn keep_part(
    tx: &Connection,
    engine: &Option<Document>,
    joined: &Joined,
    record_id: &[u8; 32],
    sealed: &Sealed,
    plaintext: &[u8],
) -> Result<Checkpointed> {
    let part = format::parse_part(plaintext)
        .map_err(|e| invalid(format!("record refused: checkpoint part: {e}")))?;
    let id = part.id;
    tx.execute(
        "INSERT INTO sync_checkpoint_parts (record_id, checkpoint, author, incarnation, epoch, plaintext)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6) ON CONFLICT (record_id) DO NOTHING",
        params![
            record_id.as_slice(),
            id.as_slice(),
            sealed.author.as_slice(),
            sealed.incarnation.as_slice(),
            sealed.epoch as i64,
            plaintext
        ],
    )?;
    tx.execute(
        "DELETE FROM sync_pending WHERE record_id = ?1",
        params![record_id.as_slice()],
    )?;
    finish(tx, engine, joined, &id)
}

/// Judge checkpoint `id` from its kept parts, if they are complete.
fn finish(
    tx: &Connection,
    engine: &Option<Document>,
    joined: &Joined,
    id: &Hash,
) -> Result<Checkpointed> {
    if let Some(outcome) = decided(tx, id)? {
        if !matches!(outcome, Checkpointed::Refused(_)) {
            tx.execute(
                "DELETE FROM sync_checkpoint_parts WHERE checkpoint = ?1",
                params![id.as_slice()],
            )?;
        }
        return Ok(outcome);
    }
    let parts: Vec<(Sealed, Vec<u8>)> = {
        let mut stmt = tx.prepare(
            "SELECT author, incarnation, epoch, plaintext FROM sync_checkpoint_parts
             WHERE checkpoint = ?1",
        )?;
        let rows = stmt.query_map(params![id.as_slice()], |r| {
            Ok((
                r.get::<_, Vec<u8>>(0)?,
                r.get::<_, Vec<u8>>(1)?,
                r.get::<_, i64>(2)?,
                r.get::<_, Vec<u8>>(3)?,
            ))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (author, incarnation, epoch, plaintext) = row?;
            out.push((
                Sealed {
                    author: stored(author)?,
                    incarnation: stored(incarnation)?,
                    epoch: epoch as u64,
                },
                plaintext,
            ));
        }
        out
    };
    let borrowed: Vec<(Sealed, &[u8])> = parts
        .iter()
        .map(|(s, p)| (s.clone(), p.as_slice()))
        .collect();
    let checkpoint = match format::assemble(&borrowed) {
        Err(error) => {
            let reason = error.to_string();
            record(tx, id, &[], "refused", Some(&reason))?;
            return Ok(Checkpointed::Refused(reason));
        }
        Ok(Assembled::Incomplete { .. }) => return Ok(Checkpointed::Part),
        Ok(Assembled::Complete(c)) => c,
    };
    let doc = engine.as_ref().ok_or_else(|| invalid("sync is off"))?;
    let coverage = checkpoint.coverage.encode()?;
    let outcome = match judge(tx, joined, doc, &checkpoint)? {
        Verdict::Wait => return Ok(Checkpointed::Part),
        Verdict::Accept => {
            record(tx, id, &coverage, "accepted", None)?;
            Checkpointed::Accepted
        }
        Verdict::Supersede => {
            record(tx, id, &coverage, "superseded", None)?;
            Checkpointed::Superseded
        }
        Verdict::Refuse(reason) => {
            record(tx, id, &coverage, "refused", Some(&reason))?;
            return Ok(Checkpointed::Refused(reason));
        }
    };
    tx.execute(
        "DELETE FROM sync_checkpoint_parts WHERE checkpoint = ?1",
        params![id.as_slice()],
    )?;
    Ok(outcome)
}

/// Judge again every complete checkpoint that waited for a writer, in the
/// caller's transaction.
pub(super) fn finish_waiting(
    conn: &Connection,
    engine: &Option<Document>,
    joined: &Joined,
) -> Result<()> {
    let ids: Vec<Vec<u8>> = {
        let mut stmt = conn.prepare(
            "SELECT DISTINCT checkpoint FROM sync_checkpoint_parts
             WHERE checkpoint NOT IN (SELECT id FROM sync_checkpoints)",
        )?;
        let rows = stmt.query_map([], |r| r.get(0))?;
        rows.collect::<rusqlite::Result<_>>()?
    };
    for id in ids {
        finish(conn, engine, joined, &stored(id)?)?;
    }
    Ok(())
}

/// A pending data record's place in its chain.
struct Link {
    record_id: [u8; 32],
    previous: Option<Hash>,
    incarnation: [u8; 32],
}

/// Recognise covered records met again: walk each chain a checkpoint
/// started back from its covered head, by hash, through the pending
/// records. Each one found is held already, so it is recorded as received
/// and no longer pending.
pub(super) fn walk_covered(conn: &Connection, joined: &Joined) -> Result<()> {
    let chains: Vec<(Vec<u8>, Vec<u8>)> = {
        let mut stmt =
            conn.prepare("SELECT incarnation, COALESCE(reach, base_head) FROM sync_receive_base")?;
        let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
        rows.collect::<rusqlite::Result<_>>()?
    };
    if chains.is_empty() {
        return Ok(());
    }
    let pending: BTreeMap<Hash, Link> = {
        let mut stmt = conn.prepare("SELECT bytes FROM sync_pending")?;
        let rows = stmt.query_map([], |r| r.get::<_, Vec<u8>>(0))?;
        let mut out = BTreeMap::new();
        for bytes in rows {
            let bytes = bytes?;
            let Ok(p) = wire::parse(&bytes, joined.route.content(&HOME_DOCUMENT)) else {
                continue;
            };
            if p.header.kind == wire::Kind::Data {
                out.insert(
                    record_hash(&bytes),
                    Link {
                        record_id: *p.header.record_id,
                        previous: p.header.previous.copied(),
                        incarnation: *p.header.incarnation,
                    },
                );
            }
        }
        out
    };
    let tx = conn;
    for (incarnation, reach) in chains {
        let mut at: Hash = stored(reach)?;
        let mut moved = false;
        while let Some(Link {
            record_id,
            previous,
            incarnation: inc,
        }) = pending.get(&at)
        {
            if inc.as_slice() != incarnation.as_slice() {
                break;
            }
            // Counted in the base already: kept apart as covered, so it is
            // known by its ID and hash without counting twice.
            tx.execute(
                "INSERT INTO sync_covered (record_id, incarnation, digest)
                 VALUES (?1, ?2, ?3) ON CONFLICT (record_id) DO NOTHING",
                params![record_id.as_slice(), inc.as_slice(), at.as_slice()],
            )?;
            tx.execute(
                "DELETE FROM sync_pending WHERE record_id = ?1",
                params![record_id.as_slice()],
            )?;
            moved = true;
            match previous {
                Some(p) => at = *p,
                None => break,
            }
        }
        if moved {
            tx.execute(
                "UPDATE sync_receive_base SET reach = ?1 WHERE incarnation = ?2",
                params![at.as_slice(), incarnation],
            )?;
        }
    }
    Ok(())
}

/// Whether this device has received, written and read nothing of the
/// collection yet.
fn nothing_synced(conn: &Connection) -> Result<bool> {
    let held: i64 = conn.query_row(
        "SELECT (SELECT COUNT(*) FROM sync_received) + (SELECT COUNT(*) FROM sync_records)
              + (SELECT COUNT(*) FROM sync_checkpoints)
              + (SELECT COUNT(*) FROM sync_cursors WHERE log = 'records')",
        [],
        |r| r.get(0),
    )?;
    Ok(held == 0)
}

impl Database {
    /// Whether this device has received, written and read nothing of the
    /// collection yet, so it may start from a checkpoint.
    pub fn holds_nothing_synced(&self) -> Result<bool> {
        self.with_conn(nothing_synced)
    }

    /// Make a checkpoint of everything this device holds, if it is fully
    /// synced: every record it wrote acknowledged by the relay, and its
    /// last pull, which ended at `cursor`, complete. The caller passes the
    /// cursor right after that pull. Returns the checkpoint's ID, or `None`
    /// when a record of its own still waits for the relay.
    pub fn create_checkpoint(&self, cursor: Option<Cursor>) -> Result<Option<Hash>> {
        self.with_sealing(|conn, engine, sync| {
            let joined = sync
                .membership
                .as_ref()
                .ok_or_else(|| invalid("sync is not resumed"))?;
            let sealing = sync
                .sealing
                .as_ref()
                .ok_or_else(|| invalid("this device does not write"))?;
            let doc = engine.as_ref().ok_or_else(|| invalid("sync is off"))?;
            let tx = conn.transaction()?;
            let unacked: i64 = tx.query_row(
                "SELECT COUNT(*) FROM sync_records WHERE acked = 0",
                [],
                |r| r.get(0),
            )?;
            if unacked > 0 {
                return Ok(None);
            }
            let pending: Vec<Vec<u8>> = {
                let mut stmt = tx.prepare("SELECT bytes FROM sync_pending ORDER BY seq")?;
                let rows = stmt.query_map([], |r| r.get(0))?;
                rows.collect::<rusqlite::Result<_>>()?
            };
            let statement = Statement {
                engine: ENGINE.into(),
                schema: DOCUMENT_SCHEMA,
                epoch: sealing.epoch(),
                writers: writers(&tx, joined)?,
                version: crdt::encode_version(&doc.version()),
                frontiers: doc.frontiers(),
                cursor,
            };
            let checkpoint = Checkpoint::new(statement, doc.snapshot()?, pending)?;
            let id = checkpoint.id()?;
            if decided(&tx, &id)?.is_some() {
                // Nothing changed since: the same checkpoint exists.
                return Ok(Some(id));
            }
            let batch: [u8; 32] = id[..32].try_into().expect("48 bytes");
            for (index, part) in checkpoint.parts()?.iter().enumerate() {
                let (record_id, bytes) = seal_snapshot(sealing, &batch, part)?;
                tx.execute(
                    "INSERT INTO sync_checkpoint_out (record_id, checkpoint, part, bytes)
                     VALUES (?1, ?2, ?3, ?4)",
                    params![record_id.as_slice(), id.as_slice(), index as i64, bytes],
                )?;
            }
            record(&tx, &id, &checkpoint.coverage.encode()?, "accepted", None)?;
            crash_point("checkpoint-before-commit");
            tx.commit()?;
            Ok(Some(id))
        })
    }

    /// Whether this device should make a checkpoint: it has none yet and
    /// holds at least `every` engine updates, or it has journalled `every`
    /// more since the latest it accepted.
    pub fn checkpoint_due(&self, every: u64) -> Result<bool> {
        self.with_conn(|conn| {
            let (journal, at): (i64, Option<i64>) = conn.query_row(
                "SELECT (SELECT COUNT(*) FROM sync_journal),
                        (SELECT journal FROM sync_checkpoints WHERE state = 'accepted'
                         ORDER BY seq DESC LIMIT 1)",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )?;
            Ok((journal - at.unwrap_or(0)) as u64 >= every.max(1))
        })
    }

    /// Checkpoint parts this device made that the relay has not yet
    /// acknowledged, as `(record ID, bytes)`: the same bytes every time.
    pub fn outgoing_checkpoint_parts(&self) -> Result<Vec<([u8; 32], Vec<u8>)>> {
        self.with_conn(|conn| {
            let mut stmt = conn.prepare(
                "SELECT record_id, bytes FROM sync_checkpoint_out WHERE acked = 0
                 ORDER BY rowid",
            )?;
            let rows = stmt.query_map([], |r| Ok((r.get::<_, Vec<u8>>(0)?, r.get(1)?)))?;
            let mut out = Vec::new();
            for row in rows {
                let (id, bytes) = row?;
                out.push((stored(id)?, bytes));
            }
            Ok(out)
        })
    }

    /// Mark a checkpoint part as durably received by the relay. Returns
    /// whether the record was one.
    pub fn acknowledge_checkpoint_part(&self, record_id: &[u8; 32]) -> Result<bool> {
        self.with_sealing(|conn, _, _| {
            Ok(conn.execute(
                "UPDATE sync_checkpoint_out SET acked = 1 WHERE record_id = ?1",
                params![record_id.as_slice()],
            )? > 0)
        })
    }

    /// The coverage of the latest accepted checkpoint.
    pub fn latest_checkpoint(&self) -> Result<Option<Coverage>> {
        self.with_conn(latest)
    }

    /// Every complete checkpoint seen, oldest first, as `(ID, state,
    /// reason)`.
    pub fn checkpoints(&self) -> Result<Vec<(Hash, String, Option<String>)>> {
        self.with_conn(|conn| {
            let mut stmt =
                conn.prepare("SELECT id, state, reason FROM sync_checkpoints ORDER BY seq")?;
            let rows =
                stmt.query_map([], |r| Ok((r.get::<_, Vec<u8>>(0)?, r.get(1)?, r.get(2)?)))?;
            let mut out = Vec::new();
            for row in rows {
                let (id, state, reason) = row?;
                out.push((stored(id)?, state, reason));
            }
            Ok(out)
        })
    }

    /// Start a freshly joined device from a checkpoint: `records` are its
    /// parts as the relay returned them, and `generation` the relay's
    /// current storage generation. Its cursor is used only in that
    /// generation; otherwise this starts nothing and returns `false`, and
    /// the device reads the whole log instead.
    ///
    /// The snapshot, each covered chain's length and head, the carried
    /// pending records, the accepted checkpoint and the relay cursor commit
    /// together.
    pub fn start_from_checkpoint(&self, records: &[Vec<u8>], generation: [u8; 16]) -> Result<bool> {
        self.with_sealing(|conn, engine, sync| {
            let joined = sync
                .membership
                .as_ref()
                .ok_or_else(|| invalid("sync is not resumed"))?;
            if !nothing_synced(conn)? {
                return Err(invalid(
                    "only a device that holds nothing yet starts from a checkpoint",
                ));
            }
            let mut parts = Vec::new();
            for bytes in records {
                match super::receive::open_part(conn, joined, bytes)? {
                    Some(opened) => parts.push(opened),
                    None => return Err(invalid("a checkpoint part waits for its key or writer")),
                }
            }
            let borrowed: Vec<(Sealed, &[u8])> = parts
                .iter()
                .map(|(_, s, p)| (s.clone(), p.as_slice()))
                .collect();
            let checkpoint = match format::assemble(&borrowed)? {
                Assembled::Complete(c) => c,
                Assembled::Incomplete { .. } => {
                    return Err(invalid("the checkpoint is incomplete"))
                }
            };
            match consistent(joined, &checkpoint)? {
                Ok(()) => {}
                Err(Verdict::Refuse(reason)) => {
                    return Err(invalid(format!("checkpoint refused: {reason}")))
                }
                Err(_) => return Err(invalid("the checkpoint cannot be used here")),
            }
            let Some(cursor) = checkpoint
                .coverage
                .cursor
                .clone()
                .filter(|c| c.generation == generation)
            else {
                return Ok(false);
            };
            let doc = engine.as_mut().ok_or_else(|| invalid("sync is off"))?;
            let id = checkpoint.id()?;
            let tx = conn.transaction()?;
            let started = (|| {
                tx.execute(
                    "INSERT INTO sync_journal (bytes, outgoing) VALUES (?1, 0)",
                    params![checkpoint.snapshot],
                )?;
                doc.merge(&checkpoint.snapshot)?;
                rebuild_rows(&tx, &doc.project()?)?;
                for w in &checkpoint.coverage.writers {
                    tx.execute(
                        "INSERT INTO sync_receive_heads (incarnation, head) VALUES (?1, ?2)",
                        params![w.incarnation.as_slice(), w.head.as_slice()],
                    )?;
                    tx.execute(
                        "INSERT INTO sync_receive_base (incarnation, base, base_head)
                         VALUES (?1, ?2, ?3)",
                        params![
                            w.incarnation.as_slice(),
                            w.records as i64,
                            w.head.as_slice()
                        ],
                    )?;
                }
                for bytes in &checkpoint.pending {
                    let p = wire::parse(bytes, joined.route.content(&HOME_DOCUMENT))
                        .map_err(|e| invalid(format!("carried record: {e}")))?;
                    tx.execute(
                        "INSERT INTO sync_pending (record_id, bytes, reason, seq)
                         VALUES (?1, ?2, 'predecessor',
                                 (SELECT COALESCE(MAX(seq), 0) + 1 FROM sync_pending))
                         ON CONFLICT (record_id) DO NOTHING",
                        params![p.header.record_id.as_slice(), bytes],
                    )?;
                }
                record(&tx, &id, &checkpoint.coverage.encode()?, "accepted", None)?;
                tx.execute(
                    "INSERT INTO sync_cursors (log, cursor) VALUES ('records', ?1)
                     ON CONFLICT (log) DO UPDATE SET cursor = excluded.cursor",
                    params![cursor.relay_cursor()],
                )?;
                crash_point("bootstrap-before-commit");
                Ok::<_, MattersError>(tx.commit()?)
            })();
            if let Err(error) = started {
                *engine = load_engine(conn)?;
                return Err(error);
            }
            super::receive::drain(conn, engine, joined)?;
            Ok(true)
        })
    }
}

impl From<format::Error> for MattersError {
    fn from(error: format::Error) -> Self {
        MattersError::Validation(format!("checkpoint: {error}"))
    }
}
