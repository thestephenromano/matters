//! The production key-distribution authority against the independent golden
//! vectors (Python cbor2/hashlib, docs/SYNC_KEY_DISTRIBUTION.md) and the
//! prototype's rejection, fork, rotation, envelope and writer cases.

use std::sync::atomic::{AtomicU64, Ordering};

use matters_sync::authority::{
    device_id, encode, envelope_aad, envelope_info, genesis, key_commitment, parse, recovery_seeds,
    request_enrollment, root_id, sign, signing_bytes, Authority, Body, Boundary, DeviceKeys,
    EpochKey, Error, Genesis, Hash, Id, Invitation, InvitationSecret, KemPublicKey, Member,
    Membership, Message, RecoveryKeys, RecoverySecret, Result, Route, WriterHandoff, WriterStart,
    CODE_LENGTH, KEM_PUBLIC_KEY_BYTES, MAX_CONTROL_BYTES, POLICY,
};
use matters_sync::generate_signing_key;
use matters_sync::record::{seal, ContentKey, SigningKey, VerificationKey, VERIFICATION_KEY_BYTES};
use matters_sync::wire::{self, SIGNATURE_BYTES};
use serde_json::Value;

const DOC: [u8; 16] = [4; 16];

/// Distinct test entropy per call. Fixture material only.
fn fresh() -> [u8; 32] {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    let mut bytes = [0xa5; 32];
    bytes[..8].copy_from_slice(&NEXT.fetch_add(1, Ordering::Relaxed).to_le_bytes());
    bytes
}

fn route() -> Route {
    Route {
        application: [1; 16],
        tenant: [2; 16],
        collection: [3; 16],
    }
}

fn device() -> DeviceKeys {
    DeviceKeys::generate(fresh(), fresh()).unwrap()
}

struct Root {
    signing: SigningKey,
    verification: VerificationKey,
    recovery: KemPublicKey,
}

struct Setup {
    root: Root,
    a: DeviceKeys,
    b: DeviceKeys,
    genesis: Vec<u8>,
    authority: Authority,
}

/// Deterministic test content key per epoch; genesis and transitions commit
/// to it. Fixture material only.
fn epoch_key_bytes(epoch: u64) -> [u8; 32] {
    let mut key = [0x5a; 32];
    key[..8].copy_from_slice(&epoch.to_be_bytes());
    key
}

fn epoch_key(epoch: u64) -> ContentKey {
    ContentKey::new(epoch_key_bytes(epoch))
}

fn commitment(epoch: u64) -> Hash {
    key_commitment(&route(), 0, epoch, &epoch_key(epoch)).unwrap()
}

fn signed(message: &Message, signer: &SigningKey) -> Vec<u8> {
    sign(message, signer, fresh()).unwrap()
}

fn setup() -> Setup {
    let (signing, verification) = generate_signing_key(fresh());
    let recovery = device().kem_public_key().clone();
    let root = Root {
        signing,
        verification,
        recovery,
    };
    let a = device();
    let b = device();
    let mut roster = vec![a.member(true), b.member(false)];
    roster.sort_by_key(|m| m.id);
    let genesis = signed(
        &Message {
            route: route(),
            signer: root_id(&root.verification).unwrap(),
            body: Body::Genesis(Box::new(Genesis {
                root_key: root.verification.clone(),
                recovery_key: root.recovery.clone(),
                generation: 0,
                roster,
                admin: a.id(),
                epoch: 0,
                checkpoint: None,
                key_commitment: commitment(0),
            })),
        },
        &root.signing,
    );
    let authority = Authority::bootstrap(&genesis, route(), &root.verification).unwrap();
    Setup {
        root,
        a,
        b,
        genesis,
        authority,
    }
}

fn transition(s: &Setup) -> Message {
    let epoch = s.authority.epoch().unwrap() + 1;
    Message {
        route: route(),
        signer: s.a.id(),
        body: Body::Membership(Membership {
            generation: 0,
            sequence: s.authority.sequence().unwrap() + 1,
            previous: s.authority.manifest(),
            roster: s.authority.members().unwrap(),
            admin: s.a.id(),
            epoch,
            recovery_key: s.authority.recovery().unwrap(),
            key_commitment: commitment(epoch),
        }),
    }
}

fn membership(message: &mut Message) -> &mut Membership {
    match &mut message.body {
        Body::Membership(m) => m,
        _ => unreachable!(),
    }
}

fn writer(
    state: &Authority,
    device: &DeviceKeys,
    incarnation: Id,
    actor: u64,
    previous: Option<Id>,
) -> Message {
    Message {
        route: route(),
        signer: device.id(),
        body: Body::WriterStart(WriterStart {
            manifest: state.manifest(),
            document: DOC,
            device: device.id(),
            incarnation,
            actor,
            previous,
        }),
    }
}

fn wrap(
    state: &Authority,
    admin: &DeviceKeys,
    recipient: Hash,
    key: &ContentKey,
) -> Result<Vec<u8>> {
    state.wrap_key(admin, recipient, key, fresh())
}

fn header<'a>(
    state: &'a Authority,
    author: &'a Hash,
    incarnation: &'a Id,
    record: &'a Id,
    manifest: &'a Hash,
) -> wire::Header<'a> {
    wire::Header {
        context: state.route().content(&DOC),
        kind: wire::Kind::Data,
        epoch: state.epoch().unwrap(),
        manifest,
        author,
        incarnation,
        record_id: record,
        logical_batch: &[9; 32],
        previous: None,
    }
}

const PLAINTEXT: &[u8] = b"task: encrypted two-device experiment";

fn seal_as(
    h: wire::Header<'_>,
    cek: &ContentKey,
    plaintext: &[u8],
    author: &DeviceKeys,
) -> Vec<u8> {
    seal(h, cek, plaintext, author.signing_key(), fresh()).unwrap()
}

/// Authenticate a record as an active writer's and decrypt it.
fn open(state: &Authority, bytes: &[u8], key: &EpochKey) -> Result<Vec<u8>> {
    state.authenticate_data(bytes, key, &DOC)?.open()
}

fn with_kem(member: &Member, kem: [u8; KEM_PUBLIC_KEY_BYTES]) -> Member {
    Member {
        kem_key: KemPublicKey::from_bytes(kem),
        ..member.clone()
    }
}

fn flipped<const N: usize>(bytes: &[u8]) -> [u8; N] {
    let mut out: [u8; N] = bytes.try_into().unwrap();
    out[0] ^= 1;
    out
}

#[test]
fn two_devices_distribute_a_key_and_open_an_authenticated_first_record() {
    let mut s = setup();
    let mut receiver = Authority::bootstrap(&s.genesis, route(), &s.root.verification).unwrap();
    let cek = epoch_key(s.authority.epoch().unwrap());
    let envelope = wrap(&s.authority, &s.a, s.b.id(), &cek).unwrap();
    let received_key = receiver.unwrap_key(&envelope, &s.b).unwrap();
    assert_eq!(received_key.epoch(), 0);
    assert_eq!(received_key.manifest(), s.authority.manifest());
    let incarnation = fresh();
    let start = signed(
        &writer(&s.authority, &s.a, incarnation, 71, None),
        s.a.signing_key(),
    );
    assert!(s.authority.admit_writer(&start, DOC).unwrap());
    assert!(receiver.admit_writer(&start, DOC).unwrap());
    let author = s.a.id();
    let manifest = s.authority.manifest();
    let record = fresh();
    let sealed = seal_as(
        header(&s.authority, &author, &incarnation, &record, &manifest),
        received_key.content_key(),
        PLAINTEXT,
        &s.a,
    );
    let authenticated = receiver
        .authenticate_data(&sealed, &received_key, &DOC)
        .unwrap();
    assert_eq!(authenticated.actor(), 71);
    assert_eq!(authenticated.parsed().header.incarnation, &incarnation);
    assert_eq!(authenticated.open().unwrap(), PLAINTEXT);
    assert!(!sealed.windows(PLAINTEXT.len()).any(|w| w == PLAINTEXT));
    let raw = epoch_key_bytes(0);
    assert!(!envelope.windows(raw.len()).any(|w| w == raw));
    // A fresh envelope has fresh HPKE randomness; transport retry retains old bytes.
    let next = wrap(&s.authority, &s.a, s.b.id(), &cek).unwrap();
    assert_ne!(next, envelope);
    receiver.unwrap_key(&envelope, &s.b).unwrap();
}

#[test]
fn bootstrap_requires_pinned_root_route_and_exact_device_key_binding() {
    let s = setup();
    let (_, stranger) = generate_signing_key(fresh());
    assert_eq!(
        Authority::bootstrap(&s.genesis, route(), &stranger).map(|_| ()),
        Err(Error::Identity)
    );
    let mut wrong = route();
    wrong.application[0] ^= 1;
    assert_eq!(
        Authority::bootstrap(&s.genesis, wrong, &s.root.verification).map(|_| ()),
        Err(Error::Context)
    );
    let original = parse(&s.genesis).unwrap().message;
    for change in 0..4 {
        let mut m = original.clone();
        if let Body::Genesis(g) = &mut m.body {
            let roster = &mut g.roster;
            match change {
                0 => roster[0] = with_kem(&roster[0], flipped(roster[0].kem_key.as_bytes())),
                1 => {
                    roster[0].signing_key =
                        VerificationKey::from_bytes(flipped(roster[0].signing_key.as_bytes()))
                }
                2 => roster.reverse(),
                _ => roster[0] = roster[1].clone(),
            }
        }
        let bytes = signed(&m, &s.root.signing);
        assert_eq!(
            Authority::bootstrap(&bytes, route(), &s.root.verification).map(|_| ()),
            Err(Error::Identity),
            "change {change}"
        );
    }
    let mut bad = s.genesis.clone();
    let last = bad.len() - 1;
    bad[last] ^= 1;
    assert_eq!(
        Authority::bootstrap(&bad, route(), &s.root.verification).map(|_| ()),
        Err(Error::Signature)
    );
}

