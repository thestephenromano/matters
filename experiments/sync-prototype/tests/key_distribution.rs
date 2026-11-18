#![cfg(all(feature = "crypto-probe", feature = "record-contract"))]
#[path = "support/record_crypto.rs"]
mod record_crypto;
#[path = "support/key_distribution.rs"]
mod support;
use loro::{ExportMode, LoroDoc, VersionVector};
use matters_sync_prototype::wire;
use support::*;

const DOC: [u8; 16] = [4; 16];
fn route() -> Route {
    Route {
        app: [1; 16],
        tenant: [2; 16],
        collection: [3; 16],
    }
}
struct Setup {
    root: Device,
    a: Device,
    b: Device,
    genesis: Vec<u8>,
    authority: Authority,
}
fn setup() -> Setup {
    let root = Device::generate().unwrap();
    let a = Device::generate().unwrap();
    let b = Device::generate().unwrap();
    let mut roster = vec![a.public(true).unwrap(), b.public(false).unwrap()];
    roster.sort_by_key(|m| m.id);
    let genesis = sign(
        &Message {
            route: route(),
            signer: root.root_id().unwrap(),
            body: Body::Genesis {
                root: root.signing.verification_key.as_slice().to_vec(),
                recovery: root.kem.public_key().as_slice().to_vec(),
                generation: 0,
                roster,
                admin: a.id().unwrap(),
                epoch: 0,
                checkpoint: None,
            },
        },
        &root,
    )
    .unwrap();
    let authority =
        Authority::bootstrap(&genesis, route(), root.signing.verification_key.as_slice()).unwrap();
    Setup {
        root,
        a,
        b,
        genesis,
        authority,
    }
}
fn transition(s: &Setup) -> Message {
    Message {
        route: route(),
        signer: s.a.id().unwrap(),
        body: Body::Membership {
            generation: 0,
            sequence: s.authority.sequence().unwrap() + 1,
            previous: s.authority.manifest(),
            roster: s.authority.members().unwrap(),
            admin: s.a.id().unwrap(),
            epoch: s.authority.epoch().unwrap() + 1,
            recovery: s.authority.recovery().unwrap(),
        },
    }
}
fn writer(
    state: &Authority,
    device: &Device,
    incarnation: Id,
    actor: u64,
    previous: Option<Id>,
) -> Message {
    Message {
        route: route(),
        signer: device.id().unwrap(),
        body: Body::Writer {
            manifest: state.manifest(),
            document: DOC,
            device: device.id().unwrap(),
            incarnation,
            actor,
            previous,
        },
    }
}
fn update(actor: u64) -> Vec<u8> {
    let doc = LoroDoc::new();
    doc.set_peer_id(actor).unwrap();
    doc.get_text("notes")
        .insert(0, "task: encrypted two-device experiment")
        .unwrap();
    doc.export(ExportMode::updates(&VersionVector::default()))
        .unwrap()
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

#[test]
fn two_devices_distribute_a_key_and_import_an_authenticated_first_update() {
    let mut s = setup();
    let mut receiver = Authority::bootstrap(
        &s.genesis,
        route(),
        s.root.signing.verification_key.as_slice(),
    )
    .unwrap();
    let cek = random32().unwrap();
    let envelope = s.authority.wrap_key(&s.a, s.b.id().unwrap(), &cek).unwrap();
    let received_key = receiver.unwrap_key(&envelope, &s.b).unwrap();
    let incarnation = random32().unwrap();
    let start = sign(&writer(&s.authority, &s.a, incarnation, 71, None), &s.a).unwrap();
    assert!(s.authority.admit_writer(&start, DOC).unwrap());
    assert!(receiver.admit_writer(&start, DOC).unwrap());
    let author = s.a.id().unwrap();
    let manifest = s.authority.manifest();
    let record = random32().unwrap();
    let plain = update(71);
    let sealed = seal_fixture(
        header(&s.authority, &author, &incarnation, &record, &manifest),
        &cek,
        &plain,
        &s.a,
    )
    .unwrap();
    let opened = receiver
        .open_first_data(&sealed, &received_key, &DOC)
        .unwrap();
    assert_eq!(opened, plain);
    let replica = LoroDoc::new();
    replica.import(&opened).unwrap();
    assert_eq!(
        replica.get_text("notes").to_string(),
        "task: encrypted two-device experiment"
    );
    let marker = b"task: encrypted two-device experiment";
    assert!(!sealed.windows(marker.len()).any(|w| w == marker));
    assert!(!envelope.windows(cek.len()).any(|w| w == cek));
    // A fresh envelope has fresh HPKE randomness; transport retry retains old bytes.
    let next = s.authority.wrap_key(&s.a, s.b.id().unwrap(), &cek).unwrap();
    assert_ne!(next, envelope);
    receiver.unwrap_key(&envelope, &s.b).unwrap();
}

#[test]
fn bootstrap_requires_pinned_root_route_and_exact_device_key_binding() {
    let s = setup();
    let stranger = Device::generate().unwrap();
    assert!(matches!(
        Authority::bootstrap(
            &s.genesis,
            route(),
            stranger.signing.verification_key.as_slice()
        ),
        Err(Error::Identity)
    ));
    let mut wrong = route();
    wrong.app[0] ^= 1;
    assert!(matches!(
        Authority::bootstrap(
            &s.genesis,
            wrong,
            s.root.signing.verification_key.as_slice()
        ),
        Err(Error::Context)
    ));
    let original = parse(&s.genesis).unwrap().message;
    for change in 0..4 {
        let mut m = original.clone();
        if let Body::Genesis { roster, .. } = &mut m.body {
            match change {
                0 => roster[0].kem[0] ^= 1,
                1 => roster[0].sig[0] ^= 1,
                2 => roster.reverse(),
                _ => roster[0] = roster[1].clone(),
            }
        }
        let bytes = sign(&m, &s.root).unwrap();
        assert!(matches!(
            Authority::bootstrap(&bytes, route(), s.root.signing.verification_key.as_slice()),
            Err(Error::Identity)
        ));
    }
    let mut bad = s.genesis.clone();
    let last = bad.len() - 1;
    bad[last] ^= 1;
    assert!(matches!(
        Authority::bootstrap(&bad, route(), s.root.signing.verification_key.as_slice()),
        Err(Error::Signature)
    ));
}

#[test]
fn membership_uses_previous_admin_and_preserves_state_on_rejection() {
    let mut s = setup();
    let original = s.authority.manifest();
    let good = transition(&s);
    let mut writer_signed = good.clone();
    writer_signed.signer = s.b.id().unwrap();
    assert_eq!(
        s.authority
            .apply_membership(&sign(&writer_signed, &s.b).unwrap()),
        Err(Error::Unauthorized)
    );
    assert_eq!(
        s.authority.apply_membership(&sign(&good, &s.b).unwrap()),
        Err(Error::Signature)
    );
    let mut absent = good.clone();
    if let Body::Membership { previous, .. } = &mut absent.body {
        *previous = [99; 48]
    }
    assert_eq!(
        s.authority.apply_membership(&sign(&absent, &s.a).unwrap()),
        Err(Error::MissingPrevious)
    );
    let mut stale = good.clone();
    if let Body::Membership { epoch, .. } = &mut stale.body {
        *epoch = 0
    }
    assert_eq!(
        s.authority.apply_membership(&sign(&stale, &s.a).unwrap()),
        Err(Error::Stale)
    );
    assert_eq!(s.authority.manifest(), original);
    let bytes = sign(&good, &s.a).unwrap();
    assert_eq!(s.authority.apply_membership(&bytes), Ok(true));
    assert_eq!(s.authority.apply_membership(&bytes), Ok(false));
    assert_eq!(s.authority.epoch(), Ok(1));
    assert_eq!(
        s.authority.apply_membership(&sign(&good, &s.b).unwrap()),
        Err(Error::Signature)
    );
    assert!(!s.authority.is_forked());
}

#[test]
fn two_valid_membership_successors_freeze_key_and_record_admission() {
    let mut s = setup();
    let first = transition(&s);
    let mut second = first.clone();
    if let Body::Membership { roster, .. } = &mut second.body {
        roster.retain(|m| m.id == s.a.id().unwrap());
    }
    s.authority
        .apply_membership(&sign(&first, &s.a).unwrap())
        .unwrap();
    let cek = random32().unwrap();
    let envelope = s.authority.wrap_key(&s.a, s.b.id().unwrap(), &cek).unwrap();
    let key = s.authority.unwrap_key(&envelope, &s.b).unwrap();
    let inc = random32().unwrap();
    let start = sign(&writer(&s.authority, &s.a, inc, 71, None), &s.a).unwrap();
    s.authority.admit_writer(&start, DOC).unwrap();
    let author = s.a.id().unwrap();
    let manifest = s.authority.manifest();
    let id = random32().unwrap();
    let record = seal_fixture(
        header(&s.authority, &author, &inc, &id, &manifest),
        &cek,
        &update(71),
        &s.a,
    )
    .unwrap();
    s.authority.open_first_data(&record, &key, &DOC).unwrap();
    assert_eq!(
        s.authority.apply_membership(&sign(&second, &s.a).unwrap()),
        Err(Error::Fork)
    );
    assert!(s.authority.is_forked());
    assert_eq!(s.authority.admit_writer(&start, DOC), Err(Error::Fork));
    assert_eq!(
        s.authority.open_first_data(&record, &key, &DOC),
        Err(Error::Fork)
    );
    assert_eq!(
        s.authority.wrap_key(&s.a, s.a.id().unwrap(), &[1; 32]),
        Err(Error::Fork)
    );
    assert!(matches!(
        s.authority.unwrap_key(&envelope, &s.b),
        Err(Error::Fork)
    ));
}

#[test]
fn unsupported_enrollment_transfer_and_recovery_are_not_implicitly_authorized() {
    let mut s = setup();
    let candidate = Device::generate().unwrap();
    for change in 0..3 {
        let mut m = transition(&s);
        if let Body::Membership {
            roster,
            admin,
            recovery,
            ..
        } = &mut m.body
        {
            match change {
                0 => {
                    roster.push(candidate.public(false).unwrap());
                    roster.sort_by_key(|m| m.id);
                }
                1 => {
                    *admin = s.b.id().unwrap();
                    for member in roster {
                        member.admin = member.id == *admin;
                    }
                }
                _ => *recovery = candidate.kem.public_key().as_slice().to_vec(),
            }
        }
        assert_eq!(
            s.authority.apply_membership(&sign(&m, &s.a).unwrap()),
            Err(Error::Unsupported)
        );
    }
}

#[test]
fn envelope_rejects_unauthorized_signers_wrong_recipients_and_substituted_keys() {
    let s = setup();
    let envelope = s
        .authority
        .wrap_key(&s.a, s.b.id().unwrap(), &random32().unwrap())
        .unwrap();
    let original = parse(&envelope).unwrap().message;
    assert!(matches!(
        s.authority.unwrap_key(&envelope, &s.a),
        Err(Error::Identity)
    ));
    assert_eq!(
        s.authority.wrap_key(&s.b, s.a.id().unwrap(), &[1; 32]),
        Err(Error::Unauthorized)
    );
    assert!(matches!(
        s.authority
            .unwrap_key(&sign(&original, &s.b).unwrap(), &s.b),
        Err(Error::Signature)
    ));
    for change in 0..6 {
        let mut m = original.clone();
        let expected = match change {
            0 => {
                m.signer = s.b.id().unwrap();
                Error::Unauthorized
            }
            1 => {
                m.route.tenant[0] ^= 1;
                Error::Context
            }
            2 => {
                if let Body::Envelope { key_fp, .. } = &mut m.body {
                    key_fp[0] ^= 1
                };
                Error::Identity
            }
            3 => {
                if let Body::Envelope { ciphertext, .. } = &mut m.body {
                    ciphertext[0] ^= 1
                };
                Error::Crypto
            }
            4 => {
                if let Body::Envelope { enc, .. } = &mut m.body {
                    enc[0] ^= 1
                };
                Error::Crypto
            }
            _ => {
                if let Body::Envelope { authority, .. } = &mut m.body {
                    *authority = s.b.id().unwrap()
                };
                Error::Unauthorized
            }
        };
        let result = s.authority.unwrap_key(&sign(&m, &s.a).unwrap(), &s.b);
        assert!(matches!(result,Err(e) if e==expected), "case {change}");
    }
}

#[test]
fn rotation_rejects_old_envelopes_and_revocation_removes_distribution_authority() {
    let mut s = setup();
    let old_cek = random32().unwrap();
    let old = s
        .authority
        .wrap_key(&s.a, s.b.id().unwrap(), &old_cek)
        .unwrap();
    let old_key = s.authority.unwrap_key(&old, &s.b).unwrap();
    let inc = random32().unwrap();
    let start = writer(&s.authority, &s.b, inc, 71, None);
    s.authority
        .admit_writer(&sign(&start, &s.b).unwrap(), DOC)
        .unwrap();
    let m = sign(&transition(&s), &s.a).unwrap();
    s.authority.apply_membership(&m).unwrap();
    assert!(matches!(
        s.authority.unwrap_key(&old, &s.b),
        Err(Error::Stale)
    ));
    let new_cek = random32().unwrap();
    let new = s
        .authority
        .wrap_key(&s.a, s.b.id().unwrap(), &new_cek)
        .unwrap();
    let new_key = s.authority.unwrap_key(&new, &s.b).unwrap();
    let author = s.b.id().unwrap();
    let manifest = s.authority.manifest();
    let record = random32().unwrap();
    let sealed = seal_fixture(
        header(&s.authority, &author, &inc, &record, &manifest),
        &new_cek,
        &update(71),
        &s.b,
    )
    .unwrap();
    assert_eq!(
        s.authority.open_first_data(&sealed, &old_key, &DOC),
        Err(Error::Stale)
    );
    // Retained bindings remain usable by a member under the current epoch.
    s.authority
        .open_first_data(&sealed, &new_key, &DOC)
        .unwrap();
    let mut revoke = transition(&s);
    if let Body::Membership { roster, .. } = &mut revoke.body {
        roster.retain(|m| m.id == s.a.id().unwrap());
    }
    s.authority
        .apply_membership(&sign(&revoke, &s.a).unwrap())
        .unwrap();
    assert_eq!(
        s.authority.wrap_key(&s.a, s.b.id().unwrap(), &[1; 32]),
        Err(Error::Unauthorized)
    );
    let cek = random32().unwrap();
    let envelope = s.authority.wrap_key(&s.a, s.a.id().unwrap(), &cek).unwrap();
    let key = s.authority.unwrap_key(&envelope, &s.a).unwrap();
    // Even a removed writer given the new key cannot acquire current authority.
    let manifest = s.authority.manifest();
    let record = random32().unwrap();
    let forged = seal_fixture(
        header(&s.authority, &author, &inc, &record, &manifest),
        &cek,
        &update(71),
        &s.b,
    )
    .unwrap();
    assert_eq!(
        s.authority.open_first_data(&forged, &key, &DOC),
        Err(Error::Unauthorized)
    );
}

#[test]
fn writer_admission_binds_document_actor_and_incarnation() {
    let mut s = setup();
    let start = writer(&s.authority, &s.a, [1; 32], 71, None);
    let bytes = sign(&start, &s.a).unwrap();
    assert_eq!(
        s.authority.admit_writer(&bytes, [99; 16]),
        Err(Error::Context)
    );
    assert_eq!(
        s.authority.admit_writer(&sign(&start, &s.b).unwrap(), DOC),
        Err(Error::Signature)
    );
    assert_eq!(s.authority.admit_writer(&bytes, DOC), Ok(true));
    assert_eq!(s.authority.admit_writer(&bytes, DOC), Ok(false));
    for (device, inc, actor, prev) in [
        (&s.b, [2; 32], 71, None),
        (&s.a, [1; 32], 72, None),
        (&s.b, [2; 32], 72, Some([1; 32])),
    ] {
        let m = writer(&s.authority, device, inc, actor, prev);
        assert_eq!(
            s.authority.admit_writer(&sign(&m, device).unwrap(), DOC),
            Err(Error::Actor)
        );
    }
    let restored = writer(&s.authority, &s.a, [2; 32], 72, Some([1; 32]));
    s.authority
        .admit_writer(&sign(&restored, &s.a).unwrap(), DOC)
        .unwrap();
    let rotation = transition(&s);
    s.authority
        .apply_membership(&sign(&rotation, &s.a).unwrap())
        .unwrap();
    assert_eq!(s.authority.admit_writer(&bytes, DOC), Err(Error::Stale));
}

#[test]
fn signed_content_still_requires_writer_epoch_actor_and_supported_history() {
    let mut s = setup();
    let cek = random32().unwrap();
    let envelope = s.authority.wrap_key(&s.a, s.b.id().unwrap(), &cek).unwrap();
    let key = s.authority.unwrap_key(&envelope, &s.b).unwrap();
    let author = s.a.id().unwrap();
    let manifest = s.authority.manifest();
    let incarnation = random32().unwrap();
    let record = random32().unwrap();
    let h = header(&s.authority, &author, &incarnation, &record, &manifest);
    let sealed = seal_fixture(h, &cek, &update(71), &s.a).unwrap();
    assert_eq!(
        s.authority.open_first_data(&sealed, &key, &DOC),
        Err(Error::Actor)
    );
    let start = writer(&s.authority, &s.a, incarnation, 71, None);
    s.authority
        .admit_writer(&sign(&start, &s.a).unwrap(), DOC)
        .unwrap();
    for change in 0..6 {
        let record = random32().unwrap();
        let mut h = header(&s.authority, &author, &incarnation, &record, &manifest);
        let mut plain = update(71);
        let expected = match change {
            0 => {
                h.epoch = 1;
                Error::Stale
            }
            1 => {
                h.previous = Some(&[99; 48]);
                Error::Unsupported
            }
            2 => {
                h.kind = wire::Kind::Snapshot;
                Error::Unsupported
            }
            3 => {
                plain = update(72);
                Error::Actor
            }
            4 => {
                plain = vec![0; 3];
                Error::Format
            }
            _ => Error::Signature,
        };
        let bytes = seal_fixture(h, &cek, &plain, if change == 5 { &s.b } else { &s.a }).unwrap();
        assert_eq!(
            s.authority.open_first_data(&bytes, &key, &DOC),
            Err(expected)
        );
    }
}

fn fixtures() -> serde_json::Value {
    serde_json::from_str(include_str!("vectors/key-distribution/golden.json")).unwrap()
}
fn unhex(value: &str) -> Vec<u8> {
    (0..value.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&value[i..i + 2], 16).unwrap())
        .collect()
}
fn bytes(v: &serde_json::Value, key: &str) -> Vec<u8> {
    unhex(v[key].as_str().unwrap())
}

