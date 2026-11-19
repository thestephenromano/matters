//! Test-only transactional outbox. The synchronous FnOnce sealer is trusted and
//! receives a durably reserved header exactly once. No keys or network code here.
use crate::record_chain::{hash, Hash, Id};
use crate::support::{parse, Body, Message, Route};
use matters_sync_prototype::{wire, Result};
use rusqlite::{params, Connection, OptionalExtension, Transaction, TransactionBehavior};
use serde::{Deserialize, Serialize};
use std::path::Path;

const FORMAT: &str = "matters-encrypted-outbox/2";
// Small retained-history experiment: at most 16 MiB each of intent plaintext and
// ciphertext payload. Writer and reservation history is retained without a
// lifetime cap: transient contention/restarts must not exhaust recovery access.
const MAX_INTENTS: i64 = 16;

use matters_sync_prototype::invalid as error;
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Binding {
    pub application: [u8; 16],
    pub tenant: [u8; 16],
    pub collection: [u8; 16],
    pub document: [u8; 16],
    pub epoch: u64,
    pub manifest: Vec<u8>,
    pub author: Vec<u8>,
}
impl Binding {
    pub fn context(&self) -> wire::Context<'_> {
        wire::Context {
            application: &self.application,
            tenant: &self.tenant,
            collection: &self.collection,
            document: &self.document,
        }
    }
    fn validate(&self) -> Result<()> {
        if self.manifest.len() != 48 || self.author.len() != 48 {
            return Err(error("invalid authority binding"));
        }
        Ok(())
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Writer {
    pub incarnation: Id,
    pub actor: u64,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Stored {
    pub id: Id,
    pub bytes: Vec<u8>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Point {
    BeforeIntentCommit,
    AfterIntentCommit,
    BeforeReservationCommit,
    AfterReservationCommit,
    AfterSeal,
    BeforeOutboxCommit,
    AfterOutboxCommit,
    BeforeAckCommit,
    AfterAckCommit,
    BeforeWriterCommit,
    AfterWriterCommit,
}
// Hooks are explicit fault injection for this test-only component. They never
// receive payloads/keys; callbacks can return an error or terminate a subprocess.
pub struct Outbox {
    conn: Connection,
    binding: Binding,
    writer: Writer,
}
fn validate_database(conn: &Connection) -> Result<bool> {
    if !matters_sync_prototype::has_user_schema(conn)? {
        return Ok(false);
    }
    let marker = conn.query_row(
        "SELECT format FROM outbox_meta WHERE singleton=1",
        [],
        |r| r.get::<_, String>(0),
    );
    if !matches!(marker.as_deref(), Ok(FORMAT)) {
        return Err(error("refusing non-outbox database"));
    }
    Ok(true)
}
fn active(conn: &Connection, writer: Writer) -> Result<()> {
    let owner: Vec<u8> =
        conn.query_row("SELECT owner FROM outbox_meta WHERE singleton=1", [], |r| {
            r.get(0)
        })?;
    if owner != writer.incarnation {
        return Err(error("writer fenced by a newer open"));
    }
    Ok(())
}
fn count_limit(conn: &Connection, query: &str, limit: i64) -> Result<()> {
    let count: i64 = conn.query_row(query, [], |r| r.get(0))?;
    if count >= limit {
        return Err(error("experiment history capacity exhausted"));
    }
    Ok(())
}
fn blob<const N: usize>(row: &rusqlite::Row<'_>, index: usize) -> rusqlite::Result<[u8; N]> {
    let bytes: Vec<u8> = row.get(index)?;
    bytes.try_into().map_err(|_| {
        rusqlite::Error::FromSqlConversionFailure(
            index,
            rusqlite::types::Type::Blob,
            error("invalid stored blob length"),
        )
    })
}
fn stored_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Stored> {
    Ok(Stored {
        id: blob(row, 0)?,
        bytes: row.get(1)?,
    })
}
fn stored(conn: &Connection, batch: &Id) -> Result<Option<Stored>> {
    Ok(conn.query_row("SELECT record_id,bytes FROM sealed JOIN intents ON intents.seq=sealed.intent WHERE batch=?1", [batch.as_slice()], stored_row).optional()?)
}
fn pending(conn: &Connection) -> Result<Vec<Stored>> {
    let mut query =
        conn.prepare("SELECT record_id,bytes FROM sealed WHERE acked=0 ORDER BY intent")?;
    let rows = query.query_map([], stored_row)?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}
fn acknowledge(
    conn: &mut Connection,
    id: Id,
    mut hook: impl FnMut(Point) -> Result<()>,
) -> Result<()> {
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    if tx.execute(
        "UPDATE sealed SET acked=1 WHERE record_id=?1",
        [id.as_slice()],
    )? != 1
    {
        return Err(error("unknown acknowledged record"));
    }
    hook(Point::BeforeAckCommit)?;
    tx.commit()?;
    hook(Point::AfterAckCommit)
}
fn check_binding(conn: &Connection, binding: &Binding) -> Result<()> {
    binding.validate()?;
    if !validate_database(conn)? {
        return Err(error("outbox database missing"));
    }
    let old: Vec<u8> = conn.query_row(
        "SELECT binding FROM outbox_meta WHERE singleton=1",
        [],
        |r| r.get(0),
    )?;
    if old != serde_json::to_vec(binding)? {
        return Err(error("outbox route/authority binding mismatch"));
    }
    Ok(())
}
/// Delivery never allocates or retires a writer, requests entropy, or seals.
/// Read-only handles additionally refuse ACK writes at SQLite's open boundary.
pub struct Delivery {
    conn: Connection,
}
impl Delivery {
    pub fn open(path: impl AsRef<Path>, binding: &Binding, read_only: bool) -> Result<Self> {
        let flags = if read_only {
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY
        } else {
            rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE
        };
        let conn = Connection::open_with_flags(path, flags)?;
        check_binding(&conn, binding)?;
        conn.execute_batch("PRAGMA synchronous=FULL; PRAGMA foreign_keys=ON;")?;
        conn.busy_timeout(std::time::Duration::ZERO)?;
        Ok(Self { conn })
    }
    /// Exact signed roots/handoffs, retained in creation order for replay.
    pub fn announcements(&self) -> Result<Vec<Vec<u8>>> {
        let mut stmt = self.conn.prepare(
            "SELECT announcement FROM writers WHERE announcement IS NOT NULL ORDER BY seq",
        )?;
        let rows = stmt.query_map([], |r| r.get(0))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }
    pub fn record(&self, batch: Id) -> Result<Option<Stored>> {
        stored(&self.conn, &batch)
    }
    pub fn pending(&self) -> Result<Vec<Stored>> {
        pending(&self.conn)
    }
    pub fn acknowledge(&mut self, id: Id, hook: impl FnMut(Point) -> Result<()>) -> Result<()> {
        acknowledge(&mut self.conn, id, hook)
    }
}
#[derive(Debug, PartialEq, Eq)]
pub struct RecoveryIntent {
    pub batch: Id,
    pub writer: Writer,
    pub plaintext: Vec<u8>,
}
/// An old writer's local head may have rolled back. Retain its intent for
/// domain-level reconciliation, but never invoke crypto for it after reopen.
#[derive(Debug)]
pub struct RecoveryRequired;
impl std::fmt::Display for RecoveryRequired {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("retired writer intent requires recovery")
    }
}
impl std::error::Error for RecoveryRequired {}
impl Outbox {
    /// Every writable open fences older handles and allocates a fresh local
    /// writer. RNG comes from the approved OS-seeded provider in the harness.
    pub fn open(
        path: impl AsRef<Path>,
        binding: Binding,
        random: impl FnMut() -> Result<Id>,
    ) -> Result<Self> {
        Self::open_inner(
            path,
            binding,
            random,
            None::<fn(&Message) -> Result<Vec<u8>>>,
            |_| Ok(()),
        )
    }
    /// Atomically retain a signed admission with the fresh writer before any
    /// content can be sealed. The signing callback is trusted and must not send
    /// or publish bytes; delivery reads only committed announcements.
    pub fn open_announced(
        path: impl AsRef<Path>,
        binding: Binding,
        random: impl FnMut() -> Result<Id>,
        sign: impl FnOnce(&Message) -> Result<Vec<u8>>,
        hook: impl FnMut(Point) -> Result<()>,
    ) -> Result<Self> {
        Self::open_inner(path, binding, random, Some(sign), hook)
    }
    fn open_inner(
        path: impl AsRef<Path>,
        binding: Binding,
        mut random: impl FnMut() -> Result<Id>,
        sign: Option<impl FnOnce(&Message) -> Result<Vec<u8>>>,
        mut hook: impl FnMut(Point) -> Result<()>,
    ) -> Result<Self> {
        binding.validate()?;
        let mut conn = Connection::open(path)?;
        validate_database(&conn)?;
        let integrated: bool = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE name='prototype_meta')",
            [],
            |r| r.get(0),
        )?;
        if integrated {
            return Err(error("integrated store requires atomic sender open"));
        }
        // Keep this bounded experiment on rollback journaling: its contention
        // fixtures and durability contract are defined for DELETE/FULL.
        let mode: String = conn.query_row("PRAGMA journal_mode=DELETE", [], |r| r.get(0))?;
        if mode != "delete" {
            return Err(error("persistent rollback journal required"));
        }
        conn.execute_batch("PRAGMA synchronous=FULL; PRAGMA foreign_keys=ON;")?;
        conn.busy_timeout(std::time::Duration::ZERO)?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let exists = validate_database(&tx)?;
        let writer = Self::start_writer(&tx, &binding, &mut random, sign, exists)?;
        hook(Point::BeforeWriterCommit)?;
        tx.commit()?;
        hook(Point::AfterWriterCommit)?;
        Ok(Self {
            conn,
            binding,
            writer,
        })
    }
    /// Reuse the exact standalone writer initialization inside the domain
    /// schema transaction. The caller owns the single commit.
    pub(super) fn start_writer(
        tx: &Transaction<'_>,
        binding: &Binding,
        mut random: impl FnMut() -> Result<Id>,
        sign: Option<impl FnOnce(&Message) -> Result<Vec<u8>>>,
        exists: bool,
    ) -> Result<Writer> {
        binding.validate()?;
        let encoded = serde_json::to_vec(&binding)?;
        if exists {
            check_binding(tx, binding)?;
        }
        let announced = sign.is_some();
        let previous = if exists {
            let old_mode: bool = tx.query_row(
                "SELECT announced FROM outbox_meta WHERE singleton=1",
                [],
                |r| r.get(0),
            )?;
            if old_mode != announced {
                return Err(error("cannot change outbox admission mode"));
            }
            Some(tx.query_row("SELECT owner,head FROM outbox_meta JOIN writers ON owner=incarnation WHERE singleton=1", [], |r| {
                let head: Option<Vec<u8>> = r.get(1)?;
                Ok((blob::<32>(r,0)?, head.map(|v| v.try_into().map_err(|_| rusqlite::Error::InvalidQuery)).transpose()?))
            })?)
        } else {
            None
        };
        let incarnation = random()?;
        let actor = u64::from_le_bytes(random()?[..8].try_into()?);
        tx.execute_batch("CREATE TABLE IF NOT EXISTS writers(seq INTEGER NOT NULL UNIQUE CHECK(seq>0),incarnation BLOB PRIMARY KEY CHECK(length(incarnation)=32),actor BLOB NOT NULL UNIQUE CHECK(length(actor)=8),head BLOB CHECK(head IS NULL OR length(head)=48),announcement BLOB);
            CREATE TABLE IF NOT EXISTS outbox_meta(singleton INTEGER PRIMARY KEY CHECK(singleton=1),format TEXT NOT NULL,binding BLOB NOT NULL,owner BLOB NOT NULL REFERENCES writers(incarnation),announced INTEGER NOT NULL);
            CREATE TABLE IF NOT EXISTS intents(seq INTEGER PRIMARY KEY,batch BLOB NOT NULL UNIQUE CHECK(length(batch)=32),writer BLOB NOT NULL REFERENCES writers(incarnation),plaintext BLOB NOT NULL);
            CREATE TABLE IF NOT EXISTS attempts(seq INTEGER PRIMARY KEY,id BLOB NOT NULL UNIQUE CHECK(length(id)=32),intent INTEGER NOT NULL REFERENCES intents(seq),header BLOB NOT NULL,status TEXT NOT NULL CHECK(status IN ('reserved','abandoned','sealed')));
            CREATE TABLE IF NOT EXISTS sealed(intent INTEGER PRIMARY KEY REFERENCES intents(seq),record_id BLOB NOT NULL UNIQUE REFERENCES attempts(id),bytes BLOB NOT NULL,acked INTEGER NOT NULL DEFAULT 0 CHECK(acked IN (0,1)));")?;
        let announcement = if let Some(sign) = sign {
            let manifest: Hash = binding.manifest.as_slice().try_into()?;
            let device: Hash = binding.author.as_slice().try_into()?;
            let body = match previous {
                None => Body::Writer {
                    manifest,
                    document: binding.document,
                    device,
                    incarnation,
                    actor,
                    previous: None,
                },
                Some((previous, closing)) => Body::Handoff {
                    manifest,
                    document: binding.document,
                    device,
                    incarnation,
                    actor,
                    previous,
                    closing,
                },
            };
            let message = Message {
                route: Route {
                    app: binding.application,
                    tenant: binding.tenant,
                    collection: binding.collection,
                },
                signer: device,
                body,
            };
            let bytes = sign(&message)?;
            if parse(&bytes)?.message != message {
                return Err(error("signer changed writer admission"));
            }
            Some(bytes)
        } else {
            None
        };
        tx.execute(
            // Explicit creation order: rowids of a BLOB-keyed table are not
            // stable across VACUUM or rewrite.
            "INSERT INTO writers(seq,incarnation,actor,announcement)
             VALUES ((SELECT coalesce(max(seq),0)+1 FROM writers),?1,?2,?3)",
            params![
                incarnation.as_slice(),
                actor.to_le_bytes().as_slice(),
                announcement
            ],
        )?;
        tx.execute("INSERT INTO outbox_meta VALUES (1,?1,?2,?3,?4) ON CONFLICT(singleton) DO UPDATE SET owner=excluded.owner", params![FORMAT, encoded, incarnation.as_slice(), announced])?;
        // Reservations are never resumed, even when the process may have died
        // before entering the crypto provider. Retain every consumed ID.
        tx.execute(
            "UPDATE attempts SET status='abandoned' WHERE status='reserved'",
            [],
        )?;
        Ok(Writer { incarnation, actor })
    }
    /// Attach sealing/delivery to an already committed integrated writer.
    /// This does not create a writer, switch journal mode or replay a request.
    pub(super) fn attach(path: &Path, binding: Binding, writer: Writer) -> Result<Self> {
        let conn = Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE)?;
        check_binding(&conn, &binding)?;
        let format: String = conn.query_row(
            "SELECT value FROM prototype_meta WHERE key='format'",
            [],
            |r| r.get(0),
        )?;
        let mode: String = conn.query_row("PRAGMA journal_mode", [], |r| r.get(0))?;
        if format != matters_sync_prototype::ATOMIC_FORMAT || mode != "wal" {
            return Err(error("integrated WAL store required"));
        }
        conn.execute_batch("PRAGMA synchronous=FULL; PRAGMA foreign_keys=ON;")?;
        active(&conn, writer)?;
        conn.busy_timeout(std::time::Duration::ZERO)?;
        Ok(Self {
            conn,
            binding,
            writer,
        })
    }
    pub fn writer(&self) -> Writer {
        self.writer
    }
    pub fn enqueue(
        &mut self,
        batch: Id,
        plaintext: &[u8],
        mut hook: impl FnMut(Point) -> Result<()>,
    ) -> Result<()> {
        if plaintext.len() > wire::MAX_PLAINTEXT_BYTES {
            return Err(error("intent payload exceeds bound"));
        }
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        if !Self::enqueue_on(&tx, self.writer, batch, plaintext)? {
            return Ok(());
        }
        hook(Point::BeforeIntentCommit)?;
        tx.commit()?;
        hook(Point::AfterIntentCommit)
    }
    pub(super) fn enqueue_on(
        tx: &Transaction<'_>,
        writer: Writer,
        batch: Id,
        plaintext: &[u8],
    ) -> Result<bool> {
        if plaintext.len() > wire::MAX_PLAINTEXT_BYTES {
            return Err(error("intent payload exceeds bound"));
        }
        active(tx, writer)?;
        let old: Option<Vec<u8>> = tx
            .query_row(
                "SELECT plaintext FROM intents WHERE batch=?1",
                [batch.as_slice()],
                |r| r.get(0),
            )
            .optional()?;
        if let Some(old) = old {
            return if old == plaintext {
                Ok(false)
            } else {
                Err(error("batch identity conflicts with retained intent"))
            };
        }
        count_limit(tx, "SELECT count(*) FROM intents", MAX_INTENTS)?;
        tx.execute(
            "INSERT INTO intents(batch,writer,plaintext) VALUES (?1,?2,?3)",
            params![batch.as_slice(), writer.incarnation.as_slice(), plaintext],
        )?;
        Ok(true)
    }
    pub(super) fn check_owner(
        tx: &Transaction<'_>,
        binding: &Binding,
        writer: Writer,
    ) -> Result<()> {
        check_binding(tx, binding)?;
        active(tx, writer)
    }
    pub(super) fn require_reconciled(tx: &Transaction<'_>, writer: Writer) -> Result<()> {
        let blocked: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM intents WHERE writer!=?1 AND NOT EXISTS(SELECT 1 FROM sealed WHERE intent=intents.seq))", [writer.incarnation.as_slice()], |r| r.get(0))?;
        if blocked {
            return Err(RecoveryRequired.into());
        }
        Ok(())
    }
    /// Existing sealed bytes return before RNG or crypto. A fresh reservation is
    /// committed before the FnOnce sealer is called. Never expose a resume API.
    pub fn seal(
        &mut self,
        batch: Id,
        mut random: impl FnMut() -> Result<Id>,
        seal: impl FnOnce(wire::Header<'_>, &[u8]) -> Result<Vec<u8>>,
        mut hook: impl FnMut(Point) -> Result<()>,
    ) -> Result<Stored> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        active(&tx, self.writer)?;
        if let Some(record) = stored(&tx, &batch)? {
            return Ok(record);
        }
        let (intent, incarnation, plaintext, previous): (i64, Vec<u8>, Vec<u8>, Option<Vec<u8>>) = tx.query_row("SELECT intents.seq,writer,plaintext,head FROM intents JOIN writers ON writer=incarnation WHERE batch=?1", [batch.as_slice()], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?)))?;
        if incarnation != self.writer.incarnation {
            return Err(RecoveryRequired.into());
        }
        let earlier: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM intents WHERE seq<?1 AND writer=?2 AND NOT EXISTS(SELECT 1 FROM sealed WHERE intent=intents.seq))", params![intent, incarnation], |r| r.get(0))?;
        if earlier {
            return Err(error("earlier intent must be sealed first"));
        }
        let record_id = random()?;
        let incarnation: Id = incarnation
            .try_into()
            .map_err(|_| error("invalid stored incarnation"))?;
        let previous: Option<Hash> = previous
            .map(|p| {
                p.try_into()
                    .map_err(|_| error("invalid stored predecessor"))
            })
            .transpose()?;
        let header = wire::Header {
            context: self.binding.context(),
            kind: wire::Kind::Data,
            epoch: self.binding.epoch,
            manifest: self.binding.manifest.as_slice().try_into()?,
            author: self.binding.author.as_slice().try_into()?,
            incarnation: &incarnation,
            record_id: &record_id,
            logical_batch: &batch,
            previous: previous.as_ref(),
        };
        let header_bytes = wire::header_bytes(header)?;
        tx.execute(
            "UPDATE attempts SET status='abandoned' WHERE intent=?1 AND status='reserved'",
            [intent],
        )?;
        tx.execute(
            "INSERT INTO attempts(id,intent,header,status) VALUES (?1,?2,?3,'reserved')",
            params![record_id.as_slice(), intent, header_bytes],
        )?;
        hook(Point::BeforeReservationCommit)?;
        tx.commit()?;
        hook(Point::AfterReservationCommit)?;
        let bytes = seal(header, &plaintext)?;
        hook(Point::AfterSeal)?;
        // A trusted sealer must honor the exact reservation. This is structural
        // validation; the harness also verifies actual signatures/AEAD output.
        let parsed = wire::parse(&bytes, self.binding.context())?;
        if parsed.header_bytes() != header_bytes {
            return Err(error("sealer changed reserved header"));
        }
        let digest = hash(&bytes);
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        active(&tx, self.writer)?;
        tx.execute(
            "INSERT INTO sealed(intent,record_id,bytes) VALUES (?1,?2,?3)",
            params![intent, record_id.as_slice(), bytes],
        )?;
        let changed = tx.execute(
            "UPDATE attempts SET status='sealed' WHERE id=?1 AND status='reserved'",
            [record_id.as_slice()],
        )?;
        if changed != 1 {
            return Err(error("reservation was abandoned"));
        }
        tx.execute(
            "UPDATE writers SET head=?1 WHERE incarnation=?2",
            params![digest.as_slice(), incarnation.as_slice()],
        )?;
        hook(Point::BeforeOutboxCommit)?;
        tx.commit()?;
        hook(Point::AfterOutboxCommit)?;
        Ok(Stored {
            id: record_id,
            bytes,
        })
    }
    pub fn record(&self, batch: Id) -> Result<Option<Stored>> {
        stored(&self.conn, &batch)
    }
    pub fn pending(&self) -> Result<Vec<Stored>> {
        pending(&self.conn)
    }
    /// Oldest unsealed intent eligible for this active writer. A sealed batch
    /// remains retrievable separately; retired intents never block fresh work.
    pub fn next_batch(&self) -> Result<Option<Id>> {
        active(&self.conn, self.writer)?;
        Ok(self.conn.query_row("SELECT batch FROM intents WHERE writer=?1 AND NOT EXISTS(SELECT 1 FROM sealed WHERE intent=intents.seq) ORDER BY seq LIMIT 1", [self.writer.incarnation.as_slice()], |r| blob(r, 0)).optional()?)
    }
    /// Observable retained work requiring domain replay/reconciliation. These
    /// bytes must not be rebound to a new Loro actor or automatically resealed.
    pub fn recovery_required(&self) -> Result<Vec<RecoveryIntent>> {
        active(&self.conn, self.writer)?;
        let mut query = self.conn.prepare("SELECT batch,writer,actor,plaintext FROM intents JOIN writers ON writer=incarnation WHERE writer!=?1 AND NOT EXISTS(SELECT 1 FROM sealed WHERE intent=intents.seq) ORDER BY intents.seq")?;
        let rows = query.query_map([self.writer.incarnation.as_slice()], |r| {
            Ok(RecoveryIntent {
                batch: blob(r, 0)?,
                writer: Writer {
                    incarnation: blob(r, 1)?,
                    actor: u64::from_le_bytes(blob(r, 2)?),
                },
                plaintext: r.get(3)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }
    /// Local receipt bookkeeping only: caller authenticates durable transport
    /// acknowledgements. This never signs or deletes; writer ownership is irrelevant.
    pub fn acknowledge(&mut self, id: Id, hook: impl FnMut(Point) -> Result<()>) -> Result<()> {
        acknowledge(&mut self.conn, id, hook)
    }
}
