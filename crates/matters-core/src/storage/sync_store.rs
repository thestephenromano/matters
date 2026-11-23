//! Matters' database as a sync client's local store (#39).

use matters_sync::authority::{self, Hash, Route};
use matters_sync::checkpoint::Cursor;
use matters_sync::store::{
    ControlOutcome, JoiningStore, LocalStore, Log, RecoveringStore, StoreResult,
};
use matters_sync::wire::SIGNATURE_BYTES;
use rusqlite::{params, OptionalExtension};
use zeroize::Zeroizing;

use super::db::Database;
use super::membership::SyncStanding;
use super::sealing::random32;
use crate::error::{MattersError, Result};

fn log_name(log: Log) -> &'static str {
    match log {
        Log::Authority => "authority",
        Log::Records => "records",
    }
}

fn not_resumed() -> MattersError {
    MattersError::Validation("sync is not resumed".into())
}

impl Database {
    /// The route and device this database syncs as, while resumed.
    pub fn sync_identity(&self) -> Result<(Route, Hash)> {
        self.with_sealing(|_, _, sync| {
            let joined = sync.membership.as_ref().ok_or_else(not_resumed)?;
            Ok((joined.route.clone(), joined.device.id()))
        })
    }

    /// Sign a relay session challenge with this device's enrolled key.
    pub fn sign_session(&self, challenge: &[u8; 32]) -> Result<[u8; SIGNATURE_BYTES]> {
        self.with_sealing(|_, _, sync| {
            let joined = sync.membership.as_ref().ok_or_else(not_resumed)?;
            Ok(authority::sign_session(
                &joined.device,
                &joined.route,
                challenge,
                random32(),
            )?)
        })
    }

    /// Where the sync client has read `log` up to.
    pub fn sync_cursor(&self, log: Log) -> Result<Option<Vec<u8>>> {
        self.with_conn(|conn| {
            Ok(conn
                .query_row(
                    "SELECT cursor FROM sync_cursors WHERE log = ?1",
                    params![log_name(log)],
                    |r| r.get(0),
                )
                .optional()?)
        })
    }

    pub fn save_sync_cursor(&self, log: Log, cursor: &[u8]) -> Result<()> {
        self.with_sealing(|conn, _, _| {
            conn.execute(
                "INSERT INTO sync_cursors (log, cursor) VALUES (?1, ?2)
                 ON CONFLICT (log) DO UPDATE SET cursor = excluded.cursor",
                params![log_name(log), cursor],
            )?;
            Ok(())
        })
    }

    /// The relay's storage generation changed: forget the cursors, and
    /// where in the old log kept records were read, and queue every sealed
    /// record, checkpoint part and published control for upload again.
    /// The relay deduplicates what it still holds.
    pub fn sync_generation_changed(&self) -> Result<()> {
        self.with_sealing(|conn, _, _| {
            let tx = conn.transaction()?;
            tx.execute("DELETE FROM sync_cursors", [])?;
            tx.execute("DELETE FROM sync_rewind", [])?;
            tx.execute("UPDATE sync_pending SET origin = NULL", [])?;
            tx.execute("UPDATE sync_records SET acked = 0", [])?;
            tx.execute("UPDATE sync_publish SET acked = 0", [])?;
            tx.execute("UPDATE sync_checkpoint_out SET acked = 0", [])?;
            tx.commit()?;
            Ok(())
        })
    }
}

impl LocalStore for Database {
    fn route(&self) -> StoreResult<Route> {
        Ok(self.sync_identity()?.0)
    }

    fn device(&self) -> StoreResult<Hash> {
        Ok(self.sync_identity()?.1)
    }

    fn sign_session(&self, challenge: &[u8; 32]) -> StoreResult<[u8; SIGNATURE_BYTES]> {
        Ok(Database::sign_session(self, challenge)?)
    }

    fn outgoing_controls(&self) -> StoreResult<Vec<(i64, Vec<u8>)>> {
        Ok(Database::outgoing_controls(self)?
            .into_iter()
            .map(|o| (o.seq, o.bytes))
            .collect())
    }

