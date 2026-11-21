//! The key-distribution authority (docs/SYNC_KEY_DISTRIBUTION.md,
//! docs/SYNC_WRITER_LIFECYCLE.md, sync-key-distribution-v0.cddl).
//!
//! [`Authority`] starts from a genesis signed by an independently pinned
//! root and admits, in memory, what that root's admin and roster sign:
//! membership transitions (enrollment, removal and epoch rotation), writer
//! admission and HPKE key envelopes, for the current epoch and, to current
//! members, for earlier ones. Two valid successors of one manifest
//! freeze the whole authority; two competing writer claims freeze that
//! device's lineage for the document. Every control is parsed, routed and
//! verified against authenticated state before it changes anything, and a
//! rejection leaves accepted state intact.
//!
//! A device joins through a one-time invitation (`enrollment`): the admin
//! accepts its signed, invitation-bound request and signs a transition that
//! adds it. Out of scope here, and refused rather than inferred: admin
//! transfer, recovery, nonempty closing frontiers and recovery-recipient
//! envelopes. Durable state, chain position and engine admission belong to
//! the receiver. Ported from the prototype unchanged in wire behavior and
//! budgets; the prototype's independent golden fixtures check it here too.

use std::collections::BTreeMap;

use crate::record::{self, ContentKey, VerificationKey};
use crate::wire;

mod enrollment;
mod envelope;
mod identity;
mod message;

pub use enrollment::{
    request_enrollment, Invitation, InvitationSecret, CODE_LENGTH, INVITATION_SECRET_BYTES,
};
#[doc(hidden)]
pub use envelope::{envelope_aad, envelope_info};
pub use envelope::{key_commitment, EpochKey, KEY_COMMITMENT_LABEL, POLICY};
pub use identity::{
    device_id, fingerprint, generate_kem_key, root_id, DeviceKeys, KemPrivateKey, KemPublicKey,
    Member, KEM_ENCAPSULATION_BYTES, KEM_KEY_LABEL, KEM_PUBLIC_KEY_BYTES, SIGNATURE_KEY_LABEL,
};
pub use message::{
    encode, parse, sign, signing_bytes, Body, Boundary, EnrollmentRequest, Genesis, KeyEnvelope,
    Membership, Message, ParsedControl, Route, WriterClaim, WriterHandoff, WriterStart,
    MAX_CONTROL_BYTES, MAX_ROSTER,
};

/// A SHA-384 hash or device ID.
pub type Hash = [u8; 48];
/// A random 32-byte identifier (incarnation, record or batch).
pub type Id = [u8; 32];

/// At most this many membership transitions per authority instance.
const MAX_TRANSITIONS: usize = 16;
/// At most this many admitted writer incarnations per authority instance.
const MAX_WRITERS: usize = 16;

/// Why a control message or record was refused. No variant carries key
/// material.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum Error {
    #[error("the message is malformed")]
    Format,
    #[error("the message is not canonically encoded")]
    NonCanonical,
    #[error("the message exceeds a size or count budget")]
    Limit,
    #[error("the message belongs to another collection or document")]
    Context,
    #[error("the signature does not verify")]
    Signature,
    #[error("a device or key identity does not match")]
    Identity,
    #[error("the signer has no authority for this message")]
    Unauthorized,
    #[error("the message names a manifest or epoch that is not current")]
    Stale,
    #[error("the predecessor is not known")]
    MissingPrevious,
    #[error("competing signed histories froze this authority or writer lineage")]
    Fork,
    #[error("the version, kind or transition is not supported")]
    Unsupported,
    #[error("the writer, actor or incarnation conflicts with an admitted writer")]
    Actor,
    #[error("the writer is not admitted")]
    MissingWriter,
    #[error("a cryptographic operation failed")]
    Crypto,
    /// The content key does not match the epoch's signed commitment.
    #[error("the content key does not match the epoch's signed commitment")]
    KeyCommitment,
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

