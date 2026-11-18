//! Bounded durable inbox: fixed trusted membership/writers, one route/document.
//! The service cursor denotes retained bytes, never successful CRDT application.
use crate::support::{hash, parse, Authority, Body, EpochKey, Hash, Id, Route};
use loro::{EncodedBlobMode, ExportMode, LoroDoc, ID};
use matters_sync_prototype::{
    causal_past_verdict, has_user_schema, invalid as error, project_document, wire, Projection,
    Result,
};
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use std::{
    cell::Cell,
    collections::{BTreeMap, BTreeSet},
    path::Path,
};

const FORMAT: &str = "matters-encrypted-inbox/1";
// Chosen fixture budget: at most 32 wire records (~32 MiB), plus projections,
// SQLite overhead and staged engine state. Not a hostile-import memory bound.
pub const MAX_RECORDS: i64 = 32;
/// Reported instead of a projection when individually valid accepted records
/// merge into a state that violates a domain invariant.
pub const MERGE_CONFLICT: &str = "domain-merge-conflict";

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
    /// Fixed configuration admits only root writer starts. A replacement moves
    /// the lineage head and would retire its predecessor's valid history.
    pub fn authority(&self) -> Result<Authority> {
        let mut authority = Authority::bootstrap(&self.genesis, self.route.clone(), &self.root)?;
        for writer in &self.writers {
            if !matches!(
                parse(writer)?.message.body,
                Body::Writer { previous: None, .. }
            ) {
                return Err(error(
                    "fixed inbox configuration admits only root writer starts",
                ));
            }
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
    /// Another writer signed a different record under this record ID. This is
    /// evidence against the relay or an admitted writer, not against this row.
    pub record_id_conflict: bool,
}

/// Expensive work performed on this thread, for budget regression tests.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Work {
    pub authentications: u32,
    pub stagings: u32,
}
thread_local!(static WORK: Cell<Work> = const { Cell::new(Work { authentications: 0, stagings: 0 }) });
pub fn take_work() -> Work {
    WORK.with(|w| w.replace(Work::default()))
}
fn count(f: impl FnOnce(&mut Work)) {
    WORK.with(|w| {
        let mut work = w.get();
        f(&mut work);
        w.set(work);
    });
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
        // Authentication facts depend only on the retained bytes and the bound
        // configuration, so they are derived once and committed with the row's
        // classification. The engine snapshot always commits together with the
        // application ordinals and projection it reflects.
        tx.execute_batch(
            "CREATE TABLE IF NOT EXISTS inbox_meta (
            singleton INTEGER PRIMARY KEY CHECK(singleton=1), format TEXT NOT NULL,
            binding BLOB NOT NULL, cursor INTEGER NOT NULL, projection TEXT,
            conflict TEXT, engine BLOB,
            CHECK((projection IS NULL) <> (conflict IS NULL)));
            CREATE TABLE IF NOT EXISTS inbox (
            sequence INTEGER PRIMARY KEY, bytes BLOB NOT NULL, status TEXT NOT NULL,
            reason TEXT NOT NULL, applied INTEGER UNIQUE,
            record_id_conflict INTEGER NOT NULL DEFAULT 0, auth TEXT,
            record_id BLOB, incarnation BLOB, previous BLOB, digest BLOB, actor INTEGER);",
        )?;
        tx.execute(
            "INSERT OR IGNORE INTO inbox_meta VALUES (1,?1,?2,0,?3,NULL,NULL)",
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
        Ok(rows(&self.conn)?.into_iter().map(|r| r.entry).collect())
    }
    /// The committed view of accepted records, or an error naming
    /// `MERGE_CONFLICT` when their merge has no valid projection.
    pub fn projection(&self) -> Result<Projection> {
        match self.merge_conflict()? {
            Some(reason) => Err(error(&reason)),
            None => {
                let json: String = self.conn.query_row(
                    "SELECT projection FROM inbox_meta WHERE singleton=1",
                    [],
                    |r| r.get(0),
                )?;
                Ok(serde_json::from_str(&json)?)
            }
        }
    }
    pub fn merge_conflict(&self) -> Result<Option<String>> {
        Ok(self.conn.query_row(
            "SELECT conflict FROM inbox_meta WHERE singleton=1",
            [],
            |r| r.get(0),
        )?)
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
            "INSERT INTO inbox(sequence,bytes,status,reason) VALUES (?1,?2,'waiting','unprocessed')",
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
    /// ahead of SQLite. Every verdict is a function of the retained signed input
    /// and the bound configuration, never of delivery order or of concurrently
    /// accepted records, so receivers holding the same input agree.
    pub fn process(
        &mut self,
        key: &EpochKey,
        mut hook: impl FnMut(Point) -> Result<()>,
    ) -> Result<()> {
        // A key for another route, manifest or epoch is an operational error.
        self.authority.check_epoch_key(key)?;
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut rows = rows(&tx)?;
        for row in &mut rows {
            if row.auth.is_none() {
                count(|w| w.authentications += 1);
                row.auth = Some(
                    match self.authority.authenticate_data(
                        &row.entry.bytes,
                        key,
                        &self.config.document,
                    ) {
                        Ok(v) => {
                            let h = v.parsed().header;
                            Ok(Facts {
                                record_id: *h.record_id,
                                incarnation: *h.incarnation,
                                previous: h.previous.copied(),
                                digest: hash(&row.entry.bytes),
                                actor: v.actor(),
                            })
                        }
                        Err(e) => Err(format!("authentication:{e}")),
                    },
                );
            }
        }

        // Claims are gathered from every authenticated record before any
        // decryption or import: a valid competing signature remains fork
        // evidence even if its payload cannot be applied.
        let mut first_copy: BTreeMap<Hash, i64> = BTreeMap::new();
        let mut first_claim: BTreeMap<Id, Id> = BTreeMap::new();
        let mut successors: BTreeMap<(Id, Option<Hash>), Hash> = BTreeMap::new();
        let mut frozen = BTreeSet::new();
        let mut shared_ids = BTreeSet::new();
        let mut candidates = Vec::new();
        for (index, row) in rows.iter_mut().enumerate() {
            let facts = match row.auth.as_ref().expect("authenticated above") {
                Ok(facts) => facts.clone(),
                Err(reason) => {
                    reject(&mut row.entry, reason);
                    continue;
                }
            };
            if let Some(first) = first_copy.get(&facts.digest) {
                if row.entry.applied.is_some() {
                    return Err(error("duplicate in accepted journal"));
                }
                row.entry.status = "duplicate".into();
                row.entry.reason = format!("duplicate-of:{first}");
                continue;
            }
            first_copy.insert(facts.digest, row.entry.sequence);
            if successors
                .insert((facts.incarnation, facts.previous), facts.digest)
                .is_some()
            {
                frozen.insert(facts.incarnation);
            }
            match first_claim.get(&facts.record_id) {
                // One signer, two different records under one ID: equivocation.
                Some(incarnation) if *incarnation == facts.incarnation => {
                    frozen.insert(facts.incarnation);
                }
                // Different signers: either writer (or the relay) could be the
                // one at fault, so neither chain is frozen. The full header is
                // in each record's key derivation, so no key is shared.
                Some(_) => {
                    shared_ids.insert(facts.record_id);
                }
                None => {
                    first_claim.insert(facts.record_id, facts.incarnation);
                }
            }
            candidates.push((index, facts));
        }

        let mut doc = LoroDoc::new();
        let engine: Option<Vec<u8>> =
            tx.query_row("SELECT engine FROM inbox_meta WHERE singleton=1", [], |r| {
                r.get(0)
            })?;
        if let Some(snapshot) = engine {
            doc.import(&snapshot)?;
        }
        let mut heads = BTreeMap::new();
        let mut accepted: Vec<_> = candidates
            .iter()
            .filter_map(|(i, f)| rows[*i].entry.applied.map(|n| (n, *i, f)))
            .collect();
        accepted.sort_by_key(|(n, ..)| *n);
        let mut ordinal = 0;
        for (n, _, facts) in &accepted {
            if facts.previous != heads.get(&facts.incarnation).copied() {
                return Err(error("accepted chain mismatch"));
            }
            heads.insert(facts.incarnation, facts.digest);
            ordinal += 1;
            if *n != ordinal {
                return Err(error("accepted order gap"));
            }
        }
        if accepted.len() != rows.iter().filter(|r| r.entry.applied.is_some()).count() {
            return Err(error("accepted journal authentication failed"));
        }
        if ordinal > 0 && doc.oplog_vv().is_empty() {
            return Err(error("accepted engine state missing"));
        }

        let mut plain: BTreeMap<usize, std::result::Result<Vec<u8>, ()>> = BTreeMap::new();
        let mut key_checked = false;
        // Payload rejections depend only on the record and its causal past, so
        // an earlier call's verdict stands. Undecryptable is key-relative and
        // is retried; fork evidence still overrides any stored verdict.
        let mut settled: BTreeSet<usize> = candidates
            .iter()
            .map(|(i, _)| *i)
            .filter(|i| rows[*i].entry.status == "quarantined")
            .collect();
        // Fixed-point drain: a later delivery may satisfy an earlier causal gap.
        loop {
            let mut progress = false;
            for (index, facts) in &candidates {
                let row = &mut rows[*index].entry;
                if frozen.contains(&facts.incarnation) {
                    row.status = "fork".into();
                    row.reason = "signed-chain-or-record-id-conflict".into();
                    continue;
                }
                if row.applied.is_some() {
                    row.status = "applied".into();
                    row.reason.clear();
                    continue;
                }
                if settled.contains(index) {
                    continue;
                }
                if facts.previous != heads.get(&facts.incarnation).copied() {
                    row.status = "waiting".into();
                    row.reason = "predecessor-not-applied".into();
                    continue;
                }
                if !key_checked {
                    key_checked = true;
                    // With accepted history, the supplied key must open it.
                    if let Some((_, last, _)) = accepted.last() {
                        if decrypt(
                            &self.authority,
                            &self.config.document,
                            &rows[*last].entry.bytes,
                            key,
                        )?
                        .is_err()
                        {
                            return Err(error("supplied epoch key does not open accepted history"));
                        }
                    }
                }
                let row = &mut rows[*index].entry;
                if !plain.contains_key(index) {
                    plain.insert(
                        *index,
                        decrypt(&self.authority, &self.config.document, &row.bytes, key)?,
                    );
                }
                let Ok(bytes) = &plain[index] else {
                    // The protocol does not commit to the epoch content key, so
                    // this is a verdict on (record, supplied key), not on input.
                    row.status = "undecryptable".into();
                    row.reason = "authenticated-decryption-with-supplied-key".into();
                    settled.insert(*index);
                    continue;
                };
                match stage(&doc, bytes, facts.actor)? {
                    Stage::Ready(staged) => {
                        doc = staged;
                        ordinal += 1;
                        row.applied = Some(ordinal);
                        row.status = "applied".into();
                        row.reason.clear();
                        heads.insert(facts.incarnation, facts.digest);
                        progress = true;
                    }
                    Stage::Waiting => {
                        row.status = "waiting".into();
                        row.reason = "causal-dependency".into();
                    }
                    Stage::Rejected(reason) => {
                        reject(row, reason);
                        settled.insert(*index);
                    }
                }
            }
            if !progress {
                break;
            }
        }
        // Explicitly distinguish descendants of retained bad/forked records from
        // records whose predecessors have simply not arrived.
        let mut blocked = BTreeMap::new();
        for row in &rows {
            let reason = match row.entry.status.as_str() {
                "quarantined" | "fork" => "blocked-by-rejected-predecessor",
                "undecryptable" => "blocked-by-undecryptable-predecessor",
                _ => continue,
            };
            blocked.insert(hash(&row.entry.bytes), reason);
        }
        for (index, facts) in &candidates {
            let row = &mut rows[*index].entry;
            row.record_id_conflict = shared_ids.contains(&facts.record_id);
            if row.status == "waiting" {
                if let Some(reason) = facts.previous.and_then(|p| blocked.get(&p)) {
                    row.reason = (*reason).into();
                }
            }
        }
        for row in &rows {
            let facts = row.auth.as_ref().and_then(|a| a.as_ref().ok());
            tx.execute(
                "UPDATE inbox SET status=?1,reason=?2,applied=?3,record_id_conflict=?4,
                 auth=?5,record_id=?6,incarnation=?7,previous=?8,digest=?9,actor=?10
                 WHERE sequence=?11",
                params![
                    row.entry.status,
                    row.entry.reason,
                    row.entry.applied,
                    row.entry.record_id_conflict,
                    match &row.auth {
                        Some(Ok(_)) => "ok",
                        Some(Err(reason)) => reason.as_str(),
                        None => return Err(error("unauthenticated row")),
                    },
                    facts.map(|f| f.record_id.to_vec()),
                    facts.map(|f| f.incarnation.to_vec()),
                    facts.and_then(|f| f.previous.map(|p| p.to_vec())),
                    facts.map(|f| f.digest.to_vec()),
                    facts.map(|f| f.actor as i64),
                    row.entry.sequence,
                ],
            )?;
        }
        // Accepted records were each valid in their own causal past, and the
        // merged engine state does not depend on application order. Its
        // projection, or the absence of one, is therefore the same everywhere.
        let (projection, conflict) = match project_document(&doc) {
            Ok(view) => (Some(serde_json::to_string(&view)?), None),
            Err(_) => (None, Some(MERGE_CONFLICT)),
        };
        tx.execute(
            "UPDATE inbox_meta SET projection=?1,conflict=?2,engine=?3 WHERE singleton=1",
            params![projection, conflict, doc.export(ExportMode::Snapshot)?],
        )?;
        hook(Point::BeforeApply)?;
        tx.commit()?;
        hook(Point::AfterApply)
    }
}
/// Authenticated decryption; only an authenticated handle can decrypt.
fn decrypt(
    authority: &Authority,
    document: &[u8; 16],
    bytes: &[u8],
    key: &EpochKey,
) -> Result<std::result::Result<Vec<u8>, ()>> {
    count(|w| w.authentications += 1);
    let verified = authority
        .authenticate_data(bytes, key, document)
        .map_err(|_| error("retained authentication changed"))?;
    Ok(verified.open().map_err(|_| ()))
}
fn validate_database(conn: &Connection, binding: &[u8]) -> Result<()> {
    if has_user_schema(conn)? {
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

#[derive(Clone)]
struct Facts {
    record_id: Id,
    incarnation: Id,
    previous: Option<Hash>,
    digest: Hash,
    actor: u64,
}
struct Row {
    entry: Entry,
    auth: Option<std::result::Result<Facts, String>>,
}
fn rows(conn: &Connection) -> Result<Vec<Row>> {
    let mut stmt = conn.prepare(
        "SELECT sequence,bytes,status,reason,applied,record_id_conflict,auth,
         record_id,incarnation,previous,digest,actor FROM inbox ORDER BY sequence",
    )?;
    let rows = stmt.query_map([], |r| {
        let auth: Option<String> = r.get(6)?;
        let fixed = |i: usize| -> rusqlite::Result<Vec<u8>> { r.get(i) };
        let auth = match auth.as_deref() {
            None => None,
            Some("ok") => {
                let previous: Option<Vec<u8>> = r.get(9)?;
                Some(Ok((
                    fixed(7)?,
                    fixed(8)?,
                    previous,
                    fixed(10)?,
                    r.get::<_, i64>(11)?,
                )))
            }
            Some(reason) => Some(Err(reason.to_string())),
        };
        Ok((
            Entry {
                sequence: r.get(0)?,
                bytes: r.get(1)?,
                status: r.get(2)?,
                reason: r.get(3)?,
                applied: r.get(4)?,
                record_id_conflict: r.get(5)?,
            },
            auth,
        ))
    })?;
    let mut out = Vec::new();
    for row in rows {
        let (entry, auth) = row?;
        let auth = match auth {
            None => None,
            Some(Err(reason)) => Some(Err(reason)),
            Some(Ok((record_id, incarnation, previous, digest, actor))) => Some(Ok(Facts {
                record_id: record_id
                    .try_into()
                    .map_err(|_| error("stored record ID"))?,
                incarnation: incarnation
                    .try_into()
                    .map_err(|_| error("stored incarnation"))?,
                previous: previous
                    .map(|p| p.try_into().map_err(|_| error("stored predecessor")))
                    .transpose()?,
                digest: digest.try_into().map_err(|_| error("stored digest"))?,
                actor: actor.try_into()?,
            })),
        };
        out.push(Row { entry, auth });
    }
    Ok(out)
}
fn reject(row: &mut Entry, reason: &str) {
    row.status = "quarantined".into();
    row.reason = reason.into();
}
enum Stage {
    Ready(LoroDoc),
    Waiting,
    Rejected(&'static str),
}
/// Every rejection here is a property of the record and its causal past alone.
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
    // Cheap metadata checks first: no copy for a record that must still wait.
    if start > counter
        || !meta
            .start_frontiers
            .iter()
            .all(|id| current.includes_id(id))
    {
        return Ok(Stage::Waiting);
    }
    count(|w| w.stagings += 1);
    let staged = doc.fork();
    if staged.import(plain).is_err() {
        return Ok(Stage::Rejected("engine-import"));
    }
    if staged.export_pending_updates()?.is_some() {
        return Ok(Stage::Waiting);
    }
    let mut expected = current;
    expected.set_end(ID::new(actor, end));
    if staged.oplog_vv() != expected {
        return Ok(Stage::Rejected("actor-effects"));
    }
    // Judge domain validity in the record's own causal past, never against
    // concurrently applied records (shared with Replica).
    if let Some(reason) =
        causal_past_verdict(&staged, &meta.partial_start_vv, &meta.partial_end_vv)?
    {
        return Ok(Stage::Rejected(reason));
    }
    Ok(Stage::Ready(staged))
}
