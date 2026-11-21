//! The signed control-message codec (docs/SYNC_KEY_DISTRIBUTION.md,
//! sync-key-distribution-v0.cddl):
//!
//! ```text
//! B = [0, "matters-sync-pq-v0", kind, app16, tenant16, collection16, signer48, body]
//! record = [B, signature3309]
//! signature context = ASCII("matters-sync/v0/" + kind)
//! ```
//!
//! Parsing checks structure, sizes and canonical encoding only: after typed
//! decoding, re-encoding the whole record must reproduce the input, which the
//! 64 KiB control budget keeps cheap. It never authorizes a message; that is
//! [`Authority`](super::Authority)'s job. Ported from the prototype unchanged
//! in behavior; its independent golden fixtures check it here too.

use libcrux_ml_dsa::ml_dsa_65;
use minicbor::{Decoder, Encoder};

use super::identity::{KemPublicKey, Member, KEM_ENCAPSULATION_BYTES, KEM_PUBLIC_KEY_BYTES};
use super::{Error, Hash, Id, Result};
use crate::record::{SigningKey, VerificationKey, VERIFICATION_KEY_BYTES};
use crate::wire::{self, SIGNATURE_BYTES, SUITE};

/// The largest control record accepted, checked before decoding.
pub const MAX_CONTROL_BYTES: usize = 64 * 1024;
/// The largest roster accepted, checked before it is allocated.
pub const MAX_ROSTER: usize = 16;
pub(crate) const PURPOSE: &str = "content-key";
/// An envelope's ciphertext bounds: a 16-byte tag at least, and at most the
/// largest canonical plaintext (1 + 9 + 34 + 17 bytes) plus the tag.
const ENVELOPE_CIPHERTEXT: std::ops::RangeInclusive<usize> = 16..=77;

/// The collection a control message belongs to, supplied by trusted local
/// configuration and compared with every message.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Route {
    pub application: [u8; 16],
    pub tenant: [u8; 16],
    pub collection: [u8; 16],
}

impl Route {
    /// The content-record context for one document of this collection.
    pub fn content<'a>(&'a self, document: &'a [u8; 16]) -> wire::Context<'a> {
        wire::Context {
            application: &self.application,
            tenant: &self.tenant,
            collection: &self.collection,
            document,
        }
    }
}

/// Root-signed collection genesis.
#[derive(Clone, Debug)]
pub struct Genesis {
    pub root_key: VerificationKey,
    pub recovery_key: KemPublicKey,
    pub generation: u64,
    pub roster: Vec<Member>,
    pub admin: Hash,
    pub epoch: u64,
    pub checkpoint: Option<Hash>,
    /// Commitment to the epoch's content key (`key_commitment`).
    pub key_commitment: Hash,
}

/// A membership transition, signed by the previous manifest's admin. The
/// closing frontier is always empty in this version.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Membership {
    pub generation: u64,
    pub sequence: u64,
    pub previous: Hash,
    pub roster: Vec<Member>,
    pub admin: Hash,
    pub epoch: u64,
    pub recovery_key: KemPublicKey,
    /// Commitment to the new epoch's content key (`key_commitment`).
    pub key_commitment: Hash,
}

/// A writer root, or a legacy replacement with no signed closing boundary.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WriterStart {
    pub manifest: Hash,
    pub document: [u8; 16],
    pub device: Hash,
    pub incarnation: Id,
    pub actor: u64,
    pub previous: Option<Id>,
}

/// A writer successor with the exact closed content prefix of its
/// predecessor (docs/SYNC_WRITER_LIFECYCLE.md); null closes an empty chain.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WriterHandoff {
    pub manifest: Hash,
    pub document: [u8; 16],
    pub device: Hash,
    pub incarnation: Id,
    pub actor: u64,
    pub previous: Id,
    pub closing: Option<Hash>,
}

/// A new device's request to join, signed with its own signing key (proof
/// of possession) and bound to a one-time invitation by `binding`, an
/// HMAC-SHA384 under the invitation secret (docs/SYNC_KEY_DISTRIBUTION.md
/// "Enrollment"). The message signer is the device ID of the two keys.
#[derive(Clone, Debug)]
pub struct EnrollmentRequest {
    pub manifest: Hash,
    pub signing_key: VerificationKey,
    pub kem_key: KemPublicKey,
    pub binding: Hash,
}

/// An admin-signed HPKE envelope of the epoch content key for one member.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KeyEnvelope {
    pub manifest: Hash,
    pub epoch: u64,
    pub recipient: Hash,
    pub recipient_key_fingerprint: Hash,
    pub encapsulation: Vec<u8>,
    pub ciphertext: Vec<u8>,
    pub authority: Hash,
}

