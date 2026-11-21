//! Fuzz entry points (#36): the bodies of the cargo-fuzz targets in
//! `fuzz/`, shared with the stable corpus replay test
//! (`tests/fuzz_replay.rs`), so an input the fuzzer once found keeps being
//! checked by every ordinary test run.
//!
//! Each takes arbitrary bytes and must never panic. Beyond that, each
//! asserts its parser's invariant: whatever is accepted re-encodes to
//! exactly the input, and nothing the fuzzer makes carries a valid
//! signature. A failed assertion is a found bug, as a panic would be.

use std::sync::OnceLock;

use crate::authority::{
    self, Authority, DeviceKeys, Invitation, InvitationSecret, RecoveryKeys, RecoverySecret, Route,
};
use crate::{checkpoint, record, wire, ContentKey, VerificationKey};

const DOCUMENT: [u8; 16] = [4; 16];

/// The route the golden vectors use, so seeded inputs reach deep states.
fn route() -> Route {
    Route {
        application: [1; 16],
        tenant: [2; 16],
        collection: [3; 16],
    }
}

/// A fixed collection to apply inputs to. Fixture keys from constant
/// seeds: never use them for anything real.
struct Fixture {
    device: DeviceKeys,
    recovery: RecoveryKeys,
    authority: Authority,
    invitation: Invitation,
    author: VerificationKey,
}

fn fixture() -> &'static Fixture {
    static FIXTURE: OnceLock<Fixture> = OnceLock::new();
    FIXTURE.get_or_init(|| {
        let device = DeviceKeys::generate([1; 32], [2; 32]).expect("fixture device");
        let recovery = RecoverySecret::new([3; 32])
            .keys(&route())
            .expect("fixture recovery keys");
        let genesis = authority::genesis(
            &route(),
            &recovery,
            &device,
            &ContentKey::new([4; 32]),
            [5; 32],
        )
        .expect("fixture genesis");
        let authority = Authority::bootstrap(&genesis, route(), recovery.root_key())
            .expect("fixture authority");
        let invitation = authority
            .invite(InvitationSecret::new([6; 16]))
            .expect("fixture invitation");
        let author = device.verification_key().clone();
        Fixture {
            device,
            recovery,
            authority,
            invitation,
            author,
        }
    })
}

/// A content record: structure, canonical bytes and signature.
pub fn record(data: &[u8]) {
    let _ = record::record_hash(data);
    let Ok(parsed) = wire::parse(data, route().content(&DOCUMENT)) else {
        return;
    };
    let again = wire::encode(parsed.header, parsed.ciphertext, parsed.signature)
        .expect("an accepted record re-encodes");
    assert_eq!(again, data, "an accepted record is canonical");
    assert!(
        record::verify(parsed, &fixture().author).is_err(),
        "no fuzzed record carries the fixture's signature"
    );
}

/// A checkpoint part, or a coverage statement on its own: whatever parses is
/// canonical, and assembling a lone part never panics.
pub fn checkpoint(data: &[u8]) {
    if let Ok(coverage) = checkpoint::Coverage::parse(data) {
        assert_eq!(
            coverage.encode().expect("accepted coverage re-encodes"),
            data,
            "accepted coverage is canonical"
        );
    }
    let Ok(part) = checkpoint::parse_part(data) else {
        return;
    };
    if let Some(bytes) = part.coverage {
        if let Ok(coverage) = checkpoint::Coverage::parse(bytes) {
            let _ = coverage.follows(&coverage);
        }
    }
    let sealed = checkpoint::Sealed {
        author: [1; 48],
        incarnation: [1; 32],
        epoch: 0,
    };
    if let Ok(checkpoint::Assembled::Complete(c)) = checkpoint::assemble(&[(sealed, data)]) {
        assert_eq!(
            c.parts().expect("an assembled checkpoint re-encodes"),
            [data],
            "an assembled checkpoint is canonical"
        );
    }
}

/// A control record: structure and canonical bytes, then every authority
/// operation that consumes one, on a fresh copy of the fixture.
pub fn control(data: &[u8]) {
    let Ok(parsed) = authority::parse(data) else {
        return;
    };
    let again = authority::encode(&parsed.message, parsed.signature)
        .expect("an accepted control re-encodes");
    assert_eq!(again, data, "an accepted control is canonical");
    let f = fixture();
    let mut a = f.authority.clone();
    assert!(
        a.apply_membership(data).is_err(),
        "no fuzzed transition is signed"
    );
    assert!(
        a.apply_recovery(data).is_err(),
        "no fuzzed recovery is signed"
    );
    assert!(
        a.admit_writer(data, DOCUMENT).is_err(),
        "no fuzzed writer is signed"
    );
    assert!(a.unwrap_key(data, &f.device).is_err());
    assert!(a.unwrap_history_key(data, &f.device).is_err());
    assert!(a.unwrap_recovery_key(data, &f.recovery).is_err());
    // An enrollment request may be validly signed by a fuzzed key, but it
    // can't carry the fixture invitation's binding.
    assert!(a.accept_enrollment(data, &f.invitation).is_err());
}

/// A scanned invitation, and a published offer under the fixture's code.
pub fn invitation(data: &[u8]) {
    if let Ok(invitation) = Invitation::parse(data) {
        let again = invitation
            .encode()
            .expect("an accepted invitation re-encodes");
        assert_eq!(again, data, "an accepted invitation is canonical");
    }
    let secret = InvitationSecret::new([6; 16]);
    if let Ok(invitation) = Invitation::from_offer(secret, &route(), data) {
        let again = invitation.offer().expect("an accepted offer re-encodes");
        assert_eq!(again, data, "an accepted offer is canonical");
    }
}

/// Typed recovery words and a typed invitation code.
pub fn typed(data: &[u8]) {
    let Ok(text) = std::str::from_utf8(data) else {
        return;
    };
    if let Ok(secret) = RecoverySecret::from_words(text) {
        let words = secret.words().expect("an accepted secret has words");
        let again = RecoverySecret::from_words(&words).expect("its words parse");
        assert_eq!(
            authority::recovery_seeds(&again, &route()).expect("seeds"),
            authority::recovery_seeds(&secret, &route()).expect("seeds"),
            "the words round-trip"
        );
    }
    if let Ok(secret) = InvitationSecret::from_code(text) {
        let again = InvitationSecret::from_code(&secret.code()).expect("its code parses");
        assert_eq!(
            again.lookup(&route()).expect("lookup"),
            secret.lookup(&route()).expect("lookup"),
            "the code round-trips"
        );
    }
}