fn validate_roster(members: &[Member], admin: &Hash) -> Result<()> {
    if members.is_empty() || members.len() > MAX_ROSTER {
        return Err(Error::Limit);
    }
    if members.windows(2).any(|w| w[0].id >= w[1].id) {
        return Err(Error::Identity);
    }
    if members.iter().filter(|m| m.admin).count() != 1 {
        return Err(Error::Unauthorized);
    }
    for m in members {
        if device_id(&m.signing_key, &m.kem_key)? != m.id {
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
    generation: u64,
    epoch: u64,
    key_commitment: Hash,
    roster: Vec<Member>,
    admin: Hash,
    recovery: KemPublicKey,
}

#[derive(Clone)]
struct Writer {
    claim: WriterClaim,
    signed_bytes: Vec<u8>,
}

#[derive(Clone)]
struct WriterLineage {
    head: Id,
    fork_evidence: Option<(Vec<u8>, Vec<u8>)>,
}

/// One admitted writer's lineage facts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WriterHistory {
    pub device: Hash,
    /// Previous writer and how it was retired.
    pub predecessor: Option<(Id, Boundary)>,
    /// None: active. Otherwise how its admitted successor retired it.
    pub closed: Option<Boundary>,
    pub frozen: bool,
}

/// The authenticated membership, epoch and writer state of one collection,
/// held in memory. Holds only public keys and signed bytes.
#[derive(Clone)]
pub struct Authority {
    route: Route,
    root: Hash,
    current: Hash,
    states: BTreeMap<Hash, State>,
    successors: BTreeMap<Hash, Vec<u8>>,
    fork_evidence: Option<(Vec<u8>, Vec<u8>)>,
    writers: BTreeMap<([u8; 16], Id), Writer>,
    lineages: BTreeMap<([u8; 16], Hash), WriterLineage>,
}

impl std::fmt::Debug for Authority {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Authority")
            .field("route", &self.route)
            .field("manifest", &self.current)
            .field("forked", &self.is_forked())
            .finish_non_exhaustive()
    }
}

impl Authority {
    /// Start from `genesis`, which must be signed by `pinned_root` for the
    /// `expected` route. Both arrive through a trusted path independent of
    /// the genesis itself: self-signing establishes nothing.
    pub fn bootstrap(
        genesis: &[u8],
        expected: Route,
        pinned_root: &VerificationKey,
    ) -> Result<Self> {
        let p = parse(genesis)?;
        if p.message.route != expected {
            return Err(Error::Context);
        }
        let Body::Genesis(g) = &p.message.body else {
            return Err(Error::Unsupported);
        };
        if g.root_key.as_bytes() != pinned_root.as_bytes()
            || p.message.signer != root_id(pinned_root)?
        {
            return Err(Error::Identity);
        }
        p.verify(pinned_root)?;
        validate_roster(&g.roster, &g.admin)?;
        if g.generation != 0 || g.epoch != 0 || g.checkpoint.is_some() {
            return Err(Error::Unsupported);
        }
        let current = identity::sha384(genesis);
        let state = State {
            sequence: 0,
            generation: 0,
            epoch: 0,
            key_commitment: g.key_commitment,
            roster: g.roster.clone(),
            admin: g.admin,
            recovery: g.recovery_key.clone(),
        };
        Ok(Self {
            route: expected,
            root: p.message.signer,
            current,
            states: BTreeMap::from([(current, state)]),
            successors: BTreeMap::new(),
            fork_evidence: None,
            writers: BTreeMap::new(),
            lineages: BTreeMap::new(),
        })
    }

    /// Start a newly enrolling device from `genesis`, pinning the root by
    /// the ID its invitation carries: the genesis's root key must hash to
    /// it, and the route must be the invitation's. The invitation is the
    /// trusted path here.
    pub fn bootstrap_from_invitation(genesis: &[u8], invitation: &Invitation) -> Result<Self> {
        let p = parse(genesis)?;
        let Body::Genesis(g) = &p.message.body else {
            return Err(Error::Unsupported);
        };
        if root_id(&g.root_key)? != invitation.root {
            return Err(Error::Identity);
        }
        Self::bootstrap(genesis, invitation.route.clone(), &g.root_key)
    }

