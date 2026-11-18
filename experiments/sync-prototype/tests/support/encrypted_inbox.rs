//! Bounded durable inbox: fixed trusted membership/writers, one route/document.
//! The service cursor denotes retained bytes, never successful CRDT application.
use crate::support::{hash, Authority, EpochKey, Hash, Id, Route};
use loro::{EncodedBlobMode, ExportMode, LoroDoc};
use matters_sync_prototype::{project_document, wire, Projection, Result};
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
};

const FORMAT: &str = "matters-encrypted-inbox/0";
// Chosen fixture budget: at most 32 wire records (~32 MiB), plus projections,
// SQLite overhead and staged engine state. Not a hostile-import memory bound.
pub const MAX_RECORDS: i64 = 32;
fn error(message: &str) -> Box<dyn std::error::Error + Send + Sync> {
    std::io::Error::other(message).into()
}

#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct Config {
    pub route: Route,
    pub document: [u8; 16],
    pub generation: Id,
    pub root: Vec<u8>,
    pub genesis: Vec<u8>,
    pub writers: Vec<Vec<u8>>,
}
impl Config {
    fn binding(&self) -> Result<Vec<u8>> {
        Ok(serde_json::to_vec(&(
            self.route.app,
            self.route.tenant,
            self.route.collection,
            self.document,
            self.generation,
            &self.root,
            &self.genesis,
            &self.writers,
        ))?)
    }
    pub fn authority(&self) -> Result<Authority> {
        let mut authority = Authority::bootstrap(&self.genesis, self.route.clone(), &self.root)?;
        for writer in &self.writers {
            authority.admit_writer(writer, self.document)?;
        }
        Ok(authority)
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Point {
    BeforeReceipt,
    AfterReceipt,
    BeforeApply,
    AfterApply,
}
#[derive(Debug, PartialEq, Eq)]
pub struct Entry {
    pub sequence: i64,
    pub bytes: Vec<u8>,
    pub status: String,
    pub reason: String,
    pub applied: Option<i64>,
}
pub struct Inbox {
    conn: Connection,
    config: Config,
    authority: Authority,
}
impl Inbox {
    pub fn open(path: impl AsRef<Path>, config: Config) -> Result<Self> {
        let authority = config.authority()?;
        let binding = config.binding()?;
        let mut conn = Connection::open(path)?;
        conn.busy_timeout(std::time::Duration::from_secs(1))?;
        validate_database(&conn, &binding)?;
        let mode: String = conn.query_row("PRAGMA journal_mode=WAL", [], |r| r.get(0))?;
        if mode != "wal" {
            return Err(error("persistent inbox requires WAL"));
        }
        conn.execute_batch("PRAGMA synchronous=FULL; PRAGMA foreign_keys=ON;")?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        validate_database(&tx, &binding)?;
        tx.execute_batch(
            "CREATE TABLE IF NOT EXISTS inbox_meta (
            singleton INTEGER PRIMARY KEY CHECK(singleton=1), format TEXT NOT NULL,
            binding BLOB NOT NULL, cursor INTEGER NOT NULL, projection TEXT NOT NULL);
            CREATE TABLE IF NOT EXISTS inbox (
            sequence INTEGER PRIMARY KEY, bytes BLOB NOT NULL, status TEXT NOT NULL,
            reason TEXT NOT NULL, applied INTEGER UNIQUE);",
        )?;
        tx.execute(
            "INSERT OR IGNORE INTO inbox_meta VALUES (1,?1,?2,0,?3)",
            params![
                FORMAT,
                binding,
                serde_json::to_string(&Projection::default())?
            ],
        )?;
        tx.commit()?;
        Ok(Self {
            conn,
            config,
            authority,
        })
    }
    pub fn cursor(&self) -> Result<i64> {
        Ok(self
            .conn
            .query_row("SELECT cursor FROM inbox_meta WHERE singleton=1", [], |r| {
                r.get(0)
            })?)
    }
    pub fn entries(&self) -> Result<Vec<Entry>> {
        entries(&self.conn)
    }
    pub fn projection(&self) -> Result<Projection> {
        let json: String = self.conn.query_row(
            "SELECT projection FROM inbox_meta WHERE singleton=1",
            [],
            |r| r.get(0),
        )?;
        Ok(serde_json::from_str(&json)?)
    }
    /// Caller supplies the next position of a single bounded ordered stream.
    /// Generation changes require an explicit future resynchronization policy.
    pub fn receive(
        &mut self,
        generation: Id,
        sequence: i64,
        bytes: &[u8],
        mut hook: impl FnMut(Point) -> Result<()>,
    ) -> Result<()> {
        if generation != self.config.generation
            || sequence <= 0
            || bytes.len() > wire::MAX_WIRE_BYTES
        {
            return Err(error("receive generation, position or byte limit"));
        }
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let old: Option<Vec<u8>> = tx
            .query_row(
                "SELECT bytes FROM inbox WHERE sequence=?1",
                [sequence],
                |r| r.get(0),
            )
            .optional()?;
        if let Some(old) = old {
            return if old == bytes {
                Ok(())
            } else {
                Err(error("conflicting bytes at retained cursor"))
            };
        }
        let cursor: i64 =
            tx.query_row("SELECT cursor FROM inbox_meta WHERE singleton=1", [], |r| {
                r.get(0)
            })?;
        if sequence != cursor + 1 || cursor >= MAX_RECORDS {
            return Err(error("receive gap or retained-record capacity"));
        }
        tx.execute(
            "INSERT INTO inbox VALUES (?1,?2,'waiting','unprocessed',NULL)",
            params![sequence, bytes],
        )?;
        tx.execute(
            "UPDATE inbox_meta SET cursor=?1 WHERE singleton=1",
            [sequence],
        )?;
        hook(Point::BeforeReceipt)?;
        tx.commit()?;
        hook(Point::AfterReceipt)
    }
    /// All state is staged inside one transaction; nothing in memory is published
    /// ahead of SQLite. Reopening/retrying replays committed authenticated bytes.
    pub fn process(
        &mut self,
        key: &EpochKey,
        mut hook: impl FnMut(Point) -> Result<()>,
    ) -> Result<()> {
        // A missing/wrong local key is an operational error, never quarantine.
        self.authority.check_epoch_key(key)?;
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut rows = entries(&tx)?;
        let mut candidates = BTreeMap::new();
        let mut seen = BTreeMap::new();
        let mut successors = BTreeMap::new();
        let mut frozen = BTreeSet::new();
        for row in &mut rows {
            let verified =
                match self
                    .authority
                    .authenticate_data(&row.bytes, key, &self.config.document)
                {
                    Ok(v) => v,
                    Err(e) => {
                        reject(row, &format!("authentication:{e}"));
                        continue;
                    }
                };
            let h = verified.parsed().header;
            let digest = hash(&row.bytes);
            let incarnation = *h.incarnation;
            if let Some((old_digest, old_incarnation)) =
                seen.insert(*h.record_id, (digest, incarnation))
            {
                if old_digest == digest {
                    if row.applied.is_some() {
                        return Err(error("duplicate in accepted journal"));
                    }
                    row.status = "duplicate".into();
                    row.reason = "exact-record-bytes".into();
                    continue;
                }
                frozen.insert(old_incarnation);
                frozen.insert(incarnation);
            }
            if let Some(old) = successors.insert((incarnation, h.previous.copied()), digest) {
                if old != digest {
                    frozen.insert(incarnation);
                }
            }
            // Claims are collected before AEAD/import: valid competing signatures
            // remain fork evidence even if one payload cannot be applied.
            let plain = verified.open();
            candidates.insert(
                row.sequence,
                Candidate {
                    digest,
                    incarnation,
                    previous: h.previous.copied(),
                    actor: verified.actor(),
                    plain,
                },
            );
        }
        let mut doc = LoroDoc::new();
        let mut heads = BTreeMap::new();
        let mut accepted: Vec<_> = rows.iter().filter(|r| r.applied.is_some()).collect();
        accepted.sort_by_key(|r| r.applied);
        let mut ordinal = 0;
        for row in accepted {
            let c = candidates
                .get(&row.sequence)
                .ok_or_else(|| error("accepted journal authentication failed"))?;
            if c.previous != heads.get(&c.incarnation).copied() {
                return Err(error("accepted chain mismatch"));
            }
            let plain = c
                .plain
                .as_ref()
                .map_err(|_| error("accepted decryption failed"))?;
            doc = match stage(&doc, plain, c.actor)? {
                Stage::Ready(d) => d,
                _ => return Err(error("accepted import no longer valid")),
            };
            heads.insert(c.incarnation, c.digest);
            ordinal += 1;
            if row.applied != Some(ordinal) {
                return Err(error("accepted order gap"));
            }
        }
        // Fixed-point drain: a later delivery may satisfy an earlier causal gap.
        loop {
            let mut progress = false;
            for row in &mut rows {
                let Some(c) = candidates.get(&row.sequence) else {
                    continue;
                };
                if frozen.contains(&c.incarnation) {
                    row.status = "fork".into();
                    row.reason = "signed-chain-or-record-id-conflict".into();
                    continue;
                }
                if row.applied.is_some() {
                    row.status = "applied".into();
                    row.reason.clear();
                    continue;
                }
                let plain = match &c.plain {
                    Ok(p) => p,
                    Err(_) => {
                        reject(row, "authenticated-decryption");
                        continue;
                    }
                };
                if c.previous != heads.get(&c.incarnation).copied() {
                    row.status = "waiting".into();
                    row.reason = "predecessor-not-applied".into();
                    continue;
                }
                match stage(&doc, plain, c.actor)? {
                    Stage::Ready(staged) => {
                        doc = staged;
                        ordinal += 1;
                        row.applied = Some(ordinal);
                        row.status = "applied".into();
                        row.reason.clear();
                        heads.insert(c.incarnation, c.digest);
                        progress = true;
                    }
                    Stage::Waiting => {
                        row.status = "waiting".into();
                        row.reason = "causal-dependency".into();
                    }
                    Stage::Rejected(reason) => reject(row, reason),
                }
            }
            if !progress {
                break;
            }
        }
        // Explicitly distinguish descendants of retained bad/forked records from
        // records whose predecessors have simply not arrived.
        let blocked: BTreeSet<_> = rows
            .iter()
            .filter(|r| matches!(r.status.as_str(), "quarantined" | "fork"))
            .map(|r| hash(&r.bytes))
            .collect();
        for row in &mut rows {
            if row.status == "waiting"
                && candidates
                    .get(&row.sequence)
                    .and_then(|c| c.previous)
                    .is_some_and(|p| blocked.contains(&p))
            {
                row.reason = "blocked-by-rejected-predecessor".into();
            }
            tx.execute(
                "UPDATE inbox SET status=?1,reason=?2,applied=?3 WHERE sequence=?4",
                params![row.status, row.reason, row.applied, row.sequence],
            )?;
        }
        tx.execute(
            "UPDATE inbox_meta SET projection=?1 WHERE singleton=1",
            [serde_json::to_string(&project_document(&doc)?)?],
        )?;
        hook(Point::BeforeApply)?;
        tx.commit()?;
        hook(Point::AfterApply)
    }
}
fn validate_database(conn: &Connection, binding: &[u8]) -> Result<()> {
    let count: i64 = conn.query_row(
        "SELECT count(*) FROM sqlite_master WHERE type='table' AND name NOT GLOB 'sqlite_*'",
        [],
        |r| r.get(0),
    )?;
    if count != 0 {
        let saved = conn.query_row(
            "SELECT format,binding FROM inbox_meta WHERE singleton=1",
            [],
            |r| Ok((r.get::<_, String>(0)?, r.get::<_, Vec<u8>>(1)?)),
        );
        if !matches!(saved, Ok((ref format, ref old)) if format == FORMAT && old == binding) {
            return Err(error("foreign database or changed inbox binding"));
        }
    }
    Ok(())
}

fn entries(conn: &Connection) -> Result<Vec<Entry>> {
    let mut stmt =
        conn.prepare("SELECT sequence,bytes,status,reason,applied FROM inbox ORDER BY sequence")?;
    let rows = stmt.query_map([], |r| {
        Ok(Entry {
            sequence: r.get(0)?,
            bytes: r.get(1)?,
            status: r.get(2)?,
            reason: r.get(3)?,
            applied: r.get(4)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}
fn reject(row: &mut Entry, reason: &str) {
    row.status = "quarantined".into();
    row.reason = reason.into();
}
struct Candidate {
    digest: Hash,
    incarnation: Id,
    previous: Option<Hash>,
    actor: u64,
    plain: crate::support::Result<Vec<u8>>,
}
enum Stage {
    Ready(LoroDoc),
    Waiting,
    Rejected(&'static str),
}
fn stage(doc: &LoroDoc, plain: &[u8], actor: u64) -> Result<Stage> {
    let meta = match LoroDoc::decode_import_blob_meta(plain, true) {
        Ok(m) => m,
        Err(_) => return Ok(Stage::Rejected("engine-format")),
    };
    if meta.mode != EncodedBlobMode::Updates {
        return Ok(Stage::Rejected("engine-mode"));
    }
    let start = meta.partial_start_vv.get(&actor).copied().unwrap_or(0);
    let end = meta.partial_end_vv.get(&actor).copied().unwrap_or(0);
    if meta.partial_end_vv.len() != 1
        || end <= start
        || meta.partial_start_vv.iter().any(|(id, _)| *id != actor)
    {
        return Ok(Stage::Rejected("actor-range"));
    }
    let current = doc.oplog_vv();
    let counter = current.get(&actor).copied().unwrap_or(0);
    if start < counter {
        return Ok(Stage::Rejected("actor-overlap"));
    }
    if start > counter {
        return Ok(Stage::Waiting);
    }
    let staged = LoroDoc::new();
    staged.import(&doc.export(ExportMode::Snapshot)?)?;
    if staged.import(plain).is_err() {
        return Ok(Stage::Rejected("engine-import"));
    }
    if staged.export_pending_updates()?.is_some() {
        return Ok(Stage::Waiting);
    }
    let mut expected = current;
    expected.set_end(loro::ID::new(actor, end));
    if staged.oplog_vv() != expected {
        return Ok(Stage::Rejected("actor-effects"));
    }
    if project_document(&staged).is_err() {
        return Ok(Stage::Rejected("domain-projection"));
    }
    Ok(Stage::Ready(staged))
}