#[derive(Clone, Debug)]
pub enum Body {
    Genesis(Box<Genesis>),
    Membership(Membership),
    WriterStart(WriterStart),
    WriterHandoff(WriterHandoff),
    KeyEnvelope(KeyEnvelope),
    EnrollmentRequest(Box<EnrollmentRequest>),
}

impl Body {
    /// The message kind, as encoded and as the signature context names it.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Genesis(_) => "genesis",
            Self::Membership(_) => "membership",
            Self::WriterStart(_) => "writer_start",
            Self::WriterHandoff(_) => "writer_handoff",
            Self::KeyEnvelope(_) => "key_envelope",
            Self::EnrollmentRequest(_) => "enrollment_request",
        }
    }

    /// The lineage step a writer control claims, independent of its
    /// randomized signature. `None` for non-writer bodies.
    pub fn writer_claim(&self) -> Option<WriterClaim> {
        match self {
            Self::WriterStart(w) => Some(WriterClaim {
                manifest: w.manifest,
                document: w.document,
                device: w.device,
                incarnation: w.incarnation,
                actor: w.actor,
                previous: w.previous.map(|p| (p, Boundary::Unbounded)),
            }),
            Self::WriterHandoff(w) => Some(WriterClaim {
                manifest: w.manifest,
                document: w.document,
                device: w.device,
                incarnation: w.incarnation,
                actor: w.actor,
                previous: Some((w.previous, Boundary::Closed(w.closing))),
            }),
            _ => None,
        }
    }
}

/// How a writer's predecessor was retired.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Boundary {
    /// Legacy `writer_start` replacement: the predecessor is retired but no
    /// retained content prefix was signed.
    Unbounded,
    /// `writer_handoff`: the exact signed closing head; null is empty.
    Closed(Option<Hash>),
}

/// One signed writer lineage step, as distinct from the bytes signing it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WriterClaim {
    pub manifest: Hash,
    pub document: [u8; 16],
    pub device: Hash,
    pub incarnation: Id,
    pub actor: u64,
    pub previous: Option<(Id, Boundary)>,
}

/// A control message before signing, or as parsed (not yet authorized).
#[derive(Clone, Debug)]
pub struct Message {
    pub route: Route,
    pub signer: Hash,
    pub body: Body,
}

pub(crate) fn signature_context(kind: &str) -> Vec<u8> {
    format!("matters-sync/v0/{kind}").into_bytes()
}

fn write_roster(e: &mut Encoder<Vec<u8>>, members: &[Member]) -> Result<()> {
    e.array(members.len() as u64)?;
    for m in members {
        e.array(4)?
            .bytes(&m.id)?
            .bytes(m.signing_key.as_bytes())?
            .bytes(m.kem_key.as_bytes())?
            .u8(u8::from(m.admin))?;
    }
    Ok(())
}

fn write_optional<const N: usize>(e: &mut Encoder<Vec<u8>>, value: &Option<[u8; N]>) -> Result<()> {
    if let Some(bytes) = value {
        e.bytes(bytes)?;
    } else {
        e.null()?;
    }
    Ok(())
}

/// The clear frame `[0, suite, kind, app, tenant, collection, signer, ...]`
/// as an 8-element array head and its first seven items.
pub(crate) fn write_frame(e: &mut Encoder<Vec<u8>>, message: &Message) -> Result<()> {
    e.array(8)?
        .u8(0)?
        .str(SUITE)?
        .str(message.body.kind())?
        .bytes(&message.route.application)?
        .bytes(&message.route.tenant)?
        .bytes(&message.route.collection)?
        .bytes(&message.signer)?;
    Ok(())
}

/// The canonical signed bytes `B` of a message.
pub fn signing_bytes(message: &Message) -> Result<Vec<u8>> {
    let mut e = Encoder::new(Vec::new());
    write_frame(&mut e, message)?;
    match &message.body {
        Body::Genesis(g) => {
            e.array(8)?
                .bytes(g.root_key.as_bytes())?
                .bytes(g.recovery_key.as_bytes())?
                .u64(g.generation)?;
            write_roster(&mut e, &g.roster)?;
            e.bytes(&g.admin)?.u64(g.epoch)?;
            write_optional(&mut e, &g.checkpoint)?;
            e.bytes(&g.key_commitment)?;
        }
        Body::Membership(m) => {
            e.array(9)?
                .u64(m.generation)?
                .u64(m.sequence)?
                .bytes(&m.previous)?;
            write_roster(&mut e, &m.roster)?;
            e.bytes(&m.admin)?
                .u64(m.epoch)?
                .bytes(m.recovery_key.as_bytes())?
                .array(0)?
                .bytes(&m.key_commitment)?;
        }
        Body::WriterStart(w) => {
            e.array(6)?
                .bytes(&w.manifest)?
                .bytes(&w.document)?
                .bytes(&w.device)?
                .bytes(&w.incarnation)?
                .u64(w.actor)?;
            write_optional(&mut e, &w.previous)?;
        }
        Body::WriterHandoff(w) => {
            e.array(7)?
                .bytes(&w.manifest)?
                .bytes(&w.document)?
                .bytes(&w.device)?
                .bytes(&w.incarnation)?
                .u64(w.actor)?
                .bytes(&w.previous)?;
            write_optional(&mut e, &w.closing)?;
        }
        Body::KeyEnvelope(k) => {
            e.array(8)?
                .bytes(&k.manifest)?
                .u64(k.epoch)?
                .bytes(&k.recipient)?
                .bytes(&k.recipient_key_fingerprint)?
                .str(PURPOSE)?
                .bytes(&k.encapsulation)?
                .bytes(&k.ciphertext)?
                .bytes(&k.authority)?;
        }
        Body::EnrollmentRequest(r) => {
            e.array(4)?
                .bytes(&r.manifest)?
                .bytes(r.signing_key.as_bytes())?
                .bytes(r.kem_key.as_bytes())?
                .bytes(&r.binding)?;
        }
    }
    Ok(e.into_writer())
}

