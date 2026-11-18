//! Test-only authority/key-distribution experiment. No application-facing API,
//! durable identity store, enrollment ceremony, or recovery implementation.
use crate::record_crypto;
use hpke_rs::{Hpke, HpkeKeyPair, HpkePublicKey, Mode};
use hpke_rs_crypto::{
    types::{AeadAlgorithm, KdfAlgorithm, KemAlgorithm},
    HpkeCrypto, TryRng,
};
use hpke_rs_libcrux::{HpkeLibcrux, HpkeLibcruxPrng};
use libcrux_ml_dsa::ml_dsa_65;
use matters_sync_prototype::wire;
use minicbor::{Decoder, Encoder};
use sha2::{Digest, Sha384};
use std::collections::BTreeMap;

pub type Hash = [u8; 48];
pub type Id = [u8; 32];
pub const SIG_PK: usize = 1952;
pub const KEM_PK: usize = 1249; // ML-KEM-768 (1184) + uncompressed P-256 (65).
pub const KEM_ENC: usize = 1153; // ML-KEM ciphertext (1088) + P-256 (65).
pub const MAX_DEVICES: usize = 16;
pub const MAX_CONTROL: usize = 64 * 1024; // Explicit experiment input budget.
const MAX_TRANSITIONS: usize = 16;
const MAX_WRITERS: usize = 16;
pub const KEM_LABEL: &str = "MLKEM768-P256/draft-ietf-hpke-pq-05";
pub const POLICY: &str = "whole-collection";
const PURPOSE: &str = "content-key";
type Provider = HpkeLibcrux<HpkeLibcruxPrng>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    Format,
    NonCanonical,
    Limit,
    Context,
    Signature,
    Identity,
    Unauthorized,
    Stale,
    MissingPrevious,
    Fork,
    Unsupported,
    Actor,
    Crypto,
}
pub type Result<T> = std::result::Result<T, Error>;
impl From<minicbor::decode::Error> for Error {
    fn from(_: minicbor::decode::Error) -> Self {
        Self::Format
    }
}
impl From<minicbor::encode::Error<std::convert::Infallible>> for Error {
    fn from(_: minicbor::encode::Error<std::convert::Infallible>) -> Self {
        Self::Format
    }
}

pub fn random32() -> Result<Id> {
    let mut bytes = [0; 32];
    HpkeLibcruxPrng::try_new()
        .map_err(|_| Error::Crypto)?
        .try_fill_bytes(&mut bytes)
        .map_err(|_| Error::Crypto)?;
    Ok(bytes)
}
fn hpke() -> Result<Hpke<Provider>> {
    Hpke::try_new(
        Mode::Base,
        KemAlgorithm::MlKem768P256,
        KdfAlgorithm::HkdfSha256,
        AeadAlgorithm::Aes256Gcm,
    )
    .map_err(|_| Error::Crypto)
}
pub fn hash(bytes: &[u8]) -> Hash {
    Sha384::digest(bytes).into()
}
pub fn fingerprint(label: &str, pk: &[u8]) -> Result<Hash> {
    let mut e = Encoder::new(Vec::new());
    e.array(2)?.str(label)?.bytes(pk)?;
    Ok(hash(&e.into_writer()))
}
pub fn device_id(sig: &[u8], kem: &[u8]) -> Result<Hash> {
    if sig.len() != SIG_PK || kem.len() != KEM_PK {
        return Err(Error::Identity);
    }
    let mut e = Encoder::new(Vec::new());
    e.array(3)?
        .str("matters-sync/v0/device")?
        .bytes(&fingerprint("ML-DSA-65", sig)?)?
        .bytes(&fingerprint(KEM_LABEL, kem)?)?;
    Ok(hash(&e.into_writer()))
}

