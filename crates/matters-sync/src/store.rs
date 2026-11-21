//! What a sync client needs from the device's local store (#39): the
//! route and device it syncs as, session signatures, the outgoing queues
//! and their acknowledgements, receiving controls and records, and durable
//! per-log cursors. Implemented by an application's database (Matters'
//! is `matters_core::Database`); the client knows nothing about tasks.
//!
//! Every call is synchronous and quick. The client calls it off its
//! network tasks, and never holds a store call across network waits.

use crate::authority::{Hash, Route};
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
    /// the new epoch's key was not supplied: retry it with one.
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
    fn cursor(&self, log: Log) -> StoreResult<Option<Vec<u8>>>;
    /// Save a page's cursor after the page's entries are durably applied.
    fn save_cursor(&self, log: Log, cursor: &[u8]) -> StoreResult<()>;
    /// The relay's storage generation changed (a restore): forget both
    /// cursors and queue every record and control again for upload.
    fn generation_changed(&self) -> StoreResult<()>;
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