/// Packages an externally produced signature; this function does not sign.
pub fn encode(message: &Message, signature: &[u8; SIGNATURE_BYTES]) -> Result<Vec<u8>> {
    let mut prefix = Encoder::new(Vec::new());
    prefix.array(2)?;
    let mut result = prefix.into_writer();
    result.extend(signing_bytes(message)?);
    let mut e = Encoder::new(result);
    e.bytes(signature)?;
    Ok(e.into_writer())
}

/// Sign `message` as `signer` and package it. `signing_randomness` must be
/// fresh OS entropy for each call; signing the same membership body twice
/// yields two distinct records, which is a fork. Retries must resend the
/// exact bytes returned here.
pub fn sign(
    message: &Message,
    signer: &SigningKey,
    signing_randomness: [u8; 32],
) -> Result<Vec<u8>> {
    let signature = ml_dsa_65::sign(
        &signer.0,
        &signing_bytes(message)?,
        &signature_context(message.body.kind()),
        signing_randomness,
    )
    .map_err(|_| Error::Crypto)?;
    encode(message, signature.as_ref())
}

/// A structurally valid, canonical control record. Not authenticated.
#[derive(Debug)]
pub struct ParsedControl<'a> {
    pub message: Message,
    /// The original signed `B` slice, verified as received.
    pub signed: &'a [u8],
    pub signature: &'a [u8; SIGNATURE_BYTES],
}

impl ParsedControl<'_> {
    /// Verify the signature for the given key. The caller takes that key
    /// from authenticated state, never from the message itself.
    pub(crate) fn verify(&self, key: &VerificationKey) -> Result<()> {
        let signature = ml_dsa_65::MLDSA65Signature::new(*self.signature);
        ml_dsa_65::verify(
            &key.0,
            self.signed,
            &signature_context(self.message.body.kind()),
            &signature,
        )
        .map_err(|_| Error::Signature)
    }
}

pub(crate) struct Reader<'a>(pub(crate) Decoder<'a>);

impl<'a> Reader<'a> {
    pub(crate) fn array(&mut self, n: u64) -> Result<()> {
        if self.0.array()? != Some(n) {
            return Err(Error::Format);
        }
        Ok(())
    }

    pub(crate) fn fixed<const N: usize>(&mut self) -> Result<[u8; N]> {
        self.0.bytes()?.try_into().map_err(|_| Error::Format)
    }

    fn optional<const N: usize>(&mut self) -> Result<Option<[u8; N]>> {
        if self.0.datatype()? == minicbor::data::Type::Null {
            self.0.null()?;
            Ok(None)
        } else {
            Ok(Some(self.fixed()?))
        }
    }

    fn signing_key(&mut self) -> Result<VerificationKey> {
        Ok(VerificationKey::from_bytes(
            self.fixed::<VERIFICATION_KEY_BYTES>()?,
        ))
    }

    fn kem_key(&mut self) -> Result<KemPublicKey> {
        Ok(KemPublicKey::from_bytes(
            self.fixed::<KEM_PUBLIC_KEY_BYTES>()?,
        ))
    }

    fn roster(&mut self) -> Result<Vec<Member>> {
        let n = self.0.array()?.ok_or(Error::Format)?;
        if n == 0 || n > MAX_ROSTER as u64 {
            return Err(Error::Limit);
        }
        let mut members = Vec::with_capacity(n as usize);
        for _ in 0..n {
            self.array(4)?;
            let id = self.fixed()?;
            let signing_key = self.signing_key()?;
            let kem_key = self.kem_key()?;
            let admin = match self.0.u8()? {
                0 => false,
                1 => true,
                _ => return Err(Error::Format),
            };
            members.push(Member {
                id,
                signing_key,
                kem_key,
                admin,
            });
        }
        Ok(members)
    }

