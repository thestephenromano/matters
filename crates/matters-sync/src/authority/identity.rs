//! Key fingerprints, device identities and device key material
//! (docs/SYNC_KEY_DISTRIBUTION.md "Exact bytes and identities").
//!
//! ```text
//! fingerprint = SHA-384(CBOR([algorithm_label, public_key_bytes]))
//! device_id   = SHA-384(CBOR(["matters-sync/v0/device", signing_fp48, kem_fp48]))
//! root_id     = fingerprint("ML-DSA-65", root_signing_key)
//! ```
//!
//! Identity hashes bind exact key bytes; they do not certify that a key is
//! mathematically valid or that its holder possesses the private half.

use hpke_rs::{Hpke, HpkePrivateKey, Mode};
use hpke_rs_crypto::types::{AeadAlgorithm, KdfAlgorithm, KemAlgorithm};
use minicbor::Encoder;

use super::{Error, Hash, Result};
use crate::record::{generate_signing_key, SigningKey, VerificationKey};

type Provider = hpke_rs_libcrux::HpkeLibcrux<hpke_rs_libcrux::HpkeLibcruxPrng>;

/// Bytes of an MLKEM768-P256 public key: ML-KEM-768 (1184) and an
/// uncompressed P-256 point (65).
pub const KEM_PUBLIC_KEY_BYTES: usize = 1249;
/// Bytes of an MLKEM768-P256 encapsulation: ML-KEM ciphertext (1088) and an
/// uncompressed P-256 point (65).
pub const KEM_ENCAPSULATION_BYTES: usize = 1153;
/// The fingerprint label of an ML-DSA-65 verification key.
pub const SIGNATURE_KEY_LABEL: &str = "ML-DSA-65";
/// The fingerprint label of an MLKEM768-P256 public key.
pub const KEM_KEY_LABEL: &str = "MLKEM768-P256/draft-ietf-hpke-pq-05";
const DEVICE_LABEL: &str = "matters-sync/v0/device";

pub(crate) fn sha384(bytes: &[u8]) -> Hash {
    libcrux_sha2::sha384(bytes)
}

/// The suite's single HPKE configuration: Base mode, MLKEM768-P256,
/// HKDF-SHA256, AES-256-GCM, with the provider's OS-seeded PRNG.
pub(crate) fn hpke() -> Result<Hpke<Provider>> {
    Hpke::try_new(
        Mode::Base,
        KemAlgorithm::MlKem768P256,
        KdfAlgorithm::HkdfSha256,
        AeadAlgorithm::Aes256Gcm,
    )
    .map_err(|_| Error::Crypto)
}

/// `SHA-384(CBOR([label, public_key]))`.
pub fn fingerprint(label: &str, public_key: &[u8]) -> Result<Hash> {
    let mut e = Encoder::new(Vec::new());
    e.array(2)?.str(label)?.bytes(public_key)?;
    Ok(sha384(&e.into_writer()))
}

/// The root signer ID: the root signing key's fingerprint.
pub fn root_id(root: &VerificationKey) -> Result<Hash> {
    fingerprint(SIGNATURE_KEY_LABEL, root.as_bytes())
}

/// The device ID binding both of a device's public keys.
pub fn device_id(signing: &VerificationKey, kem: &KemPublicKey) -> Result<Hash> {
    let mut e = Encoder::new(Vec::new());
    e.array(3)?
        .str(DEVICE_LABEL)?
        .bytes(&fingerprint(SIGNATURE_KEY_LABEL, signing.as_bytes())?)?
        .bytes(&fingerprint(KEM_KEY_LABEL, kem.as_bytes())?)?;
    Ok(sha384(&e.into_writer()))
}

/// An MLKEM768-P256 public key, as the roster lists it. Its size is fixed;
/// its validity is only established by an HPKE operation that uses it.
#[derive(Clone, PartialEq, Eq)]
pub struct KemPublicKey(Box<[u8; KEM_PUBLIC_KEY_BYTES]>);

impl KemPublicKey {
    pub fn from_bytes(bytes: [u8; KEM_PUBLIC_KEY_BYTES]) -> Self {
        Self(Box::new(bytes))
    }

    pub fn as_bytes(&self) -> &[u8] {
        self.0.as_slice()
    }
}

impl std::fmt::Debug for KemPublicKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("KemPublicKey(..)")
    }
}

/// An MLKEM768-P256 private key. Zeroed when dropped (by the provider);
/// never printed or cloned.
pub struct KemPrivateKey(HpkePrivateKey);

impl KemPrivateKey {
    pub(crate) fn hpke(&self) -> &HpkePrivateKey {
        &self.0
    }
}

impl std::fmt::Debug for KemPrivateKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("KemPrivateKey(redacted)")
    }
}

/// An MLKEM768-P256 key pair derived (RFC 9180 `DeriveKeyPair`) from 32
/// bytes of fresh OS entropy. A predictable seed gives a predictable key.
pub fn generate_kem_key(seed: [u8; 32]) -> Result<(KemPrivateKey, KemPublicKey)> {
    let pair = hpke()?.derive_key_pair(&seed).map_err(|_| Error::Crypto)?;
    let (private, public) = pair.into_keys();
    let public = public
        .as_slice()
        .try_into()
        .map_err(|_| Error::Crypto)
        .map(KemPublicKey::from_bytes)?;
    Ok((KemPrivateKey(private), public))
}

/// One device's signing and KEM keys, and the device ID they define.
/// Secret-bearing: no Clone, no serialization, and Debug prints only the ID.
pub struct DeviceKeys {
    id: Hash,
    signing: SigningKey,
    verification: VerificationKey,
    kem_private: KemPrivateKey,
    kem_public: KemPublicKey,
}

impl DeviceKeys {
    /// Generate a device from two independent 32-byte seeds of fresh OS
    /// entropy.
    pub fn generate(signing_seed: [u8; 32], kem_seed: [u8; 32]) -> Result<Self> {
        let (signing, verification) = generate_signing_key(signing_seed);
        let (kem_private, kem_public) = generate_kem_key(kem_seed)?;
        Ok(Self {
            id: device_id(&verification, &kem_public)?,
            signing,
            verification,
            kem_private,
            kem_public,
        })
    }

    pub fn id(&self) -> Hash {
        self.id
    }

    pub fn signing_key(&self) -> &SigningKey {
        &self.signing
    }

    pub fn verification_key(&self) -> &VerificationKey {
        &self.verification
    }

    pub fn kem_public_key(&self) -> &KemPublicKey {
        &self.kem_public
    }

    pub(crate) fn kem_private_key(&self) -> &KemPrivateKey {
        &self.kem_private
    }

    /// This device's roster entry.
    pub fn member(&self, admin: bool) -> Member {
        Member {
            id: self.id,
            signing_key: self.verification.clone(),
            kem_key: self.kem_public.clone(),
            admin,
        }
    }
}

impl std::fmt::Debug for DeviceKeys {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("DeviceKeys(redacted)")
    }
}

/// A roster entry `[device48, signing_key1952, kem_key1249, role]`. Role 1
/// is writer+admin, role 0 writer. The ID is checked against both keys when
/// a roster is admitted, not when it is parsed.
#[derive(Clone, Debug)]
pub struct Member {
    pub id: Hash,
    pub signing_key: VerificationKey,
    pub kem_key: KemPublicKey,
    pub admin: bool,
}

impl PartialEq for Member {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id
            && self.signing_key.as_bytes() == other.signing_key.as_bytes()
            && self.kem_key == other.kem_key
            && self.admin == other.admin
    }
}

impl Eq for Member {}
