//! Recovery after every device is lost (#37, docs/SYNC_KEY_DISTRIBUTION.md
//! "Recovery"):
//!
//! ```text
//! secret32     = 256 bits of OS entropy, shown once as 24 BIP39 English words
//! seed(label)  = HKDF-SHA384(salt = suite, ikm = secret32,
//!                            info = CBOR([label, app16, tenant16, collection16])), 32 bytes
//! root key     = ML-DSA-65 KeyGen(seed("matters-sync/v0/recovery-root"))
//! recovery KEM = MLKEM768-P256 DeriveKeyPair(seed("matters-sync/v0/recovery-kem"))
//! ```
//!
//! The root key signs the collection's genesis and, after loss, the
//! `root_recovery` transition that starts the next generation. The
//! recovery KEM key, committed in the genesis, receives a recovery envelope
//! of each epoch's content key. Only the words hold either private key: the
//! first device derives both, signs the genesis, shows the words once and
//! keeps neither. BIP39's PBKDF2 seed step is not used; the words encode
//! the 256-bit secret and its checksum only.

use bip39::{Language, Mnemonic};

use super::enrollment::derive;
use super::envelope::{key_commitment, wipe};
use super::identity::{generate_kem_key, root_id, KemPrivateKey, KemPublicKey};
use super::message::{sign, Body, Genesis, Message, Route};
use super::{DeviceKeys, Error, Hash, Result};
use crate::record::{generate_signing_key, ContentKey, SigningKey, VerificationKey};

const ROOT_LABEL: &str = "matters-sync/v0/recovery-root";
const KEM_LABEL: &str = "matters-sync/v0/recovery-kem";
/// Words in a recovery secret: 256 bits and an 8-bit checksum.
pub const RECOVERY_WORDS: usize = 24;

/// The 256-bit recovery secret. Zeroed when dropped; never printed or
/// cloned.
pub struct RecoverySecret([u8; 32]);

impl RecoverySecret {
    /// From 32 bytes of fresh OS entropy.
    pub fn new(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    /// The 24 English BIP39 words, separated by single spaces. The string
    /// is the secret: show it once and drop it.
    pub fn words(&self) -> Result<String> {
        let mnemonic = Mnemonic::from_entropy(&self.0).map_err(|_| Error::Crypto)?;
        let words: Vec<&str> = mnemonic.words().collect();
        Ok(words.join(" "))
    }

    /// Parse the words a user typed: any whitespace between them, any case.
    /// Exactly 24 words from the English list with a matching checksum.
    pub fn from_words(typed: &str) -> Result<Self> {
        let normalized = typed
            .split_whitespace()
            .map(str::to_lowercase)
            .collect::<Vec<_>>()
            .join(" ");
        let parsed = Mnemonic::parse_in_normalized(Language::English, &normalized);
        // The typed words are the secret: wipe the normalized copy (best
        // effort; the caller owns the typed string).
        wipe(&mut normalized.into_bytes());
        let mnemonic = parsed.map_err(|_| Error::Format)?;
        if mnemonic.word_count() != RECOVERY_WORDS {
            return Err(Error::Format);
        }
        let (mut entropy, len) = mnemonic.to_entropy_array();
        let secret = entropy[..len].try_into().map_err(|_| Error::Format);
        wipe(&mut entropy);
        Ok(Self(secret?))
    }

    /// The root and recovery keys this secret defines for `route`.
    pub fn keys(&self, route: &Route) -> Result<RecoveryKeys> {
        let (mut root_seed, mut kem_seed) = recovery_seeds(self, route)?;
        let (root, root_public) = generate_signing_key(root_seed);
        let kem = generate_kem_key(kem_seed);
        wipe(&mut root_seed);
        wipe(&mut kem_seed);
        let (kem, kem_public) = kem?;
        Ok(RecoveryKeys {
            root,
            root_public,
            kem,
            kem_public,
        })
    }
}

impl Drop for RecoverySecret {
    fn drop(&mut self) {
        wipe(&mut self.0);
    }
}

impl std::fmt::Debug for RecoverySecret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("RecoverySecret(redacted)")
    }
}

/// The root-key and recovery-KEM seeds of a secret. Exposed for the
/// independent known answers; callers use [`RecoverySecret::keys`].
#[doc(hidden)]
pub fn recovery_seeds(secret: &RecoverySecret, route: &Route) -> Result<([u8; 32], [u8; 32])> {
    let mut root = [0; 32];
    let mut kem = [0; 32];
    derive(&secret.0, route, ROOT_LABEL, &mut root)?;
    derive(&secret.0, route, KEM_LABEL, &mut kem)?;
    Ok((root, kem))
}

/// The root signing key and recovery KEM key derived from the words.
/// Secret-bearing: no Clone, and Debug prints only the root ID.
pub struct RecoveryKeys {
    root: SigningKey,
    root_public: VerificationKey,
    kem: KemPrivateKey,
    kem_public: KemPublicKey,
}

impl RecoveryKeys {
    pub fn root_key(&self) -> &VerificationKey {
        &self.root_public
    }

    pub fn root_id(&self) -> Result<Hash> {
        root_id(&self.root_public)
    }

    /// The root signing key. Use it only to sign the genesis and a
    /// recovery, then drop these keys.
    pub fn signing_key(&self) -> &SigningKey {
        &self.root
    }

    pub fn kem_public_key(&self) -> &KemPublicKey {
        &self.kem_public
    }

    pub(crate) fn kem_private_key(&self) -> &KemPrivateKey {
        &self.kem
    }
}

impl std::fmt::Debug for RecoveryKeys {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("RecoveryKeys(redacted)")
    }
}

/// The first device's genesis: generation 0, epoch 0, `admin` as the only
/// member and admin, committing to `key`, signed by the recovery-derived
/// root. `signing_randomness` must be fresh OS entropy.
pub fn genesis(
    route: &Route,
    recovery: &RecoveryKeys,
    admin: &DeviceKeys,
    key: &ContentKey,
    signing_randomness: [u8; 32],
) -> Result<Vec<u8>> {
    let message = Message {
        route: route.clone(),
        signer: recovery.root_id()?,
        body: Body::Genesis(Box::new(Genesis {
            root_key: recovery.root_public.clone(),
            recovery_key: recovery.kem_public.clone(),
            generation: 0,
            roster: vec![admin.member(true)],
            admin: admin.id(),
            epoch: 0,
            checkpoint: None,
            key_commitment: key_commitment(route, 0, 0, key)?,
        })),
    };
    sign(&message, &recovery.root, signing_randomness)
}