    fn genesis(&mut self) -> Result<Genesis> {
        self.array(8)?;
        Ok(Genesis {
            root_key: self.signing_key()?,
            recovery_key: self.kem_key()?,
            generation: self.0.u64()?,
            roster: self.roster()?,
            admin: self.fixed()?,
            epoch: self.0.u64()?,
            checkpoint: self.optional()?,
            key_commitment: self.fixed()?,
        })
    }

    fn membership(&mut self) -> Result<Membership> {
        self.array(9)?;
        let generation = self.0.u64()?;
        let sequence = self.0.u64()?;
        let previous = self.fixed()?;
        let roster = self.roster()?;
        let admin = self.fixed()?;
        let epoch = self.0.u64()?;
        let recovery_key = self.kem_key()?;
        self.array(0)?;
        Ok(Membership {
            generation,
            sequence,
            previous,
            roster,
            admin,
            epoch,
            recovery_key,
            key_commitment: self.fixed()?,
        })
    }

    fn writer_start(&mut self) -> Result<WriterStart> {
        self.array(6)?;
        Ok(WriterStart {
            manifest: self.fixed()?,
            document: self.fixed()?,
            device: self.fixed()?,
            incarnation: self.fixed()?,
            actor: self.0.u64()?,
            previous: self.optional()?,
        })
    }

    fn writer_handoff(&mut self) -> Result<WriterHandoff> {
        self.array(7)?;
        Ok(WriterHandoff {
            manifest: self.fixed()?,
            document: self.fixed()?,
            device: self.fixed()?,
            incarnation: self.fixed()?,
            actor: self.0.u64()?,
            previous: self.fixed()?,
            closing: self.optional()?,
        })
    }

    fn key_envelope(&mut self) -> Result<KeyEnvelope> {
        self.array(8)?;
        let manifest = self.fixed()?;
        let epoch = self.0.u64()?;
        let recipient = self.fixed()?;
        let recipient_key_fingerprint = self.fixed()?;
        if self.0.str()? != PURPOSE {
            return Err(Error::Unsupported);
        }
        let encapsulation = self.fixed::<KEM_ENCAPSULATION_BYTES>()?.to_vec();
        let ciphertext = self.0.bytes()?;
        if !ENVELOPE_CIPHERTEXT.contains(&ciphertext.len()) {
            return Err(Error::Limit);
        }
        let ciphertext = ciphertext.to_vec();
        Ok(KeyEnvelope {
            manifest,
            epoch,
            recipient,
            recipient_key_fingerprint,
            encapsulation,
            ciphertext,
            authority: self.fixed()?,
        })
    }
}

impl Reader<'_> {
    fn enrollment_request(&mut self) -> Result<EnrollmentRequest> {
        self.array(4)?;
        Ok(EnrollmentRequest {
            manifest: self.fixed()?,
            signing_key: self.signing_key()?,
            kem_key: self.kem_key()?,
            binding: self.fixed()?,
        })
    }
}

/// Parse one control record: structure, sizes and canonical bytes only.
pub fn parse(bytes: &[u8]) -> Result<ParsedControl<'_>> {
    if bytes.len() > MAX_CONTROL_BYTES {
        return Err(Error::Limit);
    }
    let mut r = Reader(Decoder::new(bytes));
    r.array(2)?;
    let start = r.0.position();
    r.array(8)?;
    if r.0.u64()? != 0 || r.0.str()? != SUITE {
        return Err(Error::Unsupported);
    }
    let kind = r.0.str()?;
    let route = Route {
        application: r.fixed()?,
        tenant: r.fixed()?,
        collection: r.fixed()?,
    };
    let signer = r.fixed()?;
    let body = match kind {
        "genesis" => Body::Genesis(Box::new(r.genesis()?)),
        "membership" => Body::Membership(r.membership()?),
        "writer_start" => Body::WriterStart(r.writer_start()?),
        "writer_handoff" => Body::WriterHandoff(r.writer_handoff()?),
        "key_envelope" => Body::KeyEnvelope(r.key_envelope()?),
        "enrollment_request" => Body::EnrollmentRequest(Box::new(r.enrollment_request()?)),
        _ => return Err(Error::Unsupported),
    };
    let end = r.0.position();
    let signature = r.0.bytes()?.try_into().map_err(|_| Error::Format)?;
    if r.0.position() != bytes.len() {
        return Err(Error::Format);
    }
    let message = Message {
        route,
        signer,
        body,
    };
    if encode(&message, signature)? != bytes {
        return Err(Error::NonCanonical);
    }
    Ok(ParsedControl {
        message,
        signed: &bytes[start..end],
        signature,
    })
}
