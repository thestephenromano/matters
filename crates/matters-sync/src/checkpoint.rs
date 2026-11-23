//! Checkpoints (#40, docs/SYNC_CHECKPOINTS.md, sync-checkpoint-v0.cddl): a
//! full-history engine snapshot, and the coverage statement that says what
//! it holds.
//!
//! A checkpoint travels as one or more `snapshot` records, sealed and signed
//! like data records. Each record's plaintext is one part:
//!
//! ```text
//! part     = ["matters-checkpoint-part", 0, id: hash48, index, chunk, coverage-bytes / null]
//! coverage = ["matters-checkpoint", 0, engine, schema, epoch, writers, version,
//!             frontiers, pending, cursor / null, payload: hash48, length, parts]
//! payload  = [snapshot: bstr, pending: [* record bytes]]
//! id       = SHA-384(coverage-bytes)
//! ```
//!
//! Part 0 carries the coverage. The payload is cut into chunks of exactly
//! [`CHUNK_BYTES`], the last one shorter, so one checkpoint has exactly one
//! encoding.
//!
//! This module checks structure, canonical encoding, the parts' agreement
//! with each other and with their records' headers, and the payload's
//! digest. It also compares one coverage statement with another. It never
//! looks inside the snapshot. Whether the snapshot's frontier matches the
//! coverage, and whether the covered heads are in the receiver's own
//! chains, are the engine's and the receiver's checks.
//!
//! A valid signature says who made a checkpoint. It does not prove the
//! checkpoint covers every acknowledged record.

use std::collections::BTreeMap;

use minicbor::{Decoder, Encoder};

use crate::authority::{Hash, Id};
use crate::record::record_hash;

/// Bytes of payload in every part but the last.
pub const CHUNK_BYTES: usize = 512 * 1024;
/// The most parts one checkpoint may have: a 256 MiB payload.
pub const MAX_PARTS: u64 = 512;
/// The largest coverage statement accepted, checked before decoding.
pub const MAX_COVERAGE_BYTES: usize = 256 * 1024;
/// The most writer chains one coverage statement may list.
pub const MAX_WRITERS: usize = 1024;
/// The most pending records one checkpoint may carry.
pub const MAX_PENDING: usize = 1024;
/// The largest encoded version vector or frontier accepted.
pub const MAX_VERSION_BYTES: usize = 64 * 1024;
const MAX_ENGINE_BYTES: usize = 32;
const FORMAT: &str = "matters-checkpoint";
const PART_FORMAT: &str = "matters-checkpoint-part";

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum Error {
    #[error("the checkpoint is malformed")]
    Format,
    #[error("the checkpoint is not canonically encoded")]
    NonCanonical,
    #[error("the checkpoint exceeds a size limit")]
    Limit,
    #[error("the checkpoint's parts disagree")]
    Inconsistent,
    #[error("the checkpoint's payload does not match its coverage")]
    Digest,
    #[error("the checkpoint's epoch is not its records' epoch")]
    Epoch,
    #[error("the checkpoint covers less than one already accepted")]
    Behind,
    #[error("the checkpoint names a different head at a known position")]
    Fork,
}

pub type Result<T> = std::result::Result<T, Error>;

impl From<minicbor::decode::Error> for Error {
    fn from(_: minicbor::decode::Error) -> Self {
        Self::Format
    }
}

impl<W> From<minicbor::encode::Error<W>> for Error {
    fn from(_: minicbor::encode::Error<W>) -> Self {
        Self::Format
    }
}

/// One writer chain as the checkpoint includes it: its first `records`
/// records, the last of which is `head`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Writer {
    pub author: Hash,
    pub incarnation: Id,
    pub records: u64,
    pub head: Hash,
}

/// Where the author had read the relay's record log up to when it made the
/// checkpoint: every record at or before `sequence` in `generation` is
/// covered or carried pending.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Cursor {
    pub generation: [u8; 16],
    pub sequence: u64,
}

impl Cursor {
    /// The relay cursor that reads on from this one: `CBOR([generation,
    /// sequence])` (sync-relay-v0.cddl).
    pub fn relay_cursor(&self) -> Vec<u8> {
        let mut e = Encoder::new(Vec::new());
        e.array(2)
            .and_then(|e| e.bytes(&self.generation))
            .and_then(|e| e.u64(self.sequence))
            .expect("writing to a Vec cannot fail");
        e.into_writer()
    }

