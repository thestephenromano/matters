//! Receiving other devices' sealed records (#39, #45,
//! docs/SYNC_DURABLE_RECEIVE.md, docs/SYNC_RECORD_CONTRACT.md "Receiver and
//! relay obligations").
//!
//! [`Database::receive_records`] takes a page of records in one
//! transaction. Each record ends in exactly one outcome, final for this
//! delivery: applied, a duplicate, kept waiting (for a named reason, and
//! retried only when what it waits for arrives), a checkpoint part, or
//! refused. A refused record is quarantined with its reason, and the page
//! goes on. Only a failure of the store itself (SQLite, I/O) aborts the
//! page, so it is fetched again; nothing a member or the relay can put in a
//! record does.
//!
//! Each record is checked in this order before anything is written:
//! 1. it parses for this collection's document;
//! 2. its record ID and digest: the same bytes already held are a
//!    duplicate, other bytes under a held ID are evidence;
//! 3. a kept, already authenticated record still out of its chain's order
//!    waits on, with no cryptography;
//! 4. the key of the epoch it was sealed in. Without one, its signature is
//!    checked against the roster of the manifest it names, if this device
//!    knows that manifest; if not, it is kept unauthenticated, within a
//!    small budget, and never applied or carried without authentication;
//! 5. its signature, its author's membership in that epoch and its
//!    writer's admission (`Authority::authenticate_at`). A record a removed
//!    author signed outside every closing prefix is kept as revoked, never
//!    applied while so; any other refusal there is final, for this record;
//! 6. its writer's lineage: frozen here on evidence, closed (by a signed
//!    handoff or an admin's closing frontier) at a head this device already
//!    holds, or forked by a second signed successor of one record;
//! 7. its place in its writer's chain;
//! 8. that its engine update carries only its writer's actor, and that the
//!    engine holds every operation it depends on.
//!
//! The record then applies: merged, journalled, recorded with its
//! predecessor and position, and its writer's chain head advanced. Once
//! the page's records are in, the rows are rebuilt once (#241). If the
//! merged document cannot be projected, the page runs again with each
//! update's effect checked on its own, and the one that breaks the document
//! is refused, so the engine and the rows never part.
//!
//! A `snapshot` record is a checkpoint part (#40). It is authenticated and
//! opened the same way, outside any chain, and handed to the checkpoint
//! module (`checkpoint.rs`), which keeps it until its checkpoint is
//! complete.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use matters_sync::authority::{self, Boundary, EpochKey, Hash, Member};
use matters_sync::checkpoint::Sealed;
use matters_sync::record::{self as sealed_record, record_hash};
use matters_sync::wire;
use rusqlite::{params, Connection, OptionalExtension};

use super::checkpoint::{self, Checkpointed};
use super::db::Database;
use super::membership::{Joined, HOME_DOCUMENT};
use super::sync::{load_engine, rebuild_rows};
use crate::crdt::Document;
use crate::error::{MattersError, Result};

/// At most this many records whose signature this device cannot check yet
/// are kept (they name a manifest it does not know), the oldest evicted
/// first.
pub const UNAUTHENTICATED_ROWS: i64 = 64;
/// And at most this many bytes of them.
pub const UNAUTHENTICATED_BYTES: i64 = 8 * 1024 * 1024;
/// At most this many records of one removed author are kept waiting as
/// revoked, the oldest evicted first: they are signed, but a removed device
/// can sign without limit.
pub const REVOKED_ROWS_PER_AUTHOR: i64 = 256;
/// And at most this many bytes of them, per author.
pub const REVOKED_BYTES_PER_AUTHOR: i64 = 16 * 1024 * 1024;
/// At most this many refused records are remembered, unsigned ones
/// forgotten first.
pub const QUARANTINE_ROWS: i64 = 1024;
/// At most this many bytes of signed refused records are kept as evidence;
/// beyond it the oldest keep their reason and lose their bytes.
pub const QUARANTINE_BYTES: i64 = 32 * 1024 * 1024;

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
    /// For this device's envelope of the record's epoch key, or for the
    /// manifest it names.
    Key,
    /// For its writer's admission.
    Writer,
    /// For the record before it in its writer's chain.
    Predecessor,
    /// For engine operations of another record it depends on.
    Dependencies,
    /// Signed by a removed author outside every signed closing prefix:
    /// never applied while so, but kept (within a per-author budget), as a
    /// later transition may name its writer's closing prefix, and tried
    /// again whenever the membership changes.
    Revoked,
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
            Self::Dependencies => "dependencies",
            Self::Revoked => "revoked",
            Self::AfterClose => "after-close",
            Self::Frozen => "frozen",
        }
    }
}

/// The reason of a pending row to receive again as if fresh: a record a
/// checkpoint carried, not yet authenticated here.
const CARRIED: &str = "carried";

fn refused(what: &str) -> MattersError {
    MattersError::Validation(format!("record refused: {what}"))
}