#[test]
fn membership_uses_previous_admin_and_preserves_state_on_rejection() {
    let mut s = setup();
    let original = s.authority.manifest();
    let good = transition(&s);
    let mut writer_signed = good.clone();
    writer_signed.signer = s.b.id();
    assert_eq!(
        s.authority
            .apply_membership(&signed(&writer_signed, s.b.signing_key())),
        Err(Error::Unauthorized)
    );
    assert_eq!(
        s.authority
            .apply_membership(&signed(&good, s.b.signing_key())),
        Err(Error::Signature)
    );
    let mut absent = good.clone();
    membership(&mut absent).previous = [99; 48];
    assert_eq!(
        s.authority
            .apply_membership(&signed(&absent, s.a.signing_key())),
        Err(Error::MissingPrevious)
    );
    let mut stale = good.clone();
    membership(&mut stale).epoch = 0;
    assert_eq!(
        s.authority
            .apply_membership(&signed(&stale, s.a.signing_key())),
        Err(Error::Stale)
    );
    assert_eq!(s.authority.manifest(), original);
    let bytes = signed(&good, s.a.signing_key());
    assert_eq!(s.authority.apply_membership(&bytes), Ok(true));
    assert_eq!(s.authority.apply_membership(&bytes), Ok(false));
    assert_eq!(s.authority.epoch(), Ok(1));
    assert_eq!(
        s.authority
            .apply_membership(&signed(&good, s.b.signing_key())),
        Err(Error::Signature)
    );
    assert!(!s.authority.is_forked());
}

#[test]
fn two_valid_membership_successors_freeze_key_and_record_admission() {
    let mut s = setup();
    let first = transition(&s);
    let mut second = first.clone();
    let a = s.a.id();
    membership(&mut second).roster.retain(|m| m.id == a);
    let first = signed(&first, s.a.signing_key());
    s.authority.apply_membership(&first).unwrap();
    let cek = epoch_key(s.authority.epoch().unwrap());
    let envelope = wrap(&s.authority, &s.a, s.b.id(), &cek).unwrap();
    let key = s.authority.unwrap_key(&envelope, &s.b).unwrap();
    let inc = fresh();
    let start = signed(
        &writer(&s.authority, &s.a, inc, 71, None),
        s.a.signing_key(),
    );
    s.authority.admit_writer(&start, DOC).unwrap();
    let author = s.a.id();
    let manifest = s.authority.manifest();
    let id = fresh();
    let record = seal_as(
        header(&s.authority, &author, &inc, &id, &manifest),
        &cek,
        PLAINTEXT,
        &s.a,
    );
    open(&s.authority, &record, &key).unwrap();
    let second = signed(&second, s.a.signing_key());
    assert_eq!(s.authority.apply_membership(&second), Err(Error::Fork));
    assert!(s.authority.is_forked());
    assert_eq!(
        s.authority.fork_evidence(),
        Some((first.as_slice(), second.as_slice()))
    );
    assert_eq!(s.authority.admit_writer(&start, DOC), Err(Error::Fork));
    assert_eq!(open(&s.authority, &record, &key), Err(Error::Fork));
    assert_eq!(
        wrap(&s.authority, &s.a, s.a.id(), &ContentKey::new([1; 32])),
        Err(Error::Fork)
    );
    assert_eq!(
        s.authority.unwrap_key(&envelope, &s.b).map(|_| ()),
        Err(Error::Fork)
    );
}

#[test]
fn unsupported_admin_transfer_and_recovery_are_not_implicitly_authorized() {
    let mut s = setup();
    let candidate = device();
    for change in 0..2 {
        let mut m = transition(&s);
        let body = membership(&mut m);
        match change {
            0 => {
                body.admin = s.b.id();
                for member in &mut body.roster {
                    member.admin = member.id == body.admin;
                }
            }
            _ => body.recovery_key = candidate.kem_public_key().clone(),
        }
        assert_eq!(
            s.authority.apply_membership(&signed(&m, s.a.signing_key())),
            Err(Error::Unsupported),
            "change {change}"
        );
    }
}

#[test]
fn envelope_rejects_unauthorized_signers_wrong_recipients_and_substituted_keys() {
    let s = setup();
    let envelope = wrap(&s.authority, &s.a, s.b.id(), &epoch_key(0)).unwrap();
    let original = parse(&envelope).unwrap().message;
    assert_eq!(
        s.authority.unwrap_key(&envelope, &s.a).map(|_| ()),
        Err(Error::Identity)
    );
    assert_eq!(
        wrap(&s.authority, &s.b, s.a.id(), &ContentKey::new([1; 32])),
        Err(Error::Unauthorized)
    );
    assert_eq!(
        s.authority
            .unwrap_key(&signed(&original, s.b.signing_key()), &s.b)
            .map(|_| ()),
        Err(Error::Signature)
    );
    for change in 0..6 {
        let mut m = original.clone();
        let Body::KeyEnvelope(k) = &mut m.body else {
            unreachable!()
        };
        let expected = match change {
            0 => {
                m.signer = s.b.id();
                Error::Unauthorized
            }
            1 => {
                m.route.tenant[0] ^= 1;
                Error::Context
            }
            2 => {
                k.recipient_key_fingerprint[0] ^= 1;
                Error::Identity
            }
            3 => {
                k.ciphertext[0] ^= 1;
                Error::Crypto
            }
            4 => {
                k.encapsulation[0] ^= 1;
                Error::Crypto
            }
            _ => {
                k.authority = s.b.id();
                Error::Unauthorized
            }
        };
        let result = s.authority.unwrap_key(&signed(&m, s.a.signing_key()), &s.b);
        assert_eq!(result.map(|_| ()), Err(expected), "case {change}");
    }
}

#[test]
fn rotation_rejects_old_envelopes_and_revocation_removes_distribution_authority() {
    let mut s = setup();
    let old = wrap(&s.authority, &s.a, s.b.id(), &epoch_key(0)).unwrap();
    let old_key = s.authority.unwrap_key(&old, &s.b).unwrap();
    let inc = fresh();
    let start = writer(&s.authority, &s.b, inc, 71, None);
    s.authority
        .admit_writer(&signed(&start, s.b.signing_key()), DOC)
        .unwrap();
    let m = signed(&transition(&s), s.a.signing_key());
    s.authority.apply_membership(&m).unwrap();
    assert_eq!(
        s.authority.unwrap_key(&old, &s.b).map(|_| ()),
        Err(Error::Stale)
    );
    assert_eq!(s.authority.check_epoch_key(&old_key), Err(Error::Stale));
    let new_cek = epoch_key(1);
    let new = wrap(&s.authority, &s.a, s.b.id(), &new_cek).unwrap();
    let new_key = s.authority.unwrap_key(&new, &s.b).unwrap();
    let author = s.b.id();
    let manifest = s.authority.manifest();
    let record = fresh();
    let sealed = seal_as(
        header(&s.authority, &author, &inc, &record, &manifest),
        &new_cek,
        PLAINTEXT,
        &s.b,
    );
    assert_eq!(open(&s.authority, &sealed, &old_key), Err(Error::Stale));
    // Retained bindings remain usable by a member under the current epoch.
    assert_eq!(open(&s.authority, &sealed, &new_key).unwrap(), PLAINTEXT);
    let mut revoke = transition(&s);
    let a = s.a.id();
    membership(&mut revoke).roster.retain(|m| m.id == a);
    s.authority
        .apply_membership(&signed(&revoke, s.a.signing_key()))
        .unwrap();
    assert_eq!(
        wrap(&s.authority, &s.a, s.b.id(), &ContentKey::new([1; 32])),
        Err(Error::Unauthorized)
    );
    let cek = epoch_key(s.authority.epoch().unwrap());
    let envelope = wrap(&s.authority, &s.a, s.a.id(), &cek).unwrap();
    let key = s.authority.unwrap_key(&envelope, &s.a).unwrap();
    // Even a removed writer given the new key cannot acquire current authority.
    let manifest = s.authority.manifest();
    let record = fresh();
    let forged = seal_as(
        header(&s.authority, &author, &inc, &record, &manifest),
        &cek,
        PLAINTEXT,
        &s.b,
    );
    assert_eq!(open(&s.authority, &forged, &key), Err(Error::Unauthorized));
}

#[test]
fn writer_admission_binds_document_actor_and_incarnation() {
    let mut s = setup();
    let start = writer(&s.authority, &s.a, [1; 32], 71, None);
    let bytes = signed(&start, s.a.signing_key());
    assert_eq!(
        s.authority.admit_writer(&bytes, [99; 16]),
        Err(Error::Context)
    );
    assert_eq!(
        s.authority
            .admit_writer(&signed(&start, s.b.signing_key()), DOC),
        Err(Error::Signature)
    );
    assert_eq!(s.authority.admit_writer(&bytes, DOC), Ok(true));
    assert_eq!(s.authority.admit_writer(&bytes, DOC), Ok(false));
    for (device, inc, actor, prev) in [
        (&s.b, [2; 32], 71, None),
        (&s.b, [1; 32], 72, None),
        (&s.a, [1; 32], 72, None),
        (&s.b, [2; 32], 72, Some([1; 32])),
    ] {
        let m = writer(&s.authority, device, inc, actor, prev);
        let mut attempt = s.authority.clone();
        let expected = if device.id() == s.a.id() {
            Error::Fork
        } else {
            Error::Actor
        };
        assert_eq!(
            attempt.admit_writer(&signed(&m, device.signing_key()), DOC),
            Err(expected)
        );
        assert_eq!(
            attempt.writer_fork(DOC, s.a.id()).is_some(),
            expected == Error::Fork
        );
    }
    let restored = writer(&s.authority, &s.a, [2; 32], 72, Some([1; 32]));
    s.authority
        .admit_writer(&signed(&restored, s.a.signing_key()), DOC)
        .unwrap();
    let rotation = transition(&s);
    s.authority
        .apply_membership(&signed(&rotation, s.a.signing_key()))
        .unwrap();
    assert_eq!(s.authority.admit_writer(&bytes, DOC), Err(Error::Stale));
}

