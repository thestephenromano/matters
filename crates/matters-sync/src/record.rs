//! Sealing, signing, verifying and opening content records with the approved
//! suite (ADR 0002, docs/SYNC_RECORD_CONTRACT.md "Cryptographic transcript"):
//!
//! ```text
//! PRK  = HKDF-Extract-SHA256(salt = record_id, IKM = CEK)
//! K    = HKDF-Expand-SHA256(PRK, CBOR(["matters-sync/v0/record-key", H]), 32)
//! body = AES-256-GCM.Seal(K, nonce = 12 zero bytes, plaintext, AAD = H)
//! sig  = ML-DSA-65.Sign(author, B, context "matters-sync/v0/<kind>")
//! ```
//!
//! Each record ID derives its own key, used for exactly one sealing; the
//! caller must never seal twice under one record ID (the outbox reserves
//! IDs). Every cryptographic call goes through the pinned libcrux providers.
//! A record must verify before it can be opened: [`verify`] returns the only
//! type that can [`Verified::open`]. Keys never print their bytes.

use hpke_rs_crypto::types::{AeadAlgorithm, TwoStageKdfAlgorithm};
use hpke_rs_crypto::HpkeCrypto;
use libcrux_ml_dsa::ml_dsa_65;

use crate::wire::{self, Header, ParsedRecord, MAX_PLAINTEXT_BYTES};

type Provider = hpke_rs_libcrux::HpkeLibcrux<hpke_rs_libcrux::HpkeLibcruxPrng>;

/// Bytes of an ML-DSA-65 verification key.
pub const VERIFICATION_KEY_BYTES: usize = 1952;

/// Why a record was refused. No variant carries key or plaintext material.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Format(#[from] wire::Error),
    #[error("plaintext is larger than {MAX_PLAINTEXT_BYTES} bytes")]
    TooLarge,
    #[error("the signature does not verify for this author")]
    Signature,
    #[error("the record does not decrypt with this epoch key")]
    Decrypt,
    #[error("a cryptographic operation failed")]
    Crypto,
}

pub type Result<T> = std::result::Result<T, Error>;

/// An epoch's 32-byte content key. Zeroed when dropped; never printed.
pub struct ContentKey(pub(crate) [u8; 32]);

impl ContentKey {
    pub fn new(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }
}

impl Drop for ContentKey {
    fn drop(&mut self) {
        // Volatile writes, so the zeroing is not optimized away.
        for byte in &mut self.0 {
            unsafe { std::ptr::write_volatile(byte, 0) };
        }
    }
}

impl std::fmt::Debug for ContentKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ContentKey(redacted)")
    }
}

/// A device's ML-DSA-65 signing key. Never printed.
pub struct SigningKey(pub(crate) ml_dsa_65::MLDSA65SigningKey);

impl std::fmt::Debug for SigningKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SigningKey(redacted)")
    }
}

/// A device's ML-DSA-65 verification key, as the membership roster lists it.
#[derive(Clone)]
pub struct VerificationKey(pub(crate) ml_dsa_65::MLDSA65VerificationKey);

impl VerificationKey {
    pub fn from_bytes(bytes: [u8; VERIFICATION_KEY_BYTES]) -> Self {
        Self(ml_dsa_65::MLDSA65VerificationKey::new(bytes))
    }

    pub fn as_bytes(&self) -> &[u8] {
        self.0.as_slice()
    }
}

impl std::fmt::Debug for VerificationKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("VerificationKey(..)")
    }
}

/// A signing key and its verification key, from 32 bytes of fresh OS
/// entropy. A predictable seed gives a predictable key.
pub fn generate_signing_key(seed: [u8; 32]) -> (SigningKey, VerificationKey) {
    let pair = ml_dsa_65::generate_key_pair(seed);
    (
        SigningKey(pair.signing_key),
        VerificationKey(pair.verification_key),
    )
}

/// The one-time key for record `header.record_id` under `cek`.
fn record_key(record_id: &[u8; 32], cek: &ContentKey, info: &[u8]) -> Result<Vec<u8>> {
    let prk = Provider::kdf_extract(TwoStageKdfAlgorithm::HkdfSha256, record_id, &cek.0)
        .map_err(|_| Error::Crypto)?;
    Provider::kdf_expand(TwoStageKdfAlgorithm::HkdfSha256, &prk, info, 32)
        .map_err(|_| Error::Crypto)
}

/// Seal `plaintext` as the record `header` describes and sign it as its
/// author. `signing_randomness` must be fresh OS entropy for each call.
/// Sealing twice under one record ID reuses its key and nonce: the caller's
/// outbox must reserve each record ID once.
pub fn seal(
    header: Header<'_>,
    cek: &ContentKey,
    plaintext: &[u8],
    author: &SigningKey,
    signing_randomness: [u8; 32],
) -> Result<Vec<u8>> {
    if plaintext.len() > MAX_PLAINTEXT_BYTES {
        return Err(Error::TooLarge);
    }
    let key = record_key(header.record_id, cek, &wire::record_key_info(header)?)?;
    let ciphertext = Provider::aead_seal(
        AeadAlgorithm::Aes256Gcm,
        &key,
        &[0; 12],
        &wire::header_bytes(header)?,
        plaintext,
    )
    .map_err(|_| Error::Crypto)?;
    let signed = wire::signing_bytes(header, &ciphertext)?;
    let signature = ml_dsa_65::sign(
        &author.0,
        &signed,
        header.kind.signature_context(),
        signing_randomness,
    )
    .map_err(|_| Error::Crypto)?;
    Ok(wire::encode(header, &ciphertext, signature.as_ref())?)
}

/// A record whose signature verified for its author.
#[derive(Debug)]
pub struct Verified<'a>(ParsedRecord<'a>);

/// Verify `record` as signed by `author`, whose key the caller takes from
/// authenticated membership, never from the record itself.
pub fn verify<'a>(record: ParsedRecord<'a>, author: &VerificationKey) -> Result<Verified<'a>> {
    let signature = ml_dsa_65::MLDSA65Signature::new(*record.signature);
    ml_dsa_65::verify(
        &author.0,
        record.signed_bytes,
        record.header.kind.signature_context(),
        &signature,
    )
    .map_err(|_| Error::Signature)?;
    Ok(Verified(record))
}

impl<'a> Verified<'a> {
    pub fn record(&self) -> &ParsedRecord<'a> {
        &self.0
    }

    /// Decrypt with the epoch key the header names.
    pub fn open(&self, cek: &ContentKey) -> Result<Vec<u8>> {
        let record = &self.0;
        let key = record_key(record.header.record_id, cek, &record.record_key_info()?)?;
        Provider::aead_open(
            AeadAlgorithm::Aes256Gcm,
            &key,
            &[0; 12],
            record.header_bytes(),
            record.ciphertext,
        )
        .map_err(|_| Error::Decrypt)
    }
}

/// The derived record key for golden-vector checks against the independent
/// oracle. Not for sealing.
#[doc(hidden)]
pub fn derived_key_for_vectors(header: Header<'_>, cek: &ContentKey) -> Result<Vec<u8>> {
    record_key(header.record_id, cek, &wire::record_key_info(header)?)
}

/// SHA-384 of a complete signed record: its identity in a writer's chain
/// (the next record's `previous`).
pub fn record_hash(bytes: &[u8]) -> [u8; 48] {
    libcrux_sha2::sha384(bytes)
}