#[cfg(feature = "test-support")]
thread_local! {
    static AUTHENTICATIONS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
    static SETTLES: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// One signature check of a record.
fn counted() {
    #[cfg(feature = "test-support")]
    AUTHENTICATIONS.with(|c| c.set(c.get() + 1));
}

/// How many record signatures this thread has checked while receiving:
/// tests bound the work a page does with it (#45).
#[cfg(feature = "test-support")]
pub fn authentications() -> u64 {
    AUTHENTICATIONS.with(std::cell::Cell::get)
}

/// One update's effect on the document checked on its own, in a careful
/// page.
fn settled() {
    #[cfg(feature = "test-support")]
    SETTLES.with(|c| c.set(c.get() + 1));
}

/// How many updates this thread has checked one by one while receiving:
/// only a careful page does, after a page that could not be projected
/// (#45).
#[cfg(feature = "test-support")]
pub fn settles() -> u64 {
    SETTLES.with(std::cell::Cell::get)
}

/// A record's header facts, copied out of its bytes.
struct Facts {
    kind: wire::Kind,
    epoch: u64,
    manifest: Hash,
    author: Hash,
    incarnation: [u8; 32],
    record_id: [u8; 32],
    previous: Option<Hash>,
    digest: Hash,
}

fn facts(joined: &Joined, bytes: &[u8]) -> std::result::Result<Facts, String> {
    let parsed =
        wire::parse(bytes, joined.route.content(&HOME_DOCUMENT)).map_err(|e| e.to_string())?;
    let h = parsed.header;
    Ok(Facts {
        kind: h.kind,
        epoch: h.epoch,
        manifest: *h.manifest,
        author: *h.author,
        incarnation: *h.incarnation,
        record_id: *h.record_id,
        previous: h.previous.copied(),
        digest: record_hash(bytes),
    })
}

/// Why a record was refused, and whether a member's signature on it was
/// checked: then its bytes are kept as evidence.
struct Refusal {
    reason: String,
    signed: bool,
}

/// How one attempt ended, before anything was written.
enum Attempt {
    Duplicate,
    Wait {
        waiting: Waiting,
        awaits: Option<Vec<u8>>,
        authenticated: bool,
    },
    Refuse(Refusal),
    /// Authenticated, in order, opened and checked: ready to apply.
    Ready {
        actor: u64,
        update: Vec<u8>,
    },
    /// A checkpoint part, authenticated and opened.
    Part {
        sealed: Sealed,
        plaintext: Vec<u8>,
    },
    /// Signed evidence against its writer's lineage: the lineage freezes,
    /// and the record is kept as `waiting`.
    Fork {
        reason: &'static str,
        held: Option<Hash>,
        waiting: Waiting,
    },
}

fn refuse(reason: impl Into<String>, signed: bool) -> Attempt {
    Attempt::Refuse(Refusal {
        reason: reason.into(),
        signed,
    })
}

fn wait(waiting: Waiting, awaits: Option<Vec<u8>>, authenticated: bool) -> Attempt {
    Attempt::Wait {
        waiting,
        awaits,
        authenticated,
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Known {
    No,
    /// These very bytes are held.
    Same,
    /// Other bytes are held under this record ID.
    Other,
}

fn known(conn: &Connection, record_id: &[u8; 32], digest: &Hash) -> Result<Known> {
    let mut found = Known::No;
    // This device's own records and checkpoint parts, by their bytes.
    let mut stmt = conn.prepare_cached(
        "SELECT bytes FROM sync_records WHERE record_id = ?1
         UNION ALL SELECT bytes FROM sync_checkpoint_out WHERE record_id = ?1",
    )?;
    let mut rows = stmt.query(params![record_id.as_slice()])?;
    while let Some(row) = rows.next()? {
        if record_hash(&row.get::<_, Vec<u8>>(0)?) == *digest {
            return Ok(Known::Same);
        }
        found = Known::Other;
    }
    // What was received, by digest. A part kept before v19 has none, and
    // counts as the same.
    let mut stmt = conn.prepare_cached(
        "SELECT digest FROM sync_received WHERE record_id = ?1
         UNION ALL SELECT digest FROM sync_covered WHERE record_id = ?1
         UNION ALL SELECT digest FROM sync_checkpoint_parts WHERE record_id = ?1",
    )?;
    let mut rows = stmt.query(params![record_id.as_slice()])?;
    while let Some(row) = rows.next()? {
        match row.get::<_, Option<Vec<u8>>>(0)? {
            None => return Ok(Known::Same),
            Some(d) if d.as_slice() == digest.as_slice() => return Ok(Known::Same),
            Some(_) => found = Known::Other,
        }
    }
    Ok(found)
}

fn stored_hash(bytes: Vec<u8>) -> Result<Hash> {
    bytes
        .try_into()
        .map_err(|_| MattersError::Engine("a stored record hash".into()))
}

fn head(conn: &Connection, incarnation: &[u8; 32]) -> Result<Option<Hash>> {
    conn.query_row(
        "SELECT head FROM sync_receive_heads WHERE incarnation = ?1",
        params![incarnation.as_slice()],
        |r| r.get::<_, Vec<u8>>(0),
    )
    .optional()?
    .map(stored_hash)
    .transpose()
}

/// A held record of `incarnation` that follows `previous` and is not
/// `digest`: two signed successors of one record are a fork. Rows received
/// before v19 know no predecessor and take no part.
fn sibling(
    conn: &Connection,
    incarnation: &[u8; 32],
    previous: Option<&Hash>,
    digest: &Hash,
) -> Result<Option<Hash>> {
    conn.query_row(
        "SELECT digest FROM sync_received
         WHERE incarnation = ?1 AND previous IS ?2 AND position IS NOT NULL AND digest != ?3
         UNION ALL SELECT digest FROM sync_covered
         WHERE incarnation = ?1 AND previous IS ?2 AND position IS NOT NULL AND digest != ?3
         LIMIT 1",
        params![
            incarnation.as_slice(),
            previous.map(|p| p.as_slice()),
            digest.as_slice()
        ],
        |r| r.get::<_, Vec<u8>>(0),
    )
    .optional()?
    .map(stored_hash)
    .transpose()
}

/// Whether this device holds record `digest` of chain `incarnation`:
/// applied, met again past a checkpoint, or a checkpoint's covered head.
fn holds_record(conn: &Connection, incarnation: &[u8; 32], digest: &Hash) -> Result<bool> {
    Ok(conn
        .query_row(
            "SELECT 1 FROM sync_received WHERE incarnation = ?1 AND digest = ?2
             UNION ALL SELECT 1 FROM sync_covered WHERE incarnation = ?1 AND digest = ?2
             UNION ALL SELECT 1 FROM sync_receive_base WHERE incarnation = ?1 AND base_head = ?2
             LIMIT 1",
            params![incarnation.as_slice(), digest.as_slice()],
            |r| r.get::<_, i64>(0),
        )
        .optional()?
        .is_some())
}

fn locally_frozen(conn: &Connection, author: &Hash) -> Result<bool> {
    Ok(conn
        .query_row(
            "SELECT 1 FROM sync_frozen WHERE author = ?1",
            params![author.as_slice()],
            |r| r.get::<_, i64>(0),
        )
        .optional()?
        .is_some())
}

/// The roster of an accepted manifest: the one place receiving reads it,
/// to check a record it holds no key for yet.
fn roster_at<'a>(joined: &'a Joined, manifest: &Hash) -> Option<(u64, &'a [Member])> {
    joined.authority.roster_at(manifest)
}

/// What to do with a record this device holds no key for: check its
/// signature against the roster of the manifest it names, if that is
/// known, and keep it; keep it unauthenticated, within the budget, if not.
fn key_wait(joined: &Joined, bytes: &[u8], f: &Facts) -> Attempt {
    let Some((epoch, roster)) = roster_at(joined, &f.manifest) else {
        return wait(Waiting::Key, Some(f.manifest.to_vec()), false);
    };
    if epoch != f.epoch {
        return refuse("it names another epoch than its manifest's", false);
    }
    let Some(member) = roster.iter().find(|m| m.id == f.author) else {
        return refuse("its author is not a member of its epoch", false);
    };
    let Ok(parsed) = wire::parse(bytes, joined.route.content(&HOME_DOCUMENT)) else {
        return refuse("it does not parse", false);
    };
    counted();
    match sealed_record::verify(parsed, &member.signing_key) {
        Ok(_) => wait(Waiting::Key, Some(f.manifest.to_vec()), true),
        Err(_) => refuse("the signature does not verify", false),
    }
}

/// This device's key of `manifest`'s epoch, unwrapped once a page, if it
/// holds a usable envelope of it. An envelope this device can no longer
/// open (it was removed, say) is no key: the record waits, authenticated
/// by its roster.
fn epoch_key<'k>(
    keys: &'k mut BTreeMap<Hash, Option<EpochKey>>,
    tx: &Connection,
    joined: &Joined,
    manifest: &Hash,
) -> Result<Option<&'k EpochKey>> {
    if !keys.contains_key(manifest) {
        let envelope: Option<Vec<u8>> = tx
            .query_row(
                "SELECT bytes FROM sync_envelopes WHERE manifest = ?1",
                params![manifest.as_slice()],
                |r| r.get(0),
            )
            .optional()?;
        let key =
            envelope.and_then(|e| joined.authority.unwrap_history_key(&e, &joined.device).ok());
        keys.insert(*manifest, key);
    }
    Ok(keys[manifest].as_ref())
}

