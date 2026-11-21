//! The production key-distribution authority against the independent golden
//! vectors (Python cbor2/hashlib, docs/SYNC_KEY_DISTRIBUTION.md) and the
//! prototype's rejection, fork, rotation, envelope and writer cases.

use std::sync::atomic::{AtomicU64, Ordering};

use matters_sync::authority::{
    device_id, encode, envelope_aad, envelope_info, key_commitment, parse, root_id, sign,
    signing_bytes, Authority, Body, Boundary, DeviceKeys, EpochKey, Error, Genesis, Hash, Id,
    KemPublicKey, Member, Membership, Message, Result, Route, WriterHandoff, WriterStart,
    KEM_PUBLIC_KEY_BYTES, MAX_CONTROL_BYTES, POLICY,
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
fn unsupported_enrollment_transfer_and_recovery_are_not_implicitly_authorized() {
    let mut s = setup();
    let candidate = device();
    for change in 0..3 {
        let mut m = transition(&s);
        let body = membership(&mut m);
        match change {
            0 => {
                body.roster.push(candidate.member(false));
                body.roster.sort_by_key(|m| m.id);
            }
            1 => {
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
}
