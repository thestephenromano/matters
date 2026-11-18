use crate::{invalid, model, Command, Projection, Result};
use loro::{ExportMode, LoroDoc};
use rusqlite::{params, Connection, OptionalExtension, Transaction};
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
}
#[derive(Debug, Clone, Copy, Default)]
pub struct RestoreStats {
    pub snapshot_loaded: bool,
    pub replayed_records: usize,
}

pub struct Replica {
    conn: Connection,
    doc: LoroDoc,
    state: Projection,
    identities: model::Index,
    jitter: u8,
    restore: RestoreStats,
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
        let mut conn = Connection::open(path)?;
        initialize(&mut conn)?;
        let doc = LoroDoc::new();
        let mut restore = RestoreStats::default();
        let checkpoint: Option<(Vec<u8>, i64, Option<Vec<u8>>)> = conn
            .query_row(
                "SELECT bytes,through_seq,pending FROM checkpoint WHERE singleton=1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()?;
        let through_seq = if let Some((bytes, seq, pending)) = checkpoint {
            doc.import(&bytes)?;
            if let Some(pending) = pending {
                doc.import(&pending)?;
            }
            restore.snapshot_loaded = true;
            seq
        } else {
            0
        };
        {
            let mut query = conn.prepare("SELECT bytes FROM journal WHERE seq>?1 ORDER BY seq")?;
            for bytes in query.query_map([through_seq], |r| r.get::<_, Vec<u8>>(0))? {
                doc.import(&bytes?)?;
                restore.replayed_records += 1;
            }
        }
        configure(&doc, CONCURRENT_JITTER_BYTES);
        let identities = model::Index::build(&doc)?;
        let state = model::project(&doc)?;
        let tx = conn.transaction()?;
        write_projection(&tx, &state)?;
        refresh_inbox(&tx, &doc)?;
        tx.commit()?;
        let mut replica = Self {
            conn,
            doc,
            state,
            identities,
            jitter: CONCURRENT_JITTER_BYTES,
            restore,
        };
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
    fn stage(&self) -> Result<LoroDoc> {
        let stage = self.doc.fork();
        stage.set_peer_id(self.doc.peer_id())?;
        if let Some(pending) = self.doc.export_pending_updates()? {
            stage.import(&pending)?;
        }
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
        let previous: Option<(Vec<u8>, Option<String>)> = self
            .conn
            .query_row(
                "SELECT bytes,request_body FROM journal WHERE id=?1",
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
        let stage = self.stage()?;
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
        let tx = self.conn.transaction()?;
        tx.execute(
            "INSERT INTO journal(id,bytes,outgoing,request_body) VALUES (?1,?2,1,?3)",
            params![record.id, record.bytes, body],
        )?;
        write_projection(&tx, &state)?;
        refresh_inbox(&tx, &stage)?;
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
    pub fn receive(&mut self, record: &Record) -> Result<Admission> {
        let existing: Option<Vec<u8>> = self
            .conn
            .query_row("SELECT bytes FROM journal WHERE id=?1", [&record.id], |r| {
                r.get(0)
            })
            .optional()?;
        if existing.as_ref() == Some(&record.bytes) {
            return status_for(&self.doc, &record.bytes);
        }
        self.conn.execute("INSERT OR IGNORE INTO inbox(id,bytes,status,reason) VALUES (?1,?2,'received','awaiting admission')",params![record.id,record.bytes])?;
        if existing.is_some() {
            return self.quarantine(record, invalid("record ID reused with different bytes"));
        }
        let collision: bool = self.conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM inbox WHERE id=?1 AND bytes<>?2)",
            params![record.id, record.bytes],
            |r| r.get(0),
        )?;
        if collision {
            return self.quarantine(
                record,
                invalid("received record ID reused with different bytes"),
            );
        }
        let (status, reason) = self.admission(record)?;
        if status == Admission::Quarantined {
            return Err(invalid(reason));
        }
        let candidate = (|| -> Result<_> {
            let stage = self.stage()?;
            stage.import(&record.bytes)?;
            let identities = model::Index::build(&stage)?;
            let state = model::project(&stage)?;
            if !self.state.tombstones.is_subset(&state.tombstones) {
                return Err(invalid("attempt to remove deletion evidence"));
            }
            Ok((stage, state, identities))
        })();
        let (stage, state, identities) = match candidate {
            Ok(c) => c,
            Err(error) => return self.quarantine(record, error),
        };
        let tx = self.conn.transaction()?;
        tx.execute(
            "INSERT INTO journal(id,bytes,outgoing) VALUES (?1,?2,0)",
            params![record.id, record.bytes],
        )?;
        write_projection(&tx, &state)?;
        refresh_inbox(&tx, &stage)?;
        tx.commit()?;
        self.doc = stage;
        self.state = state;
        self.identities = identities;
        Ok(self.admission(record)?.0)
    }
    fn quarantine(
        &self,
        record: &Record,
        error: Box<dyn std::error::Error + Send + Sync>,
    ) -> Result<Admission> {
        self.conn.execute(
            "UPDATE inbox SET status='quarantined',reason=?3 WHERE id=?1 AND bytes=?2",
            params![record.id, record.bytes, error.to_string()],
        )?;
        Err(error)
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
            .prepare("SELECT id,bytes,status,reason FROM inbox ORDER BY seq")?;
        let rows = query.query_map([], |r| {
            Ok((
                Record {
                    id: r.get(0)?,
                    bytes: r.get(1)?,
                },
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
            ))
        })?;
        rows.map(|row| {
            let (record, status, reason) = row?;
            Ok(InboxEntry {
                record,
                status: Admission::parse(&status)?,
                reason,
            })
        })
        .collect()
    }
    pub fn projection(&self) -> Result<Projection> {
        Ok(self.state.clone())
    }
    pub fn rebuild(&mut self) -> Result<()> {
        let state = model::project(&self.doc)?;
        let tx = self.conn.transaction()?;
        write_projection(&tx, &state)?;
        tx.commit()?;
        self.state = state;
        Ok(())
    }
    pub fn checkpoint(&mut self) -> Result<()> {
        let snapshot = self.doc.export(ExportMode::Snapshot)?;
        let pending = self.doc.export_pending_updates()?;
        let tx = self.conn.transaction()?;
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
        Ok(self
            .conn
            .query_row("SELECT count(*) FROM journal", [], |r| r.get(0))?)
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
fn initialize(conn: &mut Connection) -> Result<()> {
    let tables: usize = conn.query_row(
        "SELECT count(*) FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%'",
        [],
        |r| r.get(0),
    )?;
    if tables > 0 {
        let marker = conn.query_row(
            "SELECT value FROM prototype_meta WHERE key='format'",
            [],
            |r| r.get::<_, String>(0),
        );
        if !matches!(
            marker.as_deref(),
            Ok("matters-sync-prototype/0" | "matters-sync-prototype/1")
        ) {
            return Err(invalid("refusing to open a non-prototype database"));
        }
    }
    conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL;")?;
    let tx = conn.transaction()?;
    tx.execute_batch("CREATE TABLE IF NOT EXISTS prototype_meta(key TEXT PRIMARY KEY,value TEXT NOT NULL);
        CREATE TABLE IF NOT EXISTS journal(seq INTEGER PRIMARY KEY,id TEXT NOT NULL UNIQUE,bytes BLOB NOT NULL,outgoing INTEGER NOT NULL CHECK(outgoing IN (0,1)));
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
    ] {
        let mut query = tx.prepare(&format!("PRAGMA table_info({table})"))?;
        let columns = query
            .query_map([], |r| r.get::<_, String>(1))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        if !columns.iter().any(|c| c == column) {
            tx.execute_batch(ddl)?;
        }
    }
    tx.execute_batch("INSERT OR IGNORE INTO inbox(id,bytes,status,reason) SELECT id,bytes,'received','restoring admission status' FROM journal WHERE outgoing=0;
        INSERT INTO prototype_meta VALUES ('format','matters-sync-prototype/1') ON CONFLICT(key) DO UPDATE SET value=excluded.value;")?;
    tx.commit()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn receive_projects_once_and_persists_that_result() -> Result<()> {
        let mut sender = Replica::open(":memory:")?;
        let record = sender.execute(&Request::new(Command::Create {
            id: uuid::Uuid::from_u128(1).to_string(),
            kind: crate::Kind::Task,
            parent: None,
            title: "task".into(),
        }))?;
        let mut receiver = Replica::open(":memory:")?;
        model::PROJECT_CALLS.with(|n| n.set(0));
        receiver.receive(&record)?;
        model::PROJECT_CALLS.with(|n| assert_eq!(n.get(), 1));
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
