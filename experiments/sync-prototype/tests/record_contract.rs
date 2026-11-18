#![cfg(feature = "record-contract")]

use matters_sync_prototype::wire::{self, Context, Error, Header, Kind};
use serde_json::Value;
use sha2::{Digest, Sha256};

fn fixtures() -> Value {
    serde_json::from_str(include_str!("vectors/record-contract/golden.json")).unwrap()
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

#[test]
fn independent_golden_bytes_and_original_transcript_slices() {
    for v in fixtures()["vectors"].as_array().unwrap() {
        let kind = if v["kind"] == "data" {
            Kind::Data
        } else {
            Kind::Snapshot
        };
        let h = header(kind, v["epoch"].as_u64().unwrap());
        let wire = bytes(v, "structural_wire_hex");
        let parsed = wire::parse(&wire, context()).unwrap();
        assert_eq!(parsed.header, h);
        assert_eq!(parsed.header_bytes(), bytes(v, "header_hex"));
        assert_eq!(parsed.signed_bytes, bytes(v, "signed_hex"));
        assert_eq!(wire::header_bytes(h).unwrap(), bytes(v, "header_hex"));
        assert_eq!(wire::record_key_info(h).unwrap(), bytes(v, "info_hex"));
        assert_eq!(parsed.record_key_info().unwrap(), bytes(v, "info_hex"));
        assert_eq!(
            wire::signing_bytes(h, parsed.ciphertext).unwrap(),
            parsed.signed_bytes
        );
        assert_eq!(
            wire::encode(h, parsed.ciphertext, parsed.signature).unwrap(),
            wire
        );
        // B begins directly after the one-byte outer array head. Compare pointer
        // and slice length, not just equal content, to prove original-buffer use.
        assert!(std::ptr::eq(
            parsed.signed_bytes,
            &wire[1..1 + parsed.signed_bytes.len()]
        ));
        let header_offset = wire
            .windows(parsed.header_bytes().len())
            .position(|part| part == parsed.header_bytes())
            .unwrap();
        assert!(std::ptr::eq(
            parsed.header_bytes(),
            &wire[header_offset..header_offset + parsed.header_bytes().len()]
        ));
        let cipher_offset = (parsed.ciphertext.as_ptr() as usize)
            .checked_sub(wire.as_ptr() as usize)
            .unwrap();
        assert!(std::ptr::eq(
            parsed.ciphertext,
            &wire[cipher_offset..cipher_offset + parsed.ciphertext.len()]
        ));
    }
}

#[test]
fn independently_encoded_rejection_cases() {
    let f = fixtures();
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
fn every_truncation_rejects_without_panicking() {
    let f = fixtures();
    for v in f["vectors"].as_array().unwrap() {
        let wire = bytes(v, "structural_wire_hex");
        for end in 0..wire.len() {
            assert!(
                wire::parse(&wire[..end], context()).is_err(),
                "accepted prefix {end}"
            );
        }
    }
}

#[test]
fn cddl_schema_matches_structural_fixtures_with_an_independent_decoder() {
    let schema = include_str!("../../../docs/protocol/sync-record-v0.cddl");
    let f = fixtures();
    for v in f["vectors"].as_array().unwrap() {
        cddl_cat::validate_cbor_bytes("record", schema, &bytes(v, "structural_wire_hex")).unwrap();
    }
    let original = bytes(&f["vectors"][0], "structural_wire_hex");
    // CDDL expresses shape, not canonical encoding or repeated-field equality.
    // Only shape/version/type cases are expected to fail this independent oracle.
    for case in f["rejection_splices"].as_array().unwrap() {
        if !matches!(
            case["error"].as_str().unwrap(),
            "Length" | "UnsupportedVersion" | "UnsupportedSuite" | "UnsupportedType"
        ) {
            continue;
        }
        let mut bad = original.clone();
        let offset = case["offset"].as_u64().unwrap() as usize;
        let end = offset + case["remove"].as_u64().unwrap() as usize;
        bad.splice(offset..end, bytes(case, "insert_hex"));
        assert!(
            cddl_cat::validate_cbor_bytes("record", schema, &bad).is_err(),
            "{}",
            case["name"]
        );
    }
}

#[test]
fn shortest_integer_width_boundaries_match_independent_encoder() {
    for v in fixtures()["epoch_headers"].as_array().unwrap() {
        let h = header(Kind::Data, v["epoch"].as_u64().unwrap());
        assert_eq!(wire::header_bytes(h).unwrap(), bytes(v, "header_hex"));
        let encoded = wire::encode(h, &[0; 16], &[0; wire::SIGNATURE_BYTES]).unwrap();
        assert_eq!(wire::parse(&encoded, context()).unwrap().header, h);
    }
}

#[test]
fn exact_wire_budget_and_payload_bounds() {
    let f = fixtures();
    let h = header(Kind::Snapshot, u64::MAX);
    let cipher = vec![0; wire::MAX_CIPHERTEXT_BYTES];
    let encoded = wire::encode(h, &cipher, &[0; wire::SIGNATURE_BYTES]).unwrap();
    assert_eq!(&Sha256::digest(&encoded)[..], bytes(&f, "max_wire_sha256"));
    assert_eq!(
        encoded.len(),
        f["max_wire_bytes"].as_u64().unwrap() as usize
    );
    assert_eq!(encoded.len(), wire::MAX_WIRE_BYTES);
    assert_eq!(
        encoded.len() - cipher.len(),
        f["max_overhead_bytes"].as_u64().unwrap() as usize
    );
    assert!(wire::parse(&encoded, context()).is_ok());
    let mut too_big = encoded;
    too_big.push(0);
    assert_eq!(
        wire::parse(&too_big, context()).unwrap_err(),
        Error::TooLarge
    );
    assert_eq!(
        wire::encode(
            h,
            &vec![0; wire::MAX_CIPHERTEXT_BYTES + 1],
            &[0; wire::SIGNATURE_BYTES]
        )
        .unwrap_err(),
        Error::TooLarge
    );
    assert_eq!(
        wire::encode(h, &[0; 15], &[0; wire::SIGNATURE_BYTES]).unwrap_err(),
        Error::Length
    );
    let minimum = wire::encode(h, &[0; 16], &[0; wire::SIGNATURE_BYTES]).unwrap();
    assert!(wire::parse(&minimum, context()).is_ok());
    // The shorter data/no-predecessor shape leaves room below the total wire cap.
    // Exercise the independent ciphertext cap on receive, not just on encode.
    let mut oversized_payload =
        wire::encode(header(Kind::Data, 23), &cipher, &[0; wire::SIGNATURE_BYTES]).unwrap();
    let parsed = wire::parse(&oversized_payload, context()).unwrap();
    let start = parsed.ciphertext.as_ptr() as usize - oversized_payload.as_ptr() as usize;
    let end = start + parsed.ciphertext.len();
    assert_eq!(oversized_payload[start - 1], 16); // last byte of 0x00100010 length
    oversized_payload[start - 1] = 17;
    oversized_payload.insert(end, 0);
    assert!(oversized_payload.len() < wire::MAX_WIRE_BYTES);
    assert_eq!(
        wire::parse(&oversized_payload, context()).unwrap_err(),
        Error::TooLarge
    );
}

#[test]
fn route_context_must_be_supplied_independently() {
    let f = fixtures();
    let wire = bytes(&f["vectors"][0], "structural_wire_hex");
    for expected in [
        Context {
            application: &[99; 16],
            ..context()
        },
        Context {
            tenant: &[99; 16],
            ..context()
        },
        Context {
            collection: &[99; 16],
            ..context()
        },
        Context {
            document: &[99; 16],
            ..context()
        },
    ] {
        assert_eq!(
            wire::parse(&wire, expected).unwrap_err(),
            Error::ContextMismatch
        );
    }
}

#[cfg(feature = "crypto-probe")]
mod crypto {
    use super::*;
    use hpke_rs_crypto::{
        types::{AeadAlgorithm, TwoStageKdfAlgorithm},
        HpkeCrypto,
    };
    type Provider = hpke_rs_libcrux::HpkeLibcrux<hpke_rs_libcrux::HpkeLibcruxPrng>;
    use libcrux_ml_dsa::ml_dsa_65;

    fn record_key(h: Header<'_>, cek: &[u8]) -> Vec<u8> {
        let prk =
            Provider::kdf_extract(TwoStageKdfAlgorithm::HkdfSha256, h.record_id, cek).unwrap();
        Provider::kdf_expand(
            TwoStageKdfAlgorithm::HkdfSha256,
            &prk,
            &wire::record_key_info(h).unwrap(),
            32,
        )
        .unwrap()
    }

    #[test]
    fn independent_hkdf_aes_vectors_match_selected_provider() {
        for v in fixtures()["vectors"].as_array().unwrap() {
            let encoded = bytes(v, "structural_wire_hex");
            let parsed = wire::parse(&encoded, context()).unwrap();
            let cek = bytes(v, "cek_hex");
            let prk = Provider::kdf_extract(
                TwoStageKdfAlgorithm::HkdfSha256,
                parsed.header.record_id,
                &cek,
            )
            .unwrap();
            assert_eq!(prk, bytes(v, "prk_hex"));
            let key = Provider::kdf_expand(
                TwoStageKdfAlgorithm::HkdfSha256,
                &prk,
                &parsed.record_key_info().unwrap(),
                32,
            )
            .unwrap();
            assert_eq!(key, bytes(v, "key_hex"));
            let plain = bytes(v, "plaintext_hex");
            assert_eq!(
                Provider::aead_seal(
                    AeadAlgorithm::Aes256Gcm,
                    &key,
                    &[0; 12],
                    parsed.header_bytes(),
                    &plain
                )
                .unwrap(),
                parsed.ciphertext
            );
            assert_eq!(
                Provider::aead_open(
                    AeadAlgorithm::Aes256Gcm,
                    &key,
                    &[0; 12],
                    parsed.header_bytes(),
                    parsed.ciphertext
                )
                .unwrap(),
                plain
            );
            let mut bad = parsed.ciphertext.to_vec();
            bad[0] ^= 1;
            assert!(Provider::aead_open(
                AeadAlgorithm::Aes256Gcm,
                &key,
                &[0; 12],
                parsed.header_bytes(),
                &bad
            )
            .is_err());
            assert!(Provider::aead_open(
                AeadAlgorithm::Aes256Gcm,
                &[99; 32],
                &[0; 12],
                parsed.header_bytes(),
                parsed.ciphertext
            )
            .is_err());
        }
    }

    #[test]
    fn exact_signing_input_and_every_variable_header_field_are_authenticated() {
        let keys = ml_dsa_65::generate_key_pair([11; 32]); // Public test keys ONLY.
        let other = ml_dsa_65::generate_key_pair([12; 32]);
        for v in fixtures()["vectors"].as_array().unwrap() {
            let encoded = bytes(v, "structural_wire_hex");
            let parsed = wire::parse(&encoded, context()).unwrap();
            let h = parsed.header;
            let placeholder = ml_dsa_65::MLDSA65Signature::new(*parsed.signature);
            assert!(ml_dsa_65::verify(
                &keys.verification_key,
                parsed.signed_bytes,
                h.kind.signature_context(),
                &placeholder
            )
            .is_err());
            let signature = ml_dsa_65::sign(
                &keys.signing_key,
                parsed.signed_bytes,
                h.kind.signature_context(),
                [13; 32],
            )
            .unwrap();
            let signed = wire::encode(h, parsed.ciphertext, signature.as_ref()).unwrap();
            let parsed = wire::parse(&signed, context()).unwrap();
            ml_dsa_65::verify(
                &keys.verification_key,
                parsed.signed_bytes,
                h.kind.signature_context(),
                &signature,
            )
            .unwrap();
            assert!(ml_dsa_65::verify(
                &other.verification_key,
                parsed.signed_bytes,
                h.kind.signature_context(),
                &signature
            )
            .is_err());
            assert!(ml_dsa_65::verify(
                &keys.verification_key,
                parsed.signed_bytes,
                b"matters-sync/v0/membership",
                &signature
            )
            .is_err());
            // Re-encode consistent substituted headers: rejection cannot rely solely
            // on duplicated-field equality. Both KDF/AAD and signature must bind them.
            let mutations = [
                Header {
                    context: Context {
                        application: &[99; 16],
                        ..h.context
                    },
                    ..h
                },
                Header {
                    context: Context {
                        tenant: &[99; 16],
                        ..h.context
                    },
                    ..h
                },
                Header {
                    context: Context {
                        collection: &[99; 16],
                        ..h.context
                    },
                    ..h
                },
                Header {
                    context: Context {
                        document: &[99; 16],
                        ..h.context
                    },
                    ..h
                },
                Header {
                    kind: if h.kind == Kind::Data {
                        Kind::Snapshot
                    } else {
                        Kind::Data
                    },
                    ..h
                },
                Header {
                    epoch: h.epoch ^ 1,
                    ..h
                },
                Header {
                    manifest: &[99; 48],
                    ..h
                },
                Header {
                    author: &[99; 48],
                    ..h
                },
                Header {
                    incarnation: &[99; 32],
                    ..h
                },
                Header {
                    record_id: &[99; 32],
                    ..h
                },
                Header {
                    logical_batch: &[99; 32],
                    ..h
                },
                Header {
                    previous: Some(&[99; 48]),
                    ..h
                },
            ];
            // The snapshot fixture also exercises removing an existing predecessor.
            // The data fixture above already covers null -> present.
            for changed in mutations.into_iter().chain(h.previous.map(|_| Header {
                previous: None,
                ..h
            })) {
                let b = wire::signing_bytes(changed, parsed.ciphertext).unwrap();
                assert!(ml_dsa_65::verify(
                    &keys.verification_key,
                    &b,
                    changed.kind.signature_context(),
                    &signature
                )
                .is_err());
                assert!(Provider::aead_open(
                    AeadAlgorithm::Aes256Gcm,
                    &record_key(changed, &bytes(v, "cek_hex")),
                    &[0; 12],
                    &wire::header_bytes(changed).unwrap(),
                    parsed.ciphertext
                )
                .is_err());
            }
            let mut bad_cipher = parsed.ciphertext.to_vec();
            bad_cipher[0] ^= 1;
            assert!(ml_dsa_65::verify(
                &keys.verification_key,
                &wire::signing_bytes(h, &bad_cipher).unwrap(),
                h.kind.signature_context(),
                &signature
            )
            .is_err());
        }
    }
}