    /// The cursor a relay cursor names, if it is exactly `CBOR([generation,
    /// sequence])`.
    pub fn from_relay_cursor(bytes: &[u8]) -> Option<Self> {
        let mut d = Decoder::new(bytes);
        if d.array().ok()? != Some(2) {
            return None;
        }
        let generation = d.bytes().ok()?.try_into().ok()?;
        let sequence = d.u64().ok()?;
        (d.position() == bytes.len()).then_some(Self {
            generation,
            sequence,
        })
    }
}

/// What a checkpoint holds. Its canonical encoding's SHA-384 is the
/// checkpoint's ID.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Coverage {
    /// The snapshot's format, such as `loro-1`.
    pub engine: String,
    /// The domain schema the snapshot's document follows.
    pub schema: u64,
    /// The content-key epoch every part is sealed under.
    pub epoch: u64,
    /// Strictly ascending by incarnation.
    pub writers: Vec<Writer>,
    /// The engine's encoded version vector of everything included.
    pub version: Vec<u8>,
    /// The engine's encoded frontier of everything included.
    pub frontiers: Vec<u8>,
    /// Record hashes of the pending records carried, strictly ascending.
    pub pending: Vec<Hash>,
    pub cursor: Option<Cursor>,
    /// SHA-384 of the encoded payload.
    pub payload: Hash,
    pub length: u64,
    pub parts: u64,
}

/// A complete checkpoint: its coverage, the snapshot and the pending
/// records it carries, in coverage order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Checkpoint {
    pub coverage: Coverage,
    pub snapshot: Vec<u8>,
    pub pending: Vec<Vec<u8>>,
}

/// What a checkpoint states before its payload is fixed.
#[derive(Clone, Debug)]
pub struct Statement {
    pub engine: String,
    pub schema: u64,
    pub epoch: u64,
    pub writers: Vec<Writer>,
    pub version: Vec<u8>,
    pub frontiers: Vec<u8>,
    pub cursor: Option<Cursor>,
}

impl Checkpoint {
    /// A checkpoint of `snapshot`, carrying `pending` records (complete
    /// signed record bytes). Writers and pending records are put in
    /// canonical order.
    pub fn new(statement: Statement, snapshot: Vec<u8>, pending: Vec<Vec<u8>>) -> Result<Self> {
        let mut writers = statement.writers;
        writers.sort_by_key(|w| w.incarnation);
        let mut pending: Vec<(Hash, Vec<u8>)> =
            pending.into_iter().map(|p| (record_hash(&p), p)).collect();
        pending.sort_by_key(|p| p.0);
        pending.dedup_by(|a, b| a.0 == b.0);
        let (digests, pending): (Vec<Hash>, Vec<Vec<u8>>) = pending.into_iter().unzip();
        let payload = encode_payload(&snapshot, &pending)?;
        let coverage = Coverage {
            engine: statement.engine,
            schema: statement.schema,
            epoch: statement.epoch,
            writers,
            version: statement.version,
            frontiers: statement.frontiers,
            pending: digests,
            cursor: statement.cursor,
            payload: libcrux_sha2::sha384(&payload),
            length: payload.len() as u64,
            parts: (payload.len().max(1)).div_ceil(CHUNK_BYTES) as u64,
        };
        coverage.check()?;
        Ok(Self {
            coverage,
            snapshot,
            pending,
        })
    }

    pub fn id(&self) -> Result<Hash> {
        self.coverage.id()
    }

    /// The plaintext of each part, in index order: one `snapshot` record
    /// each.
    pub fn parts(&self) -> Result<Vec<Vec<u8>>> {
        let coverage = self.coverage.encode()?;
        let id = libcrux_sha2::sha384(&coverage);
        let payload = encode_payload(&self.snapshot, &self.pending)?;
        let mut parts = Vec::new();
        for (index, chunk) in payload.chunks(CHUNK_BYTES).enumerate() {
            let mut e = Encoder::new(Vec::new());
            e.array(6)?
                .str(PART_FORMAT)?
                .u8(0)?
                .bytes(&id)?
                .u64(index as u64)?
                .bytes(chunk)?;
            if index == 0 {
                e.bytes(&coverage)?;
            } else {
                e.null()?;
            }
            parts.push(e.into_writer());
        }
        Ok(parts)
    }
}

