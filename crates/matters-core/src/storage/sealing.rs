//! Sealing recorded writes into authenticated records (#36, ADR 0002).
//!
//! With sync on and a sealing identity set, every journalled local update is
//! sealed, signed and stored as a content record inside the write's own
//! transaction, before its single commit. Each record draws a fresh random
//! record ID, so the key it derives is used for exactly one sealing. A crash
//! before the commit leaves no record and no ciphertext outside the process;
//! the retried write draws a new ID. Nothing is ever left to seal later, so
//! no edit can be stranded under a retired writer.
//!
//! Each open that seals is a new writer incarnation with its own record
//! chain: a record's `previous` is the SHA-384 of the incarnation's previous
//! record. Keys live only in memory; the database stores none.

use rusqlite::{params, OptionalExtension, Transaction};

use super::db::Database;
use crate::error::{MattersError, Result};
use matters_sync::{wire, ContentKey, SigningKey};

/// What this device seals as: the route, epoch and membership it writes
/// under, the epoch's content key and its own signing key. Supplied by the
/// caller on each open (enrollment, #37); never stored.
pub struct SealingIdentity {
    pub application: [u8; 16],
    pub tenant: [u8; 16],
    pub collection: [u8; 16],
    pub document: [u8; 16],
    pub epoch: u64,
    pub manifest: [u8; 48],
    pub author: [u8; 48],
    pub content_key: ContentKey,
    pub signing_key: SigningKey,
}

impl SealingIdentity {
    /// The route a receiver expects these records under.
    pub fn context(&self) -> wire::Context<'_> {
        wire::Context {
            application: &self.application,
            tenant: &self.tenant,
            collection: &self.collection,
            document: &self.document,
        }
    }
}

/// This open's sealing writer and collection membership.
#[derive(Default)]
pub(crate) struct SyncKeys {
    pub(crate) sealing: Option<Sealing>,
    pub(crate) membership: Option<super::membership::Joined>,
    /// This device has joined a collection and is active in it: every
    /// recorded write must seal, so writes wait for its keys.
    pub(crate) required: bool,
}

/// The identity and this open's writer incarnation.
pub(crate) struct Sealing {
    identity: SealingIdentity,
    incarnation: [u8; 32],
}

/// A sealed record waiting for, or past, delivery.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SealedRecord {
    /// The journal row it seals.
    pub seq: i64,
    pub record_id: [u8; 32],
    pub bytes: Vec<u8>,
}

/// 32 bytes of fresh OS entropy.
pub(crate) fn random32() -> [u8; 32] {
    let (a, b) = (uuid::Uuid::new_v4(), uuid::Uuid::new_v4());
    let mut out = [0; 32];
    out[..16].copy_from_slice(a.as_bytes());
    out[16..].copy_from_slice(b.as_bytes());
    out
}

fn sealing_error(error: matters_sync::Error) -> MattersError {
    MattersError::Engine(format!("sealing: {error}"))
}

/// Seal journal row `seq` (local update `update`) as the next record of this
/// open's writer, store it and advance the writer's chain head, all in `tx`.
pub(crate) fn seal_update(
    tx: &Transaction<'_>,
    sealing: &Sealing,
    seq: i64,
    update: &[u8],
) -> Result<()> {
    let head: Option<Vec<u8>> = tx.query_row(
        "SELECT head FROM sync_writers WHERE incarnation = ?1",
        params![sealing.incarnation.as_slice()],
        |r| r.get(0),
    )?;
    let previous: Option<[u8; 48]> = head
        .map(|h| h.try_into())
        .transpose()
        .map_err(|_| MattersError::Engine("stored chain head".into()))?;
    let id = sealing.identity();
    let record_id = random32();
    let batch = random32();
    let header = wire::Header {
        context: id.context(),
        kind: wire::Kind::Data,
        epoch: id.epoch,
        manifest: &id.manifest,
        author: &id.author,
        incarnation: &sealing.incarnation,
        record_id: &record_id,
        logical_batch: &batch,
        previous: previous.as_ref(),
    };
    let bytes = matters_sync::seal(header, &id.content_key, update, &id.signing_key, random32())
        .map_err(sealing_error)?;
    // The UNIQUE record ID refuses a locally known collision: the write
    // then fails whole, and its retry draws another ID.
    tx.execute(
        "INSERT INTO sync_records (seq, record_id, incarnation, bytes) VALUES (?1, ?2, ?3, ?4)",
        params![
            seq,
            record_id.as_slice(),
            sealing.incarnation.as_slice(),
            bytes
        ],
    )?;
    tx.execute(
        "UPDATE sync_writers SET head = ?1 WHERE incarnation = ?2",
        params![
            matters_sync::record_hash(&bytes).as_slice(),
            sealing.incarnation.as_slice()
        ],
    )?;
    Ok(())
}

