use crate::{invalid, model, Command, Projection, Result};
use loro::{ExportMode, LoroDoc};
use rusqlite::{params, Connection, OptionalExtension, Transaction, TransactionBehavior};
use serde::{Deserialize, Serialize};
use std::path::Path;

/// Local test transport only: unauthenticated and unencrypted. Never send to a service.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Record {
    pub id: String,
    pub bytes: Vec<u8>,
}

/// Retain/serialize BEFORE submission; reuse after an uncertain acknowledgement.
/// A fresh ID means a new intent even when commands are identical.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Request {
    pub id: String,
    pub commands: Vec<Command>,
}
impl Request {
    pub fn new(command: Command) -> Self {
        Self::batch([command])
    }
    pub fn batch(commands: impl IntoIterator<Item = Command>) -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            commands: commands.into_iter().collect(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Fault {
    #[default]
    None,
    BeforeCommit,
    AfterCommit,
    ExitBeforeCommit,
    ExitAfterCommit,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Admission {
    Received,
    Pending,
    Applied,
    Quarantined,
}
impl Admission {
    fn parse(value: &str) -> Result<Self> {
        match value {
            "received" => Ok(Self::Received),
            "pending" => Ok(Self::Pending),
            "applied" => Ok(Self::Applied),
            "quarantined" => Ok(Self::Quarantined),
            _ => Err(invalid("unknown admission state")),
        }
    }
}
#[derive(Debug, Clone)]
pub struct InboxEntry {
    pub record: Record,
    pub status: Admission,
    pub reason: String,
    /// Another retained record used this transport ID with different bytes.
    /// IDs are unauthenticated transport metadata; each record is judged by
    /// its own bytes, so reuse is flagged rather than deciding a winner.
    pub record_id_conflict: bool,
}
#[derive(Debug, Clone, Copy, Default)]
pub struct RestoreStats {
    pub snapshot_loaded: bool,
    pub replayed_records: usize,
    /// Saved requests a retired writer never sealed, re-run as the new writer.
    pub reissued_requests: usize,
}

/// Trusted test-only participant in the local SQLite write boundary. It must
/// write its intent through this transaction and must not commit or publish it.
/// The integrated store marker prevents reopening through the standalone API.
/// Reachable only with the `local-outbox` feature (see `lib.rs`).
pub trait LocalOutbox {
    fn actor(&self) -> u64;
    fn check_owner(&self, tx: &Transaction<'_>) -> Result<()>;
    fn enqueue(&self, tx: &Transaction<'_>, record: &Record) -> Result<()>;
    /// Request IDs whose intents a retired writer left unsealed, oldest
    /// first. Their operations carry that writer's actor, which no receiver
    /// accepts from any other writer, so they can never be sent as they are.
    fn stranded(&self, tx: &Transaction<'_>) -> Result<Vec<String>>;
    /// Point a stranded request's intent at this writer, with the operations
    /// its request produced when re-run as this writer.
    fn reissue(&self, tx: &Transaction<'_>, record: &Record) -> Result<()>;
}
pub const ATOMIC_FORMAT: &str = "matters-sync-atomic-outbox/1";
/// How long an integrated store's connections wait for another short write
/// on the same file (a seal or an acknowledgement) before failing.
pub const INTEGRATED_BUSY_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(1);
type OutboxSetup = fn(&Transaction<'_>, bool) -> Result<Box<dyn LocalOutbox>>;
type Committed = fn() -> Result<()>;

pub struct Replica {
    conn: Connection,
    doc: LoroDoc,
    state: Projection,
    identities: model::Index,
    jitter: u8,
    restore: RestoreStats,
    local_outbox: Option<Box<dyn LocalOutbox>>,
}
// Eight random bytes allocate the width of Loro's 64-bit peer namespace to
// same-gap tie avoidance. This is a position-size budget, not a security claim.
const CONCURRENT_JITTER_BYTES: u8 = 8;
fn configure(doc: &LoroDoc, jitter: u8) {
    let tree = doc.get_tree("home");
    // Loro 1.18 initially seeds the tree RNG with zero; re-enabling after disabling
    // seeds it from the document peer, giving distinct peers distinct streams.
    tree.disable_fractional_index();
    tree.enable_fractional_index(jitter);
}
impl Replica {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        Self::from_connection(
            Connection::open(path)?,
            true,
            None::<(OutboxSetup, Committed)>,
        )
    }
    /// Explicit volatile replica for semantic fixtures; never a persistent WAL fallback.
    pub fn open_in_memory() -> Result<Self> {
        Self::from_connection(
            Connection::open_in_memory()?,
            false,
            None::<(OutboxSetup, Committed)>,
        )
    }
    /// Open a dedicated integrated experiment. Schema and writer admission
    /// commit together; `committed` runs right after that commit, before the
    /// rest of the open. Existing standalone databases are never converted.
    #[cfg(feature = "local-outbox")]
    pub fn open_with_outbox(
        path: impl AsRef<Path>,
        setup: impl FnOnce(&Transaction<'_>, bool) -> Result<Box<dyn LocalOutbox>>,
        committed: impl FnOnce() -> Result<()>,
    ) -> Result<Self> {
        Self::from_connection(Connection::open(path)?, true, Some((setup, committed)))
    }
    fn from_connection(
        mut conn: Connection,
        persistent: bool,
        setup: Option<(
            impl FnOnce(&Transaction<'_>, bool) -> Result<Box<dyn LocalOutbox>>,
            impl FnOnce() -> Result<()>,
        )>,
    ) -> Result<Self> {
        let (setup, committed) = setup.unzip();
        let local_outbox = initialize(&mut conn, persistent, setup)?;
        if let Some(committed) = committed {
            committed()?;
        }
        let integrated = local_outbox.is_some();
        let doc = LoroDoc::new();
        let mut restore = RestoreStats::default();
        // Restore, re-run stranded requests and publish the projection in one
        // write transaction: a failure leaves the store as it was, and the
        // next open tries again.
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if let Some(outbox) = &local_outbox {
            outbox.check_owner(&tx)?;
        }
        let stranded = match &local_outbox {
            Some(outbox) => stranded_requests(&tx, &outbox.stranded(&tx)?)?,
            None => vec![],
        };
        let first_stranded = stranded.first().map(|s| s.seq);
        let checkpoint: Option<(Vec<u8>, i64, Option<Vec<u8>>)> = tx
            .query_row(
                "SELECT bytes,through_seq,pending FROM checkpoint WHERE singleton=1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()?;
        let through_seq = match checkpoint {
            // A checkpoint holding stranded operations cannot be used.
            Some((_, seq, _)) if first_stranded.is_some_and(|first| seq >= first) => {
                tx.execute("DELETE FROM checkpoint", [])?;
                0
            }
            Some((bytes, seq, pending)) => {
                doc.import(&bytes)?;
                if let Some(pending) = pending {
                    doc.import(&pending)?;
                }
                restore.snapshot_loaded = true;
                seq
            }
            None => 0,
        };
        {
            let mut query =
                tx.prepare("SELECT seq,bytes FROM journal WHERE seq>?1 ORDER BY seq")?;
            let rows = query.query_map([through_seq], |r| {
                Ok((r.get::<_, i64>(0)?, r.get::<_, Vec<u8>>(1)?))
            })?;
            for row in rows {
                let (seq, bytes) = row?;
                if stranded.iter().any(|s| s.seq == seq) {
                    continue;
                }
                doc.import(&bytes)?;
                restore.replayed_records += 1;
            }
        }
        if let Some(outbox) = &local_outbox {
            if doc.oplog_vv().get(&outbox.actor()).copied().unwrap_or(0) != 0 {
                return Err(invalid("new outbox actor already has domain history"));
            }
            doc.set_peer_id(outbox.actor())?;
        }
        configure(&doc, CONCURRENT_JITTER_BYTES);
        let mut identities = model::Index::build(&doc)?;
        if let Some(outbox) = &local_outbox {
            // Only the stranded requests' own no-op neighbours can follow them
            // (sealing is in order, and new edits waited for this), so each
            // re-runs on the state it first ran on.
            for request in stranded {
                let before = doc.oplog_vv();
                for command in serde_json::from_str::<Vec<Command>>(&request.body)? {
                    model::apply(&doc, &mut identities, command)?;
                }
                if doc.oplog_vv() == before {
                    return Err(invalid("a stranded request no longer changes the document"));
                }
                let record = Record {
                    id: request.id,
                    bytes: doc.export(ExportMode::updates(&before))?,
                };
                tx.execute(
                    "UPDATE journal SET bytes=?1 WHERE seq=?2",
                    params![record.bytes, request.seq],
                )?;
                outbox.reissue(&tx, &record)?;
                restore.reissued_requests += 1;
            }
        }
        let state = model::project(&doc)?;
        write_projection(&tx, &state)?;
        // An integrated store never receives (`receive` is disabled), so it
        // has no inbox to refresh or replay.
        if !integrated {
            refresh_inbox(&tx, &doc)?;
        }
        tx.commit()?;
        let mut replica = Self {
            conn,
            doc,
            state,
            identities,
            jitter: CONCURRENT_JITTER_BYTES,
            restore,
            local_outbox,
        };
        if integrated {
            return Ok(replica);
        }
        let received: Vec<_> = replica
            .inbox()?
            .into_iter()
            .filter(|e| e.status == Admission::Received)
            .collect();
        for entry in received {
            if let Err(error) = replica.receive(&entry.record) {
                // Admission errors are durable quarantine entries. Storage failures
                // leave Received and propagate rather than being silently swallowed.
                if replica.admission(&entry.record)?.0 != Admission::Quarantined {
                    return Err(error);
                }
            }
        }
        Ok(replica)
    }
    /// Explicit fixture policy, separate from normal document initialization.
    pub fn set_test_peer(&mut self, peer: u64) -> Result<()> {
        if self.local_outbox.is_some() {
            return Err(invalid("integrated actor is owned by the outbox"));
        }
        if self.doc.oplog_vv().get(&peer).copied().unwrap_or(0) != 0 {
            return Err(invalid("test peer already used"));
        }
        self.doc.set_peer_id(peer)?;
        self.jitter = 0;
        configure(&self.doc, self.jitter);
        Ok(())
    }
    pub fn peer(&self) -> u64 {
        self.doc.peer_id()
    }
    pub fn ordering_jitter_bytes(&self) -> u8 {
        self.jitter
    }
    pub fn restore_stats(&self) -> RestoreStats {
        self.restore
    }
    /// A fork of the applied document only, without engine-pending updates.
    fn applied_stage(&self) -> Result<LoroDoc> {
        let stage = self.doc.fork();
        stage.set_peer_id(self.doc.peer_id())?;
        configure(&stage, self.jitter);
        Ok(stage)
    }
    pub fn execute(&mut self, request: &Request) -> Result<Record> {
        self.execute_with_fault(request, Fault::None)
    }
    pub fn execute_with_fault(&mut self, request: &Request, fault: Fault) -> Result<Record> {
        if uuid::Uuid::parse_str(&request.id)?.to_string() != request.id {
            return Err(invalid("noncanonical request UUID"));
        }
        let body = serde_json::to_string(&request.commands)?;
        // The retry lookup and the insert share one write transaction, so two
        // handles can never both save one request. A retained receipt is a
        // local read and needs no ownership; a new save does.
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let previous: Option<(Vec<u8>, Option<String>)> = tx
            .query_row(
                "SELECT bytes,request_body FROM journal WHERE id=?1 AND outgoing=1",
                [&request.id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        if let Some((bytes, old_body)) = previous {
            if old_body.as_deref() != Some(body.as_str()) {
                return Err(invalid("request ID reused with different intent"));
            }
            return Ok(Record {
                id: request.id.clone(),
                bytes,
            });
        }
        let stage = stage_of(&self.doc, self.jitter)?;
        let before = stage.oplog_vv();
        let mut identities = self.identities.clone();
        for command in &request.commands {
            model::apply(&stage, &mut identities, command.clone())?;
        }
        let state = model::project(&stage)?;
        let record = Record {
            id: request.id.clone(),
            bytes: stage.export(ExportMode::updates(&before))?,
        };
        if let Some(outbox) = &self.local_outbox {
            outbox.check_owner(&tx)?;
            // A no-op still retains the request receipt, but has no engine
            // operations to authenticate or deliver.
            if stage.oplog_vv() != before {
                outbox.enqueue(&tx, &record)?;
            }
        }
        tx.execute(
            "INSERT INTO journal(id,bytes,outgoing,request_body) VALUES (?1,?2,1,?3)",
            params![record.id, record.bytes, body],
        )?;
        write_projection(&tx, &state)?;
        if self.local_outbox.is_none() {
            refresh_inbox(&tx, &stage)?;
        }
        if fault == Fault::BeforeCommit {
            return Err(invalid("injected failure before commit"));
        }
        if fault == Fault::ExitBeforeCommit {
            std::process::exit(91);
        }
        tx.commit()?;
        if fault == Fault::ExitAfterCommit {
            std::process::exit(92);
        }
        self.doc = stage;
        self.state = state;
        self.identities = identities;
        if fault == Fault::AfterCommit {
            return Err(invalid("injected acknowledgement loss after commit"));
        }
        Ok(record)
    }
    /// Durable admission precedes import. Errors retain bytes and a reason;
    /// Pending outcomes and inbox entries make blocked causal streams observable.
    ///
    /// Each record is judged in its own causal past at the moment it becomes
    /// applicable, never against concurrently applied records, so verdicts do
    /// not depend on delivery order. A record waiting on dependencies cannot
    /// cause the dependency that releases it to be blamed for its content.
    pub fn receive(&mut self, record: &Record) -> Result<Admission> {
        if self.local_outbox.is_some() {
            return Err(invalid(
                "integrated sender requires authenticated receive integration",
            ));
        }
        let journaled: bool = self.conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM journal WHERE id=?1 AND bytes=?2)",
            params![record.id, record.bytes],
            |r| r.get(0),
        )?;
        if journaled {
            return status_for(&self.doc, &record.bytes);
        }
        self.conn.execute("INSERT OR IGNORE INTO inbox(id,bytes,status,reason) VALUES (?1,?2,'received','awaiting admission')",params![record.id,record.bytes])?;
        self.conn.execute(
            "UPDATE inbox SET id_conflict=1 WHERE id=?1 AND (
             EXISTS(SELECT 1 FROM inbox other WHERE other.id=inbox.id AND other.bytes<>inbox.bytes)
             OR EXISTS(SELECT 1 FROM journal own WHERE own.id=inbox.id AND own.bytes<>inbox.bytes))",
            [&record.id],
        )?;
        let (status, reason) = self.admission(record)?;
        if status == Admission::Quarantined {
            return Err(invalid(reason));
        }
        // This record plus every retained record still waiting on dependencies.
        let mut candidates = vec![record.clone()];
        {
            let mut query = self.conn.prepare(
                "SELECT inbox.id,inbox.bytes FROM inbox JOIN journal ON journal.id=inbox.id
                 AND journal.bytes=inbox.bytes WHERE inbox.status='pending' ORDER BY inbox.seq",
            )?;
            for row in query.query_map([], |r| {
                Ok(Record {
                    id: r.get(0)?,
                    bytes: r.get(1)?,
                })
            })? {
                let waiting = row?;
                if waiting != *record {
                    candidates.push(waiting);
                }
            }
        }
        let mut stage = self.applied_stage()?;
        let mut rejected: Vec<(Record, String)> = Vec::new();
        let mut decided = vec![false; candidates.len()];
        loop {
            let mut progress = false;
            for (i, candidate) in candidates.iter().enumerate() {
                if decided[i] {
                    continue;
                }
                let verdict = (|| -> Result<std::result::Result<Option<LoroDoc>, String>> {
                    let Ok(meta) = LoroDoc::decode_import_blob_meta(&candidate.bytes, true) else {
                        return Ok(Err("engine-format".into()));
                    };
                    let trial = stage.fork();
                    trial.set_peer_id(stage.peer_id())?;
                    configure(&trial, self.jitter);
                    if trial.import(&candidate.bytes).is_err() {
                        return Ok(Err("engine-import".into()));
                    }
                    if trial.export_pending_updates()?.is_some() {
                        return Ok(Ok(None));
                    }
                    Ok(
                        match crate::causal_past_verdict(
                            &trial,
                            &meta.partial_start_vv,
                            &meta.partial_end_vv,
                        )? {
                            Some(reason) => Err(reason.into()),
                            None => Ok(Some(trial)),
                        },
                    )
                })()?;
                match verdict {
                    Ok(None) => {}
                    Ok(Some(trial)) => {
                        stage = trial;
                        decided[i] = true;
                        progress = true;
                    }
                    Err(reason) => {
                        rejected.push((candidate.clone(), reason));
                        decided[i] = true;
                    }
                }
            }
            if !progress {
                break;
            }
        }
        // Records still waiting stay pending inside the engine, as before.
        for (i, candidate) in candidates.iter().enumerate() {
            if !decided[i] {
                stage.import(&candidate.bytes)?;
            }
        }
        let own_rejection = rejected
            .iter()
            .find(|(r, _)| r == record)
            .map(|(_, reason)| reason.clone());
        let identities = model::Index::build(&stage)?;
        let state = model::project(&stage)?;
        let tx = self.conn.transaction()?;
        if own_rejection.is_none() {
            tx.execute(
                "INSERT INTO journal(id,bytes,outgoing) VALUES (?1,?2,0)",
                params![record.id, record.bytes],
            )?;
        }
        for (bad, reason) in &rejected {
            tx.execute(
                "DELETE FROM journal WHERE id=?1 AND bytes=?2 AND outgoing=0",
                params![bad.id, bad.bytes],
            )?;
            tx.execute(
                "UPDATE inbox SET status='quarantined',reason=?3 WHERE id=?1 AND bytes=?2",
                params![bad.id, bad.bytes, reason],
            )?;
        }
        write_projection(&tx, &state)?;
        refresh_inbox(&tx, &stage)?;
        tx.commit()?;
        self.doc = stage;
        self.state = state;
        self.identities = identities;
        match own_rejection {
            Some(reason) => Err(invalid(reason)),
            None => Ok(self.admission(record)?.0),
        }
    }
    fn admission(&self, record: &Record) -> Result<(Admission, String)> {
        let (status, reason): (String, String) = self.conn.query_row(
            "SELECT status,reason FROM inbox WHERE id=?1 AND bytes=?2",
            params![record.id, record.bytes],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        Ok((Admission::parse(&status)?, reason))
    }
    pub fn inbox(&self) -> Result<Vec<InboxEntry>> {
        let mut query = self
            .conn
            .prepare("SELECT id,bytes,status,reason,id_conflict FROM inbox ORDER BY seq")?;
        let rows = query.query_map([], |r| {
            Ok((
                Record {
                    id: r.get(0)?,
                    bytes: r.get(1)?,
                },
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, bool>(4)?,
            ))
        })?;
        rows.map(|row| {
            let (record, status, reason, record_id_conflict) = row?;
            Ok(InboxEntry {
                record,
                status: Admission::parse(&status)?,
                reason,
                record_id_conflict,
            })
        })
        .collect()
    }
    pub fn projection(&self) -> Result<Projection> {
        Ok(self.state.clone())
    }
    pub fn rebuild(&mut self) -> Result<()> {
        let state = model::project(&self.doc)?;
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        if let Some(outbox) = &self.local_outbox {
            outbox.check_owner(&tx)?;
        }
        write_projection(&tx, &state)?;
        tx.commit()?;
        self.state = state;
        Ok(())
    }
    pub fn checkpoint(&mut self) -> Result<()> {
        let snapshot = self.doc.export(ExportMode::Snapshot)?;
        let pending = self.doc.export_pending_updates()?;
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        if let Some(outbox) = &self.local_outbox {
            outbox.check_owner(&tx)?;
        }
        let seq: i64 =
            tx.query_row("SELECT coalesce(max(seq),0) FROM journal", [], |r| r.get(0))?;
        tx.execute("INSERT INTO checkpoint(singleton,bytes,through_seq,pending) VALUES (1,?1,?2,?3) ON CONFLICT(singleton) DO UPDATE SET bytes=excluded.bytes,through_seq=excluded.through_seq,pending=excluded.pending",params![snapshot,seq,pending])?;
        tx.commit()?;
        Ok(())
    }
    pub fn outbox(&self) -> Result<Vec<Record>> {
        let mut query = self
            .conn
            .prepare("SELECT id,bytes FROM journal WHERE outgoing=1 ORDER BY seq")?;
        let records = query
            .query_map([], |r| {
                Ok(Record {
                    id: r.get(0)?,
                    bytes: r.get(1)?,
                })
            })?
            .collect::<rusqlite::Result<_>>()?;
        Ok(records)
    }
    pub fn journal_len(&self) -> Result<usize> {
        let count: i64 = self
            .conn
            .query_row("SELECT count(*) FROM journal", [], |r| r.get(0))?;
        Ok(count.try_into()?)
    }
    pub fn frontier(&self) -> loro::Frontiers {
        self.doc.oplog_frontiers()
    }
}
fn status_for(doc: &LoroDoc, bytes: &[u8]) -> Result<Admission> {
    let end = LoroDoc::decode_import_blob_meta(bytes, true)?.partial_end_vv;
    let current = doc.oplog_vv();
    Ok(
        if end
            .iter()
            .all(|(peer, counter)| current.get(peer).copied().unwrap_or(0) >= *counter)
        {
            Admission::Applied
        } else {
            Admission::Pending
        },
    )
}
fn refresh_inbox(tx: &Transaction<'_>, doc: &LoroDoc) -> Result<()> {
    let mut query=tx.prepare("SELECT inbox.seq,inbox.bytes FROM inbox JOIN journal ON journal.id=inbox.id AND journal.bytes=inbox.bytes WHERE inbox.status IN ('received','pending')")?;
    let rows = query
        .query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, Vec<u8>>(1)?)))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for (seq, bytes) in rows {
        let (state, reason) = if status_for(doc, &bytes)? == Admission::Applied {
            ("applied", "")
        } else {
            (
                "pending",
                "waiting for causal dependencies; inspect pending and quarantined inbox records",
            )
        };
        tx.execute(
            "UPDATE inbox SET status=?2,reason=?3 WHERE seq=?1",
            params![seq, state, reason],
        )?;
    }
    Ok(())
}
fn write_projection(tx: &Transaction<'_>, projection: &Projection) -> Result<()> {
    tx.execute("DELETE FROM projection", [])?;
    tx.execute(
        "INSERT INTO projection VALUES ('state',?1)",
        [serde_json::to_string(projection)?],
    )?;
    Ok(())
}
struct Stranded {
    seq: i64,
    id: String,
    body: String,
}
/// The journal rows of stranded requests, in journal order.
fn stranded_requests(tx: &Transaction<'_>, ids: &[String]) -> Result<Vec<Stranded>> {
    let mut out = Vec::with_capacity(ids.len());
    for id in ids {
        let (seq, body): (i64, Option<String>) = tx
            .query_row(
                "SELECT seq,request_body FROM journal WHERE id=?1 AND outgoing=1",
                [id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?
            .ok_or_else(|| invalid("stranded intent has no saved request"))?;
        let body = body.ok_or_else(|| invalid("stranded request has no saved body"))?;
        out.push(Stranded {
            seq,
            id: id.clone(),
            body,
        });
    }
    out.sort_by_key(|s| s.seq);
    Ok(out)
}
/// A fork of `doc` to stage a request in, configured like the replica.
fn stage_of(doc: &LoroDoc, jitter: u8) -> Result<LoroDoc> {
    let stage = doc.fork();
    stage.set_peer_id(doc.peer_id())?;
    if let Some(pending) = doc.export_pending_updates()? {
        stage.import(&pending)?;
    }
    configure(&stage, jitter);
    Ok(stage)
}
fn initialize(
    conn: &mut Connection,
    persistent: bool,
    setup: Option<impl FnOnce(&Transaction<'_>, bool) -> Result<Box<dyn LocalOutbox>>>,
) -> Result<Option<Box<dyn LocalOutbox>>> {
    let integrated = setup.is_some();
    validate_format(conn, integrated)?;
    if integrated {
        // Seals and acknowledgements share the file and are short.
        conn.busy_timeout(INTEGRATED_BUSY_TIMEOUT)?;
    }
    let (pragma, expected_mode) = if persistent {
        ("PRAGMA journal_mode=WAL", "wal")
    } else {
        ("PRAGMA journal_mode", "memory")
    };
    let mode: String = conn.query_row(pragma, [], |row| row.get(0))?;
    if mode != expected_mode {
        return Err(invalid(format!(
            "replica requires {expected_mode} journal mode; SQLite selected {mode}"
        )));
    }
    conn.execute_batch("PRAGMA synchronous=FULL;")?;
    conn.execute_batch("PRAGMA foreign_keys=ON;")?;
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let exists = validate_format(&tx, integrated)?;
    tx.execute_batch("CREATE TABLE IF NOT EXISTS prototype_meta(key TEXT PRIMARY KEY,value TEXT NOT NULL);
        CREATE TABLE IF NOT EXISTS journal(seq INTEGER PRIMARY KEY,id TEXT NOT NULL,bytes BLOB NOT NULL,outgoing INTEGER NOT NULL CHECK(outgoing IN (0,1)),UNIQUE(id,bytes));
        CREATE TABLE IF NOT EXISTS projection(id TEXT PRIMARY KEY,value TEXT NOT NULL);
        CREATE TABLE IF NOT EXISTS checkpoint(singleton INTEGER PRIMARY KEY CHECK(singleton=1),bytes BLOB NOT NULL);
        CREATE TABLE IF NOT EXISTS inbox(seq INTEGER PRIMARY KEY,id TEXT NOT NULL,bytes BLOB NOT NULL,status TEXT NOT NULL CHECK(status IN ('received','pending','applied','quarantined')),reason TEXT NOT NULL,UNIQUE(id,bytes));")?;
    for (table, column, ddl) in [
        (
            "journal",
            "request_body",
            "ALTER TABLE journal ADD COLUMN request_body TEXT",
        ),
        (
            "checkpoint",
            "through_seq",
            "ALTER TABLE checkpoint ADD COLUMN through_seq INTEGER NOT NULL DEFAULT 0",
        ),
        (
            "checkpoint",
            "pending",
            "ALTER TABLE checkpoint ADD COLUMN pending BLOB",
        ),
        (
            "inbox",
            "id_conflict",
            "ALTER TABLE inbox ADD COLUMN id_conflict INTEGER NOT NULL DEFAULT 0",
        ),
    ] {
        let mut query = tx.prepare(&format!("PRAGMA table_info({table})"))?;
        let columns = query
            .query_map([], |r| r.get::<_, String>(1))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        if !columns.iter().any(|c| c == column) {
            tx.execute_batch(ddl)?;
        }
    }
    // Format 2: transport IDs are unique only together with their bytes.
    let journal: String = tx.query_row(
        "SELECT sql FROM sqlite_schema WHERE type='table' AND name='journal'",
        [],
        |r| r.get(0),
    )?;
    if !journal.contains("UNIQUE(id,bytes)") {
        tx.execute_batch("CREATE TABLE journal_v2(seq INTEGER PRIMARY KEY,id TEXT NOT NULL,bytes BLOB NOT NULL,outgoing INTEGER NOT NULL CHECK(outgoing IN (0,1)),request_body TEXT,UNIQUE(id,bytes));
            INSERT INTO journal_v2(seq,id,bytes,outgoing,request_body) SELECT seq,id,bytes,outgoing,request_body FROM journal;
            DROP TABLE journal;
            ALTER TABLE journal_v2 RENAME TO journal;")?;
    }
    tx.execute_batch("INSERT OR IGNORE INTO inbox(id,bytes,status,reason) SELECT id,bytes,'received','restoring admission status' FROM journal WHERE outgoing=0;")?;
    let outbox = setup.map(|setup| setup(&tx, exists)).transpose()?;
    tx.execute("INSERT INTO prototype_meta VALUES ('format',?1) ON CONFLICT(key) DO UPDATE SET value=excluded.value",
        [if integrated { ATOMIC_FORMAT } else { "matters-sync-prototype/2" }])?;
    tx.commit()?;
    Ok(outbox)
}
fn validate_format(conn: &Connection, integrated: bool) -> Result<bool> {
    if !crate::has_user_schema(conn)? {
        return Ok(false);
    }
    let marker = conn.query_row(
        "SELECT value FROM prototype_meta WHERE key='format'",
        [],
        |r| r.get::<_, String>(0),
    );
    let valid = if integrated {
        matches!(marker.as_deref(), Ok(ATOMIC_FORMAT))
    } else {
        matches!(
            marker.as_deref(),
            Ok("matters-sync-prototype/0"
                | "matters-sync-prototype/1"
                | "matters-sync-prototype/2")
        )
    };
    if !valid {
        return Err(invalid("refusing incompatible prototype database"));
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn receive_projects_within_budget_and_persists_that_result() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let mut sender = Replica::open(dir.path().join("sender.db"))?;
        let record = sender.execute(&Request::new(Command::Create {
            id: uuid::Uuid::from_u128(1).to_string(),
            kind: crate::Kind::Task,
            parent: None,
            title: "task".into(),
        }))?;
        let mut receiver = Replica::open(dir.path().join("receiver.db"))?;
        model::PROJECT_CALLS.with(|n| n.set(0));
        receiver.receive(&record)?;
        // Two projections judge the record in its causal past (with and
        // without it); one projects the merged state that is persisted.
        model::PROJECT_CALLS.with(|n| assert_eq!(n.get(), 3));
        let stored: String =
            receiver
                .conn
                .query_row("SELECT value FROM projection WHERE id='state'", [], |r| {
                    r.get(0)
                })?;
        assert_eq!(serde_json::from_str::<Projection>(&stored)?, receiver.state);
        Ok(())
    }
}