fn encode_payload(snapshot: &[u8], pending: &[Vec<u8>]) -> Result<Vec<u8>> {
    if snapshot.is_empty() {
        return Err(Error::Format);
    }
    if pending.len() > MAX_PENDING {
        return Err(Error::Limit);
    }
    let mut e = Encoder::new(Vec::new());
    e.array(2)?.bytes(snapshot)?.array(pending.len() as u64)?;
    for p in pending {
        e.bytes(p)?;
    }
    let bytes = e.into_writer();
    if bytes.len() as u64 > MAX_PARTS * CHUNK_BYTES as u64 {
        return Err(Error::Limit);
    }
    Ok(bytes)
}

impl Coverage {
    pub fn encode(&self) -> Result<Vec<u8>> {
        let mut e = Encoder::new(Vec::new());
        e.array(13)?
            .str(FORMAT)?
            .u8(0)?
            .str(&self.engine)?
            .u64(self.schema)?
            .u64(self.epoch)?
            .array(self.writers.len() as u64)?;
        for w in &self.writers {
            e.array(4)?
                .bytes(&w.author)?
                .bytes(&w.incarnation)?
                .u64(w.records)?
                .bytes(&w.head)?;
        }
        e.bytes(&self.version)?
            .bytes(&self.frontiers)?
            .array(self.pending.len() as u64)?;
        for p in &self.pending {
            e.bytes(p)?;
        }
        match &self.cursor {
            Some(c) => {
                e.array(2)?.bytes(&c.generation)?.u64(c.sequence)?;
            }
            None => {
                e.null()?;
            }
        }
        e.bytes(&self.payload)?.u64(self.length)?.u64(self.parts)?;
        let bytes = e.into_writer();
        if bytes.len() > MAX_COVERAGE_BYTES {
            return Err(Error::Limit);
        }
        Ok(bytes)
    }

    pub fn id(&self) -> Result<Hash> {
        Ok(libcrux_sha2::sha384(&self.encode()?))
    }

    /// Parse a coverage statement: structure, limits, order and canonical
    /// bytes.
    pub fn parse(bytes: &[u8]) -> Result<Self> {
        if bytes.len() > MAX_COVERAGE_BYTES {
            return Err(Error::Limit);
        }
        let mut d = Decoder::new(bytes);
        array(&mut d, 13)?;
        if d.str()? != FORMAT {
            return Err(Error::Format);
        }
        if d.u64()? != 0 {
            return Err(Error::Format);
        }
        let engine = d.str()?.to_owned();
        let schema = d.u64()?;
        let epoch = d.u64()?;
        let n = d.array()?.ok_or(Error::Format)?;
        if n > MAX_WRITERS as u64 {
            return Err(Error::Limit);
        }
        let mut writers = Vec::with_capacity(n as usize);
        for _ in 0..n {
            array(&mut d, 4)?;
            writers.push(Writer {
                author: fixed(&mut d)?,
                incarnation: fixed(&mut d)?,
                records: d.u64()?,
                head: fixed(&mut d)?,
            });
        }
        let version = d.bytes()?.to_vec();
        let frontiers = d.bytes()?.to_vec();
        let n = d.array()?.ok_or(Error::Format)?;
        if n > MAX_PENDING as u64 {
            return Err(Error::Limit);
        }
        let mut pending = Vec::with_capacity(n as usize);
        for _ in 0..n {
            pending.push(fixed(&mut d)?);
        }
        let cursor = if d.datatype()? == minicbor::data::Type::Null {
            d.null()?;
            None
        } else {
            array(&mut d, 2)?;
            Some(Cursor {
                generation: fixed(&mut d)?,
                sequence: d.u64()?,
            })
        };
        let coverage = Self {
            engine,
            schema,
            epoch,
            writers,
            version,
            frontiers,
            pending,
            cursor,
            payload: fixed(&mut d)?,
            length: d.u64()?,
            parts: d.u64()?,
        };
        if d.position() != bytes.len() {
            return Err(Error::Format);
        }
        coverage.check()?;
        if coverage.encode()? != bytes {
            return Err(Error::NonCanonical);
        }
        Ok(coverage)
    }

