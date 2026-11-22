//! Checkpoints in Matters' database (#40, docs/SYNC_CHECKPOINTS.md).
//!
//! [`Database::create_checkpoint`]:
//! - is called by a fully synced member;
//! - snapshots the document with a coverage statement, carrying any
//!   pending records;
//! - seals the parts as `snapshot` records to publish, in one transaction.
//!
//! Received parts are kept, grouped by checkpoint and author, until their
//! checkpoint is complete; one member's parts never spoil another's. An
//! author's parts are held to one checkpoint's worth ([`format::MAX_PARTS`]),
//! and a part of an author's newer checkpoint drops the parts of its
//! earlier ones. A complete checkpoint is assembled once, when its last
//! part arrives, and judged against what this device holds:
//! 1. each listed writer must be admitted, under its listed author: one not
//!    admitted yet makes the checkpoint wait, recorded, before its snapshot
//!    is imported, and it is judged again only once that writer arrives;
//! 2. the snapshot's own version and frontier must be the statement's, and
//!    every actor in the version must be a listed writer's;
//! 3. where this device holds a listed chain as far, the record at the
//!    listed length must be the listed head;
//! 4. each writer's counter must agree with the chain lengths.
//!
//! What it is then compared with the latest checkpoint this device accepted
//! and could verify decides:
//! - a contradiction is refused, and its coverage kept as evidence;
//! - an older one is kept as superseded and never becomes the latest, so a
//!   relay cannot roll a device back;
//! - a newer one is accepted. One that lists a chain further than this
//!   device holds it is accepted unverified: it never becomes the latest,
//!   so claims this device cannot check never decide another checkpoint.
//!
//! Decided checkpoints beyond [`KEEP_DECIDED`] are pruned, oldest first.
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
use super::sync::{crash_point, rebuild_rows};
use crate::crdt::{self, Document};
use crate::error::{MattersError, Result};

/// The snapshot format checkpoints carry.
pub const ENGINE: &str = "loro-1";
/// The document layout checkpoints carry. A new layout bumps it.
pub const DOCUMENT_SCHEMA: u64 = 1;
/// Refused, superseded and unverified checkpoints kept, newest first; and,
/// apart from those, accepted ones.
pub const KEEP_DECIDED: i64 = 32;

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

