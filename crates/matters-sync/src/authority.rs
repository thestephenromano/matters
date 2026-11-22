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
//! adds it. After every device is lost, the root key derived from the
//! recovery words (`recovery`) signs a `root_recovery` that starts the next
//! membership generation with one admin; transitions of earlier
//! generations are then stale. Recovery envelopes carry each epoch's key to
//! the recovery KEM key.
//!
//! Revocation is by closing frontier: a transition that removes a device,
//! and a recovery that supersedes a generation, signs for each writer of a
//! leaving device the exact content prefix it retains. A device outside the
//! current roster then claims no new writer, and its records are admitted
//! only within a signed closing prefix. Out of scope here, and refused
//! rather than inferred: admin transfer. Durable state, chain position and
//! engine admission belong to the receiver. The prototype's independent
//! golden fixtures check the codec here too.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use crate::record::{self, ContentKey, VerificationKey};
use crate::wire;

mod enrollment;
mod envelope;
mod identity;
mod message;
mod recovery;
mod session;

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
    encode, parse, sign, signing_bytes, Body, Boundary, Closing, EnrollmentRequest, Genesis,
    KeyEnvelope, Membership, Message, ParsedControl, Route, WriterClaim, WriterHandoff,
    WriterStart, MAX_CONTROL_BYTES, MAX_FRONTIER, MAX_ROSTER,
};
#[doc(hidden)]
pub use recovery::recovery_seeds;
pub use recovery::{genesis, RecoveryKeys, RecoverySecret, RECOVERY_WORDS};
pub use session::{session_bytes, sign_root_session, sign_session, verify_session, SESSION_LABEL};

/// A SHA-384 hash or device ID.
pub type Hash = [u8; 48];
/// A random 32-byte identifier (incarnation, record or batch).
pub type Id = [u8; 32];

// There is no lifetime cap on transitions, recoveries or writer
// incarnations: each is a signed control the device keeps anyway. Each
// object is bounded (roster, frontier, control bytes), and a device has at
// most one active writer per document, since a second root or a second
// successor of one incarnation freezes its lineage. A handed-off
// incarnation is closed and no longer active.

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
    /// The author is not in the current roster, and the record or writer
    /// claim lies outside every signed closing prefix. Signatures are
    /// verified before this is returned. A later transition of the admin
    /// may still name the writer's closing prefix.
    #[error("the author was removed, and this lies outside its signed closing prefix")]
    Revoked,
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

/// A transition's closing frontier names, in strict order, only devices
/// that leave or left the roster `next` starts; never a current member.
fn validate_frontier(frontier: &[Closing], next: &State) -> Result<()> {
    let key = |c: &Closing| (c.device, c.document, c.incarnation);
    if frontier.windows(2).any(|w| key(&w[0]) >= key(&w[1])) {
        return Err(Error::Identity);
    }
    for c in frontier {
        if next.has(&c.device) {
            return Err(Error::Unsupported);
        }
        if !next.retired.contains(&c.device) {
            return Err(Error::Identity);
        }
    }
    Ok(())
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
    /// Shared with the predecessor's when unchanged.
    roster: Arc<Vec<Member>>,
    admin: Hash,
    recovery: KemPublicKey,
    /// Devices that were members of an earlier manifest in this state's
    /// history and are not members here. None of them can be added back.
    retired: Arc<BTreeSet<Hash>>,
}

impl State {
    fn has(&self, device: &Hash) -> bool {
        self.roster.iter().any(|m| &m.id == device)
    }