    /// The rules an encoder and a parser both hold a statement to.
    fn check(&self) -> Result<()> {
        if self.engine.is_empty() || self.engine.len() > MAX_ENGINE_BYTES {
            return Err(Error::Format);
        }
        if self.writers.len() > MAX_WRITERS || self.pending.len() > MAX_PENDING {
            return Err(Error::Limit);
        }
        if self.version.len() > MAX_VERSION_BYTES || self.frontiers.len() > MAX_VERSION_BYTES {
            return Err(Error::Limit);
        }
        if self.writers.iter().any(|w| w.records == 0)
            || self
                .writers
                .windows(2)
                .any(|w| w[0].incarnation >= w[1].incarnation)
            || self.pending.windows(2).any(|p| p[0] >= p[1])
        {
            return Err(Error::Format);
        }
        if self.length == 0
            || self.parts == 0
            || self.parts > MAX_PARTS
            || self.parts != self.length.div_ceil(CHUNK_BYTES as u64)
        {
            return Err(Error::Format);
        }
        Ok(())
    }

    pub fn writer(&self, incarnation: &Id) -> Option<&Writer> {
        self.writers
            .binary_search_by(|w| w.incarnation.cmp(incarnation))
            .ok()
            .map(|i| &self.writers[i])
    }

    /// Whether this statement covers at least what `known`, an accepted
    /// one, covers: the same or a later epoch; every writer `known` lists,
    /// with at least as many records, and the same head where the count is
    /// the same; and, in the same relay generation, a cursor no earlier.
    /// A relay that offers an older checkpoint in place of a newer one is
    /// refused as [`Error::Behind`].
    pub fn follows(&self, known: &Coverage) -> Result<()> {
        if self.epoch < known.epoch {
            return Err(Error::Behind);
        }
        for k in &known.writers {
            let w = self.writer(&k.incarnation).ok_or(Error::Behind)?;
            if w.author != k.author {
                return Err(Error::Fork);
            }
            if w.records < k.records {
                return Err(Error::Behind);
            }
            if w.records == k.records && w.head != k.head {
                return Err(Error::Fork);
            }
        }
        if let (Some(c), Some(k)) = (&self.cursor, &known.cursor) {
            if c.generation == k.generation && c.sequence < k.sequence {
                return Err(Error::Behind);
            }
        }
        Ok(())
    }
}

/// One part, parsed: which checkpoint it belongs to and where.
#[derive(Debug)]
pub struct Part<'a> {
    pub id: Hash,
    pub index: u64,
    pub chunk: &'a [u8],
    pub coverage: Option<&'a [u8]>,
}

/// Parse one part's plaintext: structure and canonical bytes only.
pub fn parse_part(bytes: &[u8]) -> Result<Part<'_>> {
    let mut d = Decoder::new(bytes);
    array(&mut d, 6)?;
    if d.str()? != PART_FORMAT || d.u64()? != 0 {
        return Err(Error::Format);
    }
    let id = fixed(&mut d)?;
    let index = d.u64()?;
    let chunk = d.bytes()?;
    let coverage = if d.datatype()? == minicbor::data::Type::Null {
        d.null()?;
        None
    } else {
        Some(d.bytes()?)
    };
    if d.position() != bytes.len() {
        return Err(Error::Format);
    }
    if index >= MAX_PARTS || chunk.is_empty() || chunk.len() > CHUNK_BYTES {
        return Err(Error::Format);
    }
    if coverage.is_some() != (index == 0) {
        return Err(Error::Format);
    }
    // Re-encoding must reproduce the input.
    let mut e = Encoder::new(Vec::with_capacity(bytes.len()));
    e.array(6)?
        .str(PART_FORMAT)?
        .u8(0)?
        .bytes(&id)?
        .u64(index)?
        .bytes(chunk)?;
    match coverage {
        Some(c) => e.bytes(c)?,
        None => e.null()?,
    };
    if e.into_writer() != bytes {
        return Err(Error::NonCanonical);
    }
    Ok(Part {
        id,
        index,
        chunk,
        coverage,
    })
}