/// One page's receiving, in its transaction.
struct Page<'a> {
    tx: &'a Connection,
    joined: &'a Joined,
    engine: &'a mut Option<Document>,
    keys: BTreeMap<Hash, Option<EpochKey>>,
    /// The engine took an update: the rows must be rebuilt.
    changed: bool,
    /// Check each update's effect on the document on its own.
    careful: bool,
    /// Pending rows to try again, by `seq`, each once while queued.
    queue: VecDeque<i64>,
    queued: BTreeSet<i64>,
}

impl Page<'_> {
    fn enqueue(&mut self, seq: i64) {
        if self.queued.insert(seq) {
            self.queue.push_back(seq);
        }
    }
}

/// Decide what to do with `bytes`. `Err` is a failure of the store only:
/// whatever the bytes are, their outcome is an [`Attempt`]. `kept` says
/// they are a kept row already authenticated, whose chain position is
/// checked before any cryptography.
fn attempt(cx: &mut Page, bytes: &[u8], kept: bool) -> Result<(Option<Facts>, Attempt)> {
    let f = match facts(cx.joined, bytes) {
        Ok(f) => f,
        Err(e) => return Ok((None, refuse(e, false))),
    };
    let a = attempt_facts(cx, bytes, &f, kept)?;
    Ok((Some(f), a))
}