#[test]
fn independent_control_bytes_fingerprints_and_hpke_contexts_match() {
    let f = fixtures();
    let schema = include_str!("../../../docs/protocol/sync-key-distribution-v0.cddl");
    for v in f["vectors"].as_array().unwrap() {
        let wire = bytes(v, "wire_hex");
        let p = parse(&wire).unwrap();
        assert_eq!(p.signed, bytes(v, "signed_hex"));
        assert!(std::ptr::eq(p.signed, &wire[1..1 + p.signed.len()]));
        assert_eq!(signing_bytes(&p.message).unwrap(), p.signed);
        assert_eq!(package(&p.message, p.signature).unwrap(), wire);
        assert_eq!(hash(&wire).as_slice(), bytes(v, "hash_hex"));
        cddl_cat::validate_cbor_bytes("control", schema, &wire).unwrap();
        if let Body::Genesis {
            root,
            roster,
            admin,
            ..
        } = &p.message.body
        {
            assert_eq!(
                fingerprint("ML-DSA-65", root).unwrap().as_slice(),
                bytes(&f, "root_id_hex")
            );
            assert_eq!(admin.as_slice(), bytes(&f, "admin_id_hex"));
            for m in roster {
                assert_eq!(device_id(&m.sig, &m.kem).unwrap(), m.id);
            }
        }
        if let Body::Envelope {
            recipient, key_fp, ..
        } = &p.message.body
        {
            assert_eq!(recipient.as_slice(), bytes(&f, "recipient_id_hex"));
            assert_eq!(
                key_fp.as_slice(),
                bytes(&f, "recipient_key_fingerprint_hex")
            );
            assert_eq!(envelope_info(&p.message).unwrap(), bytes(&f, "info_hex"));
            assert_eq!(envelope_aad(&p.message).unwrap(), bytes(&f, "aad_hex"));
        }
    }
    cddl_cat::validate_cbor_bytes("key-plaintext", schema, &bytes(&f, "key_plaintext_hex"))
        .unwrap();
}