/// What the verified record carrying a part says about it, from its
/// authenticated header.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Sealed {
    pub author: Hash,
    pub incarnation: Id,
    pub epoch: u64,
}

/// How far a set of parts goes.
#[derive(Debug, PartialEq, Eq)]
pub enum Assembled {
    /// Not all parts are here yet. `missing` is known once part 0 is.
    Incomplete {
        missing: Option<u64>,
    },
    Complete(Box<Checkpoint>),
}

/// Put one checkpoint together from the parts received so far, each with
/// the header facts of the record that carried it. Every part must name
/// the same checkpoint, come from the same writer under the same epoch, and
/// agree on chunking; the coverage must hash to the ID and name that
/// epoch; the payload must hash to the coverage's digest and carry exactly
/// the pending records it lists.
pub fn assemble(parts: &[(Sealed, &[u8])]) -> Result<Assembled> {
    let Some((first, _)) = parts.first() else {
        return Ok(Assembled::Incomplete { missing: None });
    };
    let mut id = None;
    let mut coverage = None;
    let mut chunks: BTreeMap<u64, &[u8]> = BTreeMap::new();
    for (sealed, bytes) in parts {
        if sealed != first {
            return Err(Error::Inconsistent);
        }
        let part = parse_part(bytes)?;
        if *id.get_or_insert(part.id) != part.id {
            return Err(Error::Inconsistent);
        }
        if let Some(c) = part.coverage {
            if libcrux_sha2::sha384(c) != part.id {
                return Err(Error::Digest);
            }
            coverage = Some(Coverage::parse(c)?);
        }
        if let Some(old) = chunks.insert(part.index, part.chunk) {
            if old != part.chunk {
                return Err(Error::Inconsistent);
            }
        }
    }
    let Some(coverage) = coverage else {
        return Ok(Assembled::Incomplete { missing: None });
    };
    if coverage.epoch != first.epoch {
        return Err(Error::Epoch);
    }
    let last = coverage.parts - 1;
    let last_len = coverage.length - last * CHUNK_BYTES as u64;
    for (&index, chunk) in &chunks {
        let expected = if index == last {
            last_len
        } else {
            CHUNK_BYTES as u64
        };
        if index > last || chunk.len() as u64 != expected {
            return Err(Error::Inconsistent);
        }
    }
    let have = chunks.len() as u64;
    if have < coverage.parts {
        return Ok(Assembled::Incomplete {
            missing: Some(coverage.parts - have),
        });
    }
    let payload: Vec<u8> = chunks.values().flat_map(|c| c.iter().copied()).collect();
    if libcrux_sha2::sha384(&payload) != coverage.payload {
        return Err(Error::Digest);
    }
    let (snapshot, pending) = decode_payload(&payload)?;
    let digests: Vec<Hash> = pending.iter().map(|p| record_hash(p)).collect();
    if digests != coverage.pending {
        return Err(Error::Digest);
    }
    Ok(Assembled::Complete(Box::new(Checkpoint {
        coverage,
        snapshot,
        pending,
    })))
}

fn decode_payload(bytes: &[u8]) -> Result<(Vec<u8>, Vec<Vec<u8>>)> {
    let mut d = Decoder::new(bytes);
    array(&mut d, 2)?;
    let snapshot = d.bytes()?.to_vec();
    let n = d.array()?.ok_or(Error::Format)?;
    if n > MAX_PENDING as u64 {
        return Err(Error::Limit);
    }
    let mut pending = Vec::with_capacity(n as usize);
    for _ in 0..n {
        pending.push(d.bytes()?.to_vec());
    }
    if d.position() != bytes.len() {
        return Err(Error::Format);
    }
    if encode_payload(&snapshot, &pending)? != bytes {
        return Err(Error::NonCanonical);
    }
    Ok((snapshot, pending))
}

fn array(d: &mut Decoder<'_>, n: u64) -> Result<()> {
    if d.array()? != Some(n) {
        return Err(Error::Format);
    }
    Ok(())
}

fn fixed<const N: usize>(d: &mut Decoder<'_>) -> Result<[u8; N]> {
    d.bytes()?.try_into().map_err(|_| Error::Format)
}