#[test]
fn signed_content_still_requires_writer_epoch_kind_and_author() {
    let mut s = setup();
    let cek = epoch_key(s.authority.epoch().unwrap());
    let envelope = wrap(&s.authority, &s.a, s.b.id(), &cek).unwrap();
    let key = s.authority.unwrap_key(&envelope, &s.b).unwrap();
    let author = s.a.id();
    let manifest = s.authority.manifest();
    let incarnation = fresh();
    let record = fresh();
    let h = header(&s.authority, &author, &incarnation, &record, &manifest);
    let sealed = seal_as(h, &cek, PLAINTEXT, &s.a);
    assert_eq!(open(&s.authority, &sealed, &key), Err(Error::Actor));
    // A current member's signed record may wait for its writer; a forged one
    // may not.
    assert_eq!(
        s.authority
            .authenticate_history(&sealed, &key, &DOC)
            .map(|_| ()),
        Err(Error::MissingWriter)
    );
    let forged = seal_as(h, &cek, PLAINTEXT, &s.b);
    assert_eq!(
        s.authority
            .authenticate_history(&forged, &key, &DOC)
            .map(|_| ()),
        Err(Error::Signature)
    );
    let start = writer(&s.authority, &s.a, incarnation, 71, None);
    s.authority
        .admit_writer(&signed(&start, s.a.signing_key()), DOC)
        .unwrap();
    for change in 0..3 {
        let record = fresh();
        let mut h = header(&s.authority, &author, &incarnation, &record, &manifest);
        let expected = match change {
            0 => {
                h.epoch = 1;
                Error::Stale
            }
            1 => {
                h.kind = wire::Kind::Snapshot;
                Error::Unsupported
            }
            _ => Error::Signature,
        };
        let bytes = seal_as(h, &cek, PLAINTEXT, if change == 2 { &s.b } else { &s.a });
        assert_eq!(
            open(&s.authority, &bytes, &key),
            Err(expected),
            "change {change}"
        );
    }
    // Under the wrong content key the signature still verifies; only opening
    // fails, so the caller keeps the authenticated evidence.
    let record = fresh();
    let h = header(&s.authority, &author, &incarnation, &record, &manifest);
    let bytes = seal_as(h, &ContentKey::new([7; 32]), PLAINTEXT, &s.a);
    let authenticated = s.authority.authenticate_data(&bytes, &key, &DOC).unwrap();
    assert_eq!(authenticated.open(), Err(Error::Crypto));
}

fn fixtures() -> Value {
    serde_json::from_str(include_str!(
        "../../../experiments/sync-prototype/tests/vectors/key-distribution/golden.json"
    ))
    .unwrap()
}

fn unhex(value: &str) -> Vec<u8> {
    (0..value.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&value[i..i + 2], 16).unwrap())
        .collect()
}

fn bytes(v: &Value, key: &str) -> Vec<u8> {
    unhex(v[key].as_str().unwrap())
}

#[test]
fn independent_control_bytes_fingerprints_and_hpke_contexts_match() {
    let f = fixtures();
    let commitments = f["key_commitments"].as_array().unwrap();
    let vectors = f["vectors"].as_array().unwrap();
    assert_eq!(vectors.len(), 6);
    for v in vectors {
        let wire = bytes(v, "wire_hex");
        let p = parse(&wire).unwrap();
        assert_eq!(p.message.body.kind(), v["kind"].as_str().unwrap());
        assert_eq!(p.signed, bytes(v, "signed_hex"));
        assert!(std::ptr::eq(p.signed, &wire[1..1 + p.signed.len()]));
        assert_eq!(signing_bytes(&p.message).unwrap(), p.signed);
        assert_eq!(encode(&p.message, p.signature).unwrap(), wire);
        assert_eq!(libcrux_sha2::sha384(&wire).as_slice(), bytes(v, "hash_hex"));
        match &p.message.body {
            Body::Genesis(g) => {
                assert_eq!(
                    g.key_commitment.as_slice(),
                    bytes(&commitments[0], "commitment_hex")
                );
                assert_eq!(
                    root_id(&g.root_key).unwrap().as_slice(),
                    bytes(&f, "root_id_hex")
                );
                assert_eq!(g.admin.as_slice(), bytes(&f, "admin_id_hex"));
                for m in &g.roster {
                    assert_eq!(device_id(&m.signing_key, &m.kem_key).unwrap(), m.id);
                }
            }
            Body::Membership(m) => assert_eq!(
                m.key_commitment.as_slice(),
                bytes(&commitments[1], "commitment_hex")
            ),
            Body::KeyEnvelope(k) => {
                assert_eq!(k.recipient.as_slice(), bytes(&f, "recipient_id_hex"));
                assert_eq!(
                    k.recipient_key_fingerprint.as_slice(),
                    bytes(&f, "recipient_key_fingerprint_hex")
                );
                assert_eq!(envelope_info(&p.message).unwrap(), bytes(&f, "info_hex"));
                assert_eq!(envelope_aad(&p.message).unwrap(), bytes(&f, "aad_hex"));
            }
            Body::WriterStart(_) | Body::WriterHandoff(_) => {
                assert!(p.message.body.writer_claim().is_some())
            }
            Body::EnrollmentRequest(_) | Body::RootRecovery(_) => {
                unreachable!("no golden enrollment or recovery vector")
            }
        }
    }
}

#[test]
fn malformed_control_fixtures_and_every_truncation_reject() {
    let f = fixtures();
    let base = bytes(&f["vectors"][2], "wire_hex");
    let splices = f["rejection_splices"].as_array().unwrap();
    assert_eq!(splices.len(), 10);
    for v in splices {
        let mut wire = base.clone();
        let i = v["offset"].as_u64().unwrap() as usize;
        let n = v["remove"].as_u64().unwrap() as usize;
        wire.splice(i..i + n, bytes(v, "insert_hex"));
        match parse(&wire) {
            Err(e) => assert_eq!(
                format!("{e:?}"),
                v["error"].as_str().unwrap(),
                "{}",
                v["name"]
            ),
            Ok(_) => panic!("accepted {}", v["name"]),
        }
    }
    for v in f["vectors"].as_array().unwrap() {
        let wire = bytes(v, "wire_hex");
        for end in 0..wire.len() {
            assert!(parse(&wire[..end]).is_err(), "accepted prefix {end}");
        }
    }
    assert_eq!(
        parse(&vec![0; MAX_CONTROL_BYTES + 1]).map(|_| ()),
        Err(Error::Limit)
    );
}

#[test]
fn roster_budget_is_independently_measured_and_rejects_one_more_member() {
    let f = fixtures();
    let wire = bytes(&f["vectors"][0], "wire_hex");
    let mut message = parse(&wire).unwrap().message;
    let mut members = (0..16u8)
        .map(|i| {
            let signing_key = VerificationKey::from_bytes([30 + i; VERIFICATION_KEY_BYTES]);
            let kem_key = KemPublicKey::from_bytes([60 + i; KEM_PUBLIC_KEY_BYTES]);
            Member {
                id: device_id(&signing_key, &kem_key).unwrap(),
                signing_key,
                kem_key,
                admin: i == 0,
            }
        })
        .collect::<Vec<_>>();
    members.sort_by_key(|m| m.id);
    let Body::Genesis(g) = &mut message.body else {
        unreachable!()
    };
    g.roster = members;
    let encoded = encode(&message, &[0; SIGNATURE_BYTES]).unwrap();
    assert_eq!(
        encoded.len(),
        f["max_genesis_bytes"].as_u64().unwrap() as usize
    );
    assert_eq!(
        libcrux_sha2::sha384(&encoded).as_slice(),
        bytes(&f, "max_genesis_sha384")
    );
    assert!(encoded.len() < MAX_CONTROL_BYTES);
    parse(&encoded).unwrap();
    let Body::Genesis(g) = &mut message.body else {
        unreachable!()
    };
    g.roster.push(g.roster[0].clone());
    assert_eq!(
        parse(&encode(&message, &[0; SIGNATURE_BYTES]).unwrap()).map(|_| ()),
        Err(Error::Limit)
    );
}

#[test]
fn malformed_but_admin_signed_key_plaintext_is_rejected_after_hpke() {
    use hpke_rs::{Hpke, HpkePublicKey, Mode};
    use hpke_rs_crypto::types::{AeadAlgorithm, KdfAlgorithm, KemAlgorithm};
    type Provider = hpke_rs_libcrux::HpkeLibcrux<hpke_rs_libcrux::HpkeLibcruxPrng>;
    let s = setup();
    let envelope = wrap(&s.authority, &s.a, s.b.id(), &epoch_key(0)).unwrap();
    let base = parse(&envelope).unwrap().message;
    // Change 3 is well formed but wraps a key other than the committed one:
    // an admin cannot hand this device a different key for the epoch.
    for change in 0..4 {
        let mut e = minicbor::Encoder::new(Vec::new());
        e.array(3)
            .unwrap()
            .u64(if change == 0 { 1 } else { 0 })
            .unwrap()
            .bytes(&vec![1; if change == 1 { 31 } else { 32 }])
            .unwrap()
            .str(if change == 2 { "other" } else { POLICY })
            .unwrap();
        let mut hpke: Hpke<Provider> = Hpke::try_new(
            Mode::Base,
            KemAlgorithm::MlKem768P256,
            KdfAlgorithm::HkdfSha256,
            AeadAlgorithm::Aes256Gcm,
        )
        .unwrap();
        let (enc, ciphertext) = hpke
            .seal(
                &HpkePublicKey::from(s.b.kem_public_key().as_bytes()),
                &envelope_info(&base).unwrap(),
                &envelope_aad(&base).unwrap(),
                &e.into_writer(),
                None,
                None,
                None,
            )
            .unwrap();
        let mut m = base.clone();
        let Body::KeyEnvelope(k) = &mut m.body else {
            unreachable!()
        };
        k.encapsulation = enc;
        k.ciphertext = ciphertext;
        let expected = if change == 3 {
            Error::KeyCommitment
        } else {
            Error::Format
        };
        assert_eq!(
            s.authority
                .unwrap_key(&signed(&m, s.a.signing_key()), &s.b)
                .map(|_| ()),
            Err(expected),
            "change {change}"
        );
    }
}

