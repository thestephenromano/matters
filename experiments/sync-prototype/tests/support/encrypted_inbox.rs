//! Bounded durable inbox: fixed membership, durable writers, one route/document.
//! The service cursor denotes retained bytes, never successful CRDT application.
use crate::support::{
    hash, parse, Authority, Body, Boundary, EpochKey, Error, Hash, Id, Route, WriterHistory,
    MAX_CONTROL,
};
use loro::{EncodedBlobMode, ExportMode, LoroDoc, ID};
use matters_sync_prototype::{
    causal_past_verdict, has_user_schema, invalid as error, project_document, wire, Projection,
    Record, Result,
};
use rusqlite::{params, Connection, OptionalExtension, Transaction, TransactionBehavior};
use std::{
    cell::Cell,
    collections::{BTreeMap, BTreeSet},
    path::Path,
};

const FORMAT: &str = "matters-encrypted-inbox/3";
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
    BeforeWriterReceipt,
    AfterWriterReceipt,
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
    /// Writer control admission attempts (each verifies an ML-DSA signature).
    pub writer_checks: u32,
}
thread_local!(static WORK: Cell<Work> = const {
    Cell::new(Work { authentications: 0, stagings: 0, writer_checks: 0 })
});
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

const WRITER_ADMISSION: &str = "writer-admission";
const CLOSING_HISTORY: &str = "writer-closing-history";
const BLOCKED: &str = "blocked-by-rejected-predecessor";