    /// The state a transition or recovery to `m` starts from `self`.
    fn successor(&self, m: &Membership) -> State {
        let roster = if *self.roster == m.roster {
            self.roster.clone()
        } else {
            Arc::new(m.roster.clone())
        };
        let leaving: Vec<Hash> = self
            .roster
            .iter()
            .map(|member| member.id)
            .filter(|id| !m.roster.iter().any(|n| &n.id == id))
            .collect();
        let retired = if leaving.is_empty() {
            self.retired.clone()
        } else {
            let mut set = (*self.retired).clone();
            set.extend(leaving);
            Arc::new(set)
        };
        State {
            sequence: m.sequence,
            generation: m.generation,
            epoch: m.epoch,
            key_commitment: m.key_commitment,
            roster,
            admin: m.admin,
            recovery: m.recovery_key.clone(),
            retired,
        }
    }
}

/// How far an admitted writer's records are admitted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Admission {
    /// Its device is a current member: the receiver applies its chain,
    /// bounded only by its own handoff's closing head.
    Open,
    /// A removed device's writer, up to this exact signed closing head.
    Closed(Option<Hash>),
    /// A removed device's writer outside every signed closing prefix.
    Revoked,
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

/// A content record whose author's signature verified, not yet admitted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SignedRecord {
    pub author: Hash,
    pub incarnation: Id,
    pub previous: Option<Hash>,
    /// SHA-384 of the complete record: its identity in the chain.
    pub hash: Hash,
}