/// The digest of the record at `position` (from 1) of chain
/// `incarnation`, where this device knows it: its own chain, a record it
/// received, or one it met again past a checkpoint it started from. Rows
/// received before v19 have no position.
fn at_position(
    conn: &Connection,
    incarnation: &Id,
    position: u64,
    own: bool,
) -> Result<Option<Hash>> {
    let inc = incarnation.as_slice();
    if own {
        let bytes: Option<Vec<u8>> = conn
            .query_row(
                "SELECT bytes FROM sync_records WHERE incarnation = ?1 ORDER BY seq
                 LIMIT 1 OFFSET ?2",
                params![inc, position as i64 - 1],
                |r| r.get(0),
            )
            .optional()?;
        return Ok(bytes.map(|b| record_hash(&b)));
    }
    conn.query_row(
        "SELECT digest FROM sync_received WHERE incarnation = ?1 AND position = ?2
         UNION ALL SELECT digest FROM sync_covered WHERE incarnation = ?1 AND position = ?2
         LIMIT 1",
        params![inc, position as i64],
        |r| r.get::<_, Vec<u8>>(0),
    )
    .optional()?
    .map(stored)
    .transpose()
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

/// The latest checkpoint this device accepted and could verify.
fn latest(conn: &Connection) -> Result<Option<Coverage>> {
    let bytes: Option<Vec<u8>> = conn
        .query_row(
            "SELECT coverage FROM sync_checkpoints WHERE state = 'accepted' AND verified = 1
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
    /// Newer than the latest: `verified` when this device holds every
    /// listed chain as far as it is listed.
    Accept {
        verified: bool,
    },
    Supersede,
    Refuse(String),
    /// It names a writer whose admission has not arrived yet.
    Wait(Id),
}

/// Checks 1 and 2: the parts of the judgement that need no history. The
/// writers are checked first, so a checkpoint that must wait imports no
/// snapshot.
fn consistent(joined: &Joined, c: &Checkpoint) -> std::result::Result<(), Verdict> {
    let coverage = &c.coverage;
    if coverage.engine != ENGINE {
        return Err(Verdict::Refuse(format!(
            "unknown engine {}",
            coverage.engine
        )));
    }
    if coverage.schema > DOCUMENT_SCHEMA {
        return Err(Verdict::Supersede);
    }
    let histories = joined.authority.writer_histories(HOME_DOCUMENT);
    let mut actors = BTreeMap::new();
    for w in &coverage.writers {
        let Some(history) = histories.get(&w.incarnation) else {
            return Err(Verdict::Wait(w.incarnation));
        };
        if history.device != w.author {
            return Err(Verdict::Refuse(
                "a writer listed under another author".into(),
            ));
        }
        actors.insert(history.actor, w);
    }
    let Ok(version) = crdt::decode_version(&coverage.version) else {
        return Err(Verdict::Refuse("a malformed version".into()));
    };
    if version.keys().any(|actor| !actors.contains_key(actor)) {
        return Err(Verdict::Refuse(
            "operations of an actor no listed writer holds".into(),
        ));
    }
    match crdt::snapshot_version(&c.snapshot) {
        Ok((version, frontiers))
            if version == coverage.version && frontiers == coverage.frontiers =>
        {
            Ok(())
        }
        Ok(_) => Err(Verdict::Refuse(
            "the snapshot does not match its coverage".into(),
        )),
        Err(_) => Err(Verdict::Refuse("not a full-history snapshot".into())),
    }
}

fn judge(conn: &Connection, joined: &Joined, doc: &Document, c: &Checkpoint) -> Result<Verdict> {
    if let Err(verdict) = consistent(joined, c) {
        return Ok(verdict);
    }
    let coverage = &c.coverage;
    let histories = joined.authority.writer_histories(HOME_DOCUMENT);
    let Ok(theirs) = crdt::decode_version(&coverage.version) else {
        return Ok(Verdict::Refuse("a malformed version".into()));
    };
    let mine = crdt::decode_version(&crdt::encode_version(&doc.version()))?;
    let mut verified = true;
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
                // The record at the listed length must be the listed head.
                let known = if w.records == held.base && !held.own {
                    held.base_head == Some(w.head)
                } else {
                    match at_position(conn, &w.incarnation, w.records, held.own)? {
                        Some(digest) => digest == w.head,
                        // Received before positions were kept.
                        None if w.records > held.base => {
                            holds(conn, &w.incarnation, &w.head, held.own)?
                        }
                        // Inside what this device knows only by its length.
                        None => true,
                    }
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
                // Further than this device holds: nothing here can refute
                // the listed head.
                verified = false;
            }
        }
    }
    Ok(match latest(conn)? {
        None => Verdict::Accept { verified },
        Some(known) => match coverage.follows(&known) {
            Ok(()) => Verdict::Accept { verified },
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
    verified: bool,
) -> Result<()> {
    tx.execute(
        "INSERT INTO sync_checkpoints (id, coverage, state, reason, journal, verified)
         VALUES (?1, ?2, ?3, ?4, (SELECT COUNT(*) FROM sync_journal), ?5)
         ON CONFLICT (id) DO NOTHING",
        params![id.as_slice(), coverage, state, reason, verified],
    )?;
    tx.execute(
        "DELETE FROM sync_checkpoint_waits WHERE checkpoint = ?1",
        params![id.as_slice()],
    )?;
    prune(tx)
}

/// Keep [`KEEP_DECIDED`] of the checkpoints that never decide another
/// (refused, superseded, unverified) and as many accepted ones besides,
/// newest first. The latest accepted is never pruned.
fn prune(tx: &Connection) -> Result<()> {
    tx.execute(
        "DELETE FROM sync_checkpoints WHERE seq IN (
             SELECT seq FROM sync_checkpoints
             WHERE state != 'accepted' OR verified = 0
             ORDER BY seq DESC LIMIT -1 OFFSET ?1)",
        params![KEEP_DECIDED],
    )?;
    tx.execute(
        "DELETE FROM sync_checkpoints WHERE seq IN (
             SELECT seq FROM sync_checkpoints
             WHERE state = 'accepted' AND verified = 1
             ORDER BY seq DESC LIMIT -1 OFFSET ?1)",
        params![KEEP_DECIDED],
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

/// Keep one authenticated, opened part, then judge its checkpoint if this
/// part completes it, in the caller's transaction. `Err` refuses the part's
/// record: it is no part, or its author already sent a checkpoint's worth.
pub(super) fn keep_part(
    tx: &Connection,
    engine: &Option<Document>,
    joined: &Joined,
    record_id: &[u8; 32],
    digest: &Hash,
    sealed: &Sealed,
    plaintext: &[u8],
) -> Result<std::result::Result<Checkpointed, String>> {
    let part = match format::parse_part(plaintext) {
        Ok(p) => p,
        Err(e) => return Ok(Err(e.to_string())),
    };
    let total = match part.coverage.map(Coverage::parse).transpose() {
        Ok(c) => c.map(|c| c.parts),
        Err(e) => return Ok(Err(e.to_string())),
    };
    let id = part.id;
    let author = sealed.author.as_slice();
    // An author's newer checkpoint supersedes its earlier, undecided ones.
    tx.execute(
        "DELETE FROM sync_checkpoint_parts WHERE author = ?1 AND checkpoint != ?2",
        params![author, id.as_slice()],
    )?;
    tx.execute(
        "DELETE FROM sync_checkpoint_waits WHERE author = ?1 AND checkpoint != ?2",
        params![author, id.as_slice()],
    )?;
    if let Some(outcome) = decided(tx, &id)? {
        return Ok(Ok(outcome));
    }
    tx.execute(
        "INSERT INTO sync_checkpoint_parts
             (record_id, checkpoint, author, incarnation, epoch, plaintext, digest, part, total)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9) ON CONFLICT (record_id) DO NOTHING",
        params![
            record_id.as_slice(),
            id.as_slice(),
            author,
            sealed.incarnation.as_slice(),
            sealed.epoch as i64,
            plaintext,
            digest.as_slice(),
            part.index as i64,
            total.map(|t| t as i64)
        ],
    )?;
    let (kept, have, unindexed, total): (i64, i64, i64, Option<i64>) = tx.query_row(
        "SELECT COUNT(*), COUNT(DISTINCT part), COUNT(*) - COUNT(part), MAX(total)
         FROM sync_checkpoint_parts WHERE checkpoint = ?1 AND author = ?2",
        params![id.as_slice(), author],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
    )?;
    if kept as u64 > format::MAX_PARTS {
        drop_parts(tx, &id, author, "more parts than one checkpoint has")?;
        return Ok(Err("more parts than one checkpoint has".into()));
    }
    // Assembled once, when the last part arrives.
    if unindexed == 0 && total.is_none_or(|t| have < t) {
        return Ok(Ok(Checkpointed::Part));
    }
    Ok(Ok(finish(tx, engine, joined, &id, author)?))
}

/// Refuse an author's parts of checkpoint `id` that do not assemble: their
/// records are quarantined, and the parts dropped. Nothing is recorded
/// under the checkpoint's ID, so another member's parts of it stand.
fn drop_parts(tx: &Connection, id: &Hash, author: &[u8], reason: &str) -> Result<()> {
    tx.execute(
        "INSERT INTO sync_quarantine (record_id, digest, reason)
         SELECT record_id, digest, ?3 FROM sync_checkpoint_parts
         WHERE checkpoint = ?1 AND author = ?2 AND digest IS NOT NULL
         ON CONFLICT (record_id, digest) DO NOTHING",
        params![id.as_slice(), author, format!("checkpoint part: {reason}")],
    )?;
    tx.execute(
        "DELETE FROM sync_checkpoint_parts WHERE checkpoint = ?1 AND author = ?2",
        params![id.as_slice(), author],
    )?;
    tx.execute(
        "DELETE FROM sync_checkpoint_waits WHERE checkpoint = ?1 AND author = ?2",
        params![id.as_slice(), author],
    )?;
    Ok(())
}

/// Judge checkpoint `id` from `author`'s kept parts, if they are complete.
fn finish(
    tx: &Connection,
    engine: &Option<Document>,
    joined: &Joined,
    id: &Hash,
    author: &[u8],
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
             WHERE checkpoint = ?1 AND author = ?2",
        )?;
        let rows = stmt.query_map(params![id.as_slice(), author], |r| {
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
            drop_parts(tx, id, author, &reason)?;
            return Ok(Checkpointed::Refused(reason));
        }
        Ok(Assembled::Incomplete { .. }) => return Ok(Checkpointed::Part),
        Ok(Assembled::Complete(c)) => c,
    };
    let doc = engine.as_ref().ok_or_else(|| invalid("sync is off"))?;
    let coverage = checkpoint.coverage.encode()?;
    let outcome = match judge(tx, joined, doc, &checkpoint)? {
        Verdict::Wait(incarnation) => {
            tx.execute(
                "INSERT INTO sync_checkpoint_waits (checkpoint, author, incarnation)
                 VALUES (?1, ?2, ?3)
                 ON CONFLICT (checkpoint, author) DO UPDATE SET incarnation = excluded.incarnation",
                params![id.as_slice(), author, incarnation.as_slice()],
            )?;
            return Ok(Checkpointed::Part);
        }
        Verdict::Accept { verified } => {
            record(tx, id, &coverage, "accepted", None, verified)?;
            Checkpointed::Accepted
        }
        Verdict::Supersede => {
            record(tx, id, &coverage, "superseded", None, true)?;
            Checkpointed::Superseded
        }
        Verdict::Refuse(reason) => {
            record(tx, id, &coverage, "refused", Some(&reason), true)?;
            return Ok(Checkpointed::Refused(reason));
        }
    };
    tx.execute(
        "DELETE FROM sync_checkpoint_parts WHERE checkpoint = ?1",
        params![id.as_slice()],
    )?;
    Ok(outcome)
}

/// Judge again each complete checkpoint that waited for a writer now
/// admitted, in the caller's transaction. The others are not touched.
pub(super) fn finish_waiting(
    conn: &Connection,
    engine: &Option<Document>,
    joined: &Joined,
) -> Result<()> {
    let histories = joined.authority.writer_histories(HOME_DOCUMENT);
    let ready: Vec<(Vec<u8>, Vec<u8>)> = {
        let mut stmt =
            conn.prepare("SELECT checkpoint, author, incarnation FROM sync_checkpoint_waits")?;
        let mut rows = stmt.query([])?;
        let mut out = Vec::new();
        while let Some(row) = rows.next()? {
            let incarnation: Id = stored(row.get(2)?)?;
            if histories.contains_key(&incarnation) {
                out.push((row.get(0)?, row.get(1)?));
            }
        }
        out
    };
    for (id, author) in ready {
        conn.execute(
            "DELETE FROM sync_checkpoint_waits WHERE checkpoint = ?1 AND author = ?2",
            params![id, author],
        )?;
        finish(conn, engine, joined, &stored(id)?, &author)?;
    }
    Ok(())
}

/// Recognise covered records met again: walk each chain a checkpoint
/// started back from its covered head, by hash, through the pending
/// records, looked up by digest. Each one found is held already, so it is
/// kept apart as covered, with its predecessor and position, and no longer
/// pending. The checkpoint vouches for the hash, so these need no
/// authentication.
pub(super) fn walk_covered(conn: &Connection, joined: &Joined) -> Result<()> {
    let chains: Vec<(Vec<u8>, Vec<u8>, i64)> = {
        let mut stmt = conn.prepare(
            "SELECT incarnation, COALESCE(reach, base_head), base FROM sync_receive_base",
        )?;
        let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
        rows.collect::<rusqlite::Result<_>>()?
    };
    for (incarnation, reach, base) in chains {
        let mut at: Hash = stored(reach)?;
        let mut moved = false;
        let mut covered: i64 = conn.query_row(
            "SELECT COUNT(*) FROM sync_covered WHERE incarnation = ?1",
            params![incarnation],
            |r| r.get(0),
        )?;
        loop {
            let row: Option<Vec<u8>> = conn
                .query_row(
                    "SELECT bytes FROM sync_pending WHERE digest = ?1 LIMIT 1",
                    params![at.as_slice()],
                    |r| r.get(0),
                )
                .optional()?;
            let Some(bytes) = row else {
                break;
            };
            let Ok(p) = wire::parse(&bytes, joined.route.content(&HOME_DOCUMENT)) else {
                break;
            };
            if p.header.kind != wire::Kind::Data
                || p.header.incarnation.as_slice() != incarnation.as_slice()
            {
                break;
            }
            // Counted in the base already: kept apart as covered, so it is
            // known by its ID and hash without counting twice.
            conn.execute(
                "INSERT INTO sync_covered (record_id, incarnation, digest, previous, position)
                 VALUES (?1, ?2, ?3, ?4, ?5) ON CONFLICT (record_id) DO NOTHING",
                params![
                    p.header.record_id.as_slice(),
                    incarnation,
                    at.as_slice(),
                    p.header.previous.map(|h| h.as_slice()),
                    base - covered
                ],
            )?;
            conn.execute(
                "DELETE FROM sync_pending WHERE record_id = ?1 AND digest = ?2",
                params![p.header.record_id.as_slice(), at.as_slice()],
            )?;
            covered += 1;
            moved = true;
            match p.header.previous {
                Some(previous) => at = *previous,
                None => break,
            }
        }
        if moved {
            conn.execute(
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
    /// when none can be made now: a record of its own still waits for the
    /// relay, the engine holds operations aside for missing dependencies,
    /// or more authenticated records wait than a checkpoint carries. Only
    /// pending records whose signature was checked are carried.
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
            // Operations the engine holds aside, for dependencies it lacks,
            // would be in no snapshot's version: no checkpoint then.
            if doc.has_pending() {
                return Ok(None);
            }
            let tx = conn.transaction()?;
            // A handle whose writer another open replaced makes none.
            super::sealing::fence(&tx, sealing)?;
            let unacked: i64 = tx.query_row(
                "SELECT COUNT(*) FROM sync_records WHERE acked = 0",
                [],
                |r| r.get(0),
            )?;
            if unacked > 0 {
                return Ok(None);
            }
            // Only records a member's signature was checked on travel.
            let carried: i64 = tx.query_row(
                "SELECT COUNT(*) FROM sync_pending WHERE authenticated = 1",
                [],
                |r| r.get(0),
            )?;
            if carried as usize > format::MAX_PENDING {
                return Ok(None);
            }
            let pending: Vec<Vec<u8>> = {
                let mut stmt = tx.prepare(
                    "SELECT bytes FROM sync_pending WHERE authenticated = 1 ORDER BY seq",
                )?;
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
            record(
                &tx,
                &id,
                &checkpoint.coverage.encode()?,
                "accepted",
                None,
                true,
            )?;
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
                        (SELECT journal FROM sync_checkpoints
                         WHERE state = 'accepted' AND verified = 1
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

    /// The coverage of the latest checkpoint accepted here: the latest
    /// this device verified, or, while it verified none, the latest it
    /// accepted on claims it could not check.
    pub fn latest_checkpoint(&self) -> Result<Option<Coverage>> {
        self.with_conn(|conn| {
            if let Some(verified) = latest(conn)? {
                return Ok(Some(verified));
            }
            let bytes: Option<Vec<u8>> = conn
                .query_row(
                    "SELECT coverage FROM sync_checkpoints WHERE state = 'accepted'
                     ORDER BY seq DESC LIMIT 1",
                    [],
                    |r| r.get(0),
                )
                .optional()?;
            bytes
                .map(|b| {
                    Coverage::parse(&b).map_err(|e| MattersError::Engine(format!("stored: {e}")))
                })
                .transpose()
        })
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
            match consistent(joined, &checkpoint) {
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
                if doc.has_pending() {
                    return Err(invalid("the snapshot waits for operations it lacks"));
                }
                rebuild_rows(&tx, &doc.settle()?)?;
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
                // Received again, and authenticated here, by the drain
                // below.
                for bytes in &checkpoint.pending {
                    super::receive::keep_carried(&tx, joined, bytes)?;
                }
                // The device's own starting point: what it verifies others by.
                record(
                    &tx,
                    &id,
                    &checkpoint.coverage.encode()?,
                    "accepted",
                    None,
                    true,
                )?;
                tx.execute(
                    "INSERT INTO sync_cursors (log, cursor) VALUES ('records', ?1)
                     ON CONFLICT (log) DO UPDATE SET cursor = excluded.cursor",
                    params![cursor.relay_cursor()],
                )?;
                crash_point("bootstrap-before-commit");
                Ok::<_, MattersError>(tx.commit()?)
            })();
            if let Err(error) = started {
                super::receive::reload(conn, engine)?;
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