    pub fn route(&self) -> &Route {
        &self.route
    }

    /// The pinned root's signer ID.
    pub fn root(&self) -> Hash {
        self.root
    }

    fn active(&self) -> Result<&State> {
        if self.fork_evidence.is_some() {
            return Err(Error::Fork);
        }
        self.states.get(&self.current).ok_or(Error::MissingPrevious)
    }

    /// The current manifest hash: SHA-384 of its complete signed record.
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

    pub fn recovery(&self) -> Result<KemPublicKey> {
        Ok(self.active()?.recovery.clone())
    }

    pub fn sequence(&self) -> Result<u64> {
        Ok(self.active()?.sequence)
    }

    pub fn is_forked(&self) -> bool {
        self.fork_evidence.is_some()
    }

    /// The two competing membership successors that froze this authority.
    pub fn fork_evidence(&self) -> Option<(&[u8], &[u8])> {
        self.fork_evidence
            .as_ref()
            .map(|(first, second)| (first.as_slice(), second.as_slice()))
    }

    /// A one-time invitation into this collection at the current manifest.
    /// `secret` must be fresh OS entropy; the caller shows the invitation
    /// once and forgets it after one accepted request.
    pub fn invite(&self, secret: InvitationSecret) -> Result<Invitation> {
        self.active()?;
        Ok(Invitation {
            route: self.route.clone(),
            root: self.root,
            manifest: self.current,
            secret,
        })
    }

    /// Check a new device's request against the invitation the admin
    /// issued, and return the roster entry to add in the next transition.
    /// The invitation must be for this route, root and current manifest;
    /// the request must be signed by the key it names (possession), carry
    /// the invitation's binding (the user saw the secret) and name a device
    /// that is not yet a member. Nothing changes until the admin signs and
    /// applies the transition.
    pub fn accept_enrollment(&self, bytes: &[u8], invitation: &Invitation) -> Result<Member> {
        let state = self.active()?;
        if invitation.route != self.route || invitation.root != self.root {
            return Err(Error::Context);
        }
        if invitation.manifest != self.current {
            return Err(Error::Stale);
        }
        let p = parse(bytes)?;
        if p.message.route != self.route {
            return Err(Error::Context);
        }
        let Body::EnrollmentRequest(r) = &p.message.body else {
            return Err(Error::Unsupported);
        };
        if r.manifest != invitation.manifest {
            return Err(Error::Stale);
        }
        let device = device_id(&r.signing_key, &r.kem_key)?;
        if p.message.signer != device {
            return Err(Error::Identity);
        }
        p.verify(&r.signing_key)?;
        if !enrollment::tags_match(&invitation.binding(&device, r)?, &r.binding) {
            return Err(Error::Unauthorized);
        }
        if state.roster.iter().any(|m| m.id == device) {
            return Err(Error::Identity);
        }
        if state.roster.len() >= MAX_ROSTER {
            return Err(Error::Limit);
        }
        Ok(Member {
            id: device,
            signing_key: r.signing_key.clone(),
            kem_key: r.kem_key.clone(),
            admin: false,
        })
    }

    fn member(&self, id: &Hash) -> Result<&Member> {
        self.active()?
            .roster
            .iter()
            .find(|m| &m.id == id)
            .ok_or(Error::Unauthorized)
    }

