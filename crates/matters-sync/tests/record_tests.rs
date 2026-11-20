//! The production record API against the independent golden vectors
//! (Python cbor2 and cryptography, docs/SYNC_RECORD_CONTRACT.md) and the
//! rejections #36 requires.

use matters_sync::record::{derived_key_for_vectors, Error};
use matters_sync::wire::{self, Context, Header, Kind};
use matters_sync::{generate_signing_key, seal, verify, ContentKey};
use serde_json::Value;

fn fixtures() -> Value {
    serde_json::from_str(include_str!(
        "../../../experiments/sync-prototype/tests/vectors/record-contract/golden.json"
    ))
    .unwrap()
}

fn unhex(s: &str) -> Vec<u8> {
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
        .collect()
}

fn bytes(v: &Value, key: &str) -> Vec<u8> {
    unhex(v[key].as_str().unwrap())
}

fn context() -> Context<'static> {
    Context {
        application: &[1; 16],
        tenant: &[2; 16],
        collection: &[3; 16],
        document: &[4; 16],
    }
}

fn header(kind: Kind, epoch: u64) -> Header<'static> {
    Header {
        context: context(),
        kind,
        epoch,
        manifest: &[5; 48],
        author: &[6; 48],
        incarnation: &[7; 32],
        record_id: &[8; 32],
        logical_batch: &[9; 32],
        previous: if kind == Kind::Snapshot {
            Some(&[10; 48])
        } else {
            None
        },
    }
}

fn kind_of(v: &Value) -> Kind {
    if v["kind"] == "data" {
        Kind::Data
    } else {
        Kind::Snapshot
    }
}

fn cek(v: &Value) -> ContentKey {
    ContentKey::new(bytes(v, "cek_hex").try_into().unwrap())
}

#[test]
fn the_derived_key_and_ciphertext_match_the_independent_oracle() {
    let (author, _) = generate_signing_key([42; 32]);
    for v in fixtures()["vectors"].as_array().unwrap() {
        let h = header(kind_of(v), v["epoch"].as_u64().unwrap());
        assert_eq!(wire::header_bytes(h).unwrap(), bytes(v, "header_hex"));
        assert_eq!(wire::record_key_info(h).unwrap(), bytes(v, "info_hex"));
        assert_eq!(
            derived_key_for_vectors(h, &cek(v)).unwrap(),
            bytes(v, "key_hex")
        );
        let sealed = seal(h, &cek(v), &bytes(v, "plaintext_hex"), &author, [7; 32]).unwrap();
        let parsed = wire::parse(&sealed, context()).unwrap();
        assert_eq!(parsed.ciphertext, bytes(v, "ciphertext_hex"));
        assert_eq!(parsed.signed_bytes, bytes(v, "signed_hex"));
    }
}

#[test]
fn the_structural_vectors_and_rejection_splices_parse_as_recorded() {
    let f = fixtures();
    assert_eq!(
        f["max_wire_bytes"].as_u64().unwrap() as usize,
        wire::MAX_WIRE_BYTES
    );
    for v in f["vectors"].as_array().unwrap() {
        let structural = bytes(v, "structural_wire_hex");
        let parsed = wire::parse(&structural, context()).unwrap();
        assert_eq!(
            parsed.header,
            header(kind_of(v), v["epoch"].as_u64().unwrap())
        );
        for end in 0..structural.len() {
            assert!(
                wire::parse(&structural[..end], context()).is_err(),
                "prefix {end}"
            );
        }
    }
    let original = bytes(&f["vectors"][0], "structural_wire_hex");
    for case in f["rejection_splices"].as_array().unwrap() {
        let mut bad = original.clone();
        let offset = case["offset"].as_u64().unwrap() as usize;
        let end = offset + case["remove"].as_u64().unwrap() as usize;
        bad.splice(offset..end, bytes(case, "insert_hex"));
        let error = wire::parse(&bad, context()).expect_err(case["name"].as_str().unwrap());
        assert_eq!(
            format!("{error:?}"),
            case["error"].as_str().unwrap(),
            "{}",
            case["name"]
        );
    }
}

#[test]
fn a_sealed_record_verifies_and_opens_for_its_author_and_key() {
    let (author, public) = generate_signing_key([1; 32]);
    let key = ContentKey::new([9; 32]);
    for kind in [Kind::Data, Kind::Snapshot] {
        let sealed = seal(header(kind, 3), &key, b"a change", &author, [2; 32]).unwrap();
        let verified = verify(wire::parse(&sealed, context()).unwrap(), &public).unwrap();
        assert_eq!(verified.open(&key).unwrap(), b"a change");
        assert_eq!(verified.record().header, header(kind, 3));
    }
    let empty = seal(header(Kind::Data, 0), &key, b"", &author, [3; 32]).unwrap();
    let verified = verify(wire::parse(&empty, context()).unwrap(), &public).unwrap();
    assert_eq!(verified.open(&key).unwrap(), b"");
}

