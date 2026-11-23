//! Atomic batches of writes (#203).
//!
//! A batch runs many operations in one SQLite transaction and commits them
//! together or not at all. With sync on, each operation's engine commands
//! are applied as soon as its SQL has run, so the next operation's anchors
//! and order keys read the state the earlier ones left. At the end, the
//! batch journals and seals everything it changed as one update: one row
//! and one record, as an import does (`super::import`).
//!
//! Every write goes through here: a single operation is a batch of one, so
//! the keys guard, the write fence, the size cap, the journal row, sealing,
//! the crash points and the reload after a failure exist once.

use rusqlite::{params, Connection, OptionalExtension, Transaction};

use super::db::Database;
use crate::crdt::{Command, Document};
use crate::domain::{Area, ChecklistItem, Heading, Project};
use crate::error::{MattersError, Result};

/// The largest update one batch may record: what fits in one sync record.
/// A larger batch is refused whole with [`MattersError::BatchTooLarge`];
/// it is never split.
pub const MAX_BATCH_BYTES: usize = matters_sync::wire::MAX_PLAINTEXT_BYTES;

/// How long a request ID is remembered (`Database::batch_once`): a day,
/// far longer than any caller waits before retrying.
const REQUEST_MEMORY_MS: i64 = 24 * 60 * 60 * 1000;

/// The operations of one batch, in its transaction. Get one from
/// [`Database::batch`]; each operation method runs one change.
pub struct Writer<'a> {
    tx: &'a Transaction<'a>,
    engine: Option<&'a mut Document>,
    /// The engine was changed, or may have been: a failure reloads it.
    changed: bool,
    /// An operation returned an error. Whether it refused before running
    /// any SQL or failed partway, the batch never commits, even if the
    /// caller carried on: a dropped error is a caller's bug, and one rule
    /// is simpler to rely on than two.
    failed: bool,
}

impl<'a> Writer<'a> {
    /// The batch's transaction, to read the state earlier operations left.
    pub(crate) fn tx(&self) -> &'a Transaction<'a> {
        self.tx
    }

    /// Read in the batch's transaction, as `Database::with_conn` reads
    /// outside one.
    pub(crate) fn with_conn<T>(&self, f: impl FnOnce(&Connection) -> Result<T>) -> Result<T> {
        f(self.tx)
    }

    /// [`Database::list_projects`], in this batch.
    pub fn list_projects(&self) -> Result<Vec<Project>> {
        Database::open_projects_in(self.tx)
    }

    /// [`Database::list_areas`], in this batch.
    pub fn list_areas(&self) -> Result<Vec<Area>> {
        Database::live_areas_in(self.tx)
    }

    /// [`Database::get_heading`], in this batch.
    pub fn get_heading(&self, id: &str) -> Result<Option<Heading>> {
        Database::heading_in(self.tx, id)
    }

    /// [`Database::list_headings_for_project`], in this batch.
    pub fn list_headings_for_project(&self, project_id: &str) -> Result<Vec<Heading>> {
        Database::headings_of(self.tx, project_id)
    }

    /// [`Database::get_checklist_item`], in this batch.
    pub fn get_checklist_item(&self, id: &str) -> Result<Option<ChecklistItem>> {
        Database::checklist_item_in(self.tx, id)
    }

    /// [`Database::list_checklist_items`], in this batch.
    pub fn list_checklist_items(&self, task_id: &str) -> Result<Vec<ChecklistItem>> {
        Database::checklist_in(self.tx, task_id)
    }