    /// Admit a membership transition. `Ok(true)` advances the manifest,
    /// `Ok(false)` is an exact replay of an accepted transition. A second
    /// valid successor of one manifest retains both and returns `Fork`.
    pub fn apply_membership(&mut self, bytes: &[u8]) -> Result<bool> {
        self.active()?;
        let p = parse(bytes)?;
        if p.message.route != self.route {
            return Err(Error::Context);
        }
        let Body::Membership(m) = &p.message.body else {
            return Err(Error::Unsupported);
        };
        let old = self.states.get(&m.previous).ok_or(Error::MissingPrevious)?;
        if p.message.signer != old.admin {
            return Err(Error::Unauthorized);
        }
        let signer = old
            .roster
            .iter()
            .find(|member| member.id == old.admin)
            .ok_or(Error::Unauthorized)?;
        p.verify(&signer.signing_key)?;
        validate_roster(&m.roster, &m.admin)?;
        if m.generation != 0
            || Some(m.sequence) != old.sequence.checked_add(1)
            || Some(m.epoch) != old.epoch.checked_add(1)
        {
            return Err(Error::Stale);
        }
        // The admin may add members (enrollment) and remove them, but this
        // version supports no admin transfer or recovery-key change. Never
        // infer their approval. A kept member's ID binds its keys, and the
        // one-admin rule fixes every role, so no kept entry can change.
        if m.admin != old.admin || m.recovery_key != old.recovery {
            return Err(Error::Unsupported);
        }
        let candidate = identity::sha384(bytes);
        if self.states.contains_key(&candidate) {
            return Ok(false);
        }
        if let Some(first) = self.successors.get(&m.previous) {
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
                sequence: m.sequence,
                generation: m.generation,
                epoch: m.epoch,
                key_commitment: m.key_commitment,
                roster: m.roster.clone(),
                admin: m.admin,
                recovery: m.recovery_key.clone(),
            },
        );
        self.successors.insert(m.previous, bytes.to_vec());
        self.current = candidate;
        Ok(true)
    }

    /// The competing signed writer claims that froze a device's lineage.
    pub fn writer_fork(&self, document: [u8; 16], device: Hash) -> Option<(&[u8], &[u8])> {
        self.lineages
            .get(&(document, device))?
            .fork_evidence
            .as_ref()
            .map(|(first, second)| (first.as_slice(), second.as_slice()))
    }

    /// Admit a `writer_start` or `writer_handoff` for `expected_document`.
    /// `Ok(true)` admits it, `Ok(false)` is the same claim again (even
    /// re-signed). Competing same-device claims freeze the lineage.
    pub fn admit_writer(&mut self, bytes: &[u8], expected_document: [u8; 16]) -> Result<bool> {
        self.active()?;
        let p = parse(bytes)?;
        if p.message.route != self.route {
            return Err(Error::Context);
        }
        let claim = p.message.body.writer_claim().ok_or(Error::Unsupported)?;
        let document = claim.document;
        if document != expected_document {
            return Err(Error::Context);
        }
        if claim.manifest != self.current {
            return Err(Error::Stale);
        }
        if claim.device != p.message.signer {
            return Err(Error::Identity);
        }
        p.verify(&self.member(&claim.device)?.signing_key)?;
        let lineage = (document, claim.device);
        if self
            .lineages
            .get(&lineage)
            .is_some_and(|l| l.fork_evidence.is_some())
        {
            return Err(Error::Fork);
        }
        let index = (document, claim.incarnation);
        if let Some(old) = self.writers.get(&index) {
            // ML-DSA signing is randomized: a re-signed identical claim is the
            // same lineage step, not a contradiction. The first bytes stay.
            if old.claim == claim {
                return Ok(false);
            }
            if old.claim.device == claim.device {
                let first = old.signed_bytes.clone();
                return Err(self.freeze_writer(lineage, first, bytes));
            }
            // A different device cannot freeze the owner by claiming its ID.
            return Err(Error::Actor);
        }
        let predecessor = claim.previous.map(|(prev, _)| prev);
        if let Some(prev) = predecessor {
            let old = self
                .writers
                .get(&(document, prev))
                .ok_or(Error::MissingPrevious)?;
            if old.claim.device != claim.device {
                return Err(Error::Actor);
            }
        }
        // Same-device conflicting successor claims are evidence even when
        // they reuse an actor. Never let an arrival-dependent uniqueness check
        // hide two authenticated definitions of the same lineage step.
        if let Some(first) = self.writers.iter().find_map(|((doc, _), w)| {
            (*doc == document
                && w.claim.device == claim.device
                && w.claim.previous.map(|(prev, _)| prev) == predecessor)
                .then(|| w.signed_bytes.clone())
        }) {
            return Err(self.freeze_writer(lineage, first, bytes));
        }
        if self
            .writers
            .iter()
            .any(|((doc, _), w)| *doc == document && w.claim.actor == claim.actor)
        {
            return Err(Error::Actor);
        }
        if self.writers.len() >= MAX_WRITERS {
            return Err(Error::Limit);
        }
        self.writers.insert(
            index,
            Writer {
                claim,
                signed_bytes: bytes.to_vec(),
            },
        );
        self.lineages.insert(
            lineage,
            WriterLineage {
                head: claim.incarnation,
                fork_evidence: None,
            },
        );
        Ok(true)
    }

    /// Record competing signed definitions; the result is always `Fork`.
    fn freeze_writer(&mut self, lineage: ([u8; 16], Hash), first: Vec<u8>, second: &[u8]) -> Error {
        match self.lineages.get_mut(&lineage) {
            Some(l) => {
                l.fork_evidence = Some((first, second.to_vec()));
                Error::Fork
            }
            // Unreachable: both callers found an admitted writer of this lineage.
            None => Error::Actor,
        }
    }

    /// Whether a device's writer lineage for this document holds fork evidence.
    pub fn lineage_frozen(&self, document: [u8; 16], device: &Hash) -> bool {
        self.lineages
            .get(&(document, *device))
            .is_some_and(|l| l.fork_evidence.is_some())
    }

    pub fn writer_history(&self, document: [u8; 16], incarnation: Id) -> Result<WriterHistory> {
        self.writer_histories(document)
            .remove(&incarnation)
            .ok_or(Error::MissingWriter)
    }

    /// Every admitted writer's lineage facts for one document, in one pass.
    pub fn writer_histories(&self, document: [u8; 16]) -> BTreeMap<Id, WriterHistory> {
        let mut out: BTreeMap<Id, WriterHistory> = self
            .writers
            .iter()
            .filter(|((doc, _), _)| *doc == document)
            .map(|((_, incarnation), w)| {
                (
                    *incarnation,
                    WriterHistory {
                        device: w.claim.device,
                        predecessor: w.claim.previous,
                        closed: None,
                        frozen: self.lineage_frozen(document, &w.claim.device),
                    },
                )
            })
            .collect();
        for w in self
            .writers
            .values()
            .filter(|w| w.claim.document == document)
        {
            if let Some((prev, boundary)) = w.claim.previous {
                if let Some(h) = out.get_mut(&prev) {
                    h.closed = Some(boundary);
                }
            }
        }
        out
    }

    /// Wrap the current epoch's content key for `recipient`, a current
    /// member, as the current `admin`. The recipient's KEM key comes from
    /// the verified roster, never from the caller. Each call uses a fresh
    /// HPKE context; `signing_randomness` must be fresh OS entropy.
    pub fn wrap_key(
        &self,
        admin: &DeviceKeys,
        recipient: Hash,
        key: &ContentKey,
        signing_randomness: [u8; 32],
    ) -> Result<Vec<u8>> {
        self.wrap(admin, recipient, self.current, key, signing_randomness)
    }

    /// Wrap the content key of an earlier epoch, named by its `manifest`,
    /// for `recipient`, a current member: retained history for an enrolled
    /// device. Signed by the current admin; the key must match that epoch's
    /// signed commitment. A removed device can never be a recipient.
    pub fn wrap_history_key(
        &self,
        admin: &DeviceKeys,
        recipient: Hash,
        manifest: Hash,
        key: &ContentKey,
        signing_randomness: [u8; 32],
    ) -> Result<Vec<u8>> {
        self.wrap(admin, recipient, manifest, key, signing_randomness)
    }

    fn wrap(
        &self,
        admin: &DeviceKeys,
        recipient: Hash,
        manifest: Hash,
        key: &ContentKey,
        signing_randomness: [u8; 32],
    ) -> Result<Vec<u8>> {
        if admin.id() != self.admin()? {
            return Err(Error::Unauthorized);
        }
        let member = self.member(&recipient)?;
        let state = self.states.get(&manifest).ok_or(Error::MissingPrevious)?;
        self.check_commitment(state, key)?;
        let mut envelope = KeyEnvelope {
            manifest,
            epoch: state.epoch,
            recipient,
            recipient_key_fingerprint: fingerprint(KEM_KEY_LABEL, member.kem_key.as_bytes())?,
            encapsulation: vec![],
            ciphertext: vec![],
            authority: self.admin()?,
        };
        let unsealed = Message {
            route: self.route.clone(),
            signer: self.admin()?,
            body: Body::KeyEnvelope(envelope.clone()),
        };
        let info = envelope_info(&unsealed)?;
        let aad = envelope_aad(&unsealed)?;
        let mut plaintext = envelope::key_plaintext(envelope.epoch, key)?;
        let sealed = identity::hpke()?.seal(
            &hpke_rs::HpkePublicKey::from(member.kem_key.as_bytes()),
            &info,
            &aad,
            &plaintext,
            None,
            None,
            None,
        );
        envelope::wipe(&mut plaintext);
        let (encapsulation, ciphertext) = sealed.map_err(|_| Error::Crypto)?;
        envelope.encapsulation = encapsulation;
        envelope.ciphertext = ciphertext;
        let message = Message {
            body: Body::KeyEnvelope(envelope),
            ..unsealed
        };
        sign(&message, admin.signing_key(), signing_randomness)
    }

    /// Unwrap an envelope for `recipient`. Route, current manifest and
    /// epoch, admin authority and signature, and the recipient's identity
    /// are all checked before HPKE runs; the key must then match the epoch's
    /// signed commitment.
    pub fn unwrap_key(&self, bytes: &[u8], recipient: &DeviceKeys) -> Result<EpochKey> {
        self.unwrap(bytes, recipient, false)
    }

    /// Unwrap an envelope of the current or an earlier epoch, as
    /// [`Self::wrap_history_key`] makes them. The same checks apply as for
    /// [`Self::unwrap_key`], except that the envelope may name any known
    /// manifest: its signer must be the current admin, its recipient a
    /// current member, and the key must match that epoch's commitment. The
    /// key it returns is bound to that epoch, so it seals nothing.
    pub fn unwrap_history_key(&self, bytes: &[u8], recipient: &DeviceKeys) -> Result<EpochKey> {
        self.unwrap(bytes, recipient, true)
    }

    fn unwrap(&self, bytes: &[u8], recipient: &DeviceKeys, history: bool) -> Result<EpochKey> {
        let current = self.active()?;
        let p = parse(bytes)?;
        if p.message.route != self.route {
            return Err(Error::Context);
        }
        let Body::KeyEnvelope(k) = &p.message.body else {
            return Err(Error::Unsupported);
        };
        let state = match self.states.get(&k.manifest) {
            Some(state) if history || k.manifest == self.current => state,
            _ => return Err(Error::Stale),
        };
        if k.epoch != state.epoch {
            return Err(Error::Stale);
        }
        if p.message.signer != current.admin || k.authority != current.admin {
            return Err(Error::Unauthorized);
        }
        p.verify(&self.member(&current.admin)?.signing_key)?;
        let member = self.member(&k.recipient)?;
        if k.recipient != recipient.id()
            || k.recipient_key_fingerprint != fingerprint(KEM_KEY_LABEL, member.kem_key.as_bytes())?
            || &member.kem_key != recipient.kem_public_key()
        {
            return Err(Error::Identity);
        }
        let mut plaintext = identity::hpke()?
            .open(
                &k.encapsulation,
                recipient.kem_private_key().hpke(),
                &envelope_info(&p.message)?,
                &envelope_aad(&p.message)?,
                &k.ciphertext,
                None,
                None,
                None,
            )
            .map_err(|_| Error::Crypto)?;
        let key = envelope::open_key_plaintext(&plaintext, k.epoch);
        envelope::wipe(&mut plaintext);
        let key = key?;
        // Every member checks the same signed commitment, so an admin cannot
        // hand different devices different content keys for one epoch.
        self.check_commitment(state, &key)?;
        Ok(EpochKey {
            route: self.route.clone(),
            manifest: k.manifest,
            epoch: k.epoch,
            key,
        })
    }

    fn check_commitment(&self, state: &State, key: &ContentKey) -> Result<()> {
        if key_commitment(&self.route, state.generation, state.epoch, key)? != state.key_commitment
        {
            return Err(Error::KeyCommitment);
        }
        Ok(())
    }

    /// Whether `key` belongs to the current route, manifest and epoch.
    pub fn check_epoch_key(&self, key: &EpochKey) -> Result<()> {
        let state = self.active()?;
        if key.route != self.route || key.manifest != self.current || key.epoch != state.epoch {
            return Err(Error::Stale);
        }
        Ok(())
    }

    /// Authenticate a `data` record's context, current membership and active
    /// writer before decryption. Chain position and engine admission belong
    /// to the durable receiver.
    pub fn authenticate_data<'a>(
        &self,
        bytes: &'a [u8],
        key: &'a EpochKey,
        document: &[u8; 16],
    ) -> Result<AuthenticatedData<'a>> {
        self.authenticate_record(bytes, key, document, true)
    }

    /// Signature facts for admitted active or retired writers. The receiver
    /// must independently enforce closing prefixes and frozen lineages.
    pub fn authenticate_history<'a>(
        &self,
        bytes: &'a [u8],
        key: &'a EpochKey,
        document: &[u8; 16],
    ) -> Result<AuthenticatedData<'a>> {
        self.authenticate_record(bytes, key, document, false)
    }

    fn authenticate_record<'a>(
        &self,
        bytes: &'a [u8],
        key: &'a EpochKey,
        document: &[u8; 16],
        active_only: bool,
    ) -> Result<AuthenticatedData<'a>> {
        self.check_epoch_key(key)?;
        let state = self.active()?;
        let p = wire::parse(bytes, self.route.content(document)).map_err(|_| Error::Format)?;
        let h = p.header;
        if h.kind != wire::Kind::Data {
            return Err(Error::Unsupported);
        }
        if *h.manifest != self.current || h.epoch != state.epoch {
            return Err(Error::Stale);
        }
        let author = self.member(h.author)?;
        let Some(writer) = self.writers.get(&(*document, *h.incarnation)) else {
            if active_only {
                return Err(Error::Actor);
            }
            // Only a current member's authenticated record may wait for its
            // writer admission; forged or unsigned bytes are rejected now.
            verify_record(p, &author.signing_key)?;
            return Err(Error::MissingWriter);
        };
        if writer.claim.device != *h.author {
            return Err(Error::Actor);
        }
        let lineage = self
            .lineages
            .get(&(*document, *h.author))
            .ok_or(Error::Actor)?;
        if active_only && lineage.fork_evidence.is_some() {
            return Err(Error::Fork);
        }
        if active_only && lineage.head != *h.incarnation {
            return Err(Error::Stale);
        }
        Ok(AuthenticatedData {
            verified: verify_record(p, &author.signing_key)?,
            key,
            actor: writer.claim.actor,
        })
    }
}

fn verify_record<'a>(
    p: wire::ParsedRecord<'a>,
    author: &VerificationKey,
) -> Result<record::Verified<'a>> {
    record::verify(p, author).map_err(|e| match e {
        record::Error::Signature => Error::Signature,
        _ => Error::Crypto,
    })
}

/// A verified signed record. Construction is private to [`Authority`];
/// retaining this value permits fork detection even if decryption fails.
#[derive(Debug)]
pub struct AuthenticatedData<'a> {
    verified: record::Verified<'a>,
    key: &'a EpochKey,
    actor: u64,
}

impl<'a> AuthenticatedData<'a> {
    pub fn parsed(&self) -> &wire::ParsedRecord<'a> {
        self.verified.record()
    }

    /// The engine actor its admitted writer pinned.
    pub fn actor(&self) -> u64 {
        self.actor
    }

    pub fn open(&self) -> Result<Vec<u8>> {
        self.verified.open(&self.key.key).map_err(|e| match e {
            record::Error::Format(_) => Error::Format,
            _ => Error::Crypto,
        })
    }
}
