//! Post-quantum authenticated sync records for Matters (ADR 0002, #36).
//!
//! [`wire`] parses and encodes content records; [`record`] seals, signs,
//! verifies and opens them with the approved suite. Nothing here decides
//! whether a record is admissible: membership, writer admission, chains and
//! domain checks belong to the receiver.

pub mod record;
pub mod wire;

pub use record::{
    generate_signing_key, seal, verify, ContentKey, Error, SigningKey, VerificationKey, Verified,
};