    fn acknowledge_control(&self, seq: i64) -> StoreResult<()> {
        Ok(Database::acknowledge_control(self, seq)?)
    }

    fn known_controls(&self) -> StoreResult<Vec<Vec<u8>>> {
        Ok(self.sync_controls()?)
    }

    fn is_current_member(&self) -> StoreResult<bool> {
        Ok(self.sync_membership()?.standing == SyncStanding::Active)
    }

    /// Sealed records, then checkpoint parts.
    fn outgoing_records(&self) -> StoreResult<Vec<([u8; 32], Vec<u8>)>> {
        let mut out: Vec<([u8; 32], Vec<u8>)> = Database::outgoing_records(self)?
            .into_iter()
            .map(|r| (r.record_id, r.bytes))
            .collect();
        out.extend(self.outgoing_checkpoint_parts()?);
        Ok(out)
    }

    fn acknowledge_record(&self, record_id: &[u8; 32]) -> StoreResult<()> {
        if self.acknowledge_checkpoint_part(record_id)? {
            return Ok(());
        }
        Ok(Database::acknowledge_record(self, record_id)?)
    }

    fn receive_control(
        &self,
        control: &[u8],
        envelope: Option<&[u8]>,
    ) -> StoreResult<ControlOutcome> {
        match self.apply_control(control, envelope) {
            Ok(SyncStanding::Active) => Ok(ControlOutcome::Done),
            Ok(_) => Ok(ControlOutcome::Stopped),
            Err(MattersError::EnvelopeMissing) => Ok(ControlOutcome::NeedsEnvelope),
            Err(MattersError::Authority(authority::Error::Fork)) => Ok(ControlOutcome::Stopped),
            // A genesis replayed from the log, or a control of a kind this
            // device does not apply.
            Err(MattersError::Authority(authority::Error::Unsupported)) => Ok(ControlOutcome::Done),
            // Forged, malformed, or made stale by a later transition.
            Err(MattersError::Authority(_)) => Ok(ControlOutcome::Refused),
            Err(error) => Err(error.into()),
        }
    }

    fn receive_record(&self, record: &[u8]) -> StoreResult<()> {
        Database::receive_record(self, record)?;
        Ok(())
    }

    fn receive_records(&self, records: &[Vec<u8>]) -> StoreResult<Vec<StoreResult<()>>> {
        let borrowed: Vec<&[u8]> = records.iter().map(Vec::as_slice).collect();
        Ok(Database::receive_records(self, &borrowed)?
            .into_iter()
            .map(|outcome| outcome.map(|_| ()).map_err(Into::into))
            .collect())
    }

    fn receive_page(
        &self,
        generation: [u8; 16],
        records: &[(u64, Vec<u8>)],
    ) -> StoreResult<Vec<StoreResult<()>>> {
        let borrowed: Vec<(u64, &[u8])> = records.iter().map(|(s, b)| (*s, b.as_slice())).collect();
        Ok(Database::receive_page(self, generation, &borrowed)?
            .into_iter()
            .map(|outcome| outcome.map(|_| ()).map_err(Into::into))
            .collect())
    }

    fn rewind_records(&self) -> StoreResult<()> {
        Database::rewind_records(self)?;
        Ok(())
    }

    fn cursor(&self, log: Log) -> StoreResult<Option<Vec<u8>>> {
        Ok(self.sync_cursor(log)?)
    }

    fn save_cursor(&self, log: Log, cursor: &[u8]) -> StoreResult<()> {
        Ok(self.save_sync_cursor(log, cursor)?)
    }

    fn generation_changed(&self) -> StoreResult<()> {
        Ok(self.sync_generation_changed()?)
    }

    fn wants_checkpoint(&self) -> StoreResult<bool> {
        Ok(self.holds_nothing_synced()?)
    }

    fn start_from_checkpoint(&self, parts: &[Vec<u8>], generation: [u8; 16]) -> StoreResult<bool> {
        Ok(Database::start_from_checkpoint(self, parts, generation)?)
    }

