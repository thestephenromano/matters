#![cfg(feature = "crypto-probe")]

use hpke_rs::{Hpke, Mode};
use hpke_rs_crypto::types::{AeadAlgorithm, KdfAlgorithm, KemAlgorithm};
use hpke_rs_libcrux::HpkeLibcrux;
use libcrux_ml_dsa::ml_dsa_65;

fn hpke(aead: AeadAlgorithm) -> Hpke<HpkeLibcrux> {
    Hpke::try_new(
        Mode::Base,
        KemAlgorithm::MlKem768P256,
        KdfAlgorithm::HkdfSha256,
        aead,
    )
    .expect("OS entropy must be available for the provider probe")
}

fn hex(text: &str) -> Vec<u8> {
    assert_eq!(text.len() % 2, 0);
    (0..text.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&text[i..i + 2], 16).unwrap())
        .collect()
}

#[test]
fn published_draft_vector_checks_hybrid_key_derivation_and_decryption() {
    // Draft A.4 uses AES-128-GCM. This vector is NOT a claim about the full selected AES-256 suite.
    let vector: serde_json::Value = serde_json::from_str(include_str!(
        "vectors/mlkem768-p256-hkdfsha256-aes128gcm.json"
    ))
    .unwrap();
    let bytes = |key: &str| hex(vector[key].as_str().unwrap());
    let config = hpke(AeadAlgorithm::Aes128Gcm);
    let recipient = config.derive_key_pair(&bytes("ikmR")).unwrap();
    assert_eq!(recipient.public_key().as_slice(), bytes("pkRm"));
    let encryption = &vector["encryptions"][0];
    let plain = config
        .open(
            &bytes("enc"),
            recipient.private_key(),
            &bytes("info"),
            &hex(encryption["aad"].as_str().unwrap()),
            &hex(encryption["ct"].as_str().unwrap()),
            None,
            None,
            None,
        )
        .unwrap();
    assert_eq!(plain, hex(encryption["pt"].as_str().unwrap()));
}

#[test]
fn selected_aes256_suite_round_trips_and_rejects_context_or_ciphertext_changes() {
    let mut config = hpke(AeadAlgorithm::Aes256Gcm);
    let recipient = config.generate_key_pair().unwrap();
    let wrong_recipient = config.generate_key_pair().unwrap();
    // Public synthetic payload and contexts; no application wire encoding is claimed here.
    let info = b"matters prototype collection A / recipient 1";
    let aad = b"epoch 7 / purpose content-key";
    let plain = [42; 32];
    let (enc, cipher) = config
        .seal(recipient.public_key(), info, aad, &plain, None, None, None)
        .unwrap();
    assert_eq!(
        config
            .open(
                &enc,
                recipient.private_key(),
                info,
                aad,
                &cipher,
                None,
                None,
                None
            )
            .unwrap(),
        plain
    );
    assert!(config
        .open(
            &enc,
            recipient.private_key(),
            b"collection B",
            aad,
            &cipher,
            None,
            None,
            None
        )
        .is_err());
    assert!(config
        .open(
            &enc,
            recipient.private_key(),
            info,
            b"epoch 8",
            &cipher,
            None,
            None,
            None
        )
        .is_err());
    assert!(config
        .open(
            &enc,
            wrong_recipient.private_key(),
            info,
            aad,
            &cipher,
            None,
            None,
            None
        )
        .is_err());
    let mut corrupted = cipher.clone();
    corrupted[0] ^= 1;
    assert!(config
        .open(
            &enc,
            recipient.private_key(),
            info,
            aad,
            &corrupted,
            None,
            None,
            None
        )
        .is_err());
    assert!(config
        .open(
            &enc[..enc.len() - 1],
            recipient.private_key(),
            info,
            aad,
            &cipher,
            None,
            None,
            None
        )
        .is_err());
    let (next_enc, next_cipher) = config
        .seal(recipient.public_key(), info, aad, &plain, None, None, None)
        .unwrap();
    assert_ne!(next_enc, enc);
    assert_ne!(next_cipher, cipher);
}

#[test]
fn mldsa65_verifies_authorship_and_domain_context() {
    // Fixed public test seeds ONLY. No key-generation helper is exposed to application code.
    let keys = ml_dsa_65::generate_key_pair([7; 32]);
    let other = ml_dsa_65::generate_key_pair([8; 32]);
    let message = b"synthetic signed envelope bytes";
    let context = b"matters-sync/v0/key_envelope";
    let signature = ml_dsa_65::sign(&keys.signing_key, message, context, [9; 32]).unwrap();
    ml_dsa_65::verify(&keys.verification_key, message, context, &signature).unwrap();
    assert!(ml_dsa_65::verify(&other.verification_key, message, context, &signature).is_err());
    assert!(ml_dsa_65::verify(
        &keys.verification_key,
        b"substituted envelope",
        context,
        &signature
    )
    .is_err());
    assert!(ml_dsa_65::verify(
        &keys.verification_key,
        message,
        b"matters-sync/v0/membership",
        &signature
    )
    .is_err());
}