#[test]
fn epoch_key_commitments_bind_each_epoch_and_refuse_other_keys() {
    let f = fixtures();
    for c in f["key_commitments"].as_array().unwrap() {
        let key = ContentKey::new(bytes(c, "key_hex").try_into().unwrap());
        let epoch = c["epoch"].as_u64().unwrap();
        assert_eq!(
            key_commitment(&route(), c["generation"].as_u64().unwrap(), epoch, &key)
                .unwrap()
                .as_slice(),
            bytes(c, "commitment_hex")
        );
    }
    let mut s = setup();
    let b = s.b.id();
    assert_eq!(
        wrap(&s.authority, &s.a, b, &epoch_key(1)),
        Err(Error::KeyCommitment)
    );
    let m = signed(&transition(&s), s.a.signing_key());
    s.authority.apply_membership(&m).unwrap();
    // The previous epoch's key no longer matches; the rotated key does.
    assert_eq!(
        wrap(&s.authority, &s.a, b, &epoch_key(0)),
        Err(Error::KeyCommitment)
    );
    let envelope = wrap(&s.authority, &s.a, b, &epoch_key(1)).unwrap();
    s.authority.unwrap_key(&envelope, &s.b).unwrap();
}

#[test]
fn unsupported_roster_key_material_and_state_limits_fail_closed() {
    let mut s = setup();
    let mut genesis = parse(&s.genesis).unwrap().message;
    let Body::Genesis(g) = &mut genesis.body else {
        unreachable!()
    };
    let target = g.roster.iter_mut().find(|m| !m.admin).unwrap();
    *target = with_kem(target, [0; KEM_PUBLIC_KEY_BYTES]);
    target.id = device_id(&target.signing_key, &target.kem_key).unwrap();
    g.roster.sort_by_key(|m| m.id);
    let bytes = signed(&genesis, &s.root.signing);
    let state = Authority::bootstrap(&bytes, route(), &s.root.verification).unwrap();
    let bad = state
        .members()
        .unwrap()
        .into_iter()
        .find(|m| !m.admin)
        .unwrap();
    assert_eq!(
        wrap(&state, &s.a, bad.id, &epoch_key(0)),
        Err(Error::Crypto)
    );
    for _ in 0..16 {
        let m = transition(&s);
        s.authority
            .apply_membership(&signed(&m, s.a.signing_key()))
            .unwrap();
    }
    let next = transition(&s);
    assert_eq!(
        s.authority
            .apply_membership(&signed(&next, s.a.signing_key())),
        Err(Error::Limit)
    );
    for i in 0..16 {
        let previous = if i == 0 { None } else { Some([i - 1; 32]) };
        let m = writer(&s.authority, &s.a, [i; 32], i as u64, previous);
        s.authority
            .admit_writer(&signed(&m, s.a.signing_key()), DOC)
            .unwrap();
    }
    let extra = writer(&s.authority, &s.a, [17; 32], 17, Some([15; 32]));
    assert_eq!(
        s.authority
            .admit_writer(&signed(&extra, s.a.signing_key()), DOC),
        Err(Error::Limit)
    );
}

#[test]
fn replacement_retires_old_writer_and_replay_does_not_reactivate_it() {
    let mut s = setup();
    let cek = epoch_key(s.authority.epoch().unwrap());
    let envelope = wrap(&s.authority, &s.a, s.b.id(), &cek).unwrap();
    let key = s.authority.unwrap_key(&envelope, &s.b).unwrap();
    let author = s.a.id();
    let manifest = s.authority.manifest();
    let old = [21; 32];
    let next = [22; 32];
    let start = signed(
        &writer(&s.authority, &s.a, old, 71, None),
        s.a.signing_key(),
    );
    s.authority.admit_writer(&start, DOC).unwrap();
    let id = fresh();
    let old_record = seal_as(
        header(&s.authority, &author, &old, &id, &manifest),
        &cek,
        PLAINTEXT,
        &s.a,
    );
    open(&s.authority, &old_record, &key).unwrap();
    let replacement = signed(
        &writer(&s.authority, &s.a, next, 72, Some(old)),
        s.a.signing_key(),
    );
    s.authority.admit_writer(&replacement, DOC).unwrap();
    assert_eq!(open(&s.authority, &old_record, &key), Err(Error::Stale));
    // The retired writer's record still authenticates as history.
    let history = s
        .authority
        .authenticate_history(&old_record, &key, &DOC)
        .unwrap();
    assert_eq!(history.actor(), 71);
    assert_eq!(history.open().unwrap(), PLAINTEXT);
    assert_eq!(s.authority.admit_writer(&start, DOC), Ok(false));
    // Observe the old retry before any newer retry could restore the right head.
    assert_eq!(open(&s.authority, &old_record, &key), Err(Error::Stale));
    assert_eq!(s.authority.admit_writer(&replacement, DOC), Ok(false));
    assert_eq!(open(&s.authority, &old_record, &key), Err(Error::Stale));
    let id = fresh();
    let record = seal_as(
        header(&s.authority, &author, &next, &id, &manifest),
        &cek,
        PLAINTEXT,
        &s.a,
    );
    open(&s.authority, &record, &key).unwrap();
}

#[test]
fn writer_history_reports_legacy_replacement_as_unbounded_not_as_a_root() {
    let mut s = setup();
    let (old, next, third) = ([21; 32], [22; 32], [23; 32]);
    let root = signed(
        &writer(&s.authority, &s.a, old, 71, None),
        s.a.signing_key(),
    );
    let legacy = signed(
        &writer(&s.authority, &s.a, next, 72, Some(old)),
        s.a.signing_key(),
    );
    let handoff = Message {
        route: route(),
        signer: s.a.id(),
        body: Body::WriterHandoff(WriterHandoff {
            manifest: s.authority.manifest(),
            document: DOC,
            device: s.a.id(),
            incarnation: third,
            actor: 73,
            previous: next,
            closing: None,
        }),
    };
    for bytes in [root, legacy, signed(&handoff, s.a.signing_key())] {
        assert_eq!(s.authority.admit_writer(&bytes, DOC), Ok(true));
    }
    let history = |i| s.authority.writer_history(DOC, i).unwrap();
    assert_eq!(history(old).predecessor, None);
    assert_eq!(history(old).closed, Some(Boundary::Unbounded));
    assert_eq!(history(next).predecessor, Some((old, Boundary::Unbounded)));
    assert_eq!(history(next).closed, Some(Boundary::Closed(None)));
    assert_eq!(
        history(third).predecessor,
        Some((next, Boundary::Closed(None)))
    );
    assert_eq!(history(third).closed, None);
    assert!(!history(third).frozen);
    assert_eq!(s.authority.writer_histories(DOC).len(), 3);
    assert_eq!(
        s.authority.writer_history(DOC, [9; 32]),
        Err(Error::MissingWriter)
    );
}

#[test]
fn resigned_identical_writer_claim_is_idempotent_and_freezes_nothing() {
    let mut s = setup();
    let message = writer(&s.authority, &s.a, [21; 32], 71, None);
    let first = signed(&message, s.a.signing_key());
    let again = signed(&message, s.a.signing_key());
    assert_ne!(first, again, "ML-DSA signing is randomized");
    assert_eq!(s.authority.admit_writer(&first, DOC), Ok(true));
    assert_eq!(s.authority.admit_writer(&again, DOC), Ok(false));
    assert!(s.authority.writer_fork(DOC, s.a.id()).is_none());
    // A changed claim for the same incarnation is still a contradiction.
    let changed = signed(
        &writer(&s.authority, &s.a, [21; 32], 72, None),
        s.a.signing_key(),
    );
    assert_eq!(s.authority.admit_writer(&changed, DOC), Err(Error::Fork));
    let (kept, competing) = s.authority.writer_fork(DOC, s.a.id()).unwrap();
    assert_eq!((kept, competing), (first.as_slice(), changed.as_slice()));
    assert!(s.authority.lineage_frozen(DOC, &s.a.id()));
}

#[test]
fn competing_writer_successors_freeze_the_lineage_in_both_orders() {
    for root_conflict in [false, true] {
        for reversed in [false, true] {
            let mut s = setup();
            let cek = epoch_key(s.authority.epoch().unwrap());
            let envelope = wrap(&s.authority, &s.a, s.b.id(), &cek).unwrap();
            let key = s.authority.unwrap_key(&envelope, &s.b).unwrap();
            let base = [20; 32];
            if !root_conflict {
                let m = signed(
                    &writer(&s.authority, &s.a, base, 70, None),
                    s.a.signing_key(),
                );
                s.authority.admit_writer(&m, DOC).unwrap();
            }
            let prev = if root_conflict { None } else { Some(base) };
            let candidates = [([21; 32], 71), ([22; 32], 72)];
            let (first_inc, first_actor) = candidates[usize::from(reversed)];
            let (second_inc, second_actor) = candidates[usize::from(!reversed)];
            let first = signed(
                &writer(&s.authority, &s.a, first_inc, first_actor, prev),
                s.a.signing_key(),
            );
            s.authority.admit_writer(&first, DOC).unwrap();
            let second_message = writer(&s.authority, &s.a, second_inc, second_actor, prev);
            assert_eq!(
                s.authority
                    .admit_writer(&signed(&second_message, s.b.signing_key()), DOC),
                Err(Error::Signature)
            );
            let author = s.a.id();
            let manifest = s.authority.manifest();
            let id = fresh();
            let record = seal_as(
                header(&s.authority, &author, &first_inc, &id, &manifest),
                &cek,
                PLAINTEXT,
                &s.a,
            );
            open(&s.authority, &record, &key).unwrap();
            if !root_conflict {
                // A delayed competitor of an older parent still freezes the chain.
                let advance = signed(
                    &writer(&s.authority, &s.a, [26; 32], 76, Some(first_inc)),
                    s.a.signing_key(),
                );
                s.authority.admit_writer(&advance, DOC).unwrap();
            }
            let second = signed(&second_message, s.a.signing_key());
            assert_eq!(s.authority.admit_writer(&second, DOC), Err(Error::Fork));
            assert_eq!(
                s.authority.writer_fork(DOC, author),
                Some((first.as_slice(), second.as_slice()))
            );
            assert_eq!(open(&s.authority, &record, &key), Err(Error::Fork));
            assert_eq!(s.authority.admit_writer(&first, DOC), Err(Error::Fork));
            let later = signed(
                &writer(&s.authority, &s.a, [23; 32], 73, Some(first_inc)),
                s.a.signing_key(),
            );
            assert_eq!(s.authority.admit_writer(&later, DOC), Err(Error::Fork));
            // The freeze is scoped to this device/document, not all members.
            let other = signed(
                &writer(&s.authority, &s.b, [24; 32], 74, None),
                s.b.signing_key(),
            );
            s.authority.admit_writer(&other, DOC).unwrap();
            let mut other_doc = writer(&s.authority, &s.a, [25; 32], 75, None);
            if let Body::WriterStart(w) = &mut other_doc.body {
                w.document = [5; 16];
            }
            s.authority
                .admit_writer(&signed(&other_doc, s.a.signing_key()), [5; 16])
                .unwrap();
            wrap(&s.authority, &s.a, s.b.id(), &cek).unwrap();
        }
    }
}