fn attempt_facts(cx: &mut Page, bytes: &[u8], f: &Facts, kept: bool) -> Result<Attempt> {
    let tx = cx.tx;
    let joined = cx.joined;
    let held = known(tx, &f.record_id, &f.digest)?;
    if held == Known::Same {
        return Ok(Attempt::Duplicate);
    }
    if !matches!(f.kind, wire::Kind::Data | wire::Kind::Snapshot) {
        return Ok(refuse("neither a data record nor a checkpoint part", false));
    }
    // Still out of order: no need to authenticate it again yet.
    if kept && f.kind == wire::Kind::Data && held == Known::No && f.previous.is_some() {
        let head = head(tx, &f.incarnation)?;
        if f.previous != head
            && sibling(tx, &f.incarnation, f.previous.as_ref(), &f.digest)?.is_none()
        {
            return Ok(wait(
                Waiting::Predecessor,
                f.previous.map(|p| p.to_vec()),
                true,
            ));
        }
    }
    let Some(key) = epoch_key(&mut cx.keys, tx, joined, &f.manifest)? else {
        return Ok(key_wait(joined, bytes, f));
    };
    counted();
    let authenticated = if f.kind == wire::Kind::Snapshot {
        joined
            .authority
            .authenticate_snapshot_at(bytes, key, &HOME_DOCUMENT)
    } else {
        joined.authority.authenticate_at(bytes, key, &HOME_DOCUMENT)
    };
    let authenticated = match authenticated {
        Ok(a) => a,
        Err(authority::Error::MissingWriter) => {
            return Ok(wait(Waiting::Writer, Some(f.incarnation.to_vec()), true))
        }
        // A frozen writer lineage verifies the signature first; a frozen
        // authority does not.
        Err(authority::Error::Fork) => {
            return Ok(wait(Waiting::Frozen, None, !joined.authority.is_forked()))
        }
        // Signed, by an author since removed: kept, tagged with its author
        // and the manifest it was last tried under.
        Err(authority::Error::Revoked) => {
            let mut awaits = f.author.to_vec();
            awaits.extend(joined.authority.manifest());
            return Ok(wait(Waiting::Revoked, Some(awaits), true));
        }
        // Every other refusal is final, for this record: forged, not a
        // member, stale, revoked.
        Err(error) => return Ok(refuse(error.to_string(), false)),
    };
    if held == Known::Other {
        return Ok(refuse(
            "another record under a record ID this device holds",
            true,
        ));
    }
    if locally_frozen(tx, &f.author)? {
        return Ok(wait(Waiting::Frozen, None, true));
    }
    if f.kind == wire::Kind::Snapshot {
        return Ok(match authenticated.open() {
            Ok(plaintext) => Attempt::Part {
                sealed: Sealed {
                    author: f.author,
                    incarnation: f.incarnation,
                    epoch: f.epoch,
                },
                plaintext,
            },
            Err(e) => refuse(e.to_string(), true),
        });
    }
    let head = head(tx, &f.incarnation)?;
    // A retired writer's records end at its signed closing head, by a
    // handoff or an admin's closing frontier: nothing new of it applies
    // once this device holds that head, or holds anything when the chain
    // closed empty. A removed device may have signed on before it learnt
    // of its removal, so that is no fork: the record is kept, never applied.
    if let Ok(history) = joined
        .authority
        .writer_history(HOME_DOCUMENT, f.incarnation)
    {
        if let Some(Boundary::Closed(closing)) = history.closed {
            let past = match closing {
                None => true,
                Some(c) => head == Some(c) || holds_record(tx, &f.incarnation, &c)?,
            };
            if past {
                return Ok(wait(Waiting::AfterClose, None, true));
            }
        }
    }
    if f.previous != head {
        if let Some(held) = sibling(tx, &f.incarnation, f.previous.as_ref(), &f.digest)? {
            return Ok(Attempt::Fork {
                reason: "a second signed successor of one record",
                held: Some(held),
                waiting: Waiting::Frozen,
            });
        }
        return Ok(wait(
            Waiting::Predecessor,
            f.previous.map(|p| p.to_vec()),
            true,
        ));
    }
    let update = match authenticated.open() {
        Ok(u) => u,
        Err(e) => return Ok(refuse(e.to_string(), true)),
    };
    match crate::crdt::update_peers(&update) {
        Ok(peers) if peers == BTreeSet::from([authenticated.actor()]) => {}
        Ok(_) => {
            return Ok(refuse(
                "its update carries another actor than its writer's",
                true,
            ))
        }
        Err(e) => return Ok(refuse(e.to_string(), true)),
    }
    let doc = cx
        .engine
        .as_ref()
        .ok_or_else(|| MattersError::Validation("sync is off".into()))?;
    Ok(match doc.missing_dependency(&update) {
        Ok(None) => Attempt::Ready {
            actor: authenticated.actor(),
            update,
        },
        Ok(Some((peer, counter))) => {
            wait(Waiting::Dependencies, Some(dependency(peer, counter)), true)
        }
        Err(e) => refuse(e.to_string(), true),
    })
}

/// The `awaits` of a record waiting for operation `counter` of `peer`:
/// ordered by peer, then counter.
fn dependency(peer: u64, counter: i32) -> Vec<u8> {
    let mut out = peer.to_be_bytes().to_vec();
    out.extend((counter.max(0) as u32).to_be_bytes());
    out
}

fn dependency_held(doc: &Document, awaits: &[u8]) -> bool {
    let Ok(id) = <[u8; 12]>::try_from(awaits) else {
        return true;
    };
    let peer = u64::from_be_bytes(id[..8].try_into().expect("8 bytes"));
    let counter = u32::from_be_bytes(id[8..].try_into().expect("4 bytes"));
    doc.holds(peer, counter.min(i32::MAX as u32) as i32)
}

/// Rebuild the engine from the journal, keeping its engine actor: after an
/// update it could not take, or a page rolled back.
pub(super) fn reload(conn: &Connection, engine: &mut Option<Document>) -> Result<()> {
    let peer = engine.as_ref().map(Document::peer);
    *engine = load_engine(conn)?;
    if let (Some(doc), Some(peer)) = (engine.as_mut(), peer) {
        doc.set_peer(peer)?;
    }
    Ok(())
}

