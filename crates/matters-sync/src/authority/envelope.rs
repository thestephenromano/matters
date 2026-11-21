//! Key-envelope transcripts and the epoch key commitment
//! (docs/SYNC_KEY_DISTRIBUTION.md "Key envelope transcripts" and "Epoch
//! content-key commitment"):
//!
//! ```text
//! info = [suite, app16, tenant16, collection16, "content-key", recipient_key_fingerprint48]
//! AAD  = [0, suite, "key_envelope", app16, tenant16, collection16, admin48,
//!         [manifest48, epoch, recipient48, recipient_key_fingerprint48, "content-key", admin48]]
//! plaintext = [epoch, content_epoch_key32, "whole-collection"]
//! key_commitment = SHA-384(CBOR(["matters-sync/v0/epoch-key-commitment", suite,
//!                                app16, tenant16, collection16, generation, epoch, CEK32]))
//! ```

use minicbor::{Decoder, Encoder};

use super::identity::sha384;
use super::message::{write_frame, Body, Message, Reader, Route, PURPOSE};
use super::{Error, Hash, Result};
use crate::record::ContentKey;
use crate::wire::SUITE;

/// The only key policy of this version.
pub const POLICY: &str = "whole-collection";
pub const KEY_COMMITMENT_LABEL: &str = "matters-sync/v0/epoch-key-commitment";

/// Commitment to an epoch content key, carried in the signed genesis or
/// membership message that defines the epoch. Binding rests on SHA-384
/// collision resistance; the key is 256 uniformly random bits, so publishing
/// the commitment reveals nothing usable about it.
pub fn key_commitment(
    route: &Route,
    generation: u64,
    epoch: u64,
    key: &ContentKey,
) -> Result<Hash> {
    let mut e = Encoder::new(Vec::new());
    e.array(8)?
        .str(KEY_COMMITMENT_LABEL)?
        .str(SUITE)?
        .bytes(&route.application)?
        .bytes(&route.tenant)?
        .bytes(&route.collection)?
        .u64(generation)?
        .u64(epoch)?
        .bytes(&key.0)?;
    let mut preimage = e.into_writer();
    let commitment = sha384(&preimage);
    wipe(&mut preimage);
    Ok(commitment)
}

/// The HPKE `info` of an envelope. Exposed for the golden vectors.
#[doc(hidden)]
pub fn envelope_info(message: &Message) -> Result<Vec<u8>> {
    let Body::KeyEnvelope(k) = &message.body else {
        return Err(Error::Unsupported);
    };
    let mut e = Encoder::new(Vec::new());
    e.array(6)?
        .str(SUITE)?
        .bytes(&message.route.application)?
        .bytes(&message.route.tenant)?
        .bytes(&message.route.collection)?
        .str(PURPOSE)?
        .bytes(&k.recipient_key_fingerprint)?;
    Ok(e.into_writer())
}

/// The HPKE AAD of an envelope: its clear frame without the encapsulation
/// and ciphertext. Exposed for the golden vectors.
#[doc(hidden)]
pub fn envelope_aad(message: &Message) -> Result<Vec<u8>> {
    let Body::KeyEnvelope(k) = &message.body else {
        return Err(Error::Unsupported);
    };
    let mut e = Encoder::new(Vec::new());
    write_frame(&mut e, message)?;
    e.array(6)?
        .bytes(&k.manifest)?
        .u64(k.epoch)?
        .bytes(&k.recipient)?
        .bytes(&k.recipient_key_fingerprint)?
        .str(PURPOSE)?
        .bytes(&k.authority)?;
    Ok(e.into_writer())
}

/// The canonical envelope plaintext. The caller wipes it after use.
pub(crate) fn key_plaintext(epoch: u64, key: &ContentKey) -> Result<Vec<u8>> {
    let mut e = Encoder::new(Vec::new());
    e.array(3)?.u64(epoch)?.bytes(&key.0)?.str(POLICY)?;
    Ok(e.into_writer())
}

/// Decode an authenticated envelope plaintext: exact shape, policy, full
/// consumption, the expected epoch and canonical bytes.
pub(crate) fn open_key_plaintext(plaintext: &[u8], epoch: u64) -> Result<ContentKey> {
    let mut r = Reader(Decoder::new(plaintext));
    r.array(3)?;
    let inner_epoch = r.0.u64()?;
    let key = ContentKey::new(r.fixed()?);
    if r.0.str()? != POLICY || r.0.position() != plaintext.len() || inner_epoch != epoch {
        return Err(Error::Format);
    }
    let mut canonical = key_plaintext(inner_epoch, &key)?;
    let matches = canonical == plaintext;
    wipe(&mut canonical);
    if !matches {
        return Err(Error::NonCanonical);
    }
    Ok(key)
}

/// Overwrite key-bearing scratch bytes before they are freed.
pub(crate) fn wipe(bytes: &mut [u8]) {
    for byte in bytes {
        // Volatile writes, so the zeroing is not optimized away.
        unsafe { std::ptr::write_volatile(byte, 0) };
    }
}

/// The content key of one epoch, as unwrapped from an authenticated
/// envelope and checked against the epoch's signed commitment. Bound to its
/// route, manifest and epoch; never printed.
pub struct EpochKey {
    pub(crate) route: Route,
    pub(crate) manifest: Hash,
    pub(crate) epoch: u64,
    pub(crate) key: ContentKey,
}

impl EpochKey {
    pub fn route(&self) -> &Route {
        &self.route
    }

    pub fn manifest(&self) -> Hash {
        self.manifest
    }

    pub fn epoch(&self) -> u64 {
        self.epoch
    }

    /// The key to seal this epoch's records with ([`crate::record::seal`]).
    pub fn content_key(&self) -> &ContentKey {
        &self.key
    }

    /// The key itself, for a sealer that owns it.
    pub fn into_content_key(self) -> ContentKey {
        self.key
    }
}

impl std::fmt::Debug for EpochKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EpochKey")
            .field("epoch", &self.epoch)
            .finish_non_exhaustive()
    }
}