#[test]
fn parser_admits_structural_values_the_authority_does_not_support() {
    let s = setup();
    for change in 0..4 {
        let mut message = if change == 3 {
            transition(&s)
        } else {
            parse(&s.genesis).unwrap().message
        };
        match &mut message.body {
            Body::Genesis(g) => match change {
                0 => g.generation = u64::MAX,
                1 => g.epoch = u64::MAX,
                _ => g.checkpoint = Some([7; 48]),
            },
            Body::Membership(m) => m.generation = u64::MAX,
            _ => unreachable!(),
        }
        let signer = if change == 3 {
            s.a.signing_key()
        } else {
            &s.root.signing
        };
        let bytes = signed(&message, signer);
        parse(&bytes).unwrap();
        if change == 3 {
            let mut state = s.authority.clone();
            assert_eq!(state.apply_membership(&bytes), Err(Error::Stale));
            assert_eq!(state.manifest(), s.authority.manifest());
        } else {
            assert_eq!(
                Authority::bootstrap(&bytes, route(), &s.root.verification).map(|_| ()),
                Err(Error::Unsupported)
            );
        }
    }
}

#[test]
fn secret_bearing_values_never_print_their_bytes() {
    let s = setup();
    let envelope = wrap(&s.authority, &s.a, s.b.id(), &epoch_key(0)).unwrap();
    let key = s.authority.unwrap_key(&envelope, &s.b).unwrap();
    assert_eq!(format!("{:?}", s.a), "DeviceKeys(redacted)");
    assert_eq!(format!("{key:?}"), "EpochKey { epoch: 0, .. }");
    assert_eq!(format!("{:?}", key.content_key()), "ContentKey(redacted)");
    let invitation = s
        .authority
        .invite(InvitationSecret::new([0xab; 16]))
        .unwrap();
    let printed = format!("{invitation:?}");
    assert!(
        printed.contains("secret: InvitationSecret(redacted)"),
        "{printed}"
    );
    assert!(!printed.contains("171, 171"), "no secret bytes: {printed}");
}

// Enrollment through a one-time invitation (#37).

fn secret() -> InvitationSecret {
    let mut bytes = [0; 16];
    bytes.copy_from_slice(&fresh()[..16]);
    InvitationSecret::new(bytes)
}

/// The admin's transition adding `member`, as the next epoch.
fn adding(s: &Setup, member: Member) -> Vec<u8> {
    let mut m = transition(s);
    let roster = &mut membership(&mut m).roster;
    roster.push(member);
    roster.sort_by_key(|m| m.id);
    signed(&m, s.a.signing_key())
}

#[test]
fn a_device_enrolls_through_an_invitation_and_receives_current_and_retained_keys() {
    let mut s = setup();
    let genesis = s.authority.manifest();
    let rotation = signed(&transition(&s), s.a.signing_key());
    s.authority.apply_membership(&rotation).unwrap();
    let epoch_one = s.authority.manifest();
    // The admin shows a QR code; the new device scans it.
    let shown = s.authority.invite(secret()).unwrap();
    let scanned = Invitation::parse(&shown.encode().unwrap()).unwrap();
    let c = device();
    let request = request_enrollment(&scanned, &c, fresh()).unwrap();
    let member = s.authority.accept_enrollment(&request, &shown).unwrap();
    assert_eq!(member, c.member(false));
    let add = adding(&s, member);
    assert!(s.authority.apply_membership(&add).unwrap());
    assert!(s.authority.members().unwrap().contains(&c.member(false)));
    // The new device pins the root from the invitation and replays history.
    let mut joined = Authority::bootstrap_from_invitation(&s.genesis, &scanned).unwrap();
    joined.apply_membership(&rotation).unwrap();
    joined.apply_membership(&add).unwrap();
    assert_eq!(joined.manifest(), s.authority.manifest());
    let current = wrap(&s.authority, &s.a, c.id(), &epoch_key(2)).unwrap();
    let key = joined.unwrap_key(&current, &c).unwrap();
    assert!(is_epoch_key(key.content_key(), 2));
    joined.check_epoch_key(&key).unwrap();
    // Retained history: each earlier epoch's key, by its manifest.
    for (manifest, epoch) in [(genesis, 0), (epoch_one, 1)] {
        let envelope = s
            .authority
            .wrap_history_key(&s.a, c.id(), manifest, &epoch_key(epoch), fresh())
            .unwrap();
        assert_eq!(
            joined.unwrap_key(&envelope, &c).map(|_| ()),
            Err(Error::Stale),
            "the current-epoch unwrap refuses history"
        );
        let old = joined.unwrap_history_key(&envelope, &c).unwrap();
        assert_eq!((old.manifest(), old.epoch()), (manifest, epoch));
        assert!(is_epoch_key(old.content_key(), epoch));
        // A history key opens that epoch's records and seals none.
        assert_eq!(joined.check_epoch_key(&old), Err(Error::Stale));
    }
}

/// Whether `key` is the fixture content key of `epoch`.
fn is_epoch_key(key: &ContentKey, epoch: u64) -> bool {
    key_commitment(&route(), 0, epoch, key).unwrap() == commitment(epoch)
}

#[test]
fn a_typed_code_and_its_offer_rebuild_the_invitation() {
    let s = setup();
    let shown = s.authority.invite(secret()).unwrap();
    let code = shown.secret.code();
    assert_eq!(code.len(), CODE_LENGTH);
    let lookup = shown.secret.lookup(&route()).unwrap();
    let offer = shown.offer().unwrap();
    // Typed loosely: lower case, grouped, O for 0 and L for 1.
    let typed: String = code
        .to_lowercase()
        .chars()
        .enumerate()
        .flat_map(|(i, c)| {
            let c = match c {
                '0' => 'o',
                '1' => 'l',
                c => c,
            };
            (i % 4 == 0 && i > 0).then_some('-').into_iter().chain([c])
        })
        .collect();
    let typed = InvitationSecret::from_code(&typed).unwrap();
    assert_eq!(typed.lookup(&route()).unwrap(), lookup);
    let rebuilt = Invitation::from_offer(typed, &route(), &offer).unwrap();
    assert_eq!(
        (rebuilt.route.clone(), rebuilt.root, rebuilt.manifest),
        (route(), s.authority.root(), s.authority.manifest())
    );
    // The rebuilt invitation enrolls exactly like a scanned one.
    let c = device();
    let request = request_enrollment(&rebuilt, &c, fresh()).unwrap();
    s.authority.accept_enrollment(&request, &shown).unwrap();
    // A forged or misrouted offer, or the wrong code, is refused.
    let code_of = |text: &str| InvitationSecret::from_code(text).unwrap();
    let mut forged = offer.clone();
    let at = forged.len() - 60; // inside the manifest
    forged[at] ^= 1;
    assert_eq!(
        Invitation::from_offer(code_of(&code), &route(), &forged).map(|_| ()),
        Err(Error::Unauthorized)
    );
    let mut elsewhere = route();
    elsewhere.collection = [9; 16];
    assert_eq!(
        Invitation::from_offer(code_of(&code), &elsewhere, &offer).map(|_| ()),
        Err(Error::Context)
    );
    assert_ne!(code_of(&code).lookup(&elsewhere).unwrap(), lookup);
    assert_eq!(
        Invitation::from_offer(secret(), &route(), &offer).map(|_| ()),
        Err(Error::Unauthorized)
    );
    // Malformed codes: length, alphabet, and more than 128 bits.
    for bad in [
        &code[1..],
        &format!("{code}0"),
        &format!("U{}", &code[1..]),
        &format!("8{}", &code[1..]),
        "",
    ] {
        assert_eq!(
            InvitationSecret::from_code(bad).map(|_| ()),
            Err(Error::Format),
            "{bad}"
        );
    }
}