/// Apply a ready record in the page's transaction: merged into the engine,
/// journalled, recorded as received with its predecessor and position, its
/// chain head advanced, and the rows that waited for it queued. An update
/// the engine cannot take, or (when careful) that leaves the document
/// unreadable, refuses the record and leaves the engine as it was.
fn apply(
    cx: &mut Page,
    f: &Facts,
    actor: u64,
    update: &[u8],
) -> Result<std::result::Result<(), String>> {
    let doc = cx
        .engine
        .as_mut()
        .ok_or_else(|| MattersError::Validation("sync is off".into()))?;
    let failed = match doc.merge_unindexed(update) {
        Err(e) => Some(e.to_string()),
        Ok(()) if cx.careful => {
            settled();
            doc.settle()
                .err()
                .map(|e| format!("its update leaves the document unreadable: {e}"))
        }
        Ok(()) => None,
    };
    if let Some(reason) = failed {
        // The journal holds everything applied before it.
        reload(cx.tx, &mut *cx.engine)?;
        return Ok(Err(reason));
    }
    let tx = cx.tx;
    tx.execute(
        "INSERT INTO sync_journal (bytes, outgoing) VALUES (?1, 0)",
        params![update],
    )?;
    // One past its predecessor's: the chain's head, held as received, met
    // again past a checkpoint, or the checkpoint's covered head.
    let position: Option<i64> = tx.query_row(
        "SELECT CASE WHEN ?2 IS NULL THEN 1 ELSE COALESCE(
             (SELECT position FROM sync_received WHERE incarnation = ?1 AND digest = ?2),
             (SELECT position FROM sync_covered WHERE incarnation = ?1 AND digest = ?2),
             (SELECT base FROM sync_receive_base WHERE incarnation = ?1 AND base_head = ?2))
             + 1 END",
        params![
            f.incarnation.as_slice(),
            f.previous.as_ref().map(|p| p.as_slice())
        ],
        |r| r.get(0),
    )?;
    // A predecessor received before v19 has none: count the chain.
    let position: i64 = match position {
        Some(p) => p,
        None => tx.query_row(
            "SELECT COALESCE((SELECT base FROM sync_receive_base WHERE incarnation = ?1), 0)
                  + (SELECT COUNT(*) FROM sync_received WHERE incarnation = ?1) + 1",
            params![f.incarnation.as_slice()],
            |r| r.get(0),
        )?,
    };
    tx.execute(
        "INSERT INTO sync_received (record_id, incarnation, digest, previous, position)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![
            f.record_id.as_slice(),
            f.incarnation.as_slice(),
            f.digest.as_slice(),
            f.previous.as_ref().map(|p| p.as_slice()),
            position
        ],
    )?;
    tx.execute(
        "INSERT INTO sync_receive_heads (incarnation, head) VALUES (?1, ?2)
         ON CONFLICT (incarnation) DO UPDATE SET head = excluded.head",
        params![f.incarnation.as_slice(), f.digest.as_slice()],
    )?;
    unkeep(tx, f)?;
    cx.changed = true;
    // Its successor, and what waited for its operations, may apply now.
    let mut unblocked = Vec::new();
    {
        let mut stmt = tx.prepare_cached(
            "SELECT seq FROM sync_pending WHERE reason = 'predecessor' AND awaits = ?1",
        )?;
        let rows = stmt.query_map(params![f.digest.as_slice()], |r| r.get::<_, i64>(0))?;
        for seq in rows {
            unblocked.push(seq?);
        }
        let mut stmt = tx.prepare_cached(
            "SELECT seq, awaits FROM sync_pending
             WHERE reason = 'dependencies' AND awaits BETWEEN ?1 AND ?2",
        )?;
        let rows = stmt.query_map(
            params![dependency(actor, 0), dependency(actor, i32::MAX)],
            |r| Ok((r.get::<_, i64>(0)?, r.get::<_, Vec<u8>>(1)?)),
        )?;
        let doc = cx.engine.as_ref().expect("the engine took the update");
        for row in rows {
            let (seq, awaits) = row?;
            if dependency_held(doc, &awaits) {
                unblocked.push(seq);
            }
        }
    }
    for seq in unblocked {
        cx.enqueue(seq);
    }
    Ok(Ok(()))
}

/// Forget a kept row: these exact bytes are settled.
fn unkeep(tx: &Connection, f: &Facts) -> Result<()> {
    tx.execute(
        "DELETE FROM sync_pending WHERE record_id = ?1 AND digest = ?2",
        params![f.record_id.as_slice(), f.digest.as_slice()],
    )?;
    Ok(())
}

/// Keep `bytes` waiting, or update the reason of the row that holds them.
fn keep(
    tx: &Connection,
    f: &Facts,
    bytes: &[u8],
    reason: &str,
    awaits: Option<&[u8]>,
    authenticated: bool,
) -> Result<()> {
    tx.execute(
        "INSERT INTO sync_pending (record_id, digest, bytes, reason, awaits, authenticated)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)
         ON CONFLICT (record_id, digest) DO UPDATE SET
             reason = excluded.reason, awaits = excluded.awaits,
             authenticated = MAX(authenticated, excluded.authenticated)",
        params![
            f.record_id.as_slice(),
            f.digest.as_slice(),
            bytes,
            reason,
            awaits,
            authenticated
        ],
    )?;
    if !authenticated {
        evict_unauthenticated(tx)?;
    }
    if reason == Waiting::Revoked.as_str() {
        evict_revoked(tx, &f.author)?;
    }
    Ok(())
}

/// Hold a removed author's revoked records to their budget, the oldest
/// evicted first.
fn evict_revoked(tx: &Connection, author: &Hash) -> Result<()> {
    tx.execute(
        &format!(
            "DELETE FROM sync_pending WHERE seq IN ({})",
            beyond_budget(
                "reason = 'revoked' AND substr(awaits, 1, 48) = ?1",
                "?2",
                "?3"
            )
        ),
        params![
            author.as_slice(),
            REVOKED_ROWS_PER_AUTHOR,
            REVOKED_BYTES_PER_AUTHOR
        ],
    )?;
    Ok(())
}

/// The `seq` of the rows of `table` matching `filter` that a budget of
/// `rows` rows and `bytes` bytes leaves out, keeping the newest: each
/// row's rank and running total of bytes, newest first, past either
/// limit. The same rows as evicting the oldest one at a time until both
/// hold, in one statement.
fn beyond_budget(filter: &str, rows: &str, bytes: &str) -> String {
    format!(
        "SELECT seq FROM (
             SELECT seq, ROW_NUMBER() OVER newest AS n, SUM(length(bytes)) OVER newest AS kept
             FROM sync_pending WHERE {filter}
             WINDOW newest AS (ORDER BY seq DESC))
         WHERE n > {rows} OR kept > {bytes}"
    )
}

/// Hold the records kept without authentication to their budget, the
/// oldest evicted first. Carried records are received again in the same
/// transaction that kept them, so they are outside it.
fn evict_unauthenticated(tx: &Connection) -> Result<()> {
    tx.execute(
        &format!(
            "DELETE FROM sync_pending WHERE seq IN ({})",
            beyond_budget("authenticated = 0 AND reason != 'carried'", "?1", "?2")
        ),
        params![UNAUTHENTICATED_ROWS, UNAUTHENTICATED_BYTES],
    )?;
    Ok(())
}

/// Remember a refused record: its ID, digest and reason, and its bytes if
/// a member signed it, within the quarantine's budget.
fn quarantine(tx: &Connection, f: &Facts, bytes: &[u8], r: &Refusal) -> Result<()> {
    tx.execute(
        "INSERT INTO sync_quarantine (record_id, digest, reason, bytes) VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT (record_id, digest) DO UPDATE SET reason = excluded.reason,
             bytes = COALESCE(excluded.bytes, bytes)",
        params![
            f.record_id.as_slice(),
            f.digest.as_slice(),
            r.reason,
            r.signed.then_some(bytes)
        ],
    )?;
    tx.execute(
        "DELETE FROM sync_quarantine WHERE seq IN (
             SELECT seq FROM sync_quarantine ORDER BY (bytes IS NOT NULL), seq
             LIMIT MAX(0, (SELECT COUNT(*) FROM sync_quarantine) - ?1))",
        params![QUARANTINE_ROWS],
    )?;
    // The oldest lose their bytes first: those whose running total, newest
    // first, passes the budget.
    tx.execute(
        "UPDATE sync_quarantine SET bytes = NULL WHERE seq IN (
             SELECT seq FROM (
                 SELECT seq, SUM(length(bytes)) OVER (ORDER BY seq DESC) AS kept
                 FROM sync_quarantine WHERE bytes IS NOT NULL)
             WHERE kept > ?1)",
        params![QUARANTINE_BYTES],
    )?;
    unkeep(tx, f)
}