    /// One operation: run `apply`, then, with sync on, apply to the engine
    /// at time `at` the commands `commands` derives from its result.
    pub(crate) fn write<T>(
        &mut self,
        at: i64,
        apply: impl FnOnce(&Transaction<'_>) -> Result<T>,
        commands: impl FnOnce(&Transaction<'_>, &T) -> Result<Vec<Command>>,
    ) -> Result<T> {
        let done = self.step(at, apply, commands);
        self.settle(done)
    }

    /// Every operation's result passes through here on its way out: an
    /// error fails the batch (`failed`).
    fn settle<T>(&mut self, done: Result<T>) -> Result<T> {
        self.failed |= done.is_err();
        done
    }

    /// Run one operation's body: whatever error it returns fails the
    /// batch, whether it refused before any SQL or failed partway.
    pub(crate) fn op<T>(&mut self, body: impl FnOnce(&mut Self) -> Result<T>) -> Result<T> {
        let done = body(self);
        self.settle(done)
    }

    fn step<T>(
        &mut self,
        at: i64,
        apply: impl FnOnce(&Transaction<'_>) -> Result<T>,
        commands: impl FnOnce(&Transaction<'_>, &T) -> Result<Vec<Command>>,
    ) -> Result<T> {
        let value = apply(self.tx)?;
        if let Some(doc) = self.engine.as_deref_mut() {
            let commands = commands(self.tx, &value)?;
            if !commands.is_empty() {
                // Set first: a refused command may leave the engine
                // half-changed, and it is rebuilt either way.
                self.changed = true;
                doc.apply(&commands, at)?;
            }
        }
        Ok(value)
    }
}

/// `Ok` when an update of `bytes` fits in one sync record: exactly the
/// plaintext `matters_sync::seal` accepts, so whatever passes here seals.
fn within_record(bytes: usize) -> Result<()> {
    if bytes > MAX_BATCH_BYTES {
        return Err(MattersError::BatchTooLarge {
            bytes,
            limit: MAX_BATCH_BYTES,
        });
    }
    Ok(())
}

impl Database {
    /// Run `operations` as one batch: in one transaction, recorded for
    /// sync as one update and one sealed record. If any operation fails,
    /// or the update is larger than [`MAX_BATCH_BYTES`], nothing changes
    /// and the error is returned.
    pub fn batch<T>(&self, operations: impl FnOnce(&mut Writer<'_>) -> Result<T>) -> Result<T> {
        self.with_sealing(|conn, engine, keys| {
            // A joined, active device seals every write; until its keys are
            // loaded nothing is written, so nothing is left to seal later.
            if keys.required && keys.sealing.is_none() {
                return Err(MattersError::Validation(
                    "sync keys are not loaded; resume sync first".into(),
                ));
            }
            let tx = conn.transaction()?;
            // Another open of this database took over the writer: refuse
            // before anything is written, so the caller can reload (#45).
            if let Some(sealing) = keys.sealing.as_ref() {
                super::sealing::fence(&tx, sealing)?;
            }
            let before = engine.as_ref().map(Document::version);
            let mut writer = Writer {
                tx: &tx,
                engine: engine.as_mut(),
                changed: false,
                failed: false,
            };
            let value = operations(&mut writer).and_then(|value| match writer.failed {
                true => Err(MattersError::Validation(
                    "an operation in this batch failed, so none of it was made".into(),
                )),
                false => Ok(value),
            });
            let changed = writer.changed;
            let committed = value.and_then(|value| {
                if let (true, Some(doc), Some(before)) = (changed, engine.as_ref(), &before) {
                    let update = doc.updates_since(before)?;
                    within_record(update.len())?;
                    tx.execute(
                        "INSERT INTO sync_journal (bytes, outgoing) VALUES (?1, 1)",
                        params![update],
                    )?;
                    // Sealed in this transaction: the record commits with
                    // the edits or not at all (#36).
                    if let Some(sealing) = keys.sealing.as_ref() {
                        super::sealing::seal_update(&tx, sealing, tx.last_insert_rowid(), &update)?;
                    }
                }
                super::sync::crash_point("before-commit");
                tx.commit()?;
                Ok(value)
            });
            match committed {
                Ok(value) => {
                    super::sync::crash_point("after-commit");
                    Ok(value)
                }
                Err(error) => {
                    // The transaction rolled back; so must the engine.
                    if changed {
                        *engine = super::sync::load_engine(conn)?;
                    }
                    Err(error)
                }
            }
        })
    }

    /// [`Self::batch`], at most once per request ID: a caller that may
    /// retry (a call that timed out after its batch committed) names the
    /// request with `request` and gives a digest of what it asks for.
    ///
    /// The first time, `operations` runs and its result is stored with the
    /// request, in the batch's transaction. A retry with the same ID and
    /// digest changes nothing and returns the stored result; the same ID
    /// with another digest is refused. IDs are remembered for a day, on
    /// this device only.
    pub fn batch_once(
        &self,
        request: &str,
        digest: &[u8; 32],
        operations: impl FnOnce(&mut Writer<'_>) -> Result<Vec<u8>>,
    ) -> Result<Vec<u8>> {
        let now = crate::clock::now_millis();
        self.batch(|w| {
            let tx = w.tx();
            tx.execute(
                "DELETE FROM batch_requests WHERE created_at < ?1",
                params![now - REQUEST_MEMORY_MS],
            )?;
            let seen: Option<(Vec<u8>, Vec<u8>)> = tx
                .query_row(
                    "SELECT digest, result FROM batch_requests WHERE id = ?1",
                    params![request],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .optional()?;
            match seen {
                Some((seen, result)) if seen == digest.as_slice() => Ok(result),
                Some(_) => Err(MattersError::Validation(format!(
                    "request '{request}' was already made with different changes"
                ))),
                None => {
                    let result = operations(w)?;
                    w.tx().execute(
                        "INSERT INTO batch_requests (id, digest, result, created_at)
                         VALUES (?1, ?2, ?3, ?4)",
                        params![request, digest.as_slice(), result, now],
                    )?;
                    Ok(result)
                }
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The cap is the seal's own boundary: an update of exactly
    /// `MAX_PLAINTEXT_BYTES` seals, so it must pass, and one byte more
    /// would not.
    #[test]
    fn the_cap_is_exactly_what_one_record_holds() {
        assert!(within_record(MAX_BATCH_BYTES).is_ok());
        assert!(matches!(
            within_record(MAX_BATCH_BYTES + 1),
            Err(MattersError::BatchTooLarge { bytes, limit })
                if bytes == MAX_BATCH_BYTES + 1 && limit == MAX_BATCH_BYTES
        ));
        assert!(within_record(0).is_ok());
    }
}