#[test]
fn invitation_formats_match_independent_known_answers() {
    // tests/vectors/invitation_kat.py: Python standard-library HKDF, HMAC,
    // CBOR and base32, independent of this crate.
    let invitation = Invitation {
        route: route(),
        root: [0x11; 48],
        manifest: [0x22; 48],
        secret: InvitationSecret::new(std::array::from_fn(|i| i as u8)),
    };
    assert_eq!(invitation.secret.code(), "00041061050R3GG28A1C60T3GF");
    assert_eq!(
        hex(&invitation.secret.lookup(&route()).unwrap()),
        "f9a70d84bbbe4c079fb398fa2006ef28"
    );
    let encoded = invitation.encode().unwrap();
    assert_eq!(
        hex(&encoded),
        concat!(
            "8900726d6174746572732d73796e632d70712d76306a696e7669746174696f6e",
            "50010101010101010101010101010101015002020202020202020202020202020202",
            "5003030303030303030303030303030303",
            "5830111111111111111111111111111111111111111111111111111111111111111111111111111111111111111111111111",
            "5830222222222222222222222222222222222222222222222222222222222222222222222222222222222222222222222222",
            "50000102030405060708090a0b0c0d0e0f"
        )
    );
    assert_eq!(
        hex(&invitation.offer().unwrap()[encoded.len() - 17 + 6..]),
        concat!(
            "5830e64b19ef3139a7f3d9f530a1b869e9161c79d6e18eb91e06fea2aa4b0235",
            "7d8942e3b5b2dfbad48a81db4d6d2d7a2e42"
        ),
        "the offer's tag"
    );
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[test]
fn invitations_reject_noncanonical_and_oversized_payloads() {
    let s = setup();
    let shown = s.authority.invite(secret()).unwrap();
    let encoded = shown.encode().unwrap();
    for (name, bytes, error) in [
        (
            "truncated",
            encoded[..encoded.len() - 1].to_vec(),
            Error::Format,
        ),
        (
            "trailing",
            [encoded.clone(), vec![0]].concat(),
            Error::Format,
        ),
        ("oversized", vec![0; 257], Error::Limit),
        ("an offer", shown.offer().unwrap(), Error::Unsupported),
    ] {
        assert_eq!(Invitation::parse(&bytes).map(|_| ()), Err(error), "{name}");
    }
    // A non-minimal length for the secret (0x58 0x10 instead of 0x50).
    let mut long = encoded[..encoded.len() - 17].to_vec();
    long.extend([0x58, 0x10]);
    long.extend(&encoded[encoded.len() - 16..]);
    assert_eq!(
        Invitation::parse(&long).map(|_| ()),
        Err(Error::NonCanonical)
    );
}

#[test]
fn enrollment_refuses_server_substituted_keys_and_requests_without_the_secret() {
    let mut s = setup();
    let shown = s.authority.invite(secret()).unwrap();
    let c = device();
    let honest = request_enrollment(&shown, &c, fresh()).unwrap();
    let request = |bytes: &[u8]| match parse(bytes).unwrap().message.body {
        Body::EnrollmentRequest(r) => *r,
        _ => unreachable!(),
    };
    let resign = |r: &matters_sync::authority::EnrollmentRequest, as_: &DeviceKeys| {
        signed(
            &Message {
                route: route(),
                signer: device_id(&r.signing_key, &r.kem_key).unwrap(),
                body: Body::EnrollmentRequest(Box::new(r.clone())),
            },
            as_.signing_key(),
        )
    };
    // A server swaps in its own keys and signs for them: no secret, so the
    // binding fails.
    let server = device();
    let mut swapped = request(&honest);
    swapped.signing_key = server.verification_key().clone();
    swapped.kem_key = server.kem_public_key().clone();
    assert_eq!(
        s.authority
            .accept_enrollment(&resign(&swapped, &server), &shown)
            .map(|_| ()),
        Err(Error::Unauthorized)
    );
    // Swapping only the KEM key breaks the device ID the signature names.
    let mut kem_only = request(&honest);
    kem_only.kem_key = server.kem_public_key().clone();
    let mut message = parse(&honest).unwrap().message;
    message.body = Body::EnrollmentRequest(Box::new(kem_only));
    assert_eq!(
        s.authority
            .accept_enrollment(&signed(&message, c.signing_key()), &shown)
            .map(|_| ()),
        Err(Error::Identity)
    );
    // Signed by a key other than the one it names: no proof of possession.
    assert_eq!(
        s.authority
            .accept_enrollment(&resign(&request(&honest), &server), &shown)
            .map(|_| ()),
        Err(Error::Signature)
    );
    // A request made for another invitation, or for none (login only).
    let other = s.authority.invite(secret()).unwrap();
    assert_eq!(
        s.authority.accept_enrollment(&honest, &other).map(|_| ()),
        Err(Error::Unauthorized)
    );
    let mut unbound = request(&honest);
    unbound.binding = [0; 48];
    assert_eq!(
        s.authority
            .accept_enrollment(&resign(&unbound, &c), &shown)
            .map(|_| ()),
        Err(Error::Unauthorized)
    );
    // An existing member, another collection, another kind of message.
    let b_request = request_enrollment(&shown, &s.b, fresh()).unwrap();
    assert_eq!(
        s.authority
            .accept_enrollment(&b_request, &shown)
            .map(|_| ()),
        Err(Error::Identity)
    );
    let mut misrouted = Invitation::parse(&shown.encode().unwrap()).unwrap();
    misrouted.route.tenant = [9; 16];
    assert_eq!(
        s.authority
            .accept_enrollment(&honest, &misrouted)
            .map(|_| ()),
        Err(Error::Context)
    );
    let elsewhere = request_enrollment(&misrouted, &c, fresh()).unwrap();
    assert_eq!(
        s.authority
            .accept_enrollment(&elsewhere, &shown)
            .map(|_| ()),
        Err(Error::Context)
    );
    let mut other_root = Invitation::parse(&shown.encode().unwrap()).unwrap();
    other_root.root = [9; 48];
    assert_eq!(
        s.authority
            .accept_enrollment(&honest, &other_root)
            .map(|_| ()),
        Err(Error::Context)
    );
    assert_eq!(
        Authority::bootstrap_from_invitation(&s.genesis, &other_root).map(|_| ()),
        Err(Error::Identity)
    );
    assert_eq!(
        s.authority
            .accept_enrollment(&s.genesis, &shown)
            .map(|_| ()),
        Err(Error::Unsupported)
    );
    // After the manifest moves on, the invitation and its request are stale.
    let rotation = signed(&transition(&s), s.a.signing_key());
    s.authority.apply_membership(&rotation).unwrap();
    assert_eq!(
        s.authority.accept_enrollment(&honest, &shown).map(|_| ()),
        Err(Error::Stale)
    );
    let fresh_invite = s.authority.invite(secret()).unwrap();
    assert_eq!(
        s.authority
            .accept_enrollment(&honest, &fresh_invite)
            .map(|_| ()),
        Err(Error::Stale),
        "a request names the manifest it was made under"
    );
}

#[test]
fn history_envelopes_reach_current_members_only_under_each_epochs_commitment() {
    let mut s = setup();
    let genesis = s.authority.manifest();
    let rotation = signed(&transition(&s), s.a.signing_key());
    s.authority.apply_membership(&rotation).unwrap();
    // Wrapping: the admin only, a known manifest, its epoch's key.
    assert_eq!(
        s.authority
            .wrap_history_key(&s.b, s.b.id(), genesis, &epoch_key(0), fresh())
            .map(|_| ()),
        Err(Error::Unauthorized)
    );
    assert_eq!(
        s.authority
            .wrap_history_key(&s.a, s.b.id(), [7; 48], &epoch_key(0), fresh())
            .map(|_| ()),
        Err(Error::MissingPrevious)
    );
    assert_eq!(
        s.authority
            .wrap_history_key(&s.a, s.b.id(), genesis, &epoch_key(1), fresh())
            .map(|_| ()),
        Err(Error::KeyCommitment)
    );
    let to_b = s
        .authority
        .wrap_history_key(&s.a, s.b.id(), genesis, &epoch_key(0), fresh())
        .unwrap();
    assert_eq!(
        s.authority.unwrap_history_key(&to_b, &s.a).map(|_| ()),
        Err(Error::Identity),
        "another member cannot open it"
    );
    // Once b is removed it is no recipient, and its envelopes stop opening.
    let mut revoke = transition(&s);
    let a = s.a.id();
    membership(&mut revoke).roster.retain(|m| m.id == a);
    s.authority
        .apply_membership(&signed(&revoke, s.a.signing_key()))
        .unwrap();
    assert_eq!(
        s.authority
            .wrap_history_key(&s.a, s.b.id(), genesis, &epoch_key(0), fresh())
            .map(|_| ()),
        Err(Error::Unauthorized)
    );
    assert_eq!(
        s.authority.unwrap_history_key(&to_b, &s.b).map(|_| ()),
        Err(Error::Unauthorized)
    );
    // An envelope whose epoch disagrees with its manifest's is stale.
    let to_a = s
        .authority
        .wrap_history_key(&s.a, s.a.id(), genesis, &epoch_key(0), fresh())
        .unwrap();
    s.authority.unwrap_history_key(&to_a, &s.a).unwrap();
    let mut message = parse(&to_a).unwrap().message;
    if let Body::KeyEnvelope(k) = &mut message.body {
        k.epoch = 1;
    }
    assert_eq!(
        s.authority
            .unwrap_history_key(&signed(&message, s.a.signing_key()), &s.a)
            .map(|_| ()),
        Err(Error::Stale)
    );
}

#[test]
fn a_full_roster_refuses_enrollment_before_the_admin_signs_anything() {
    let mut s = setup();
    let mut m = transition(&s);
    let roster = &mut membership(&mut m).roster;
    for i in 0..14u8 {
        // Fixture keys: the roster binds bytes, not key validity.
        let signing_key = VerificationKey::from_bytes([30 + i; VERIFICATION_KEY_BYTES]);
        let kem_key = KemPublicKey::from_bytes([60 + i; KEM_PUBLIC_KEY_BYTES]);
        roster.push(Member {
            id: device_id(&signing_key, &kem_key).unwrap(),
            signing_key,
            kem_key,
            admin: false,
        });
    }
    roster.sort_by_key(|m| m.id);
    s.authority
        .apply_membership(&signed(&m, s.a.signing_key()))
        .unwrap();
    let shown = s.authority.invite(secret()).unwrap();
    let request = request_enrollment(&shown, &device(), fresh()).unwrap();
    assert_eq!(
        s.authority.accept_enrollment(&request, &shown).map(|_| ()),
        Err(Error::Limit)
    );
}

#[test]
fn offers_refuse_other_versions_and_noncanonical_encodings() {
    let s = setup();
    let shown = s.authority.invite(secret()).unwrap();
    let offer = shown.offer().unwrap();
    let code = shown.secret.code();
    let from = |bytes: &[u8]| {
        Invitation::from_offer(InvitationSecret::from_code(&code).unwrap(), &route(), bytes)
            .map(|_| ())
    };
    let mut version = offer.clone();
    version[1] = 1;
    assert_eq!(from(&version), Err(Error::Unsupported));
    // The tag's length as two bytes (0x59 0x0030): the tag still verifies,
    // but the bytes are not the canonical encoding.
    let at = offer.len() - 50;
    assert_eq!(&offer[at..at + 2], &[0x58, 0x30]);
    let long = [&offer[..at], &[0x59, 0x00, 0x30], &offer[at + 2..]].concat();
    assert_eq!(from(&long), Err(Error::NonCanonical));
    let mut invitation = shown.encode().unwrap();
    invitation[1] = 1;
    assert_eq!(
        Invitation::parse(&invitation).map(|_| ()),
        Err(Error::Unsupported)
    );
}

// Recovery after every device is lost (#37).

/// A collection whose root and recovery key come from recovery words, with
/// `admin` as its first device.
struct Recoverable {
    words: String,
    keys: RecoveryKeys,
    admin: DeviceKeys,
    genesis: Vec<u8>,
    authority: Authority,
}

fn recoverable() -> Recoverable {
    let secret = RecoverySecret::new(fresh());
    let keys = secret.keys(&route()).unwrap();
    let admin = device();
    let genesis = genesis(&route(), &keys, &admin, &epoch_key(0), fresh()).unwrap();
    let authority = Authority::bootstrap(&genesis, route(), keys.root_key()).unwrap();
    Recoverable {
        words: secret.words().unwrap(),
        keys,
        admin,
        genesis,
        authority,
    }
}

/// The next membership transition of `authority` as `admin`, same roster.
fn step(authority: &Authority, admin: &DeviceKeys) -> Message {
    let epoch = authority.epoch().unwrap() + 1;
    let generation = authority.generation().unwrap();
    Message {
        route: route(),
        signer: admin.id(),
        body: Body::Membership(Membership {
            generation,
            sequence: authority.sequence().unwrap() + 1,
            previous: authority.manifest(),
            roster: authority.members().unwrap(),
            admin: admin.id(),
            epoch,
            recovery_key: authority.recovery().unwrap(),
            key_commitment: key_commitment(&route(), generation, epoch, &epoch_key(epoch)).unwrap(),
        }),
    }
}

fn recovery_body(message: &mut Message) -> &mut Membership {
    match &mut message.body {
        Body::RootRecovery(m) => m,
        _ => unreachable!(),
    }
}

#[test]
fn recovery_words_match_the_published_bip39_vectors_and_refuse_bad_input() {
    // BIP39's reference vectors (trezor/python-mnemonic vectors.json),
    // English, 256-bit entropy.
    for (entropy, words) in [
        (
            [0x00; 32],
            "abandon abandon abandon abandon abandon abandon abandon abandon abandon \
             abandon abandon abandon abandon abandon abandon abandon abandon abandon \
             abandon abandon abandon abandon abandon art",
        ),
        (
            [0x7f; 32],
            "legal winner thank year wave sausage worth useful legal winner thank year \
             wave sausage worth useful legal winner thank year wave sausage worth title",
        ),
        (
            [0x80; 32],
            "letter advice cage absurd amount doctor acoustic avoid letter advice cage \
             absurd amount doctor acoustic avoid letter advice cage absurd amount doctor \
             acoustic bless",
        ),
        (
            [0xff; 32],
            "zoo zoo zoo zoo zoo zoo zoo zoo zoo zoo zoo zoo zoo zoo zoo zoo zoo zoo zoo \
             zoo zoo zoo zoo vote",
        ),
    ] {
        let words = words.split_whitespace().collect::<Vec<_>>().join(" ");
        assert_eq!(RecoverySecret::new(entropy).words().unwrap(), words);
        // Typed back loosely: any case, any whitespace.
        let typed = format!("  {}\n", words.to_uppercase().replace(' ', " \t "));
        let parsed = RecoverySecret::from_words(&typed).unwrap();
        assert_eq!(
            recovery_seeds(&parsed, &route()).unwrap(),
            recovery_seeds(&RecoverySecret::new(entropy), &route()).unwrap()
        );
    }
    let zeros = RecoverySecret::new([0; 32]).words().unwrap();
    for (name, bad) in [
        ("bad checksum", zeros.replace(" art", " abandon")),
        ("23 words", zeros.replacen("abandon ", "", 1)),
        (
            "12 words",
            "abandon abandon abandon abandon abandon abandon abandon abandon abandon \
             abandon abandon about"
                .into(),
        ),
        ("unknown word", zeros.replacen("abandon", "abandonn", 1)),
        ("empty", String::new()),
    ] {
        assert_eq!(
            RecoverySecret::from_words(&bad).map(|_| ()),
            Err(Error::Format),
            "{name}"
        );
    }
}

#[test]
fn recovery_seeds_match_independent_known_answers_and_bind_the_route() {
    // tests/vectors/invitation_kat.py, Python standard library.
    let secret = RecoverySecret::new(std::array::from_fn(|i| i as u8));
    let (root, kem) = recovery_seeds(&secret, &route()).unwrap();
    assert_eq!(
        hex(&root),
        "78dd282134c40fa3b8ff0a8b0524b97c872f4722ec60a8f83e67ac44e4e2f162"
    );
    assert_eq!(
        hex(&kem),
        "62caaa193d194693ac6c853115cee129325ac232f83e9b9aedcab8a2e74aeae1"
    );
    let mut elsewhere = route();
    elsewhere.collection = [9; 16];
    let (other_root, other_kem) = recovery_seeds(&secret, &elsewhere).unwrap();
    assert!(other_root != root && other_kem != kem);
    // The words always define the same keys for a route.
    let again = secret.keys(&route()).unwrap();
    let keys = secret.keys(&route()).unwrap();
    assert_eq!(again.root_id().unwrap(), keys.root_id().unwrap());
    assert_eq!(again.kem_public_key(), keys.kem_public_key());
    assert_ne!(
        secret.keys(&elsewhere).unwrap().root_id().unwrap(),
        keys.root_id().unwrap()
    );
}

#[test]
fn after_every_device_is_lost_the_words_recover_history_and_start_a_new_generation() {
    let mut r = recoverable();
    let b = device();
    // The collection grows and rotates; the admin keeps a recovery
    // envelope of every epoch.
    let mut add = step(&r.authority, &r.admin);
    let roster = &mut membership(&mut add).roster;
    roster.push(b.member(false));
    roster.sort_by_key(|m| m.id);
    let mut transitions = vec![signed(&add, r.admin.signing_key())];
    r.authority.apply_membership(&transitions[0]).unwrap();
    transitions.push(signed(&step(&r.authority, &r.admin), r.admin.signing_key()));
    r.authority.apply_membership(&transitions[1]).unwrap();
    let mut bundle = Vec::new();
    {
        let mut replay = Authority::bootstrap(&r.genesis, route(), r.keys.root_key()).unwrap();
        for epoch in 0..3 {
            if epoch > 0 {
                replay.apply_membership(&transitions[epoch - 1]).unwrap();
            }
            bundle.push(
                r.authority
                    .wrap_recovery_key(
                        &r.admin,
                        replay.manifest(),
                        &epoch_key(epoch as u64),
                        fresh(),
                    )
                    .unwrap(),
            );
        }
    }
    let mut on_b = r.authority.clone();
    // Every device is lost. From the words, the relay's control records and
    // the bundle, a new device recovers.
    let keys = RecoverySecret::from_words(&r.words)
        .unwrap()
        .keys(&route())
        .unwrap();
    let mut lost = Authority::bootstrap(&r.genesis, route(), keys.root_key()).unwrap();
    for t in &transitions {
        lost.apply_membership(t).unwrap();
    }
    for (epoch, envelope) in bundle.iter().enumerate() {
        let key = lost.unwrap_recovery_key(envelope, &keys).unwrap();
        assert_eq!(key.epoch(), epoch as u64);
        assert!(is_epoch_key(key.content_key(), epoch as u64));
        assert_eq!(lost.check_epoch_key(&key).is_ok(), epoch == 2);
    }
    let d = device();
    let first = ContentKey::new([0x3d; 32]);
    let recovery = lost.recover(&keys, &d, &first, fresh()).unwrap();
    assert!(lost.apply_recovery(&recovery).unwrap());
    assert!(!lost.apply_recovery(&recovery).unwrap(), "an exact replay");
    assert_eq!(lost.generation().unwrap(), 1);
    assert_eq!(lost.epoch().unwrap(), 3);
    assert_eq!(lost.members().unwrap(), vec![d.member(true)]);
    // The new admin distributes the new generation's key as usual.
    let envelope = lost.wrap_key(&d, d.id(), &first, fresh()).unwrap();
    let key = lost.unwrap_key(&envelope, &d).unwrap();
    lost.check_epoch_key(&key).unwrap();
    // A retained device learns of the recovery and is no longer a member;
    // the old admin can no longer change membership.
    assert!(on_b.apply_recovery(&recovery).unwrap());
    assert_eq!(on_b.manifest(), lost.manifest());
    assert_eq!(
        on_b.unwrap_key(&envelope, &b).map(|_| ()),
        Err(Error::Identity)
    );
    let late = signed(&step(&r.authority, &r.admin), r.admin.signing_key());
    assert_eq!(on_b.apply_membership(&late), Err(Error::Stale));
    assert_eq!(
        on_b.apply_membership(&transitions[1]),
        Ok(false),
        "an accepted transition replays"
    );
    // Its recovery envelopes still open there: recovery reads history.
    on_b.unwrap_recovery_key(&bundle[0], &keys).unwrap();
}

#[test]
fn wrong_words_and_missing_envelopes_fail_safely() {
    let mut r = recoverable();
    let first = r
        .authority
        .wrap_recovery_key(&r.admin, r.authority.manifest(), &epoch_key(0), fresh())
        .unwrap();
    let rotation = signed(&step(&r.authority, &r.admin), r.admin.signing_key());
    r.authority.apply_membership(&rotation).unwrap();
    // Another collection's words: another root, another recovery key.
    let wrong = RecoverySecret::new(fresh()).keys(&route()).unwrap();
    assert_eq!(
        Authority::bootstrap(&r.genesis, route(), wrong.root_key()).map(|_| ()),
        Err(Error::Identity)
    );
    assert_eq!(
        r.authority
            .recover(&wrong, &device(), &epoch_key(2), fresh())
            .map(|_| ()),
        Err(Error::Identity)
    );
    assert_eq!(
        r.authority.unwrap_recovery_key(&first, &wrong).map(|_| ()),
        Err(Error::Identity)
    );
    // The right words open exactly the epochs that have envelopes: a member
    // envelope is not one, so epoch 1's key is explicitly unrecoverable
    // until the admin adds its recovery envelope.
    r.authority.unwrap_recovery_key(&first, &r.keys).unwrap();
    let member = wrap(&r.authority, &r.admin, r.admin.id(), &epoch_key(1)).unwrap();
    assert_eq!(
        r.authority
            .unwrap_recovery_key(&member, &r.keys)
            .map(|_| ()),
        Err(Error::Identity)
    );
    // Only the epoch's admin signs recovery envelopes, for a known manifest
    // and with that epoch's key.
    let other = device();
    assert_eq!(
        r.authority
            .wrap_recovery_key(&other, r.authority.manifest(), &epoch_key(1), fresh())
            .map(|_| ()),
        Err(Error::Unauthorized)
    );
    assert_eq!(
        r.authority
            .wrap_recovery_key(&r.admin, [7; 48], &epoch_key(1), fresh())
            .map(|_| ()),
        Err(Error::MissingPrevious)
    );
    assert_eq!(
        r.authority
            .wrap_recovery_key(&r.admin, r.authority.manifest(), &epoch_key(0), fresh())
            .map(|_| ()),
        Err(Error::KeyCommitment)
    );
    let mut forged = parse(&first).unwrap().message;
    forged.signer = other.id();
    if let Body::KeyEnvelope(k) = &mut forged.body {
        k.authority = other.id();
    }
    assert_eq!(
        r.authority
            .unwrap_recovery_key(&signed(&forged, other.signing_key()), &r.keys)
            .map(|_| ()),
        Err(Error::Unauthorized)
    );
    let mut misdated = parse(&first).unwrap().message;
    if let Body::KeyEnvelope(k) = &mut misdated.body {
        k.epoch = 1;
    }
    assert_eq!(
        r.authority
            .unwrap_recovery_key(&signed(&misdated, r.admin.signing_key()), &r.keys)
            .map(|_| ()),
        Err(Error::Stale)
    );
}

#[test]
fn recoveries_are_root_signed_ordered_by_generation_and_frozen_when_they_compete() {
    let mut r = recoverable();
    let genesis_manifest = r.authority.manifest();
    let stale_view = r.authority.clone();
    let rotation = signed(&step(&r.authority, &r.admin), r.admin.signing_key());
    r.authority.apply_membership(&rotation).unwrap();
    let d = device();
    let honest = r
        .authority
        .recover(&r.keys, &d, &ContentKey::new([1; 32]), fresh())
        .unwrap();
    let base = parse(&honest).unwrap().message;
    let variant = |change: &dyn Fn(&mut Membership), signer: &SigningKey| {
        let mut m = base.clone();
        change(recovery_body(&mut m));
        signed(&m, signer)
    };
    let root = r.keys.signing_key();
    let other = device();
    for (name, bytes, error) in [
        (
            "signed by the admin",
            variant(&|_| (), r.admin.signing_key()),
            Error::Signature,
        ),
        (
            "two members",
            variant(
                &|m| {
                    m.roster.push(other.member(false));
                    m.roster.sort_by_key(|m| m.id);
                },
                root,
            ),
            Error::Unsupported,
        ),
        (
            "another recovery key",
            variant(&|m| m.recovery_key = other.kem_public_key().clone(), root),
            Error::Unsupported,
        ),
        (
            "a skipped generation",
            variant(&|m| m.generation = 2, root),
            Error::Stale,
        ),
        (
            "a skipped epoch",
            variant(&|m| m.epoch += 1, root),
            Error::Stale,
        ),
        (
            "a repeated sequence",
            variant(&|m| m.sequence -= 1, root),
            Error::Stale,
        ),
        (
            "an unknown previous",
            variant(&|m| m.previous = [7; 48], root),
            Error::MissingPrevious,
        ),
    ] {
        let mut a = r.authority.clone();
        assert_eq!(a.apply_recovery(&bytes), Err(error), "{name}");
        assert_eq!(a.manifest(), r.authority.manifest(), "{name}: unchanged");
    }
    let mut message = base.clone();
    message.signer = r.admin.id();
    assert_eq!(
        r.authority
            .clone()
            .apply_recovery(&signed(&message, r.admin.signing_key())),
        Err(Error::Unauthorized),
        "only the root recovers"
    );
    assert_eq!(
        r.authority.clone().apply_recovery(&rotation),
        Err(Error::Unsupported)
    );
    // A recoverer whose view stopped at genesis still recovers; the
    // generation it starts supersedes the later epoch it did not see.
    let from_genesis = stale_view
        .recover(&r.keys, &d, &ContentKey::new([2; 32]), fresh())
        .unwrap();
    assert_eq!(
        recovery_body(&mut parse(&from_genesis).unwrap().message).previous,
        genesis_manifest
    );
    let mut a = r.authority.clone();
    assert!(a.apply_recovery(&from_genesis).unwrap());
    assert_eq!(a.generation().unwrap(), 1);
    // A second, different recovery to generation 1 freezes, keeping both;
    // a recovery to generation 2 is the way out.
    assert_eq!(a.apply_recovery(&honest), Err(Error::Fork));
    assert!(a.is_forked());
    assert_eq!(a.epoch(), Err(Error::Fork));
    let mut frozen_view = r.authority.clone();
    frozen_view.apply_recovery(&from_genesis).unwrap();
    let next = frozen_view
        .recover(&r.keys, &d, &ContentKey::new([3; 32]), fresh())
        .unwrap();
    assert!(a.apply_recovery(&next).unwrap());
    assert!(!a.is_forked());
    assert_eq!(a.generation().unwrap(), 2);
}

#[test]
fn a_recovery_clears_a_membership_fork() {
    let mut r = recoverable();
    let one = signed(&step(&r.authority, &r.admin), r.admin.signing_key());
    let two = signed(&step(&r.authority, &r.admin), r.admin.signing_key());
    r.authority.apply_membership(&one).unwrap();
    assert_eq!(r.authority.apply_membership(&two), Err(Error::Fork));
    let recovery = r
        .authority
        .recover(&r.keys, &device(), &ContentKey::new([4; 32]), fresh())
        .unwrap();
    assert!(r.authority.apply_recovery(&recovery).unwrap());
    assert!(!r.authority.is_forked());
    assert_eq!(r.authority.generation().unwrap(), 1);
}

#[test]
fn recovery_secrets_and_keys_never_print() {
    let secret = RecoverySecret::new([0xab; 32]);
    assert_eq!(format!("{secret:?}"), "RecoverySecret(redacted)");
    let keys = secret.keys(&route()).unwrap();
    assert_eq!(format!("{keys:?}"), "RecoveryKeys(redacted)");
}

#[test]
fn records_authenticate_at_their_own_epoch_after_rotation_and_removal() {
    let mut s = setup();
    let inc = fresh();
    let start = writer(&s.authority, &s.b, inc, 81, None);
    s.authority
        .admit_writer(&signed(&start, s.b.signing_key()), DOC)
        .unwrap();
    let envelope0 = wrap(&s.authority, &s.a, s.b.id(), &epoch_key(0)).unwrap();
    let key0 = s.authority.unwrap_key(&envelope0, &s.b).unwrap();
    let (author, manifest0, record) = (s.b.id(), s.authority.manifest(), fresh());
    let sealed = seal_as(
        header(&s.authority, &author, &inc, &record, &manifest0),
        &epoch_key(0),
        PLAINTEXT,
        &s.b,
    );
    // B is removed in the next epoch; its epoch-0 record still reads at
    // epoch 0, with epoch 0's key.
    let mut revoke = transition(&s);
    let a = s.a.id();
    membership(&mut revoke).roster.retain(|m| m.id == a);
    s.authority
        .apply_membership(&signed(&revoke, s.a.signing_key()))
        .unwrap();
    let history = s
        .authority
        .wrap_history_key(&s.a, s.a.id(), manifest0, &epoch_key(0), fresh())
        .unwrap();
    let old_key = s.authority.unwrap_history_key(&history, &s.a).unwrap();
    let read = s
        .authority
        .authenticate_at(&sealed, &old_key, &DOC)
        .unwrap();
    assert_eq!(read.actor(), 81);
    assert_eq!(read.open().unwrap(), PLAINTEXT);
    // The current-epoch path refuses it as stale; authenticate_at refuses
    // a key of another epoch, another document and an outsider's record.
    assert_eq!(
        s.authority
            .authenticate_history(&sealed, &old_key, &DOC)
            .map(|_| ()),
        Err(Error::Stale)
    );
    let current = wrap(&s.authority, &s.a, s.a.id(), &epoch_key(1)).unwrap();
    let key1 = s.authority.unwrap_key(&current, &s.a).unwrap();
    assert_eq!(
        s.authority
            .authenticate_at(&sealed, &key1, &DOC)
            .map(|_| ()),
        Err(Error::Stale)
    );
    assert_eq!(
        s.authority
            .authenticate_at(&sealed, &old_key, &[5; 16])
            .map(|_| ()),
        Err(Error::Format)
    );
    let outsider = device();
    let (outsider_id, theirs_id) = (outsider.id(), fresh());
    let mut theirs_header = header(&s.authority, &outsider_id, &inc, &theirs_id, &manifest0);
    theirs_header.epoch = 0;
    let theirs = seal_as(theirs_header, &epoch_key(0), PLAINTEXT, &outsider);
    let _ = key0;
    assert_eq!(
        s.authority
            .authenticate_at(&theirs, &old_key, &DOC)
            .map(|_| ()),
        Err(Error::Unauthorized)
    );
}