    fn make_checkpoint(&self, at: Cursor, every: u64) -> StoreResult<bool> {
        Ok(self.checkpoint_due(every)? && self.create_checkpoint(Some(at))?.is_some())
    }
}

/// A database joining a collection, with the key store its keys are in,
/// as a sync client's joining store (#42).
pub struct Enrolling<K> {
    pub db: std::sync::Arc<Database>,
    pub keys: K,
}

impl<K: super::membership::KeyStore + Send + Sync> JoiningStore for Enrolling<K> {
    fn route(&self) -> StoreResult<Route> {
        Ok(self.db.enrolling()?.ok_or_else(not_enrolling)?.0)
    }

    fn device(&self) -> StoreResult<Hash> {
        Ok(self.db.enrolling()?.ok_or_else(not_enrolling)?.1)
    }

    fn sign_session(&self, challenge: &[u8; 32]) -> StoreResult<[u8; SIGNATURE_BYTES]> {
        Ok(self.db.sign_enrolling_session(&self.keys, challenge)?)
    }

    fn finish(&self, controls: &[Vec<u8>], envelopes: &[Vec<u8>]) -> StoreResult<bool> {
        match self.db.finish_enrollment(&self.keys, controls, envelopes) {
            Ok(_) => Ok(true),
            Err(MattersError::Validation(m))
                if m.contains("has not added this device")
                    || m.contains("no key envelope for the current epoch") =>
            {
                Ok(false)
            }
            Err(error) => Err(error.into()),
        }
    }
}

fn not_enrolling() -> MattersError {
    MattersError::Validation("this device is not joining a collection".into())
}

/// A database recovering a collection from its recovery words, as a sync
/// client's recovering store (#42). The words are held only as long as this
/// value lives, and wiped from memory when it is dropped.
pub struct Recovering<K> {
    pub db: std::sync::Arc<Database>,
    pub keys: K,
    pub words: Zeroizing<String>,
    pub route: Route,
}

impl<K> Recovering<K> {
    fn recovery_keys(&self) -> Result<authority::RecoveryKeys> {
        Ok(authority::RecoverySecret::from_words(&self.words)?.keys(&self.route)?)
    }
}

impl<K: super::membership::KeyStore + Send + Sync> RecoveringStore for Recovering<K> {
    fn route(&self) -> StoreResult<Route> {
        Ok(self.route.clone())
    }

    fn root(&self) -> StoreResult<Hash> {
        Ok(self.recovery_keys()?.root_id()?)
    }

    fn sign_root_session(&self, challenge: &[u8; 32]) -> StoreResult<[u8; SIGNATURE_BYTES]> {
        Ok(authority::sign_root_session(
            &self.recovery_keys()?,
            &self.route,
            challenge,
            random32(),
        )?)
    }

    fn recover(
        &self,
        controls: &[Vec<u8>],
        envelopes: &[Vec<u8>],
        records: &[Vec<u8>],
    ) -> StoreResult<()> {
        // Only the envelopes addressed to the recovery key open with it.
        let recipient = self.recovery_keys()?.recipient()?;
        let recovery: Vec<Vec<u8>> = envelopes
            .iter()
            .filter(|e| {
                matches!(authority::parse(e).map(|p| p.message.body),
                    Ok(authority::Body::KeyEnvelope(k)) if k.recipient == recipient)
            })
            .cloned()
            .collect();
        // With the relay's records, the recovery closes every lost writer
        // at what the relay holds of it (#45).
        self.db.recover_collection_holding(
            &self.keys,
            &self.words,
            self.route.clone(),
            controls,
            &recovery,
            records,
        )?;
        Ok(())
    }

    fn outgoing(&self) -> StoreResult<Vec<(i64, Vec<u8>)>> {
        Ok(self
            .db
            .outgoing_controls()?
            .into_iter()
            .map(|o| (o.seq, o.bytes))
            .collect())
    }

    fn acknowledge(&self, seq: i64) -> StoreResult<()> {
        Ok(self.db.acknowledge_control(seq)?)
    }
}