#[test]
fn tampering_another_author_or_another_route_is_refused() {
    let (author, public) = generate_signing_key([1; 32]);
    let (_, stranger) = generate_signing_key([2; 32]);
    let key = ContentKey::new([9; 32]);
    let sealed = seal(header(Kind::Data, 3), &key, b"a change", &author, [3; 32]).unwrap();

    // Every flipped bit is refused somewhere before the plaintext.
    for i in (0..sealed.len()).step_by(7) {
        let mut bad = sealed.clone();
        bad[i] ^= 0x01;
        let opened = wire::parse(&bad, context())
            .map_err(Error::from)
            .and_then(|p| verify(p, &public))
            .and_then(|v| v.open(&key));
        assert!(opened.is_err(), "flip at {i} accepted");
    }
    // Another author's key.
    assert_eq!(
        verify(wire::parse(&sealed, context()).unwrap(), &stranger).unwrap_err(),
        Error::Signature
    );
    // A copy into another tenant, collection or document.
    for other in [
        Context {
            tenant: &[22; 16],
            ..context()
        },
        Context {
            collection: &[33; 16],
            ..context()
        },
        Context {
            document: &[44; 16],
            ..context()
        },
    ] {
        assert_eq!(
            wire::parse(&sealed, other).unwrap_err(),
            wire::Error::ContextMismatch
        );
    }
    // The wrong epoch key decrypts nothing.
    let verified = verify(wire::parse(&sealed, context()).unwrap(), &public).unwrap();
    assert_eq!(
        verified.open(&ContentKey::new([8; 32])).unwrap_err(),
        Error::Decrypt
    );
}

#[test]
fn an_altered_epoch_or_author_field_breaks_the_signature() {
    let (author, public) = generate_signing_key([1; 32]);
    let key = ContentKey::new([9; 32]);
    let sealed = seal(header(Kind::Data, 3), &key, b"x", &author, [3; 32]).unwrap();
    let parsed = wire::parse(&sealed, context()).unwrap();
    // Re-encode the same ciphertext and signature under another epoch, or
    // naming another author: the bytes parse, the signature does not hold.
    let other_author = [66; 48];
    for h in [
        header(Kind::Data, 4),
        Header {
            author: &other_author,
            ..header(Kind::Data, 3)
        },
    ] {
        let forged = wire::encode(h, parsed.ciphertext, parsed.signature).unwrap();
        let reparsed = wire::parse(&forged, context()).unwrap();
        assert_eq!(verify(reparsed, &public).unwrap_err(), Error::Signature);
    }
}

#[test]
fn downgrades_and_oversized_input_are_refused_before_any_crypto() {
    let (author, _) = generate_signing_key([1; 32]);
    let key = ContentKey::new([9; 32]);
    let sealed = seal(header(Kind::Data, 3), &key, b"x", &author, [3; 32]).unwrap();
    // The suite label sits right after the version in the signed body.
    let at = sealed
        .windows(wire::SUITE.len())
        .position(|w| w == wire::SUITE.as_bytes())
        .unwrap();
    let mut other_suite = sealed.clone();
    other_suite[at + wire::SUITE.len() - 1] = b'1';
    assert_eq!(
        wire::parse(&other_suite, context()).unwrap_err(),
        wire::Error::UnsupportedSuite
    );
    let mut other_version = sealed.clone();
    other_version[at - 2] = 0x01;
    assert_eq!(
        wire::parse(&other_version, context()).unwrap_err(),
        wire::Error::UnsupportedVersion
    );
    let mut huge = sealed.clone();
    huge.resize(wire::MAX_WIRE_BYTES + 1, 0);
    assert_eq!(
        wire::parse(&huge, context()).unwrap_err(),
        wire::Error::TooLarge
    );
    let too_big = vec![0; wire::MAX_PLAINTEXT_BYTES + 1];
    assert_eq!(
        seal(header(Kind::Data, 3), &key, &too_big, &author, [3; 32]).unwrap_err(),
        Error::TooLarge
    );
}

#[test]
fn keys_never_print_their_bytes() {
    let (author, public) = generate_signing_key([1; 32]);
    assert_eq!(
        format!("{:?}", ContentKey::new([9; 32])),
        "ContentKey(redacted)"
    );
    assert_eq!(format!("{author:?}"), "SigningKey(redacted)");
    assert_eq!(format!("{public:?}"), "VerificationKey(..)");
    assert_eq!(
        public.as_bytes().len(),
        matters_sync::record::VERIFICATION_KEY_BYTES
    );
}
