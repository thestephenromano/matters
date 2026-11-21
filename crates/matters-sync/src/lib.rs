//! Post-quantum authenticated sync for Matters (ADR 0002, #36, #37).
//!
//! [`wire`] parses and encodes content records; [`record`] seals, signs,
//! verifies and opens them with the approved suite. [`authority`] admits the
//! signed control messages that say who may write and which content key an
//! epoch uses: genesis, membership, writer admission and key envelopes.
//! [`checkpoint`] encodes and assembles checkpoints: engine snapshots with a
//! statement of what they cover (#40). Chains, durable receive and domain
//! checks belong to the receiver.

pub mod authority;
pub mod checkpoint;
#[cfg(feature = "fuzzing")]
#[doc(hidden)]
pub mod fuzzing;
pub mod record;
pub mod wire;

pub use authority::{Authority, DeviceKeys, EpochKey, Route};
pub use record::{
    generate_signing_key, record_hash, seal, verify, ContentKey, Error, SigningKey,
    VerificationKey, Verified,
};
