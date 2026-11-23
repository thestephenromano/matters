//! What a sync client needs from the device's local store (#39): the
//! route and device it syncs as, session signatures, the outgoing queues
//! and their acknowledgements, receiving controls and records, and durable
//! per-log cursors. Implemented by an application's database (Matters'
//! is `matters_core::Database`); the client knows nothing about tasks.
//!
//! Every call is synchronous and quick. The client calls it off its
//! network tasks, and never holds a store call across network waits.

use crate::authority::{Hash, Route};
use crate::checkpoint::Cursor;
use crate::wire::SIGNATURE_BYTES;

/// A store failure, reported to the client's status as it is.
pub type StoreError = Box<dyn std::error::Error + Send + Sync>;
pub type StoreResult<T> = Result<T, StoreError>;

/// The two logs a route keeps on the relay.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Log {
    Authority,
    Records,
}

/// What applying a received control did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ControlOutcome {
    /// Applied, replayed or ignored (another device's envelope).
    Done,
    /// A transition that keeps this device a member, and its envelope of
    /// the new epoch's key was not supplied, or the one supplied does not
    /// open (a forgery the relay served): retry it with another.
    NeedsEnvelope,
    /// This device is no longer a current member, or its authority froze:
    /// it keeps working locally and stops uploading.
    Stopped,
    /// The authority refused it: forged, malformed, or a claim made stale
    /// by a later transition. Skipped; the log moves on.
    Refused,
}

pub trait LocalStore: Send + Sync {
    fn route(&self) -> StoreResult<Route>;
    fn device(&self) -> StoreResult<Hash>;
    /// Sign the relay's session challenge with the device's enrolled key.
    fn sign_session(&self, challenge: &[u8; 32]) -> StoreResult<[u8; SIGNATURE_BYTES]>;
    /// Controls this device signed for others, oldest first: `(seq, bytes)`.
    fn outgoing_controls(&self) -> StoreResult<Vec<(i64, Vec<u8>)>>;
    fn acknowledge_control(&self, seq: i64) -> StoreResult<()>;
    /// The relay refused this outgoing control for good: malformed, too
    /// large, or made stale by a transition this device has not received
    /// yet. Take it out of the queue, so the round goes on to pull; a
    /// generation change queues it again. By default it is acknowledged.
    fn set_aside_control(&self, seq: i64) -> StoreResult<()> {
        self.acknowledge_control(seq)
    }
    /// Every control this device's authority accepted, in order, whoever
    /// signed it: what it re-publishes to a relay that lost them (a restore
    /// from a backup older than its own membership).
    fn known_controls(&self) -> StoreResult<Vec<Vec<u8>>> {
        Ok(Vec::new())
    }
    /// Whether this device's own authority holds it a current member, as
    /// far as it has received.
    fn is_current_member(&self) -> StoreResult<bool> {
        Ok(true)
    }
    /// Sealed records not yet acknowledged, oldest first.
    fn outgoing_records(&self) -> StoreResult<Vec<([u8; 32], Vec<u8>)>>;
    fn acknowledge_record(&self, record_id: &[u8; 32]) -> StoreResult<()>;
    /// Apply one authority object, with this device's envelope for the new
    /// epoch when it is a transition that keeps it a member.
    fn receive_control(
        &self,
        control: &[u8],
        envelope: Option<&[u8]>,
    ) -> StoreResult<ControlOutcome>;
    /// Receive one record: applied, kept pending, or a duplicate are all
    /// `Ok`; `Err` is a refused record (forged or malformed).
    fn receive_record(&self, record: &[u8]) -> StoreResult<()>;
    /// Receive a page of records in order, each with its own outcome as
    /// `receive_record` gives it. The outer `Err` is a store failure that
    /// applied none of them. A store that can apply a page at once should
    /// (Matters' database rebuilds its rows once a page, #241).
    fn receive_records(&self, records: &[Vec<u8>]) -> StoreResult<Vec<StoreResult<()>>> {
        Ok(records.iter().map(|r| self.receive_record(r)).collect())
    }
    /// Receive a page of the record log of storage `generation`: each
    /// record with its sequence in the log, as `receive_records` does. A
    /// store that lets a record go before it can check it (Matters' keeps
    /// only a budget of records it cannot authenticate yet) remembers where
    /// the record was, to read it again ([`Self::rewind_records`]).
    fn receive_page(
        &self,
        _generation: [u8; 16],
        records: &[(u64, Vec<u8>)],
    ) -> StoreResult<Vec<StoreResult<()>>> {
        let bytes: Vec<Vec<u8>> = records.iter().map(|(_, b)| b.clone()).collect();
        self.receive_records(&bytes)
    }
    /// Called before the record log is read: move the records cursor back
    /// to the earliest record the store let go of, if what it has received
    /// since may let it take that record now. At most once for each such
    /// change, so a flood of records it cannot check never makes it read
    /// the log again and again.
    fn rewind_records(&self) -> StoreResult<()> {
        Ok(())
    }
    fn cursor(&self, log: Log) -> StoreResult<Option<Vec<u8>>>;
    /// Save a page's cursor after the page's entries are durably applied.
    fn save_cursor(&self, log: Log, cursor: &[u8]) -> StoreResult<()>;
    /// The relay's storage generation changed (a restore): forget both
    /// cursors and queue every record and control again for upload.
    fn generation_changed(&self) -> StoreResult<()>;

