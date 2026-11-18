use std::path::Path;

use loro::{ExportMode, LoroDoc};
use rusqlite::{params, Connection, OptionalExtension, Transaction};
use serde::{Deserialize, Serialize};

use crate::{invalid, model, Command, Projection, Result};

/// Local test transport only: unauthenticated and unencrypted. Never send to a service.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Record {
    pub id: String,
    pub bytes: Vec<u8>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Fault {
    #[default]
    None,
    BeforeCommit,
    AfterCommit,
    /// Test subprocess exits without destructors while a transaction is open.
    ExitBeforeCommit,
    /// Test subprocess exits after durable commit but before an acknowledgement.
    ExitAfterCommit,
}

/// A correctness-first adapter. Staging a fork is O(history), not a latency optimization.
pub struct Replica {
    conn: Connection,
    doc: LoroDoc,
}

impl Replica {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let conn = Connection::open(path)?;
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
            if marker.as_deref() != Ok("matters-sync-prototype/0") {
                return Err(invalid("refusing to open a non-prototype database"));
            }
        }
        conn.execute_batch(
            "PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL;
             CREATE TABLE IF NOT EXISTS prototype_meta(key TEXT PRIMARY KEY, value TEXT NOT NULL);
             INSERT OR IGNORE INTO prototype_meta VALUES ('format','matters-sync-prototype/0');
             CREATE TABLE IF NOT EXISTS journal (
               seq INTEGER PRIMARY KEY, id TEXT NOT NULL UNIQUE, bytes BLOB NOT NULL,
               outgoing INTEGER NOT NULL CHECK(outgoing IN (0,1)));
             CREATE TABLE IF NOT EXISTS projection (id TEXT PRIMARY KEY, value TEXT NOT NULL);
             CREATE TABLE IF NOT EXISTS checkpoint (singleton INTEGER PRIMARY KEY CHECK(singleton=1), bytes BLOB NOT NULL);",
        )?;
        let doc = LoroDoc::new();
        // Always replay the raw journal, including imports absent from a normal snapshot.
        // A new document also gives every writable open a fresh random engine actor.
        {
            let mut query = conn.prepare("SELECT bytes FROM journal ORDER BY seq")?;
            for bytes in query.query_map([], |row| row.get::<_, Vec<u8>>(0))? {
                doc.import(&bytes?)?;
            }
        }
        let mut replica = Self { conn, doc };
        replica.rebuild()?;
        Ok(replica)
    }

    /// Fixed peer IDs are only for reproducible single-process semantic fixtures.
    pub fn set_test_peer(&self, peer: u64) -> Result<()> {
        if self.doc.oplog_vv().get(&peer).copied().unwrap_or(0) != 0 {
            return Err(invalid("test peer already used"));
        }
        self.doc.set_peer_id(peer)?;
        Ok(())
    }

    pub fn peer(&self) -> u64 {
        self.doc.peer_id()
    }

    fn stage(&self) -> Result<LoroDoc> {
        let stage = self.doc.fork();
        stage.set_peer_id(self.doc.peer_id())?;
        if let Some(pending) = self.doc.export_pending_updates()? {
            stage.import(&pending)?;
        }
        Ok(stage)
    }

    pub fn execute(&mut self, command: Command) -> Result<Record> {
        self.execute_with_fault(command, Fault::None)
    }

    pub fn execute_with_fault(&mut self, command: Command, fault: Fault) -> Result<Record> {
        self.execute_batch_with_fault([command], fault)
    }

    /// One atomic seed/import batch; ordinary user commands use `execute`.
    pub fn execute_batch(&mut self, commands: impl IntoIterator<Item = Command>) -> Result<Record> {
        self.execute_batch_with_fault(commands, Fault::None)
    }

    fn execute_batch_with_fault(
        &mut self,
        commands: impl IntoIterator<Item = Command>,
        fault: Fault,
    ) -> Result<Record> {
        let stage = self.stage()?;
        let before = stage.oplog_vv();
        for command in commands {
            model::apply(&stage, command)?;
        }
        let record = Record {
            id: uuid::Uuid::new_v4().to_string(),
            bytes: stage.export(ExportMode::updates(&before))?,
        };
        self.persist(stage, &record, true, fault)?;
        Ok(record)
    }

    pub fn receive(&mut self, record: &Record) -> Result<()> {
        if let Some(existing) = self
            .conn
            .query_row("SELECT bytes FROM journal WHERE id=?1", [&record.id], |r| {
                r.get::<_, Vec<u8>>(0)
            })
            .optional()?
        {
            return if existing == record.bytes {
                Ok(())
            } else {
                Err(invalid("record ID reused with different bytes"))
            };
        }
        let stage = self.stage()?;
        stage.import(&record.bytes)?;
        let old = model::project(&self.doc)?;
        let new = model::project(&stage)?;
        if !old.tombstones.is_subset(&new.tombstones) {
            return Err(invalid("attempt to remove deletion evidence"));
        }
        self.persist(stage, record, false, Fault::None)
    }

    fn persist(
        &mut self,
        stage: LoroDoc,
        record: &Record,
        outgoing: bool,
        fault: Fault,
    ) -> Result<()> {
        let projection = model::project(&stage)?;
        let tx = self.conn.transaction()?;
        tx.execute(
            "INSERT INTO journal(id,bytes,outgoing) VALUES (?1,?2,?3)",
            params![record.id, record.bytes, outgoing],
        )?;
        write_projection(&tx, &projection)?;
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
        // Replace speculative state only after durable commit. An uncertain ACK can be retried.
        self.doc = stage;
        if fault == Fault::AfterCommit {
            return Err(invalid("injected acknowledgement loss after commit"));
        }
        Ok(())
    }

    pub fn projection(&self) -> Result<Projection> {
        let text: String =
            self.conn
                .query_row("SELECT value FROM projection WHERE id='state'", [], |r| {
                    r.get(0)
                })?;
        Ok(serde_json::from_str(&text)?)
    }

    pub fn rebuild(&mut self) -> Result<()> {
        let projection = model::project(&self.doc)?;
        let tx = self.conn.transaction()?;
        write_projection(&tx, &projection)?;
        tx.commit()?;
        Ok(())
    }

    pub fn checkpoint(&self) -> Result<()> {
        let snapshot = self.doc.export(ExportMode::Snapshot)?;
        self.conn.execute("INSERT INTO checkpoint VALUES (1,?1) ON CONFLICT(singleton) DO UPDATE SET bytes=excluded.bytes",[snapshot])?;
        Ok(())
    }

    pub fn outbox(&self) -> Result<Vec<Record>> {
        let mut query = self
            .conn
            .prepare("SELECT id,bytes FROM journal WHERE outgoing=1 ORDER BY seq")?;
        let records = query.query_map([], |r| {
            Ok(Record {
                id: r.get(0)?,
                bytes: r.get(1)?,
            })
        })?;
        Ok(records.collect::<rusqlite::Result<_>>()?)
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

fn write_projection(tx: &Transaction<'_>, projection: &Projection) -> Result<()> {
    tx.execute("DELETE FROM projection", [])?;
    tx.execute(
        "INSERT INTO projection VALUES ('state',?1)",
        [serde_json::to_string(projection)?],
    )?;
    Ok(())
}