impl Sealing {
    fn identity(&self) -> &SealingIdentity {
        &self.identity
    }

    /// Start sealing as `identity` under a new writer incarnation.
    pub(crate) fn begin(conn: &rusqlite::Connection, identity: SealingIdentity) -> Result<Self> {
        let incarnation = random32();
        conn.execute(
            "INSERT INTO sync_writers (incarnation, opened_at) VALUES (?1, ?2)",
            params![incarnation.as_slice(), crate::clock::now_millis()],
        )?;
        Ok(Self {
            identity,
            incarnation,
        })
    }

    /// Continue this writer's chain under a new epoch's identity.
    pub(crate) fn rekey(&mut self, identity: SealingIdentity) {
        self.identity = identity;
    }
}

impl Database {
    /// Seal this open's writes as `identity`, as a new writer incarnation.
    /// Sync must be on. Each open that seals calls this again, so a copied
    /// or restored database never continues another copy's chain.
    #[cfg(feature = "test-support")]
    pub fn start_sealing(&self, identity: SealingIdentity) -> Result<()> {
        self.with_sealing(|conn, engine, keys| {
            if engine.is_none() {
                return Err(MattersError::Validation("sync is off".into()));
            }
            keys.sealing = Some(Sealing::begin(conn, identity)?);
            Ok(())
        })
    }

    /// This open's writer incarnation, while it seals.
    pub fn writer_incarnation(&self) -> Option<[u8; 32]> {
        self.with_sealing(|_, _, keys| Ok(keys.sealing.as_ref().map(|s| s.incarnation)))
            .unwrap_or(None)
    }

    /// Sealed records not yet acknowledged, oldest first: exactly the bytes
    /// first stored, however often they are asked for.
    pub fn outgoing_records(&self) -> Result<Vec<SealedRecord>> {
        self.with_conn(|conn| {
            let mut stmt = conn.prepare(
                "SELECT seq, record_id, bytes FROM sync_records WHERE acked = 0 ORDER BY seq",
            )?;
            let rows = stmt.query_map([], |r| {
                let id: Vec<u8> = r.get(1)?;
                Ok((r.get(0)?, id, r.get(2)?))
            })?;
            let mut records = Vec::new();
            for row in rows {
                let (seq, id, bytes) = row?;
                records.push(SealedRecord {
                    seq,
                    record_id: id
                        .try_into()
                        .map_err(|_| MattersError::Engine("stored record ID".into()))?,
                    bytes,
                });
            }
            Ok(records)
        })
    }

    /// Mark a record as durably received by the relay. Acknowledging again
    /// changes nothing; the record stays stored.
    pub fn acknowledge_record(&self, record_id: &[u8; 32]) -> Result<()> {
        self.write(
            crate::clock::now_millis(),
            |tx| {
                let known: Option<i64> = tx
                    .query_row(
                        "SELECT seq FROM sync_records WHERE record_id = ?1",
                        params![record_id.as_slice()],
                        |r| r.get(0),
                    )
                    .optional()?;
                if known.is_none() {
                    return Err(MattersError::NotFound {
                        entity: "record",
                        id: record_id.iter().map(|b| format!("{b:02x}")).collect(),
                    });
                }
                tx.execute(
                    "UPDATE sync_records SET acked = 1 WHERE record_id = ?1",
                    params![record_id.as_slice()],
                )?;
                Ok(())
            },
            |_, ()| Ok(Vec::new()),
        )
    }
}