    /// Whether this device holds nothing yet, so it may start from a
    /// checkpoint instead of reading the whole record log (#40).
    fn wants_checkpoint(&self) -> StoreResult<bool> {
        Ok(false)
    }

    /// Start from a checkpoint: `parts` are its snapshot records as the
    /// relay returned them, in its current `generation`. True if it
    /// started, and its records cursor is then saved.
    fn start_from_checkpoint(
        &self,
        _parts: &[Vec<u8>],
        _generation: [u8; 16],
    ) -> StoreResult<bool> {
        Ok(false)
    }

    /// Called after a complete pull that ended at `at`. Make a checkpoint
    /// if one is due, spaced `every` engine updates apart, and the device
    /// is fully synced. True if it made one, whose parts are then
    /// outgoing records.
    fn make_checkpoint(&self, _at: Cursor, _every: u64) -> StoreResult<bool> {
        Ok(false)
    }
}

/// What a sync client needs from a device that is joining a collection
/// (#42): it is not a member until the admin's transition is published, and
/// its keys are in the key store rather than an open membership.
pub trait JoiningStore: Send + Sync {
    fn route(&self) -> StoreResult<Route>;
    fn device(&self) -> StoreResult<Hash>;
    fn sign_session(&self, challenge: &[u8; 32]) -> StoreResult<[u8; SIGNATURE_BYTES]>;
    /// Finish joining with the collection's controls as the relay's
    /// authority log holds them (key envelopes left out), and the envelopes
    /// addressed to this device. True once joined; false while the admin's
    /// transition is not there yet.
    fn finish(&self, controls: &[Vec<u8>], envelopes: &[Vec<u8>]) -> StoreResult<bool>;
}

/// What a sync client needs from a device recovering a collection from its
/// recovery words after every device was lost (#42). It is no member, so it
/// signs in as the collection's root, which may only read the authority and
/// record logs and publish the root recovery.
pub trait RecoveringStore: Send + Sync {
    fn route(&self) -> StoreResult<Route>;
    /// The root's ID: the device a root session signs in as.
    fn root(&self) -> StoreResult<Hash>;
    fn sign_root_session(&self, challenge: &[u8; 32]) -> StoreResult<[u8; SIGNATURE_BYTES]>;
    /// Recover from the relay's authority log (its controls in order, and
    /// its key envelopes) and its content records. The records let the
    /// recovery close each lost writer at the exact head of the chain the
    /// relay holds (#45). Afterwards the recovery waits to be published.
    fn recover(
        &self,
        controls: &[Vec<u8>],
        envelopes: &[Vec<u8>],
        records: &[Vec<u8>],
    ) -> StoreResult<()>;
    /// What the recovery signed for publishing, oldest first: `(seq, bytes)`.
    fn outgoing(&self) -> StoreResult<Vec<(i64, Vec<u8>)>>;
    fn acknowledge(&self, seq: i64) -> StoreResult<()>;
}