// Deliberately no Clone/Debug/serialization of secret-bearing test keys.
pub struct Device {
    pub signing: ml_dsa_65::MLDSA65KeyPair,
    pub kem: HpkeKeyPair,
}
impl Device {
    pub fn generate() -> Result<Self> {
        Ok(Self {
            signing: ml_dsa_65::generate_key_pair(random32()?),
            kem: hpke()?.generate_key_pair().map_err(|_| Error::Crypto)?,
        })
    }
    pub fn public(&self, admin: bool) -> Result<Member> {
        let sig = self.signing.verification_key.as_slice().to_vec();
        let kem = self.kem.public_key().as_slice().to_vec();
        Ok(Member {
            id: device_id(&sig, &kem)?,
            sig,
            kem,
            admin,
        })
    }
    pub fn id(&self) -> Result<Hash> {
        device_id(
            self.signing.verification_key.as_slice(),
            self.kem.public_key().as_slice(),
        )
    }
    pub fn root_id(&self) -> Result<Hash> {
        fingerprint("ML-DSA-65", self.signing.verification_key.as_slice())
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Route {
    pub app: [u8; 16],
    pub tenant: [u8; 16],
    pub collection: [u8; 16],
}
impl Route {
    pub fn content<'a>(&'a self, document: &'a [u8; 16]) -> wire::Context<'a> {
        wire::Context {
            application: &self.app,
            tenant: &self.tenant,
            collection: &self.collection,
            document,
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Member {
    pub id: Hash,
    pub sig: Vec<u8>,
    pub kem: Vec<u8>,
    pub admin: bool,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Body {
    Genesis {
        root: Vec<u8>,
        recovery: Vec<u8>,
        generation: u64,
        roster: Vec<Member>,
        admin: Hash,
        epoch: u64,
        checkpoint: Option<Hash>,
    },
    Membership {
        generation: u64,
        sequence: u64,
        previous: Hash,
        roster: Vec<Member>,
        admin: Hash,
        epoch: u64,
        recovery: Vec<u8>,
    }, // empty closing frontier only
    Writer {
        manifest: Hash,
        document: [u8; 16],
        device: Hash,
        incarnation: Id,
        actor: u64,
        previous: Option<Id>,
    },
    Envelope {
        manifest: Hash,
        epoch: u64,
        recipient: Hash,
        key_fp: Hash,
        enc: Vec<u8>,
        ciphertext: Vec<u8>,
        authority: Hash,
    },
}
impl Body {
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Genesis { .. } => "genesis",
            Self::Membership { .. } => "membership",
            Self::Writer { .. } => "writer_start",
            Self::Envelope { .. } => "key_envelope",
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Message {
    pub route: Route,
    pub signer: Hash,
    pub body: Body,
}
fn roster(e: &mut Encoder<Vec<u8>>, members: &[Member]) -> Result<()> {
    e.array(members.len() as u64)?;
    for m in members {
        e.array(4)?
            .bytes(&m.id)?
            .bytes(&m.sig)?
            .bytes(&m.kem)?
            .u8(u8::from(m.admin))?;
    }
    Ok(())
}
fn optional<const N: usize>(e: &mut Encoder<Vec<u8>>, value: &Option<[u8; N]>) -> Result<()> {
    if let Some(bytes) = value {
        e.bytes(bytes)?;
    } else {
        e.null()?;
    }
    Ok(())
}
fn frame(e: &mut Encoder<Vec<u8>>, message: &Message) -> Result<()> {
    e.array(8)?
        .u8(0)?
        .str(wire::SUITE)?
        .str(message.body.kind())?
        .bytes(&message.route.app)?
        .bytes(&message.route.tenant)?
        .bytes(&message.route.collection)?
        .bytes(&message.signer)?;
    Ok(())
}
pub fn signing_bytes(message: &Message) -> Result<Vec<u8>> {
    let mut e = Encoder::new(Vec::new());
    frame(&mut e, message)?;
    match &message.body {
        Body::Genesis {
            root,
            recovery,
            generation,
            roster: members,
            admin,
            epoch,
            checkpoint,
        } => {
            e.array(7)?.bytes(root)?.bytes(recovery)?.u64(*generation)?;
            roster(&mut e, members)?;
            e.bytes(admin)?.u64(*epoch)?;
            optional(&mut e, checkpoint)?;
        }
        Body::Membership {
            generation,
            sequence,
            previous,
            roster: members,
            admin,
            epoch,
            recovery,
        } => {
            e.array(8)?
                .u64(*generation)?
                .u64(*sequence)?
                .bytes(previous)?;
            roster(&mut e, members)?;
            e.bytes(admin)?.u64(*epoch)?.bytes(recovery)?.array(0)?;
        }
        Body::Writer {
            manifest,
            document,
            device,
            incarnation,
            actor,
            previous,
        } => {
            e.array(6)?
                .bytes(manifest)?
                .bytes(document)?
                .bytes(device)?
                .bytes(incarnation)?
                .u64(*actor)?;
            optional(&mut e, previous)?;
        }
        Body::Envelope {
            manifest,
            epoch,
            recipient,
            key_fp,
            enc,
            ciphertext,
            authority,
        } => {
            e.array(8)?
                .bytes(manifest)?
                .u64(*epoch)?
                .bytes(recipient)?
                .bytes(key_fp)?
                .str(PURPOSE)?
                .bytes(enc)?
                .bytes(ciphertext)?
                .bytes(authority)?;
        }
    }
    Ok(e.into_writer())
}
pub fn package(message: &Message, signature: &[u8; wire::SIGNATURE_BYTES]) -> Result<Vec<u8>> {
    let mut prefix = Encoder::new(Vec::new());
    prefix.array(2)?;
    let mut result = prefix.into_writer();
    result.extend(signing_bytes(message)?);
    let mut e = Encoder::new(result);
    e.bytes(signature)?;
    Ok(e.into_writer())
}
pub fn sign(message: &Message, signer: &Device) -> Result<Vec<u8>> {
    let context = format!("matters-sync/v0/{}", message.body.kind());
    let sig = ml_dsa_65::sign(
        &signer.signing.signing_key,
        &signing_bytes(message)?,
        context.as_bytes(),
        random32()?,
    )
    .map_err(|_| Error::Crypto)?;
    package(message, sig.as_ref())
}
pub struct Parsed<'a> {
    pub message: Message,
    pub signed: &'a [u8],
    pub signature: &'a [u8; wire::SIGNATURE_BYTES],
}
struct Reader<'a>(Decoder<'a>);
impl<'a> Reader<'a> {
    fn array(&mut self, n: u64) -> Result<()> {
        if self.0.array()? != Some(n) {
            return Err(Error::Format);
        }
        Ok(())
    }
    fn fixed<const N: usize>(&mut self) -> Result<[u8; N]> {
        self.0.bytes()?.try_into().map_err(|_| Error::Format)
    }
    fn key(&mut self, n: usize) -> Result<Vec<u8>> {
        let v = self.0.bytes()?;
        if v.len() != n {
            return Err(Error::Format);
        }
        Ok(v.to_vec())
    }
    fn optional<const N: usize>(&mut self) -> Result<Option<[u8; N]>> {
        if self.0.datatype()? == minicbor::data::Type::Null {
            self.0.null()?;
            Ok(None)
        } else {
            Ok(Some(self.fixed()?))
        }
    }
    fn roster(&mut self) -> Result<Vec<Member>> {
        let n = self.0.array()?.ok_or(Error::Format)?;
        if n == 0 || n > MAX_DEVICES as u64 {
            return Err(Error::Limit);
        }
        let mut members = Vec::with_capacity(n as usize);
        for _ in 0..n {
            self.array(4)?;
            let id = self.fixed()?;
            let sig = self.key(SIG_PK)?;
            let kem = self.key(KEM_PK)?;
            let admin = match self.0.u8()? {
                0 => false,
                1 => true,
                _ => return Err(Error::Format),
            };
            members.push(Member {
                id,
                sig,
                kem,
                admin,
            });
        }
        Ok(members)
    }
}
pub fn parse(bytes: &[u8]) -> Result<Parsed<'_>> {
    if bytes.len() > MAX_CONTROL {
        return Err(Error::Limit);
    }
    let mut r = Reader(Decoder::new(bytes));
    r.array(2)?;
    let start = r.0.position();
    r.array(8)?;
    if r.0.u64()? != 0 || r.0.str()? != wire::SUITE {
        return Err(Error::Unsupported);
    }
    let kind = r.0.str()?;
    let route = Route {
        app: r.fixed()?,
        tenant: r.fixed()?,
        collection: r.fixed()?,
    };
    let signer = r.fixed()?;
    let body = match kind {
        "genesis" => {
            r.array(7)?;
            Body::Genesis {
                root: r.key(SIG_PK)?,
                recovery: r.key(KEM_PK)?,
                generation: r.0.u64()?,
                roster: r.roster()?,
                admin: r.fixed()?,
                epoch: r.0.u64()?,
                checkpoint: r.optional()?,
            }
        }
        "membership" => {
            r.array(8)?;
            let b = Body::Membership {
                generation: r.0.u64()?,
                sequence: r.0.u64()?,
                previous: r.fixed()?,
                roster: r.roster()?,
                admin: r.fixed()?,
                epoch: r.0.u64()?,
                recovery: r.key(KEM_PK)?,
            };
            r.array(0)?;
            b
        }
        "writer_start" => {
            r.array(6)?;
            Body::Writer {
                manifest: r.fixed()?,
                document: r.fixed()?,
                device: r.fixed()?,
                incarnation: r.fixed()?,
                actor: r.0.u64()?,
                previous: r.optional()?,
            }
        }
        "key_envelope" => {
            r.array(8)?;
            let manifest = r.fixed()?;
            let epoch = r.0.u64()?;
            let recipient = r.fixed()?;
            let key_fp = r.fixed()?;
            if r.0.str()? != PURPOSE {
                return Err(Error::Unsupported);
            }
            let enc = r.key(KEM_ENC)?;
            let ct = r.0.bytes()?;
            if !(16..=77).contains(&ct.len()) {
                return Err(Error::Limit);
            }
            let ciphertext = ct.to_vec();
            let authority = r.fixed()?;
            Body::Envelope {
                manifest,
                epoch,
                recipient,
                key_fp,
                enc,
                ciphertext,
                authority,
            }
        }
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
    if package(&message, signature)? != bytes {
        return Err(Error::NonCanonical);
    }
    Ok(Parsed {
        message,
        signed: &bytes[start..end],
        signature,
    })
}
fn verify_signature(
    pk: &[u8],
    signed: &[u8],
    context: &[u8],
    signature: &[u8; wire::SIGNATURE_BYTES],
) -> Result<()> {
    let pk = ml_dsa_65::MLDSA65VerificationKey::new(pk.try_into().map_err(|_| Error::Identity)?);
    let sig = ml_dsa_65::MLDSA65Signature::new(*signature);
    ml_dsa_65::verify(&pk, signed, context, &sig).map_err(|_| Error::Signature)
}
fn verify(parsed: &Parsed<'_>, pk: &[u8]) -> Result<()> {
    let ctx = format!("matters-sync/v0/{}", parsed.message.body.kind());
    verify_signature(pk, parsed.signed, ctx.as_bytes(), parsed.signature)
}
fn validate_roster(members: &[Member], admin: &Hash) -> Result<()> {
    if members.is_empty() || members.len() > MAX_DEVICES {
        return Err(Error::Limit);
    }
    if members.windows(2).any(|w| w[0].id >= w[1].id) {
        return Err(Error::Identity);
    }
    if members.iter().filter(|m| m.admin).count() != 1 {
        return Err(Error::Unauthorized);
    }
    for m in members {
        if device_id(&m.sig, &m.kem)? != m.id {
            return Err(Error::Identity);
        }
        if m.admin && &m.id != admin {
            return Err(Error::Unauthorized);
        }
    }
    Ok(())
}
#[derive(Clone)]
struct State {
    sequence: u64,
    epoch: u64,
    roster: Vec<Member>,
    admin: Hash,
    recovery: Vec<u8>,
}
#[derive(Clone)]
struct Writer {
    device: Hash,
    actor: u64,
    previous: Option<Id>,
    signed_bytes: Vec<u8>,
}
#[derive(Clone)]
struct WriterLineage {
    head: Id,
    fork_evidence: Option<(Vec<u8>, Vec<u8>)>,
}
#[derive(Clone)]
pub struct Authority {
    route: Route,
    current: Hash,
    states: BTreeMap<Hash, State>,
    successors: BTreeMap<Hash, Vec<u8>>,
    fork_evidence: Option<(Vec<u8>, Vec<u8>)>,
    writers: BTreeMap<([u8; 16], Id), Writer>,
    lineages: BTreeMap<([u8; 16], Hash), WriterLineage>,
}
impl Authority {
    pub fn route(&self) -> &Route {
        &self.route
    }
    pub fn bootstrap(bytes: &[u8], expected: Route, pinned_root: &[u8]) -> Result<Self> {
        let p = parse(bytes)?;
        if p.message.route != expected {
            return Err(Error::Context);
        }
        let Body::Genesis {
            root,
            recovery,
            generation,
            roster,
            admin,
            epoch,
            checkpoint,
        } = &p.message.body
        else {
            return Err(Error::Unsupported);
        };
        if root != pinned_root || p.message.signer != fingerprint("ML-DSA-65", pinned_root)? {
            return Err(Error::Identity);
        }
        verify(&p, pinned_root)?;
        validate_roster(roster, admin)?;
        if *generation != 0 || *epoch != 0 || checkpoint.is_some() {
            return Err(Error::Unsupported);
        }
        let current = hash(bytes);
        let state = State {
            sequence: 0,
            epoch: 0,
            roster: roster.clone(),
            admin: *admin,
            recovery: recovery.clone(),
        };
        Ok(Self {
            route: expected,
            current,
            states: BTreeMap::from([(current, state)]),
            successors: BTreeMap::new(),
            fork_evidence: None,
            writers: BTreeMap::new(),
            lineages: BTreeMap::new(),
        })
    }
    fn active(&self) -> Result<&State> {
        if self.fork_evidence.is_some() {
            return Err(Error::Fork);
        }
        self.states.get(&self.current).ok_or(Error::MissingPrevious)
    }
    pub fn manifest(&self) -> Hash {
        self.current
    }
    pub fn epoch(&self) -> Result<u64> {
        Ok(self.active()?.epoch)
    }
    pub fn admin(&self) -> Result<Hash> {
        Ok(self.active()?.admin)
    }
    pub fn members(&self) -> Result<Vec<Member>> {
        Ok(self.active()?.roster.clone())
    }
    pub fn recovery(&self) -> Result<Vec<u8>> {
        Ok(self.active()?.recovery.clone())
    }
    pub fn sequence(&self) -> Result<u64> {
        Ok(self.active()?.sequence)
    }
    pub fn is_forked(&self) -> bool {
        self.fork_evidence.is_some()
    }
    fn member(&self, id: &Hash) -> Result<&Member> {
        self.active()?
            .roster
            .iter()
            .find(|m| &m.id == id)
            .ok_or(Error::Unauthorized)
    }
    pub fn apply_membership(&mut self, bytes: &[u8]) -> Result<bool> {
        self.active()?;
        let p = parse(bytes)?;
        if p.message.route != self.route {
            return Err(Error::Context);
        }
        let Body::Membership {
            generation,
            sequence,
            previous,
            roster,
            admin,
            epoch,
            recovery,
        } = &p.message.body
        else {
            return Err(Error::Unsupported);
        };
        let old = self.states.get(previous).ok_or(Error::MissingPrevious)?;
        if p.message.signer != old.admin {
            return Err(Error::Unauthorized);
        }
        let signer = old
            .roster
            .iter()
            .find(|m| m.id == old.admin)
            .ok_or(Error::Unauthorized)?;
        verify(&p, &signer.sig)?;
        validate_roster(roster, admin)?;
        if *generation != 0
            || Some(*sequence) != old.sequence.checked_add(1)
            || Some(*epoch) != old.epoch.checked_add(1)
        {
            return Err(Error::Stale);
        }
        // This slice supports removal/rotation, not unimplemented enrollment,
        // admin transfer or recovery ceremonies. Never infer their approval.
        if *admin != old.admin
            || recovery != &old.recovery
            || roster.iter().any(|m| !old.roster.contains(m))
        {
            return Err(Error::Unsupported);
        }
        let candidate = hash(bytes);
        if self.states.contains_key(&candidate) {
            return Ok(false);
        }
        if let Some(first) = self.successors.get(previous) {
            self.fork_evidence = Some((first.clone(), bytes.to_vec()));
            return Err(Error::Fork);
        }
        // Every known non-head manifest already has a successor, handled above.
        if self.successors.len() >= MAX_TRANSITIONS {
            return Err(Error::Limit);
        }
        self.states.insert(
            candidate,
            State {
                sequence: *sequence,
                epoch: *epoch,
                roster: roster.clone(),
                admin: *admin,
                recovery: recovery.clone(),
            },
        );
        self.successors.insert(*previous, bytes.to_vec());
        self.current = candidate;
        Ok(true)
    }
    pub fn writer_fork(&self, document: [u8; 16], device: Hash) -> Option<(&[u8], &[u8])> {
        self.lineages
            .get(&(document, device))?
            .fork_evidence
            .as_ref()
            .map(|(first, second)| (first.as_slice(), second.as_slice()))
    }
    pub fn admit_writer(&mut self, bytes: &[u8], expected_document: [u8; 16]) -> Result<bool> {
        self.active()?;
        let p = parse(bytes)?;
        if p.message.route != self.route {
            return Err(Error::Context);
        }
        let Body::Writer {
            manifest,
            document,
            device,
            incarnation,
            actor,
            previous,
        } = &p.message.body
        else {
            return Err(Error::Unsupported);
        };
        if *document != expected_document {
            return Err(Error::Context);
        }
        if manifest != &self.current {
            return Err(Error::Stale);
        }
        if *device != p.message.signer {
            return Err(Error::Identity);
        }
        verify(&p, &self.member(device)?.sig)?;
        let lineage = (*document, *device);
        if self
            .lineages
            .get(&lineage)
            .is_some_and(|l| l.fork_evidence.is_some())
        {
            return Err(Error::Fork);
        }
        let index = (*document, *incarnation);
        if let Some(old) = self.writers.get(&index) {
            if old.signed_bytes == bytes {
                return Ok(false);
            }
            return Err(Error::Actor);
        }
        if self
            .writers
            .iter()
            .any(|((doc, _), w)| doc == document && w.actor == *actor)
        {
            return Err(Error::Actor);
        }
        if let Some(prev) = previous {
            let old = self
                .writers
                .get(&(*document, *prev))
                .ok_or(Error::MissingPrevious)?;
            if old.device != *device {
                return Err(Error::Actor);
            }
        }
        // Null is also a predecessor: there is one root per device/document.
        // Historical bindings are retained to detect competing successors even
        // after later replacements. A valid fork freezes only this lineage.
        if let Some((_, first)) = self
            .writers
            .iter()
            .find(|((doc, _), w)| doc == document && w.device == *device && w.previous == *previous)
        {
            self.lineages
                .get_mut(&lineage)
                .ok_or(Error::Actor)?
                .fork_evidence = Some((first.signed_bytes.clone(), bytes.to_vec()));
            return Err(Error::Fork);
        }
        if self.writers.len() >= MAX_WRITERS {
            return Err(Error::Limit);
        }
        self.writers.insert(
            index,
            Writer {
                device: *device,
                actor: *actor,
                previous: *previous,
                signed_bytes: bytes.to_vec(),
            },
        );
        self.lineages.insert(
            lineage,
            WriterLineage {
                head: *incarnation,
                fork_evidence: None,
            },
        );
        Ok(true)
    }
    pub fn wrap_key(&self, admin: &Device, recipient: Hash, key: &Id) -> Result<Vec<u8>> {
        if admin.id()? != self.admin()? {
            return Err(Error::Unauthorized);
        }
        let member = self.member(&recipient)?;
        let mut message = Message {
            route: self.route.clone(),
            signer: self.admin()?,
            body: Body::Envelope {
                manifest: self.current,
                epoch: self.epoch()?,
                recipient,
                key_fp: fingerprint(KEM_LABEL, &member.kem)?,
                enc: vec![],
                ciphertext: vec![],
                authority: self.admin()?,
            },
        };
        let info = envelope_info(&message)?;
        let aad = envelope_aad(&message)?;
        let mut e = Encoder::new(Vec::new());
        e.array(3)?.u64(self.epoch()?)?.bytes(key)?.str(POLICY)?;
        let (enc, ciphertext) = hpke()?
            .seal(
                &HpkePublicKey::from(member.kem.as_slice()),
                &info,
                &aad,
                &e.into_writer(),
                None,
                None,
                None,
            )
            .map_err(|_| Error::Crypto)?;
        if let Body::Envelope {
            enc: out,
            ciphertext: ct,
            ..
        } = &mut message.body
        {
            *out = enc;
            *ct = ciphertext;
        }
        sign(&message, admin)
    }
    pub fn unwrap_key(&self, bytes: &[u8], recipient: &Device) -> Result<EpochKey> {
        let state = self.active()?;
        let p = parse(bytes)?;
        if p.message.route != self.route {
            return Err(Error::Context);
        }
        let Body::Envelope {
            manifest,
            epoch,
            recipient: id,
            key_fp,
            enc,
            ciphertext,
            authority,
        } = &p.message.body
        else {
            return Err(Error::Unsupported);
        };
        if *manifest != self.current || *epoch != state.epoch {
            return Err(Error::Stale);
        }
        if p.message.signer != state.admin || *authority != state.admin {
            return Err(Error::Unauthorized);
        }
        verify(&p, &self.member(&state.admin)?.sig)?;
        let member = self.member(id)?;
        if *id != recipient.id()?
            || *key_fp != fingerprint(KEM_LABEL, &member.kem)?
            || member.kem != recipient.kem.public_key().as_slice()
        {
            return Err(Error::Identity);
        }
        let plaintext = hpke()?
            .open(
                enc,
                recipient.kem.private_key(),
                &envelope_info(&p.message)?,
                &envelope_aad(&p.message)?,
                ciphertext,
                None,
                None,
                None,
            )
            .map_err(|_| Error::Crypto)?;
        let mut r = Reader(Decoder::new(&plaintext));
        r.array(3)?;
        let inner_epoch = r.0.u64()?;
        let key = r.fixed()?;
        if r.0.str()? != POLICY || r.0.position() != plaintext.len() || inner_epoch != *epoch {
            return Err(Error::Format);
        }
        let mut e = Encoder::new(Vec::new());
        e.array(3)?.u64(inner_epoch)?.bytes(&key)?.str(POLICY)?;
        if e.into_writer() != plaintext {
            return Err(Error::NonCanonical);
        }
        Ok(EpochKey {
            route: self.route.clone(),
            manifest: *manifest,
            epoch: *epoch,
            key,
        })
    }
    /// First data record only, with a single admitted engine actor. Does not
    /// advance a receive journal or authorize snapshots/history continuation.
    pub fn open_first_data(
        &self,
        bytes: &[u8],
        key: &EpochKey,
        document: &[u8; 16],
    ) -> Result<Vec<u8>> {
        let state = self.active()?;
        let p = wire::parse(bytes, self.route.content(document)).map_err(|_| Error::Format)?;
        let h = p.header;
        if h.kind != wire::Kind::Data || h.previous.is_some() {
            return Err(Error::Unsupported);
        }
        if *h.manifest != self.current
            || h.epoch != state.epoch
            || key.route != self.route
            || key.manifest != self.current
            || key.epoch != state.epoch
        {
            return Err(Error::Stale);
        }
        let author = self.member(h.author)?;
        let writer = self
            .writers
            .get(&(*document, *h.incarnation))
            .ok_or(Error::Actor)?;
        if writer.device != *h.author {
            return Err(Error::Actor);
        }
        let lineage = self
            .lineages
            .get(&(*document, *h.author))
            .ok_or(Error::Actor)?;
        if lineage.fork_evidence.is_some() {
            return Err(Error::Fork);
        }
        if lineage.head != *h.incarnation {
            return Err(Error::Stale);
        }
        verify_signature(
            &author.sig,
            p.signed_bytes,
            h.kind.signature_context(),
            p.signature,
        )?;
        let derived = content_key(
            h.record_id,
            &key.key,
            &p.record_key_info().map_err(|_| Error::Format)?,
        )?;
        let plain = Provider::aead_open(
            AeadAlgorithm::Aes256Gcm,
            &derived,
            &[0; 12],
            p.header_bytes(),
            p.ciphertext,
        )
        .map_err(|_| Error::Crypto)?;
        let meta =
            loro::LoroDoc::decode_import_blob_meta(&plain, true).map_err(|_| Error::Format)?;
        if meta.mode != loro::EncodedBlobMode::Updates {
            return Err(Error::Unsupported);
        }
        if meta.partial_end_vv.len() != 1
            || meta.partial_end_vv.get(&writer.actor).copied().unwrap_or(0) <= 0
            || meta
                .partial_start_vv
                .get(&writer.actor)
                .copied()
                .unwrap_or(0)
                != 0
            || !meta.start_frontiers.is_empty()
        {
            return Err(Error::Actor);
        }
        // Metadata is not itself an authorization oracle. Confirm it against a
        // fresh engine import, including absence of unresolved causal material.
        let staged = loro::LoroDoc::new();
        staged.import(&plain).map_err(|_| Error::Format)?;
        if staged.oplog_vv() != meta.partial_end_vv
            || staged
                .export_pending_updates()
                .map_err(|_| Error::Format)?
                .is_some()
        {
            return Err(Error::Actor);
        }
        Ok(plain)
    }
}
pub struct EpochKey {
    route: Route,
    manifest: Hash,
    epoch: u64,
    key: Id,
}
pub fn envelope_info(message: &Message) -> Result<Vec<u8>> {
    let Body::Envelope { key_fp, .. } = &message.body else {
        return Err(Error::Unsupported);
    };
    let mut e = Encoder::new(Vec::new());
    e.array(6)?
        .str(wire::SUITE)?
        .bytes(&message.route.app)?
        .bytes(&message.route.tenant)?
        .bytes(&message.route.collection)?
        .str(PURPOSE)?
        .bytes(key_fp)?;
    Ok(e.into_writer())
}
pub fn envelope_aad(message: &Message) -> Result<Vec<u8>> {
    let Body::Envelope {
        manifest,
        epoch,
        recipient,
        key_fp,
        authority,
        ..
    } = &message.body
    else {
        return Err(Error::Unsupported);
    };
    let mut e = Encoder::new(Vec::new());
    frame(&mut e, message)?;
    e.array(6)?
        .bytes(manifest)?
        .u64(*epoch)?
        .bytes(recipient)?
        .bytes(key_fp)?
        .str(PURPOSE)?
        .bytes(authority)?;
    Ok(e.into_writer())
}
fn content_key(id: &Id, cek: &Id, info: &[u8]) -> Result<Vec<u8>> {
    record_crypto::content_key(id, cek, info).map_err(|_| Error::Crypto)
}
/// Test fixture builder ONLY: each call supplies a fresh random record ID.
/// This helper owns no reservation/retry state; the outbox harness supplies
/// committed reservations. It is not an application sealing API.
pub fn seal_fixture(
    h: wire::Header<'_>,
    cek: &Id,
    plaintext: &[u8],
    signer: &Device,
) -> Result<Vec<u8>> {
    let key = content_key(
        h.record_id,
        cek,
        &wire::record_key_info(h).map_err(|_| Error::Format)?,
    )?;
    let ct = Provider::aead_seal(
        AeadAlgorithm::Aes256Gcm,
        &key,
        &[0; 12],
        &wire::header_bytes(h).map_err(|_| Error::Format)?,
        plaintext,
    )
    .map_err(|_| Error::Crypto)?;
    let b = wire::signing_bytes(h, &ct).map_err(|_| Error::Format)?;
    let sig = ml_dsa_65::sign(
        &signer.signing.signing_key,
        &b,
        h.kind.signature_context(),
        random32()?,
    )
    .map_err(|_| Error::Crypto)?;
    wire::encode(h, &ct, sig.as_ref()).map_err(|_| Error::Format)
}