/// Freeze `f`'s author's writer lineage here, keeping the record that
/// contradicted it and the digest of the one held at its place.
fn freeze(
    tx: &Connection,
    f: &Facts,
    bytes: &[u8],
    reason: &str,
    held: Option<Hash>,
) -> Result<()> {
    tx.execute(
        "INSERT INTO sync_frozen (author, reason, held, bytes) VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT (author) DO NOTHING",
        params![
            f.author.as_slice(),
            reason,
            held.as_ref().map(|h| h.as_slice()),
            bytes
        ],
    )?;
    Ok(())
}

/// Act on one attempt in the page's transaction: the record's outcome.
fn settle(
    cx: &mut Page,
    bytes: &[u8],
    f: Option<Facts>,
    attempt: Attempt,
) -> Result<Result<Received>> {
    let Some(f) = f else {
        // Not a record of this collection: nothing to keep it by.
        return Ok(Err(match attempt {
            Attempt::Refuse(r) => refused(&r.reason),
            _ => refused("it does not parse"),
        }));
    };
    Ok(match attempt {
        Attempt::Duplicate => {
            unkeep(cx.tx, &f)?;
            Ok(Received::Duplicate)
        }
        Attempt::Wait {
            waiting,
            awaits,
            authenticated,
        } => {
            keep(
                cx.tx,
                &f,
                bytes,
                waiting.as_str(),
                awaits.as_deref(),
                authenticated,
            )?;
            Ok(Received::Pending(waiting))
        }
        Attempt::Refuse(r) => {
            quarantine(cx.tx, &f, bytes, &r)?;
            Err(refused(&r.reason))
        }
        Attempt::Ready { actor, update } => match apply(cx, &f, actor, &update)? {
            Ok(()) => Ok(Received::Applied),
            Err(reason) => {
                let r = Refusal {
                    reason,
                    signed: true,
                };
                quarantine(cx.tx, &f, bytes, &r)?;
                Err(refused(&r.reason))
            }
        },
        Attempt::Part { sealed, plaintext } => {
            match checkpoint::keep_part(
                cx.tx,
                cx.engine,
                cx.joined,
                &f.record_id,
                &f.digest,
                &sealed,
                &plaintext,
            )? {
                Ok(outcome) => {
                    unkeep(cx.tx, &f)?;
                    Ok(Received::Checkpoint(outcome))
                }
                Err(reason) => {
                    let r = Refusal {
                        reason: format!("checkpoint part: {reason}"),
                        signed: true,
                    };
                    quarantine(cx.tx, &f, bytes, &r)?;
                    Err(refused(&r.reason))
                }
            }
        }
        Attempt::Fork {
            reason,
            held,
            waiting,
        } => {
            freeze(cx.tx, &f, bytes, reason, held)?;
            keep(cx.tx, &f, bytes, waiting.as_str(), None, true)?;
            Ok(Received::Pending(waiting))
        }
    })
}

/// Receive one record fresh, in the page's transaction.
fn receive_one(cx: &mut Page, bytes: &[u8]) -> Result<Result<Received>> {
    let (f, a) = attempt(cx, bytes, false)?;
    settle(cx, bytes, f, a)
}

/// Queue the kept rows that something outside a page may have unblocked:
/// a key or manifest that arrived, a writer admitted, a chain head a
/// checkpoint set, engine operations a snapshot brought. Each check is
/// cheap, row by row, with no cryptography.
fn seed(cx: &mut Page) -> Result<()> {
    let tx = cx.tx;
    let joined = cx.joined;
    let mut ready = Vec::new();
    {
        let mut stmt = tx.prepare(
            "SELECT seq, awaits, authenticated FROM sync_pending WHERE reason = 'key' ORDER BY seq",
        )?;
        let mut rows = stmt.query([])?;
        let mut manifests: BTreeMap<Hash, bool> = BTreeMap::new();
        while let Some(row) = rows.next()? {
            let (seq, awaits, authenticated): (i64, Option<Vec<u8>>, bool) =
                (row.get(0)?, row.get(1)?, row.get(2)?);
            let Some(manifest) = awaits.and_then(|a| Hash::try_from(a).ok()) else {
                ready.push(seq);
                continue;
            };
            let has_key = match manifests.get(&manifest) {
                Some(&k) => k,
                None => {
                    let k = epoch_key(&mut cx.keys, tx, joined, &manifest)?.is_some();
                    manifests.insert(manifest, k);
                    k
                }
            };
            if has_key || (!authenticated && roster_at(joined, &manifest).is_some()) {
                ready.push(seq);
            }
        }
    }
    {
        let histories = joined.authority.writer_histories(HOME_DOCUMENT);
        let mut stmt = tx
            .prepare("SELECT seq, awaits FROM sync_pending WHERE reason = 'writer' ORDER BY seq")?;
        let mut rows = stmt.query([])?;
        while let Some(row) = rows.next()? {
            let (seq, awaits): (i64, Option<Vec<u8>>) = (row.get(0)?, row.get(1)?);
            let admitted = awaits
                .and_then(|a| <[u8; 32]>::try_from(a).ok())
                .is_none_or(|i| histories.contains_key(&i));
            if admitted {
                ready.push(seq);
            }
        }
    }
    {
        // A closing frontier arrives with a membership transition, which
        // changes the manifest: only then may a revoked record pass.
        let mut stmt = tx.prepare(
            "SELECT seq FROM sync_pending
             WHERE reason = 'revoked' AND substr(awaits, 49) != ?1 ORDER BY seq",
        )?;
        let rows = stmt.query_map(params![joined.authority.manifest().as_slice()], |r| {
            r.get::<_, i64>(0)
        })?;
        for seq in rows {
            ready.push(seq?);
        }
    }
    {
        let mut stmt = tx.prepare(
            "SELECT p.seq FROM sync_pending p JOIN sync_receive_heads h ON p.awaits = h.head
             WHERE p.reason = 'predecessor' ORDER BY p.seq",
        )?;
        let rows = stmt.query_map([], |r| r.get::<_, i64>(0))?;
        for seq in rows {
            ready.push(seq?);
        }
    }
    if let Some(doc) = cx.engine.as_ref() {
        let mut stmt = tx.prepare(
            "SELECT seq, awaits FROM sync_pending WHERE reason = 'dependencies' ORDER BY seq",
        )?;
        let mut rows = stmt.query([])?;
        while let Some(row) = rows.next()? {
            let (seq, awaits): (i64, Option<Vec<u8>>) = (row.get(0)?, row.get(1)?);
            if awaits.is_none_or(|a| dependency_held(doc, &a)) {
                ready.push(seq);
            }
        }
    }
    for seq in ready {
        cx.enqueue(seq);
    }
    Ok(())
}

