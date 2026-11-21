//! A relay session's proof of possession (docs/SYNC_SERVICE_CONTRACT.md
//! "Device sessions"): the device signs the relay's one-time challenge with
//! its enrolled ML-DSA-65 key.
//!
//! ```text
//! signed = CBOR(["matters-sync/v0/relay-session", app16, tenant16, collection16, device48, challenge32])
//! signature context = ASCII("matters-sync/v0/relay-session")
//! ```

use libcrux_ml_dsa::ml_dsa_65;
use minicbor::Encoder;

use super::{DeviceKeys, Error, Hash, Result, Route};
use crate::record::VerificationKey;
use crate::wire::SIGNATURE_BYTES;

pub const SESSION_LABEL: &str = "matters-sync/v0/relay-session";

/// The bytes a session signature covers.
pub fn session_bytes(route: &Route, device: &Hash, challenge: &[u8; 32]) -> Result<Vec<u8>> {
    let mut e = Encoder::new(Vec::new());
    e.array(6)?
        .str(SESSION_LABEL)?
        .bytes(&route.application)?
        .bytes(&route.tenant)?
        .bytes(&route.collection)?
        .bytes(device)?
        .bytes(challenge)?;
    Ok(e.into_writer())
}

/// Sign the relay's `challenge` for `route` as `device`.
/// `signing_randomness` must be fresh OS entropy.
pub fn sign_session(
    device: &DeviceKeys,
    route: &Route,
    challenge: &[u8; 32],
    signing_randomness: [u8; 32],
) -> Result<[u8; SIGNATURE_BYTES]> {
    let bytes = session_bytes(route, &device.id(), challenge)?;
    let signature = ml_dsa_65::sign(
        &device.signing_key().0,
        &bytes,
        SESSION_LABEL.as_bytes(),
        signing_randomness,
    )
    .map_err(|_| Error::Crypto)?;
    Ok(*signature.as_ref())
}

/// Sign a relay session challenge as the collection's root, with the keys
/// the recovery words derive (#42). A device that lost every other device
/// uses this session only to read the authority log and publish its root
/// recovery. The relay grants nothing else to it.
pub fn sign_root_session(
    keys: &super::RecoveryKeys,
    route: &Route,
    challenge: &[u8; 32],
    signing_randomness: [u8; 32],
) -> Result<[u8; SIGNATURE_BYTES]> {
    let bytes = session_bytes(route, &keys.root_id()?, challenge)?;
    let signature = ml_dsa_65::sign(
        &keys.signing_key().0,
        &bytes,
        SESSION_LABEL.as_bytes(),
        signing_randomness,
    )
    .map_err(|_| Error::Crypto)?;
    Ok(*signature.as_ref())
}

/// Verify a session signature with the signing key the route's roster
/// lists for `device`; the caller takes that key from authenticated state.
pub fn verify_session(
    key: &VerificationKey,
    route: &Route,
    device: &Hash,
    challenge: &[u8; 32],
    signature: &[u8; SIGNATURE_BYTES],
) -> Result<()> {
    let bytes = session_bytes(route, device, challenge)?;
    ml_dsa_65::verify(
        &key.0,
        &bytes,
        SESSION_LABEL.as_bytes(),
        &ml_dsa_65::MLDSA65Signature::new(*signature),
    )
    .map_err(|_| Error::Signature)
}
