//! Matters' database as a sync client's local store (#39).

use matters_sync::authority::{self, Hash, Route};
use matters_sync::store::{ControlOutcome, LocalStore, Log, StoreResult};
use matters_sync::wire::SIGNATURE_BYTES;
use rusqlite::{params, OptionalExtension};

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

    /// The relay's storage generation changed: forget the cursors and
    /// queue every sealed record and published control for upload again.
    /// The relay deduplicates what it still holds.
    pub fn sync_generation_changed(&self) -> Result<()> {
        self.with_sealing(|conn, _, _| {
            let tx = conn.transaction()?;
            tx.execute("DELETE FROM sync_cursors", [])?;
            tx.execute("UPDATE sync_records SET acked = 0", [])?;
            tx.execute("UPDATE sync_publish SET acked = 0", [])?;
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

    fn outgoing_records(&self) -> StoreResult<Vec<([u8; 32], Vec<u8>)>> {
        Ok(Database::outgoing_records(self)?
            .into_iter()
            .map(|r| (r.record_id, r.bytes))
            .collect())
    }

    fn acknowledge_record(&self, record_id: &[u8; 32]) -> StoreResult<()> {
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

    fn cursor(&self, log: Log) -> StoreResult<Option<Vec<u8>>> {
        Ok(self.sync_cursor(log)?)
    }

    fn save_cursor(&self, log: Log, cursor: &[u8]) -> StoreResult<()> {
        Ok(self.save_sync_cursor(log, cursor)?)
    }

    fn generation_changed(&self) -> StoreResult<()> {
        Ok(self.sync_generation_changed()?)
    }
}