/// Try the queued rows again, and whatever each one applied unblocks.
fn run_queue(cx: &mut Page) -> Result<()> {
    while let Some(seq) = cx.queue.pop_front() {
        cx.queued.remove(&seq);
        let row: Option<(Vec<u8>, String, bool)> = cx
            .tx
            .query_row(
                "SELECT bytes, reason, authenticated FROM sync_pending WHERE seq = ?1",
                params![seq],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()?;
        let Some((bytes, reason, authenticated)) = row else {
            continue;
        };
        let kept = authenticated && reason != CARRIED;
        let (f, a) = attempt(cx, &bytes, kept)?;
        // A kept row's outcome is its own; nobody waits for it.
        let _outcome = settle(cx, &bytes, f, a)?;
    }
    Ok(())
}

/// Receive again, once, what was kept before v19 and what a checkpoint
/// carried: neither was authenticated here.
fn receive_again(cx: &mut Page) -> Result<()> {
    loop {
        let row: Option<(i64, Vec<u8>)> = cx
            .tx
            .query_row(
                "SELECT rowid, bytes FROM sync_pending_v16 ORDER BY seq LIMIT 1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        let Some((rowid, bytes)) = row else {
            break;
        };
        cx.tx.execute(
            "DELETE FROM sync_pending_v16 WHERE rowid = ?1",
            params![rowid],
        )?;
        let _outcome = receive_one(cx, &bytes)?;
    }
    let carried: Vec<i64> = {
        let mut stmt = cx
            .tx
            .prepare("SELECT seq FROM sync_pending WHERE reason = ?1 ORDER BY seq")?;
        let rows = stmt.query_map(params![CARRIED], |r| r.get(0))?;
        rows.collect::<rusqlite::Result<_>>()?
    };
    for seq in carried {
        cx.enqueue(seq);
    }
    Ok(())
}

/// Retry what the page, or what arrived since the last one, unblocked, in
/// the page's transaction. Each kept row is tried again only when what it
/// waits for has arrived.
fn drain_in(cx: &mut Page) -> Result<()> {
    checkpoint::walk_covered(cx.tx, cx.joined)?;
    receive_again(cx)?;
    seed(cx)?;
    run_queue(cx)?;
    checkpoint::walk_covered(cx.tx, cx.joined)?;
    checkpoint::finish_waiting(cx.tx, cx.engine, cx.joined)?;
    Ok(())
}

/// The page could not be projected as merged: run it again, carefully.
struct Unsettled;

type Outcomes = Vec<Result<Received>>;

fn run_page(
    conn: &mut Connection,
    engine: &mut Option<Document>,
    joined: &Joined,
    records: &[&[u8]],
    careful: bool,
) -> Result<std::result::Result<Outcomes, Unsettled>> {
    let tx = conn.transaction()?;
    let received = (|| {
        if careful {
            // A document that cannot be projected before the page is no
            // record's fault.
            engine
                .as_mut()
                .ok_or_else(|| MattersError::Validation("sync is off".into()))?
                .settle()?;
        }
        let mut cx = Page {
            tx: &tx,
            joined,
            engine: &mut *engine,
            keys: BTreeMap::new(),
            changed: false,
            careful,
            queue: VecDeque::new(),
            queued: BTreeSet::new(),
        };
        let mut outcomes = Vec::with_capacity(records.len());
        for bytes in records {
            outcomes.push(receive_one(&mut cx, bytes)?);
        }
        drain_in(&mut cx)?;
        if cx.changed {
            let doc = engine
                .as_mut()
                .ok_or_else(|| MattersError::Validation("sync is off".into()))?;
            // A careful page checked each update as it applied, so it is
            // projected; one that still is not is the engine's failure,
            // which receive_all reports.
            match doc.settle() {
                Ok(snapshot) => rebuild_rows(&tx, &snapshot)?,
                Err(_) => return Ok(Err(Unsettled)),
            }
        }
        Ok::<_, MattersError>(Ok(outcomes))
    })();
    match received {
        Ok(Ok(outcomes)) => {
            super::sync::crash_point("receive-before-commit");
            match tx.commit() {
                Ok(()) => Ok(Ok(outcomes)),
                Err(error) => {
                    reload(conn, engine)?;
                    Err(error.into())
                }
            }
        }
        Ok(Err(unsettled)) => {
            drop(tx);
            reload(conn, engine)?;
            Ok(Err(unsettled))
        }
        Err(error) => {
            drop(tx);
            reload(conn, engine)?;
            Err(error)
        }
    }
}

/// Receive `records` in order, then retry what they unblocked, all in one
/// transaction: the rows are rebuilt once, however many records applied
/// (#241). Each record has its own outcome. Only a failure of the store
/// rolls the page back, and the engine is reloaded.
fn receive_all(
    conn: &mut Connection,
    engine: &mut Option<Document>,
    joined: &Joined,
    records: &[&[u8]],
) -> Result<Outcomes> {
    match run_page(conn, engine, joined, records, false)? {
        Ok(outcomes) => Ok(outcomes),
        Err(Unsettled) => match run_page(conn, engine, joined, records, true)? {
            Ok(outcomes) => Ok(outcomes),
            Err(Unsettled) => Err(MattersError::Engine(
                "the received records leave the document unreadable".into(),
            )),
        },
    }
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

/// Keep `bytes`, a record a checkpoint carried, for the next drain to
/// receive again: it is authenticated here only then.
pub(super) fn keep_carried(tx: &Connection, joined: &Joined, bytes: &[u8]) -> Result<()> {
    let f = facts(joined, bytes).map_err(|e| refused(&format!("carried record: {e}")))?;
    tx.execute(
        "INSERT INTO sync_pending (record_id, digest, bytes, reason, awaits, authenticated)
         VALUES (?1, ?2, ?3, ?4, NULL, 0) ON CONFLICT (record_id, digest) DO NOTHING",
        params![f.record_id.as_slice(), f.digest.as_slice(), bytes, CARRIED],
    )?;
    Ok(())
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
    let f = facts(joined, bytes).map_err(|e| refused(&e))?;
    if f.kind != wire::Kind::Snapshot {
        return Err(refused("not a checkpoint part"));
    }
    let mut keys = BTreeMap::new();
    let Some(key) = epoch_key(&mut keys, conn, joined, &f.manifest)? else {
        return Ok(None);
    };
    counted();
    let authenticated = match joined
        .authority
        .authenticate_snapshot_at(bytes, key, &HOME_DOCUMENT)
    {
        Ok(a) => a,
        Err(authority::Error::MissingWriter) => return Ok(None),
        Err(error) => return Err(refused(&error.to_string())),
    };
    let plaintext = authenticated.open().map_err(|e| refused(&e.to_string()))?;
    let sealed = Sealed {
        author: f.author,
        incarnation: f.incarnation,
        epoch: f.epoch,
    };
    Ok(Some((f.record_id, sealed, plaintext)))
}

/// A refused record remembered: its record ID, its reason, and whether its
/// signed bytes are kept as evidence.
pub type Quarantined = ([u8; 32], (String, bool));

fn record_ids<T>(
    rows: impl Iterator<Item = rusqlite::Result<(Vec<u8>, T)>>,
) -> Result<Vec<([u8; 32], T)>> {
    let mut out = Vec::new();
    for row in rows {
        let (id, rest) = row?;
        out.push((
            id.try_into()
                .map_err(|_| MattersError::Validation("stored record ID".into()))?,
            rest,
        ));
    }
    Ok(out)
}

impl Database {
    /// Receive one sealed record fetched from the relay. Applies it if it
    /// can, keeps it pending if it must wait, and refuses forged or
    /// malformed bytes. Then retries whatever it unblocked.
    pub fn receive_record(&self, bytes: &[u8]) -> Result<Received> {
        self.receive_records(&[bytes])?
            .pop()
            .expect("one outcome for one record")
    }

    /// Receive a page of records, in order, as `receive_record` does each,
    /// in one transaction with one rebuild of the rows (#241). Each record
    /// has its own outcome: a refused one is an error, quarantined with its
    /// reason, and the others go on. The outer `Err` is a failure of the
    /// store, which applied none of the page.
    pub fn receive_records(&self, records: &[&[u8]]) -> Result<Outcomes> {
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

    /// The records kept pending, with why, oldest first. Rows kept before
    /// v19 are listed until the next receive takes them again.
    pub fn pending_records(&self) -> Result<Vec<([u8; 32], String)>> {
        self.with_conn(|conn| {
            let mut stmt = conn.prepare(
                "SELECT record_id, reason FROM (
                     SELECT 0 AS k, seq AS s, record_id, reason FROM sync_pending_v16
                     UNION ALL SELECT 1, seq, record_id, reason FROM sync_pending)
                 ORDER BY k, s",
            )?;
            let rows = stmt.query_map([], |r| Ok((r.get::<_, Vec<u8>>(0)?, r.get(1)?)))?;
            record_ids(rows)
        })
    }

    /// How many pending records are kept without authentication, and
    /// their bytes.
    pub fn unauthenticated_pending(&self) -> Result<(i64, i64)> {
        self.with_conn(|conn| {
            Ok(conn.query_row(
                "SELECT COUNT(*), COALESCE(SUM(length(bytes)), 0) FROM sync_pending
                 WHERE authenticated = 0",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )?)
        })
    }

    /// The refused records remembered, oldest first: record ID, reason, and
    /// whether their signed bytes are kept as evidence.
    pub fn quarantined_records(&self) -> Result<Vec<Quarantined>> {
        self.with_conn(|conn| {
            let mut stmt = conn.prepare(
                "SELECT record_id, reason, bytes IS NOT NULL FROM sync_quarantine ORDER BY seq",
            )?;
            let rows = stmt.query_map([], |r| {
                Ok((r.get::<_, Vec<u8>>(0)?, (r.get(1)?, r.get(2)?)))
            })?;
            record_ids(rows)
        })
    }

    /// The writer lineages this device froze on evidence it holds: the
    /// author's device ID and why.
    pub fn frozen_lineages(&self) -> Result<Vec<(Hash, String)>> {
        self.with_conn(|conn| {
            let mut stmt =
                conn.prepare("SELECT author, reason FROM sync_frozen ORDER BY author")?;
            let rows = stmt.query_map([], |r| Ok((r.get::<_, Vec<u8>>(0)?, r.get(1)?)))?;
            let mut out = Vec::new();
            for row in rows {
                let (author, reason) = row?;
                out.push((stored_hash(author)?, reason));
            }
            Ok(out)
        })
    }
}