#[test]
fn malformed_control_fixtures_and_every_truncation_reject() {
    let f = fixtures();
    let base = bytes(&f["vectors"][2], "wire_hex");
    for v in f["rejection_splices"].as_array().unwrap() {
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
    assert!(matches!(
        parse(&vec![0; MAX_CONTROL + 1]),
        Err(Error::Limit)
    ));
}

#[test]
fn roster_budget_is_independently_measured_and_rejects_one_more_member() {
    let f = fixtures();
    let wire = bytes(&f["vectors"][0], "wire_hex");
    let mut message = parse(&wire).unwrap().message;
    let members = (0..16)
        .map(|i| {
            let sig = vec![30 + i; SIG_PK];
            let kem = vec![60 + i; KEM_PK];
            Member {
                id: device_id(&sig, &kem).unwrap(),
                sig,
                kem,
                admin: i == 0,
            }
        })
        .collect::<Vec<_>>();
    if let Body::Genesis { roster, .. } = &mut message.body {
        *roster = members;
        roster.sort_by_key(|m| m.id);
    }
    let encoded = package(&message, &[0; wire::SIGNATURE_BYTES]).unwrap();
    assert_eq!(
        encoded.len(),
        f["max_genesis_bytes"].as_u64().unwrap() as usize
    );
    assert_eq!(hash(&encoded).as_slice(), bytes(&f, "max_genesis_sha384"));
    assert!(encoded.len() < MAX_CONTROL);
    parse(&encoded).unwrap();
    if let Body::Genesis { roster, .. } = &mut message.body {
        roster.push(roster[0].clone());
    }
    assert!(matches!(
        parse(&package(&message, &[0; wire::SIGNATURE_BYTES]).unwrap()),
        Err(Error::Limit)
    ));
}

#[test]
fn first_update_gate_rejects_snapshot_and_causal_tail_payloads() {
    let mut s = setup();
    let cek = random32().unwrap();
    let key = s
        .authority
        .unwrap_key(
            &s.authority.wrap_key(&s.a, s.b.id().unwrap(), &cek).unwrap(),
            &s.b,
        )
        .unwrap();
    let inc = random32().unwrap();
    let start = sign(&writer(&s.authority, &s.a, inc, 71, None), &s.a).unwrap();
    s.authority.admit_writer(&start, DOC).unwrap();
    let doc = LoroDoc::new();
    doc.set_peer_id(71).unwrap();
    doc.get_text("notes").insert(0, "first").unwrap();
    let before = doc.oplog_vv();
    let snapshot = doc.export(ExportMode::Snapshot).unwrap();
    doc.get_text("notes").insert(5, " tail").unwrap();
    let tail = doc.export(ExportMode::updates(&before)).unwrap();
    let author = s.a.id().unwrap();
    let manifest = s.authority.manifest();
    for (payload, expected) in [(snapshot, Error::Unsupported), (tail, Error::Actor)] {
        let record = random32().unwrap();
        let sealed = seal_fixture(
            header(&s.authority, &author, &inc, &record, &manifest),
            &cek,
            &payload,
            &s.a,
        )
        .unwrap();
        assert_eq!(
            s.authority.open_first_data(&sealed, &key, &DOC),
            Err(expected)
        );
    }
}

#[test]
fn malformed_but_admin_signed_key_plaintext_is_rejected_after_hpke() {
    use hpke_rs::{Hpke, Mode};
    use hpke_rs_crypto::types::{AeadAlgorithm, KdfAlgorithm, KemAlgorithm};
    use hpke_rs_libcrux::HpkeLibcrux;
    let s = setup();
    let envelope = s
        .authority
        .wrap_key(&s.a, s.b.id().unwrap(), &random32().unwrap())
        .unwrap();
    let base = parse(&envelope).unwrap().message;
    for change in 0..3 {
        let mut e = minicbor::Encoder::new(Vec::new());
        e.array(3)
            .unwrap()
            .u64(if change == 0 { 1 } else { 0 })
            .unwrap()
            .bytes(&vec![1; if change == 1 { 31 } else { 32 }])
            .unwrap()
            .str(if change == 2 { "other" } else { POLICY })
            .unwrap();
        let mut hpke: Hpke<HpkeLibcrux> = Hpke::try_new(
            Mode::Base,
            KemAlgorithm::MlKem768P256,
            KdfAlgorithm::HkdfSha256,
            AeadAlgorithm::Aes256Gcm,
        )
        .unwrap();
        let (enc, ciphertext) = hpke
            .seal(
                s.b.kem.public_key(),
                &envelope_info(&base).unwrap(),
                &envelope_aad(&base).unwrap(),
                &e.into_writer(),
                None,
                None,
                None,
            )
            .unwrap();
        let mut m = base.clone();
        if let Body::Envelope {
            enc: out,
            ciphertext: ct,
            ..
        } = &mut m.body
        {
            *out = enc;
            *ct = ciphertext;
        }
        assert!(matches!(
            s.authority.unwrap_key(&sign(&m, &s.a).unwrap(), &s.b),
            Err(Error::Format)
        ));
    }
}

#[test]
fn unsupported_roster_key_material_and_experiment_state_limits_fail_closed() {
    let mut s = setup();
    let mut genesis = parse(&s.genesis).unwrap().message;
    if let Body::Genesis { roster, .. } = &mut genesis.body {
        let target = roster.iter_mut().find(|m| !m.admin).unwrap();
        target.kem = vec![0; KEM_PK];
        target.id = device_id(&target.sig, &target.kem).unwrap();
        roster.sort_by_key(|m| m.id);
    }
    let bytes = sign(&genesis, &s.root).unwrap();
    let state =
        Authority::bootstrap(&bytes, route(), s.root.signing.verification_key.as_slice()).unwrap();
    let bad = state
        .members()
        .unwrap()
        .into_iter()
        .find(|m| !m.admin)
        .unwrap();
    assert_eq!(state.wrap_key(&s.a, bad.id, &[1; 32]), Err(Error::Crypto));
    for _ in 0..16 {
        let m = transition(&s);
        s.authority
            .apply_membership(&sign(&m, &s.a).unwrap())
            .unwrap();
    }
    let next = transition(&s);
    assert_eq!(
        s.authority.apply_membership(&sign(&next, &s.a).unwrap()),
        Err(Error::Limit)
    );
    for i in 0..16 {
        let previous = if i == 0 { None } else { Some([i - 1; 32]) };
        let m = writer(&s.authority, &s.a, [i; 32], i as u64, previous);
        s.authority
            .admit_writer(&sign(&m, &s.a).unwrap(), DOC)
            .unwrap();
    }
    let extra = writer(&s.authority, &s.a, [17; 32], 17, Some([15; 32]));
    assert_eq!(
        s.authority.admit_writer(&sign(&extra, &s.a).unwrap(), DOC),
        Err(Error::Limit)
    );
}

#[test]
fn replacement_retires_old_writer_and_replay_does_not_reactivate_it() {
    let mut s = setup();
    let cek = random32().unwrap();
    let envelope = s.authority.wrap_key(&s.a, s.b.id().unwrap(), &cek).unwrap();
    let key = s.authority.unwrap_key(&envelope, &s.b).unwrap();
    let author = s.a.id().unwrap();
    let manifest = s.authority.manifest();
    let old = [21; 32];
    let next = [22; 32];
    let start = sign(&writer(&s.authority, &s.a, old, 71, None), &s.a).unwrap();
    s.authority.admit_writer(&start, DOC).unwrap();
    let id = random32().unwrap();
    let old_record = seal_fixture(
        header(&s.authority, &author, &old, &id, &manifest),
        &cek,
        &update(71),
        &s.a,
    )
    .unwrap();
    s.authority
        .open_first_data(&old_record, &key, &DOC)
        .unwrap();
    let replacement = sign(&writer(&s.authority, &s.a, next, 72, Some(old)), &s.a).unwrap();
    s.authority.admit_writer(&replacement, DOC).unwrap();
    assert_eq!(
        s.authority.open_first_data(&old_record, &key, &DOC),
        Err(Error::Stale)
    );
    assert_eq!(s.authority.admit_writer(&start, DOC), Ok(false));
    // Observe the old retry before any newer retry could restore the right head.
    assert_eq!(
        s.authority.open_first_data(&old_record, &key, &DOC),
        Err(Error::Stale)
    );
    assert_eq!(s.authority.admit_writer(&replacement, DOC), Ok(false));
    assert_eq!(
        s.authority.open_first_data(&old_record, &key, &DOC),
        Err(Error::Stale)
    );
    let id = random32().unwrap();
    let record = seal_fixture(
        header(&s.authority, &author, &next, &id, &manifest),
        &cek,
        &update(72),
        &s.a,
    )
    .unwrap();
    s.authority.open_first_data(&record, &key, &DOC).unwrap();
}

#[test]
fn competing_writer_successors_freeze_the_lineage_in_both_orders() {
    for root_conflict in [false, true] {
        for reversed in [false, true] {
            let mut s = setup();
            let cek = random32().unwrap();
            let envelope = s.authority.wrap_key(&s.a, s.b.id().unwrap(), &cek).unwrap();
            let key = s.authority.unwrap_key(&envelope, &s.b).unwrap();
            let base = [20; 32];
            if !root_conflict {
                let m = sign(&writer(&s.authority, &s.a, base, 70, None), &s.a).unwrap();
                s.authority.admit_writer(&m, DOC).unwrap();
            }
            let prev = if root_conflict { None } else { Some(base) };
            let candidates = [([21; 32], 71), ([22; 32], 72)];
            let (first_inc, first_actor) = candidates[usize::from(reversed)];
            let (second_inc, second_actor) = candidates[usize::from(!reversed)];
            let first = sign(
                &writer(&s.authority, &s.a, first_inc, first_actor, prev),
                &s.a,
            )
            .unwrap();
            s.authority.admit_writer(&first, DOC).unwrap();
            let second_message = writer(&s.authority, &s.a, second_inc, second_actor, prev);
            assert_eq!(
                s.authority
                    .admit_writer(&sign(&second_message, &s.b).unwrap(), DOC),
                Err(Error::Signature)
            );
            let author = s.a.id().unwrap();
            let manifest = s.authority.manifest();
            let id = random32().unwrap();
            let record = seal_fixture(
                header(&s.authority, &author, &first_inc, &id, &manifest),
                &cek,
                &update(first_actor),
                &s.a,
            )
            .unwrap();
            s.authority.open_first_data(&record, &key, &DOC).unwrap();
            if !root_conflict {
                // A delayed competitor of an older parent still freezes the chain.
                let advance = sign(
                    &writer(&s.authority, &s.a, [26; 32], 76, Some(first_inc)),
                    &s.a,
                )
                .unwrap();
                s.authority.admit_writer(&advance, DOC).unwrap();
            }
            let second = sign(&second_message, &s.a).unwrap();
            assert_eq!(s.authority.admit_writer(&second, DOC), Err(Error::Fork));
            assert_eq!(
                s.authority.writer_fork(DOC, author),
                Some((first.as_slice(), second.as_slice()))
            );
            assert_eq!(
                s.authority.open_first_data(&record, &key, &DOC),
                Err(Error::Fork)
            );
            assert_eq!(s.authority.admit_writer(&first, DOC), Err(Error::Fork));
            let later = sign(
                &writer(&s.authority, &s.a, [23; 32], 73, Some(first_inc)),
                &s.a,
            )
            .unwrap();
            assert_eq!(s.authority.admit_writer(&later, DOC), Err(Error::Fork));
            // The freeze is scoped to this device/document, not all members.
            let other = sign(&writer(&s.authority, &s.b, [24; 32], 74, None), &s.b).unwrap();
            s.authority.admit_writer(&other, DOC).unwrap();
            let mut other_doc = writer(&s.authority, &s.a, [25; 32], 75, None);
            if let Body::Writer { document, .. } = &mut other_doc.body {
                *document = [5; 16];
            }
            s.authority
                .admit_writer(&sign(&other_doc, &s.a).unwrap(), [5; 16])
                .unwrap();
            s.authority.wrap_key(&s.a, s.b.id().unwrap(), &cek).unwrap();
        }
    }
}

#[test]
fn schema_and_parser_agree_on_structural_bootstrap_and_generation_values() {
    let s = setup();
    let schema = include_str!("../../../docs/protocol/sync-key-distribution-v0.cddl");
    for change in 0..4 {
        let mut message = if change == 3 {
            transition(&s)
        } else {
            parse(&s.genesis).unwrap().message
        };
        match &mut message.body {
            Body::Genesis {
                generation,
                epoch,
                checkpoint,
                ..
            } => match change {
                0 => *generation = u64::MAX,
                1 => *epoch = u64::MAX,
                _ => *checkpoint = Some([7; 48]),
            },
            Body::Membership { generation, .. } => *generation = u64::MAX,
            _ => unreachable!(),
        }
        let bytes = sign(&message, if change == 3 { &s.a } else { &s.root }).unwrap();
        parse(&bytes).unwrap();
        cddl_cat::validate_cbor_bytes("control", schema, &bytes).unwrap();
        if change == 3 {
            let mut state = s.authority.clone();
            assert_eq!(state.apply_membership(&bytes), Err(Error::Stale));
            assert_eq!(state.manifest(), s.authority.manifest());
        } else {
            assert!(matches!(
                Authority::bootstrap(&bytes, route(), s.root.signing.verification_key.as_slice()),
                Err(Error::Unsupported)
            ));
        }
    }
}

#[path = "support/encrypted_outbox.rs"]
mod encrypted_outbox;
#[path = "cases/encrypted_outbox.rs"]
mod outbox_cases;