/// One admitted writer's lineage facts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WriterHistory {
    pub device: Hash,
    /// The engine actor the writer's claim pinned.
    pub actor: u64,
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
    root_key: VerificationKey,
    /// The accepted `root_recovery` of each generation it started.
    recoveries: BTreeMap<u64, Vec<u8>>,
    current: Hash,
    states: BTreeMap<Hash, State>,
    successors: BTreeMap<Hash, Vec<u8>>,
    fork_evidence: Option<(Vec<u8>, Vec<u8>)>,
    writers: BTreeMap<([u8; 16], Id), Writer>,
    lineages: BTreeMap<([u8; 16], Hash), WriterLineage>,
    /// Each admitted claim by (document, device, predecessor): a second
    /// claim of one step is a competing definition.
    steps: BTreeMap<([u8; 16], Hash, Option<Id>), Id>,
    /// The admitted successor of each retired incarnation.
    successor_of: BTreeMap<([u8; 16], Id), Id>,
    /// Engine actors reserved per document.
    actors: BTreeSet<([u8; 16], u64)>,
    /// Signed closing-frontier entries by (document, incarnation): the
    /// device and the exact retained head. The first signed entry binds.
    closures: BTreeMap<([u8; 16], Id), (Hash, Option<Hash>)>,
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
            roster: Arc::new(g.roster.clone()),
            admin: g.admin,
            recovery: g.recovery_key.clone(),
            retired: Arc::new(BTreeSet::new()),
        };
        Ok(Self {
            route: expected,
            root: p.message.signer,
            root_key: pinned_root.clone(),
            recoveries: BTreeMap::new(),
            current,
            states: BTreeMap::from([(current, state)]),
            successors: BTreeMap::new(),
            fork_evidence: None,
            writers: BTreeMap::new(),
            lineages: BTreeMap::new(),
            steps: BTreeMap::new(),
            successor_of: BTreeMap::new(),
            actors: BTreeSet::new(),
            closures: BTreeMap::new(),
        })
    }

    /// Start a newly enrolling device from `genesis`, pinning the root by
    /// the ID its invitation carries: the genesis's root key must hash to
    /// it, and the route must be the invitation's. The invitation is the
    /// trusted path here.
    pub fn bootstrap_from_invitation(genesis: &[u8], invitation: &Invitation) -> Result<Self> {
        Self::bootstrap_by_root_id(genesis, invitation.route.clone(), invitation.root)
    }

    /// Start from `genesis`, pinning the root by its signer ID: the
    /// genesis's root key must hash to `root`. For a device that stored
    /// the ID when it first pinned the root (an invitation, or its own
    /// genesis), so the ID is the trusted path.
    pub fn bootstrap_by_root_id(genesis: &[u8], expected: Route, root: Hash) -> Result<Self> {
        let p = parse(genesis)?;
        let Body::Genesis(g) = &p.message.body else {
            return Err(Error::Unsupported);
        };
        if root_id(&g.root_key)? != root {
            return Err(Error::Identity);
        }
        Self::bootstrap(genesis, expected, &g.root_key)
    }

    pub fn route(&self) -> &Route {
        &self.route
    }

    /// The pinned root's signer ID.
    pub fn root(&self) -> Hash {
        self.root
    }

    /// The pinned root's verification key.
    pub fn root_key(&self) -> &VerificationKey {
        &self.root_key
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
        Ok(self.active()?.roster.to_vec())
    }

    pub fn recovery(&self) -> Result<KemPublicKey> {
        Ok(self.active()?.recovery.clone())
    }

    pub fn sequence(&self) -> Result<u64> {
        Ok(self.active()?.sequence)
    }

    /// The membership generation: 0 from genesis, one more per recovery.
    pub fn generation(&self) -> Result<u64> {
        Ok(self.active()?.generation)
    }

    /// The generation of the newest accepted manifest, even while frozen.
    fn top_generation(&self) -> u64 {
        self.states
            .get(&self.current)
            .map_or(0, |state| state.generation)
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
        // A removed device's ID never returns: rejoining is a fresh
        // enrollment under new keys.
        if state.has(&device) || state.retired.contains(&device) {
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
        let candidate = identity::sha384(bytes);
        if self.states.contains_key(&candidate) {
            return Ok(false);
        }
        // A recovery closed every earlier generation: its admins can no
        // longer change membership, whatever they sign.
        if old.generation != self.top_generation()
            || m.generation != old.generation
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
        // A removed device is never added back under the same ID.
        if m.roster.iter().any(|n| old.retired.contains(&n.id)) {
            return Err(Error::Unsupported);
        }
        let next = old.successor(m);
        validate_frontier(&m.frontier, &next)?;
        if let Some(first) = self.successors.get(&m.previous) {
            self.fork_evidence = Some((first.clone(), bytes.to_vec()));
            return Err(Error::Fork);
        }
        self.states.insert(candidate, next);
        self.successors.insert(m.previous, bytes.to_vec());
        self.close(&m.frontier);
        self.current = candidate;
        Ok(true)
    }

    /// Record a frontier's closing prefixes. The first signed entry for an
    /// incarnation binds; a later one changes nothing.
    fn close(&mut self, frontier: &[Closing]) {
        for c in frontier {
            self.closures
                .entry((c.document, c.incarnation))
                .or_insert((c.device, c.head));
        }
    }

    /// Admit a `root_recovery`: signed by the pinned root, it starts
    /// generation N+1 from a known manifest of the newest generation N,
    /// with one member who is its admin, the same recovery key and the
    /// next sequence and epoch. `Ok(true)` makes it current, `Ok(false)` is
    /// an exact replay. It is admitted even while a membership fork freezes
    /// the authority, and clears that freeze: generation N is closed. A
    /// second, different recovery to the same generation retains both and
    /// returns `Fork`.
    pub fn apply_recovery(&mut self, bytes: &[u8]) -> Result<bool> {
        let p = parse(bytes)?;
        if p.message.route != self.route {
            return Err(Error::Context);
        }
        let Body::RootRecovery(m) = &p.message.body else {
            return Err(Error::Unsupported);
        };
        if p.message.signer != self.root {
            return Err(Error::Unauthorized);
        }
        p.verify(&self.root_key)?;
        validate_roster(&m.roster, &m.admin)?;
        let candidate = identity::sha384(bytes);
        if self.states.contains_key(&candidate) {
            return Ok(false);
        }
        let old = self.states.get(&m.previous).ok_or(Error::MissingPrevious)?;
        if Some(m.generation) != old.generation.checked_add(1)
            || Some(m.sequence) != old.sequence.checked_add(1)
            || Some(m.epoch) != old.epoch.checked_add(1)
        {
            return Err(Error::Stale);
        }
        if m.roster.len() != 1 || m.recovery_key != old.recovery {
            return Err(Error::Unsupported);
        }
        if m.roster.iter().any(|n| old.retired.contains(&n.id)) {
            return Err(Error::Unsupported);
        }
        let next = old.successor(m);
        validate_frontier(&m.frontier, &next)?;
        // A valid, different recovery to a generation already started is
        // a fork the root signed: freeze, keeping both. Once frozen, a
        // further different recovery to that generation changes nothing:
        // the first evidence pair stays, and nothing new is retained. Only
        // recoveries start generations, so once this passes, `old` is in
        // the newest.
        if let Some(first) = self.recoveries.get(&m.generation) {
            if self.fork_evidence.is_none() {
                self.fork_evidence = Some((first.clone(), bytes.to_vec()));
            }
            return Err(Error::Fork);
        }
        self.states.insert(candidate, next);
        self.recoveries.insert(m.generation, bytes.to_vec());
        self.close(&m.frontier);
        self.current = candidate;
        self.fork_evidence = None;
        Ok(true)
    }

    /// Sign the `root_recovery` that makes `device` the sole member and
    /// admin of the next generation, from the newest known manifest, with
    /// `key` as its first epoch's content key. The keys must be the ones
    /// this collection pinned: wrong words fail here with `Identity`,
    /// before anything is signed. `signing_randomness` must be fresh OS
    /// entropy; retries resend the bytes returned here.
    pub fn recover(
        &self,
        recovery: &RecoveryKeys,
        device: &DeviceKeys,
        key: &ContentKey,
        signing_randomness: [u8; 32],
    ) -> Result<Vec<u8>> {
        self.recover_closing(recovery, device, key, Vec::new(), signing_randomness)
    }

    /// As [`Self::recover`], closing the superseded generation's writers at
    /// `frontier`: for each writer the recovering device knows, the exact
    /// prefix it holds (see [`Self::unclosed_writers`]). Writers it leaves
    /// out admit no records until a later transition names them.
    pub fn recover_closing(
        &self,
        recovery: &RecoveryKeys,
        device: &DeviceKeys,
        key: &ContentKey,
        mut frontier: Vec<Closing>,
        signing_randomness: [u8; 32],
    ) -> Result<Vec<u8>> {
        frontier.sort();
        let old = self
            .states
            .get(&self.current)
            .ok_or(Error::MissingPrevious)?;
        if recovery.root_id()? != self.root || recovery.kem_public_key() != &old.recovery {
            return Err(Error::Identity);
        }
        let generation = old.generation.checked_add(1).ok_or(Error::Limit)?;
        let epoch = old.epoch.checked_add(1).ok_or(Error::Limit)?;
        let message = Message {
            route: self.route.clone(),
            signer: self.root,
            body: Body::RootRecovery(Membership {
                generation,
                sequence: old.sequence.checked_add(1).ok_or(Error::Limit)?,
                previous: self.current,
                roster: vec![device.member(true)],
                admin: device.id(),
                epoch,
                recovery_key: old.recovery.clone(),
                frontier,
                key_commitment: key_commitment(&self.route, generation, epoch, key)?,
            }),
        };
        sign(&message, recovery.signing_key(), signing_randomness)
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
        let current = self.active()?;
        let p = parse(bytes)?;
        if p.message.route != self.route {
            return Err(Error::Context);
        }
        let claim = p.message.body.writer_claim().ok_or(Error::Unsupported)?;
        let document = claim.document;
        if document != expected_document {
            return Err(Error::Context);
        }
        if claim.device != p.message.signer {
            return Err(Error::Identity);
        }
        // A claim signed under any known manifest of the current
        // generation is admitted if its device is a current member and was
        // a member there: replicas read the log in order, and a claim made
        // just before a concurrent transition reaches every one of them
        // after it. A device the current roster no longer lists claims
        // nothing new: only a writer a signed closing frontier names is
        // still admitted, and only within its prefix. Admission grants no
        // authorship by itself; each record is still checked against its
        // own epoch's roster (`authenticate_at`).
        let at = self.states.get(&claim.manifest).ok_or(Error::Stale)?;
        let named = self
            .closures
            .get(&(document, claim.incarnation))
            .is_some_and(|(device, _)| *device == claim.device);
        if !named {
            if at.generation != current.generation {
                return Err(Error::Stale);
            }
            if !current.has(&claim.device) {
                let signer = at
                    .roster
                    .iter()
                    .find(|m| m.id == claim.device)
                    .ok_or(Error::Unauthorized)?;
                p.verify(&signer.signing_key)?;
                return Err(Error::Revoked);
            }
        }
        let signer = at
            .roster
            .iter()
            .find(|m| m.id == claim.device)
            .ok_or(Error::Unauthorized)?;
        p.verify(&signer.signing_key)?;
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
        if let Some(first) = self
            .steps
            .get(&(document, claim.device, predecessor))
            .and_then(|incarnation| self.writers.get(&(document, *incarnation)))
            .map(|w| w.signed_bytes.clone())
        {
            return Err(self.freeze_writer(lineage, first, bytes));
        }
        if self.actors.contains(&(document, claim.actor)) {
            return Err(Error::Actor);
        }
        self.writers.insert(
            index,
            Writer {
                claim,
                signed_bytes: bytes.to_vec(),
            },
        );
        self.steps
            .insert((document, claim.device, predecessor), claim.incarnation);
        self.actors.insert((document, claim.actor));
        if let Some(prev) = predecessor {
            self.successor_of
                .insert((document, prev), claim.incarnation);
        }
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

    /// The newest accepted state, even while frozen.
    fn top(&self) -> Option<&State> {
        self.states.get(&self.current)
    }

    /// How far an admitted writer's records are admitted under `current`.
    /// A writer a signed frontier names is closed at its entry. Otherwise a
    /// current member's writer is open. A removed device's writer is closed
    /// at its own successor's signed handoff head, if the chain of signed
    /// handoffs after it reaches a writer a frontier names; otherwise it is
    /// revoked. That makes the result independent of the order replicas
    /// read claims and transitions in.
    fn admission(&self, document: [u8; 16], incarnation: Id, current: &State) -> Admission {
        let Some(writer) = self.writers.get(&(document, incarnation)) else {
            return Admission::Revoked;
        };
        let device = writer.claim.device;
        let named = |incarnation: &Id| {
            self.closures
                .get(&(document, *incarnation))
                .filter(|(d, _)| *d == device)
                .map(|(_, head)| *head)
        };
        if let Some(head) = named(&incarnation) {
            return Admission::Closed(head);
        }
        if current.has(&device) {
            return Admission::Open;
        }
        let mut at = incarnation;
        let mut bound = None;
        while let Some(next) = self.successor_of.get(&(document, at)) {
            let Some((_, Boundary::Closed(head))) = self
                .writers
                .get(&(document, *next))
                .and_then(|w| w.claim.previous)
            else {
                break;
            };
            let bound = *bound.get_or_insert(head);
            if named(next).is_some() {
                return Admission::Closed(bound);
            }
            at = *next;
        }
        Admission::Revoked
    }

    fn history(&self, document: [u8; 16], incarnation: Id, w: &Writer) -> WriterHistory {
        let admission = self.top().map_or(Admission::Open, |c| {
            self.admission(document, incarnation, c)
        });
        let closed = match admission {
            Admission::Closed(head) => Some(Boundary::Closed(head)),
            Admission::Revoked => Some(Boundary::Closed(None)),
            Admission::Open => self
                .successor_of
                .get(&(document, incarnation))
                .and_then(|next| self.writers.get(&(document, *next)))
                .and_then(|next| next.claim.previous)
                .map(|(_, boundary)| boundary),
        };
        WriterHistory {
            device: w.claim.device,
            actor: w.claim.actor,
            predecessor: w.claim.previous,
            closed,
            frozen: self.lineage_frozen(document, &w.claim.device),
        }
    }

    /// One admitted writer's lineage facts. `closed` is the exact signed
    /// prefix its records end at: its successor's handoff head, or for a
    /// removed device's writer the closing frontier's (null when no signed
    /// prefix admits anything of it).
    pub fn writer_history(&self, document: [u8; 16], incarnation: Id) -> Result<WriterHistory> {
        let w = self
            .writers
            .get(&(document, incarnation))
            .ok_or(Error::MissingWriter)?;
        Ok(self.history(document, incarnation, w))
    }

    /// Every admitted writer's lineage facts for one document, in one pass.
    pub fn writer_histories(&self, document: [u8; 16]) -> BTreeMap<Id, WriterHistory> {
        self.writers
            .range((document, [0; 32])..=(document, [0xff; 32]))
            .map(|((_, incarnation), w)| (*incarnation, self.history(document, *incarnation, w)))
            .collect()
    }

    /// The writers a transition to the roster `keep` (device IDs) should
    /// close in its frontier: each admitted writer for `document` of a
    /// device outside `keep` that no signed prefix closes yet, neither a
    /// frontier entry nor its successor's handoff. The admin closes each at
    /// the exact head of the chain it holds (null if it holds nothing).
    pub fn unclosed_writers(&self, document: [u8; 16], keep: &[Hash]) -> Vec<(Hash, Id)> {
        self.writers
            .range((document, [0; 32])..=(document, [0xff; 32]))
            .filter(|(_, w)| !keep.contains(&w.claim.device))
            .filter(|((_, incarnation), w)| {
                !self
                    .closures
                    .get(&(document, *incarnation))
                    .is_some_and(|(d, _)| *d == w.claim.device)
            })
            .filter(|((_, incarnation), _)| {
                !matches!(
                    self.successor_of
                        .get(&(document, *incarnation))
                        .and_then(|next| self.writers.get(&(document, *next)))
                        .and_then(|next| next.claim.previous),
                    Some((_, Boundary::Closed(_)))
                )
            })
            .map(|((_, incarnation), w)| (w.claim.device, *incarnation))
            .collect()
    }

    /// Check a `data` record's signature by its author, a member of the
    /// epoch it names, without admitting it: for an admin or a recovering
    /// device reading back the chains it holds to sign a closing frontier.
    pub fn verify_signed_record(&self, bytes: &[u8], document: &[u8; 16]) -> Result<SignedRecord> {
        let p = wire::parse(bytes, self.route.content(document)).map_err(|_| Error::Format)?;
        let h = p.header;
        if h.kind != wire::Kind::Data {
            return Err(Error::Unsupported);
        }
        let state = self.states.get(h.manifest).ok_or(Error::Stale)?;
        if h.epoch != state.epoch {
            return Err(Error::Stale);
        }
        let author = state
            .roster
            .iter()
            .find(|m| &m.id == h.author)
            .ok_or(Error::Unauthorized)?;
        let facts = SignedRecord {
            author: *h.author,
            incarnation: *h.incarnation,
            previous: h.previous.copied(),
            hash: record::record_hash(bytes),
        };
        verify_record(p, &author.signing_key)?;
        Ok(facts)
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
        let kem = member.kem_key.clone();
        self.seal_envelope(
            admin,
            manifest,
            state,
            recipient,
            &kem,
            key,
            signing_randomness,
        )
    }

    /// HPKE-seal `key` for `recipient_kem` under `state`'s epoch and sign
    /// the envelope as `admin`, the current admin.
    #[allow(clippy::too_many_arguments)]
    fn seal_envelope(
        &self,
        admin: &DeviceKeys,
        manifest: Hash,
        state: &State,
        recipient: Hash,
        recipient_kem: &KemPublicKey,
        key: &ContentKey,
        signing_randomness: [u8; 32],
    ) -> Result<Vec<u8>> {
        self.check_commitment(state, key)?;
        let mut envelope = KeyEnvelope {
            manifest,
            epoch: state.epoch,
            recipient,
            recipient_key_fingerprint: fingerprint(KEM_KEY_LABEL, recipient_kem.as_bytes())?,
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
            &hpke_rs::HpkePublicKey::from(recipient_kem.as_bytes()),
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

    /// Wrap the content key of the epoch named by `manifest` for the
    /// collection's recovery KEM key, as the current admin. The manifest
    /// must be one this admin administers, so the envelope's signer is
    /// that epoch's admin. Keep one per epoch, so the words alone recover
    /// every epoch's records.
    pub fn wrap_recovery_key(
        &self,
        admin: &DeviceKeys,
        manifest: Hash,
        key: &ContentKey,
        signing_randomness: [u8; 32],
    ) -> Result<Vec<u8>> {
        if admin.id() != self.admin()? {
            return Err(Error::Unauthorized);
        }
        let state = self.states.get(&manifest).ok_or(Error::MissingPrevious)?;
        if state.admin != admin.id() {
            return Err(Error::Unauthorized);
        }
        let recipient = fingerprint(KEM_KEY_LABEL, state.recovery.as_bytes())?;
        let kem = state.recovery.clone();
        self.seal_envelope(
            admin,
            manifest,
            state,
            recipient,
            &kem,
            key,
            signing_randomness,
        )
    }

    /// Unwrap a recovery envelope with the keys the words derive. The
    /// envelope must name a known manifest and its epoch, be signed by that
    /// epoch's admin and name the collection's recovery key as recipient;
    /// the key must match that epoch's commitment. Works while the
    /// authority is frozen: reading back is what recovery is for.
    pub fn unwrap_recovery_key(&self, bytes: &[u8], recovery: &RecoveryKeys) -> Result<EpochKey> {
        let p = parse(bytes)?;
        if p.message.route != self.route {
            return Err(Error::Context);
        }
        let Body::KeyEnvelope(k) = &p.message.body else {
            return Err(Error::Unsupported);
        };
        let state = self.states.get(&k.manifest).ok_or(Error::Stale)?;
        if k.epoch != state.epoch {
            return Err(Error::Stale);
        }
        if p.message.signer != state.admin || k.authority != state.admin {
            return Err(Error::Unauthorized);
        }
        let admin = state
            .roster
            .iter()
            .find(|m| m.id == state.admin)
            .ok_or(Error::Unauthorized)?;
        p.verify(&admin.signing_key)?;
        let recipient = fingerprint(KEM_KEY_LABEL, state.recovery.as_bytes())?;
        if k.recipient != recipient
            || k.recipient_key_fingerprint != recipient
            || recovery.kem_public_key() != &state.recovery
        {
            return Err(Error::Identity);
        }
        self.open_envelope(&p.message, k, state, recovery.kem_private_key())
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
        self.open_envelope(&p.message, k, state, recipient.kem_private_key())
    }

    /// HPKE-open an authenticated envelope and check the key against
    /// `state`'s signed commitment.
    fn open_envelope(
        &self,
        message: &Message,
        k: &KeyEnvelope,
        state: &State,
        private: &KemPrivateKey,
    ) -> Result<EpochKey> {
        let mut plaintext = identity::hpke()?
            .open(
                &k.encapsulation,
                private.hpke(),
                &envelope_info(message)?,
                &envelope_aad(message)?,
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

    /// Authenticate a `data` record at the epoch it was sealed in, which
    /// `key` names (current or earlier, as history envelopes give them):
    /// the record names the key's manifest and epoch, its author was a
    /// member of that epoch's roster, and its writer is admitted for the
    /// document, signed by the author. Retired writers' records pass, as
    /// history; the receiver enforces chain position and closing prefixes.
    /// A frozen authority or writer lineage refuses with `Fork`.
    pub fn authenticate_at<'a>(
        &self,
        bytes: &'a [u8],
        key: &'a EpochKey,
        document: &[u8; 16],
    ) -> Result<AuthenticatedData<'a>> {
        self.authenticate_kind_at(bytes, key, document, wire::Kind::Data)
    }

    /// Authenticate a `snapshot` record (a checkpoint part, #40) as
    /// [`authenticate_at`](Self::authenticate_at) does a `data` record. A
    /// snapshot record stands outside its writer's chain, so it must name
    /// no predecessor.
    pub fn authenticate_snapshot_at<'a>(
        &self,
        bytes: &'a [u8],
        key: &'a EpochKey,
        document: &[u8; 16],
    ) -> Result<AuthenticatedData<'a>> {
        self.authenticate_kind_at(bytes, key, document, wire::Kind::Snapshot)
    }

    fn authenticate_kind_at<'a>(
        &self,
        bytes: &'a [u8],
        key: &'a EpochKey,
        document: &[u8; 16],
        kind: wire::Kind,
    ) -> Result<AuthenticatedData<'a>> {
        let current = self.active()?;
        if key.route != self.route {
            return Err(Error::Context);
        }
        let state = self.states.get(&key.manifest).ok_or(Error::Stale)?;
        if key.epoch != state.epoch {
            return Err(Error::Stale);
        }
        let p = wire::parse(bytes, self.route.content(document)).map_err(|_| Error::Format)?;
        let h = p.header;
        if h.kind != kind {
            return Err(Error::Unsupported);
        }
        if kind == wire::Kind::Snapshot && h.previous.is_some() {
            return Err(Error::Format);
        }
        if *h.manifest != key.manifest || h.epoch != key.epoch {
            return Err(Error::Stale);
        }
        let author = state
            .roster
            .iter()
            .find(|m| &m.id == h.author)
            .ok_or(Error::Unauthorized)?;
        // A frozen lineage holds every record of the device for the
        // document, even one whose competing writer was never admitted.
        if self.lineage_frozen(*document, h.author) {
            verify_record(p, &author.signing_key)?;
            return Err(Error::Fork);
        }
        let Some(writer) = self.writers.get(&(*document, *h.incarnation)) else {
            // A member's authenticated record may wait for its writer's
            // admission; forged or unsigned bytes are rejected now. A
            // removed device's may wait only for a writer a frontier names.
            verify_record(p, &author.signing_key)?;
            if !current.has(h.author) && !self.closures.contains_key(&(*document, *h.incarnation)) {
                return Err(Error::Revoked);
            }
            return Err(Error::MissingWriter);
        };
        if writer.claim.device != *h.author {
            return Err(Error::Actor);
        }
        // A removed device's writer is admitted only within its signed
        // closing prefix; the receiver walks the chain to its exact head
        // (`writer_history`). Nothing of an empty or unnamed one is.
        if matches!(
            self.admission(*document, *h.incarnation, current),
            Admission::Revoked | Admission::Closed(None)
        ) {
            verify_record(p, &author.signing_key)?;
            return Err(Error::Revoked);
        }
        Ok(AuthenticatedData {
            verified: verify_record(p, &author.signing_key)?,
            key,
            actor: writer.claim.actor,
        })
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

    /// The epoch and roster of the accepted manifest `manifest`, current or
    /// earlier. Read-only: a receiver that holds no key of that epoch yet
    /// can still check a record's author and signature before it keeps the
    /// record. Admission stays with `authenticate_at`.
    pub fn roster_at(&self, manifest: &Hash) -> Option<(u64, &[Member])> {
        self.states
            .get(manifest)
            .map(|state| (state.epoch, state.roster.as_slice()))
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