pub struct Inbox {
    conn: Connection,
    config: Config,
    /// Verified configuration authority, built once per open.
    base: Authority,
    /// Committed admission ordinal and the authority it produced. Admission
    /// ordinals are immutable once committed, so only later ones are replayed.
    /// Updated only after the processing transaction commits.
    admitted: (i64, Authority),
    integrated: bool,
}
pub(super) struct Prepared {
    pub doc: LoroDoc,
    pub imports: Vec<Record>,
    pub admitted: (i64, Authority),
}
impl Inbox {
    pub fn open(path: impl AsRef<Path>, config: Config) -> Result<Self> {
        let base = config.authority()?;
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
        initialize_tables(&tx, &binding, false)?;
        tx.commit()?;
        Ok(Self {
            conn,
            config,
            admitted: (0, base.clone()),
            base,
            integrated: false,
        })
    }
    /// Persist exact signed control bytes before attempting admission. This
    /// bounded control inbox has no remote cursor/ACK semantics.
    pub fn receive_writer(
        &mut self,
        bytes: &[u8],
        mut hook: impl FnMut(Point) -> Result<()>,
    ) -> Result<()> {
        if bytes.len() > MAX_CONTROL {
            return Err(error("writer control exceeds input budget"));
        }
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        if !retain_writer(&tx, bytes)? {
            return Ok(());
        }
        hook(Point::BeforeWriterReceipt)?;
        tx.commit()?;
        hook(Point::AfterWriterReceipt)
    }
    pub fn writer_entries(&self) -> Result<Vec<WriterEntry>> {
        writer_entries(&self.conn)
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
        if self.integrated {
            return Err(error("shared projection is owned by the replica"));
        }
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
                "SELECT bytes FROM received_records WHERE sequence=?1",
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
            "INSERT INTO received_records(sequence,bytes,status,reason) VALUES (?1,?2,'waiting','unprocessed')",
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
        if self.integrated {
            return Err(error("shared inbox requires the replica transaction"));
        }
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let prepared = prepare_on(
            &tx,
            &self.config,
            &self.base,
            self.admitted.clone(),
            key,
            None,
            &BTreeMap::new(),
        )?;
        hook(Point::BeforeApply)?;
        tx.commit()?;
        self.admitted = prepared.admitted;
        hook(Point::AfterApply)
    }
    pub(super) fn initialize_shared(tx: &Transaction<'_>, config: &Config) -> Result<()> {
        let binding = config.binding()?;
        let exists: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE name='inbox_meta')",
            [],
            |r| r.get(0),
        )?;
        if exists {
            validate_binding(tx, &binding, true)?;
        }
        initialize_tables(tx, &binding, true)
    }
    pub(super) fn attach(path: &Path, config: Config) -> Result<Self> {
        let conn = Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE)?;
        validate_binding(&conn, &config.binding()?, true)?;
        let base = config.authority()?;
        conn.busy_timeout(matters_sync_prototype::INTEGRATED_BUSY_TIMEOUT)?;
        conn.execute_batch("PRAGMA synchronous=FULL; PRAGMA foreign_keys=ON;")?;
        Ok(Self {
            conn,
            config,
            admitted: (0, base.clone()),
            base,
            integrated: true,
        })
    }
    pub(super) fn prepare_shared(
        &self,
        tx: &Transaction<'_>,
        key: &EpochKey,
        doc: &LoroDoc,
    ) -> Result<Prepared> {
        if !self.integrated {
            return Err(error("shared inbox required"));
        }
        // Receipt may proceed while local work is unsealed. Application may
        // not: a remote journal row must never follow a stranded local request.
        let unsealed: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM intents WHERE NOT EXISTS(SELECT 1 FROM sealed WHERE intent=intents.seq))", [], |r| r.get(0))?;
        if unsealed {
            return Err(error("seal local intents before applying incoming records"));
        }
        let mut controls = tx.prepare("SELECT announcement FROM writers ORDER BY seq")?;
        for bytes in controls.query_map([], |r| r.get::<_, Vec<u8>>(0))? {
            retain_writer(tx, &bytes?)?;
        }
        let mut stmt = tx.prepare("SELECT intents.seq,sealed.bytes,intents.plaintext FROM intents JOIN sealed ON sealed.intent=intents.seq ORDER BY intents.seq")?;
        let mut local_plain = BTreeMap::new();
        for row in stmt.query_map([], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, Vec<u8>>(1)?,
                r.get::<_, Vec<u8>>(2)?,
            ))
        })? {
            let (seq, bytes, plain) = row?;
            let saved: Option<Vec<u8>> = tx
                .query_row(
                    "SELECT bytes FROM received_records WHERE sequence=?1",
                    [-seq],
                    |r| r.get(0),
                )
                .optional()?;
            if let Some(saved) = saved {
                if saved != bytes {
                    return Err(error("local receipt changed"));
                }
            } else {
                if rows(tx)?.len() >= MAX_RECORDS as usize {
                    return Err(error("receive capacity"));
                }
                tx.execute("INSERT INTO received_records(sequence,bytes,status,reason) VALUES (?1,?2,'waiting','local-sealed')", params![-seq, bytes])?;
            }
            local_plain.insert(-seq, plain);
        }
        let prepared = prepare_on(
            tx,
            &self.config,
            &self.base,
            self.admitted.clone(),
            key,
            Some(doc),
            &local_plain,
        )?;
        let encoded: Vec<u8> = tx.query_row(
            "SELECT binding FROM outbox_meta WHERE singleton=1",
            [],
            |r| r.get(0),
        )?;
        let binding: crate::encrypted_outbox::Binding = serde_json::from_slice(&encoded)?;
        let author: Hash = binding.author.as_slice().try_into()?;
        let frozen = prepared
            .admitted
            .1
            .lineage_frozen(self.config.document, &author)
            || rows(tx)?
                .iter()
                .any(|r| r.entry.sequence < 0 && r.entry.status == "fork");
        if frozen {
            tx.execute("UPDATE inbox_meta SET local_frozen=1 WHERE singleton=1", [])?;
        }
        Ok(prepared)
    }
    pub(super) fn publish(&mut self, admitted: (i64, Authority)) {
        self.admitted = admitted;
    }
}
fn initialize_tables(tx: &Transaction<'_>, binding: &[u8], integrated: bool) -> Result<()> {
    // Successful signature facts depend on retained bytes and immutable
    // admitted actor/key bindings. Retirement eligibility is checked afresh;
    // missing writer admissions remain retryable. The engine snapshot always commits together with the
    // application ordinals and projection it reflects.
    tx.execute_batch(
            "CREATE TABLE IF NOT EXISTS inbox_meta (
            singleton INTEGER PRIMARY KEY CHECK(singleton=1), format TEXT NOT NULL,
            binding BLOB NOT NULL, integrated INTEGER NOT NULL, cursor INTEGER NOT NULL, projection TEXT,
            conflict TEXT, engine BLOB, local_frozen INTEGER NOT NULL DEFAULT 0,
            CHECK((projection IS NULL) <> (conflict IS NULL)));
            CREATE TABLE IF NOT EXISTS received_records (
            sequence INTEGER PRIMARY KEY, bytes BLOB NOT NULL, status TEXT NOT NULL,
            reason TEXT NOT NULL, applied INTEGER UNIQUE,
            record_id_conflict INTEGER NOT NULL DEFAULT 0, auth TEXT,
            record_id BLOB, incarnation BLOB, previous BLOB, digest BLOB, actor BLOB CHECK(actor IS NULL OR length(actor)=8));
            CREATE TABLE IF NOT EXISTS writer_receipts (
            id INTEGER PRIMARY KEY, digest BLOB NOT NULL UNIQUE, bytes BLOB NOT NULL,
            status TEXT NOT NULL, reason TEXT NOT NULL, admitted INTEGER UNIQUE);",
        )?;
    tx.execute(
        "INSERT OR IGNORE INTO inbox_meta VALUES (1,?1,?2,?3,0,?4,NULL,NULL,0)",
        params![
            FORMAT,
            binding,
            integrated,
            serde_json::to_string(&Projection::default())?
        ],
    )?;
    Ok(())
}
fn retain_writer(tx: &Transaction<'_>, bytes: &[u8]) -> Result<bool> {
    if bytes.len() > MAX_CONTROL {
        return Err(error("writer control exceeds input budget"));
    }
    let digest = hash(bytes);
    let old: Option<Vec<u8>> = tx
        .query_row(
            "SELECT bytes FROM writer_receipts WHERE digest=?1",
            [digest.as_slice()],
            |r| r.get(0),
        )
        .optional()?;
    if let Some(old) = old {
        return if old == bytes {
            Ok(false)
        } else {
            Err(error("control digest conflict"))
        };
    }
    let count: i64 = tx.query_row("SELECT count(*) FROM writer_receipts", [], |r| r.get(0))?;
    if count >= MAX_RECORDS {
        return Err(error("writer receipt capacity"));
    }
    tx.execute("INSERT INTO writer_receipts(digest,bytes,status,reason) VALUES (?1,?2,'waiting','unprocessed')",
            params![digest.as_slice(), bytes])?;
    Ok(true)
}
fn prepare_on(
    tx: &Transaction<'_>,
    config: &Config,
    base: &Authority,
    cached: (i64, Authority),
    key: &EpochKey,
    initial: Option<&LoroDoc>,
    local_plain: &BTreeMap<i64, Vec<u8>>,
) -> Result<Prepared> {
    // A key for another route, manifest or epoch is an operational error.
    base.check_epoch_key(key)?;
    let document = config.document;
    let context = config.route.content(&document);
    let admitted = admit_receipts(tx, document, cached)?;
    let authority = &admitted.1;
    let histories = authority.writer_histories(document);
    let mut rows = rows(tx)?;
    for row in &mut rows {
        // A member-signed record waiting for its writer is re-authenticated
        // only once that writer has been admitted.
        let retry = match row.auth.as_ref() {
            None => true,
            Some(Err(reason)) if reason == WRITER_ADMISSION => {
                wire::parse(&row.entry.bytes, context)
                    .is_ok_and(|p| histories.contains_key(p.header.incarnation))
            }
            Some(_) => false,
        };
        if retry {
            count(|w| w.authentications += 1);
            row.auth = Some(
                match authority.authenticate_history(&row.entry.bytes, key, &config.document) {
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
                    Err(Error::MissingWriter) => Err(WRITER_ADMISSION.into()),
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
                if reason == WRITER_ADMISSION {
                    // The signature is verified; only the writer binding is
                    // unknown. A refused competing handoff is never
                    // admitted, so content in a frozen lineage is a fork.
                    let frozen = wire::parse(&row.entry.bytes, context)
                        .is_ok_and(|p| authority.lineage_frozen(document, p.header.author));
                    if frozen {
                        row.entry.status = "fork".into();
                        row.entry.reason = "frozen-writer-lineage".into();
                    } else {
                        row.entry.status = "waiting".into();
                        row.entry.reason = reason.clone();
                    }
                } else {
                    reject(&mut row.entry, reason);
                }
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

    // Retirement eligibility is mutable; cached signature facts are not.
    // Re-evaluate closing prefixes and device freezes on every pass.
    let gates = history_gates(&histories, &candidates, &frozen)?;
    for (_, facts) in &candidates {
        if gates
            .frozen_devices
            .contains(&histories[&facts.incarnation].device)
        {
            frozen.insert(facts.incarnation);
        }
    }
    // Closing heads only advance to their signed value (a record past one
    // freezes the device above), so eligibility is monotonic in a pass.
    let mut eligible: BTreeSet<Id> = BTreeSet::new();

    let mut doc = initial.map(LoroDoc::fork).unwrap_or_default();
    let engine: Option<Vec<u8>> =
        tx.query_row("SELECT engine FROM inbox_meta WHERE singleton=1", [], |r| {
            r.get(0)
        })?;
    if initial.is_none() {
        if let Some(snapshot) = engine {
            doc.import(&snapshot)?;
        }
    }
    let mut imports = Vec::new();
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
    // Payload rejections depend only on the record and its causal past, so
    // an earlier call's verdict stands; fork evidence still overrides it.
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
            if !eligible.contains(&facts.incarnation) {
                if !gates.chains[&facts.incarnation]
                    .iter()
                    .all(|(previous, closing)| heads.get(previous).copied() == *closing)
                {
                    row.status = "waiting".into();
                    row.reason = CLOSING_HISTORY.into();
                    continue;
                }
                eligible.insert(facts.incarnation);
            }
            if facts.previous != heads.get(&facts.incarnation).copied() {
                row.status = "waiting".into();
                row.reason = "predecessor-not-applied".into();
                continue;
            }
            let row = &mut rows[*index].entry;
            if !plain.contains_key(index) {
                plain.insert(*index, decrypt(authority, &document, &row.bytes, key)?);
            }
            let Ok(bytes) = &plain[index] else {
                // The epoch key handle can only hold the key its signed
                // epoch commits to (#112), so authenticated-decryption
                // failure is a verdict on the record.
                reject(row, "authenticated-decryption");
                settled.insert(*index);
                continue;
            };
            let local = local_plain.get(&row.sequence);
            if local.is_some_and(|saved| saved != bytes) {
                return Err(error(
                    "sealed local payload differs from its durable intent",
                ));
            }
            match stage(&doc, bytes, facts.actor, local.is_some())? {
                Stage::Ready(staged) => {
                    doc = staged;
                    if local.is_none() {
                        imports.push(Record {
                            id: format!(
                                "authenticated:{}",
                                facts
                                    .digest
                                    .iter()
                                    .map(|b| format!("{b:02x}"))
                                    .collect::<String>()
                            ),
                            bytes: bytes.clone(),
                        });
                    }
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
    let blocked: BTreeSet<Hash> = rows
        .iter()
        .filter(|r| matches!(r.entry.status.as_str(), "quarantined" | "fork"))
        .map(|r| hash(&r.entry.bytes))
        .collect();
    for (index, facts) in &candidates {
        let row = &mut rows[*index].entry;
        row.record_id_conflict = shared_ids.contains(&facts.record_id);
        if row.status == "waiting" && facts.previous.is_some_and(|p| blocked.contains(&p)) {
            row.reason = BLOCKED.into();
        }
        // A closed predecessor whose complete signed prefix holds a
        // rejected record can never reach its closing head: the handoff
        // chains through it exactly as a same-writer successor would.
        if row.status == "waiting"
            && row.reason == CLOSING_HISTORY
            && gates.chains[&facts.incarnation]
                .iter()
                .any(|(previous, _)| {
                    gates
                        .prefixes
                        .get(previous)
                        .is_some_and(|prefix| !prefix.is_disjoint(&blocked))
                })
        {
            row.reason = BLOCKED.into();
        }
    }
    for row in &rows {
        let facts = row.auth.as_ref().and_then(|a| a.as_ref().ok());
        tx.execute(
            "UPDATE received_records SET status=?1,reason=?2,applied=?3,record_id_conflict=?4,
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
                facts.map(|f| f.actor.to_le_bytes().to_vec()),
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
    if initial.is_none() {
        tx.execute(
            "UPDATE inbox_meta SET projection=?1,conflict=?2,engine=?3 WHERE singleton=1",
            params![projection, conflict, doc.export(ExportMode::Snapshot)?],
        )?;
    } else if conflict.is_some() {
        return Err(error(MERGE_CONFLICT));
    }
    Ok(Prepared {
        doc,
        imports,
        admitted,
    })
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
        .authenticate_history(bytes, key, document)
        .map_err(|_| error("retained authentication changed"))?;
    Ok(verified.open().map_err(|_| ()))
}
fn validate_database(conn: &Connection, binding: &[u8]) -> Result<()> {
    if has_user_schema(conn)? {
        validate_binding(conn, binding, false)?;
    }
    Ok(())
}
fn validate_binding(conn: &Connection, binding: &[u8], integrated: bool) -> Result<()> {
    let saved = conn.query_row(
        "SELECT format,binding,integrated FROM inbox_meta WHERE singleton=1",
        [],
        |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, Vec<u8>>(1)?,
                r.get::<_, bool>(2)?,
            ))
        },
    );
    if !matches!(saved, Ok((ref format, ref old, mode)) if format == FORMAT && old == binding && mode == integrated)
    {
        return Err(error("incompatible inbox database or binding"));
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
         record_id,incarnation,previous,digest,actor FROM received_records ORDER BY sequence",
    )?;
    let rows = stmt.query_map([], |r| {
        let auth: Option<String> = r.get(6)?;
        let fixed = |i: usize| -> rusqlite::Result<Vec<u8>> { r.get(i) };
        let auth = match auth.as_deref() {
            None => None,
            Some("ok") => {
                let previous: Option<Vec<u8>> = r.get(9)?;
                Some(Ok((fixed(7)?, fixed(8)?, previous, fixed(10)?, fixed(11)?)))
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
                actor: u64::from_le_bytes(actor.try_into().map_err(|_| error("stored actor"))?),
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
fn stage(doc: &LoroDoc, plain: &[u8], actor: u64, retained_local: bool) -> Result<Stage> {
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
    if retained_local {
        if end > counter
            || !meta
                .start_frontiers
                .iter()
                .all(|id| current.includes_id(id))
        {
            return Err(error("durable local update missing from shared document"));
        }
        if let Some(reason) =
            causal_past_verdict(doc, &meta.partial_start_vv, &meta.partial_end_vv)?
        {
            return Err(error(format!("invalid durable local update: {reason}")));
        }
        return Ok(Stage::Ready(doc.fork()));
    }
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

/// Durable public signed control input and its replay/admission verdict.
#[derive(Debug, PartialEq, Eq)]
pub struct WriterEntry {
    pub id: i64,
    pub bytes: Vec<u8>,
    pub status: String,
    pub reason: String,
    pub admitted: Option<i64>,
}
fn writer_entries(conn: &Connection) -> Result<Vec<WriterEntry>> {
    let mut stmt =
        conn.prepare("SELECT id,bytes,status,reason,admitted FROM writer_receipts ORDER BY id")?;
    let entries = stmt
        .query_map([], |r| {
            Ok(WriterEntry {
                id: r.get(0)?,
                bytes: r.get(1)?,
                status: r.get(2)?,
                reason: r.get(3)?,
                admitted: r.get(4)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(entries)
}
fn admit_receipts(
    conn: &Connection,
    document: [u8; 16],
    (mut ordinal, mut authority): (i64, Authority),
) -> Result<(i64, Authority)> {
    let mut receipts = writer_entries(conn)?;
    // Committed ordinals are immutable; the cached authority already holds
    // 1..=ordinal. Replay only admissions committed after it was built.
    let cached = receipts
        .iter()
        .filter(|r| r.admitted.is_some_and(|n| n <= ordinal))
        .count();
    if i64::try_from(cached)? != ordinal {
        return Err(error("writer admission order gap"));
    }
    let mut replay: Vec<_> = receipts
        .iter()
        .filter(|r| r.admitted.is_some_and(|n| n > ordinal))
        .collect();
    replay.sort_by_key(|r| r.admitted);
    for r in replay {
        count(|w| w.writer_checks += 1);
        let result = authority.admit_writer(&r.bytes, document);
        if !(result.is_ok() && r.status == "admitted"
            || result == Err(Error::Fork) && r.status == "fork")
        {
            return Err(error("writer admission replay mismatch"));
        }
        ordinal += 1;
        if r.admitted != Some(ordinal) {
            return Err(error("writer admission order gap"));
        }
    }
    loop {
        let mut progress = false;
        for r in &mut receipts {
            // A uniqueness conflict can later become same-device equivocation
            // evidence when another signed definition is admitted. Retry it;
            // permanent format/context/signature failures remain terminal.
            if r.admitted.is_some() || (r.status == "quarantined" && r.reason != "writer:Actor") {
                continue;
            }
            let result = match parse(&r.bytes) {
                Ok(p)
                    if matches!(
                        p.message.body,
                        Body::Writer { previous: None, .. } | Body::Handoff { .. }
                    ) =>
                {
                    count(|w| w.writer_checks += 1);
                    authority.admit_writer(&r.bytes, document)
                }
                Ok(_) => Err(Error::Unsupported),
                Err(e) => Err(e),
            };
            match result {
                Ok(_) | Err(Error::Fork) => {
                    ordinal += 1;
                    r.admitted = Some(ordinal);
                    progress = true;
                    let (status, reason) = match result {
                        Ok(true) => ("admitted", ""),
                        // Same claim, fresh randomized signature.
                        Ok(false) => ("admitted", "same-writer-claim"),
                        Err(_) => ("fork", "signed-writer-branch"),
                    };
                    r.status = status.into();
                    r.reason = reason.into();
                }
                Err(Error::MissingPrevious) => {
                    r.status = "waiting".into();
                    r.reason = "writer-predecessor".into();
                }
                Err(e) => {
                    r.status = "quarantined".into();
                    r.reason = format!("writer:{e}");
                }
            }
        }
        if !progress {
            break;
        }
    }
    for r in receipts {
        conn.execute(
            "UPDATE writer_receipts SET status=?1,reason=?2,admitted=?3 WHERE id=?4",
            params![r.status, r.reason, r.admitted, r.id],
        )?;
    }
    Ok((ordinal, authority))
}
struct Gates {
    /// Devices whose writer lineage holds signed contradictory history.
    frozen_devices: BTreeSet<Hash>,
    /// Each candidate incarnation's retired predecessors and closing heads.
    chains: BTreeMap<Id, Vec<(Id, Option<Hash>)>>,
    /// Complete signed prefixes of closed incarnations.
    prefixes: BTreeMap<Id, BTreeSet<Hash>>,
}
/// Retirement eligibility is mutable; cached signature facts are not.
/// Re-evaluated on every pass from the admitted writer histories.
fn history_gates(
    histories: &BTreeMap<Id, WriterHistory>,
    candidates: &[(usize, Facts)],
    frozen: &BTreeSet<Id>,
) -> Result<Gates> {
    let closing = |boundary: Boundary| match boundary {
        Boundary::Closed(head) => Ok(head),
        // admit_receipts refuses legacy replacements; a durable lineage
        // without a signed closing head would have no retained prefix.
        Boundary::Unbounded => Err(error("unbounded writer replacement in durable authority")),
    };
    let history = |incarnation: &Id| {
        histories
            .get(incarnation)
            .ok_or_else(|| error("authenticated writer missing from authority"))
    };
    let by_digest: BTreeMap<_, _> = candidates.iter().map(|(_, f)| (f.digest, f)).collect();
    let mut chains = BTreeMap::new();
    let mut incarnations = BTreeSet::new();
    for (_, facts) in candidates {
        if chains.contains_key(&facts.incarnation) {
            continue;
        }
        // Admission requires an admitted predecessor, so this walk ends.
        let mut chain = Vec::new();
        let mut cursor = facts.incarnation;
        incarnations.insert(cursor);
        while let Some((previous, boundary)) = history(&cursor)?.predecessor {
            chain.push((previous, closing(boundary)?));
            incarnations.insert(previous);
            cursor = previous;
        }
        chains.insert(facts.incarnation, chain);
    }
    let mut frozen_devices = BTreeSet::new();
    let mut prefixes = BTreeMap::new();
    for incarnation in incarnations {
        let history = history(&incarnation)?;
        if history.frozen || frozen.contains(&incarnation) {
            frozen_devices.insert(history.device);
        }
        let Some(boundary) = history.closed else {
            continue;
        };
        // Walk the signed content chain back from the closing head. Until the
        // whole prefix is present, the retired writer's own records still
        // apply in chain order: none can pass an absent closing head, and a
        // record outside the prefix freezes the device once it is complete.
        let mut cursor = closing(boundary)?;
        let mut prefix = BTreeSet::new();
        let mut complete = true;
        while let Some(digest) = cursor {
            let Some(f) = by_digest.get(&digest) else {
                complete = false;
                break;
            };
            if f.incarnation != incarnation || !prefix.insert(digest) {
                frozen_devices.insert(history.device);
                complete = false;
                break;
            }
            cursor = f.previous;
        }
        if !complete {
            continue;
        }
        if candidates
            .iter()
            .any(|(_, f)| f.incarnation == incarnation && !prefix.contains(&f.digest))
        {
            // A signed handoff excludes signed history already observed.
            // Preserve all bytes and previous application; don't pick a branch.
            frozen_devices.insert(history.device);
        }
        prefixes.insert(incarnation, prefix);
    }
    Ok(Gates {
        frozen_devices,
        chains,
        prefixes,
    })
}
