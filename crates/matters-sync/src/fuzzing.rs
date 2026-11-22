//! Fuzz entry points (#36): the bodies of the cargo-fuzz targets in
//! `fuzz/`, shared with the stable corpus replay test
//! (`tests/fuzz_replay.rs`), so an input the fuzzer once found keeps being
//! checked by every ordinary test run.
//!
//! Each takes arbitrary bytes and must never panic. Beyond that, each
//! asserts its parser's invariant: whatever is accepted re-encodes to
//! exactly the input, and nothing the fuzzer makes carries a valid
//! signature. A failed assertion is a found bug, as a panic would be.
//!
//! The parsers' targets stop at the signature, which fuzzed bytes never
//! carry. The structure-aware targets (#45) sign what the fuzzer chooses
//! with the fixture's keys, so the logic after authentication is reached:
//! [`signed_record`] seals fuzzed plaintext as the fixture's admitted
//! writer, [`signed_control`] signs a fuzzed control's message as its
//! fixture signer, and [`built_checkpoint`] builds a checkpoint from fuzzed
//! fields and assembles it from its own parts.

use std::sync::OnceLock;

use crate::authority::{
    self, Authority, Body, DeviceKeys, EpochKey, Invitation, InvitationSecret, Message,
    RecoveryKeys, RecoverySecret, Route, WriterStart,
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

/// The fixture with a writer admitted for [`DOCUMENT`], and the epoch key,
/// so records the fuzzer has sealed authenticate.
struct Writing {
    authority: Authority,
    key: EpochKey,
    incarnation: [u8; 32],
}

const WRITER_ACTOR: u64 = 42;

fn writing() -> &'static Writing {
    static WRITING: OnceLock<Writing> = OnceLock::new();
    WRITING.get_or_init(|| {
        let f = fixture();
        let mut authority = f.authority.clone();
        let incarnation = [7; 32];
        let claim = authority::sign(
            &Message {
                route: route(),
                signer: f.device.id(),
                body: Body::WriterStart(WriterStart {
                    manifest: authority.manifest(),
                    document: DOCUMENT,
                    device: f.device.id(),
                    incarnation,
                    actor: WRITER_ACTOR,
                    previous: None,
                }),
            },
            f.device.signing_key(),
            [8; 32],
        )
        .expect("fixture writer claim");
        authority
            .admit_writer(&claim, DOCUMENT)
            .expect("fixture writer admitted");
        let envelope = authority
            .wrap_key(&f.device, f.device.id(), &ContentKey::new([4; 32]), [9; 32])
            .expect("fixture envelope");
        let key = authority
            .unwrap_key(&envelope, &f.device)
            .expect("fixture epoch key");
        Writing {
            authority,
            key,
            incarnation,
        }
    })
}

fn fixed<const N: usize>(bytes: &[u8]) -> [u8; N] {
    let mut out = [0; N];
    for (o, b) in out.iter_mut().zip(bytes) {
        *o = *b;
    }
    out
}

/// A record sealed by the fixture's admitted writer: the first byte picks
/// the kind and whether it names a predecessor, the next 48 the
/// predecessor, the rest is the plaintext. It parses, re-encodes exactly,
/// authenticates (a checkpoint part only without a predecessor) and opens
/// to the plaintext; any changed byte stops it authenticating. An opened
/// part is assembled on its own.
pub fn signed_record(data: &[u8]) {
    let w = writing();
    let f = fixture();
    let (&choice, rest) = data.split_first().unwrap_or((&0, &[]));
    let kind = if choice & 1 == 0 {
        wire::Kind::Data
    } else {
        wire::Kind::Snapshot
    };
    let previous: Option<[u8; 48]> = (choice & 2 != 0).then(|| fixed(rest));
    let plaintext = if previous.is_some() {
        rest.get(48..).unwrap_or(&[])
    } else {
        rest
    };
    let manifest = w.authority.manifest();
    let epoch = w.authority.epoch().expect("fixture epoch");
    let author = f.device.id();
    let record_id = fixed::<32>(&record::record_hash(data));
    let fixed_route = route();
    let header = wire::Header {
        context: fixed_route.content(&DOCUMENT),
        kind,
        epoch,
        manifest: &manifest,
        author: &author,
        incarnation: &w.incarnation,
        record_id: &record_id,
        logical_batch: &[3; 32],
        previous: previous.as_ref(),
    };
    let Ok(bytes) = record::seal(
        header,
        w.key.content_key(),
        plaintext,
        f.device.signing_key(),
        [5; 32],
    ) else {
        assert!(
            plaintext.len() > wire::MAX_PLAINTEXT_BYTES,
            "only an oversized plaintext fails to seal"
        );
        return;
    };
    let parsed = wire::parse(&bytes, route().content(&DOCUMENT)).expect("a sealed record parses");
    let again = wire::encode(parsed.header, parsed.ciphertext, parsed.signature)
        .expect("a sealed record re-encodes");
    assert_eq!(again, bytes, "a sealed record is canonical");
    let authenticate = |bytes: &[u8]| -> Option<Vec<u8>> {
        let a = match kind {
            wire::Kind::Snapshot => w
                .authority
                .authenticate_snapshot_at(bytes, &w.key, &DOCUMENT),
            _ => w.authority.authenticate_at(bytes, &w.key, &DOCUMENT),
        };
        let a = a.ok()?;
        assert_eq!(a.actor(), WRITER_ACTOR, "the admitted writer's actor");
        Some(a.open().expect("an authenticated record opens"))
    };
    let opened = authenticate(&bytes);
    if kind == wire::Kind::Snapshot && previous.is_some() {
        assert!(opened.is_none(), "a checkpoint part names no predecessor");
        return;
    }
    let opened = opened.expect("the fixture writer's record authenticates");
    assert_eq!(opened, plaintext, "it opens to its plaintext");
    let mut changed = bytes.clone();
    let at = usize::from(choice) * 7919 % changed.len();
    changed[at] ^= 1;
    assert!(
        authenticate(&changed).is_none(),
        "a changed byte stops it authenticating"
    );
    if kind == wire::Kind::Snapshot {
        let sealed = checkpoint::Sealed {
            author,
            incarnation: w.incarnation,
            epoch,
        };
        if let Ok(checkpoint::Assembled::Complete(c)) =
            checkpoint::assemble(&[(sealed, opened.as_slice())])
        {
            assert_eq!(
                c.parts().expect("an assembled checkpoint re-encodes"),
                [opened],
                "an assembled checkpoint is canonical"
            );
        }
    }
}

/// A fuzzed control, its message signed again by the fixture key that
/// signs its kind (the root for a genesis or recovery, the admin device
/// otherwise), then given to every authority operation that takes one. It
/// parses and re-encodes exactly; whatever the authority then admits, it
/// stays consistent: its current manifest has a roster, or it is frozen.
pub fn signed_control(data: &[u8]) {
    let Ok(parsed) = authority::parse(data) else {
        return;
    };
    let f = fixture();
    let mut message: Message = parsed.message.clone();
    message.route = route();
    let root = matches!(message.body, Body::Genesis(_) | Body::RootRecovery(_));
    let (signer, key) = if root {
        (
            f.recovery.root_id().expect("fixture root"),
            f.recovery.signing_key(),
        )
    } else {
        (f.device.id(), f.device.signing_key())
    };
    message.signer = signer;
    let Ok(bytes) = authority::sign(&message, key, [6; 32]) else {
        return;
    };
    let reparsed = authority::parse(&bytes).expect("a signed control parses");
    assert_eq!(
        authority::encode(&reparsed.message, reparsed.signature).expect("it re-encodes"),
        bytes,
        "a signed control is canonical"
    );
    let mut a = writing().authority.clone();
    let _ = a.apply_membership(&bytes);
    let _ = a.apply_recovery(&bytes);
    let _ = a.admit_writer(&bytes, DOCUMENT);
    let _ = a.unwrap_key(&bytes, &f.device);
    let _ = a.unwrap_history_key(&bytes, &f.device);
    let _ = a.unwrap_recovery_key(&bytes, &f.recovery);
    let _ = a.writer_histories(DOCUMENT);
    assert!(
        a.is_forked() || a.roster_at(&a.manifest()).is_some(),
        "the current manifest has a roster"
    );
}

/// A checkpoint built from fuzzed fields: the first byte is the number of
/// writers, each 1 + 48 + 32 + 48 bytes (records, author, incarnation,
/// head), then the epoch byte, then the snapshot. Whatever builds
/// assembles from its own parts into itself, follows itself, and is
/// refused under another epoch.
pub fn built_checkpoint(data: &[u8]) {
    let (&n, mut rest) = data.split_first().unwrap_or((&0, &[]));
    let mut writers = Vec::new();
    for _ in 0..(n % 8) {
        if rest.len() < 129 {
            return;
        }
        let (w, tail) = rest.split_at(129);
        writers.push(checkpoint::Writer {
            records: u64::from(w[0]),
            author: fixed(&w[1..49]),
            incarnation: fixed(&w[49..81]),
            head: fixed(&w[81..129]),
        });
        rest = tail;
    }
    let Some((&epoch, snapshot)) = rest.split_first() else {
        return;
    };
    let statement = checkpoint::Statement {
        engine: "loro-1".into(),
        schema: 1,
        epoch: u64::from(epoch),
        writers,
        version: snapshot.iter().take(24).copied().collect(),
        frontiers: snapshot.iter().skip(24).take(12).copied().collect(),
        cursor: (epoch & 1 == 1).then_some(checkpoint::Cursor {
            generation: [epoch; 16],
            sequence: u64::from(n),
        }),
    };
    let Ok(c) = checkpoint::Checkpoint::new(statement, snapshot.to_vec(), Vec::new()) else {
        return;
    };
    let parts = c.parts().expect("a built checkpoint has parts");
    let sealed = |epoch: u64| checkpoint::Sealed {
        author: [1; 48],
        incarnation: [1; 32],
        epoch,
    };
    let with: Vec<(checkpoint::Sealed, &[u8])> = parts
        .iter()
        .map(|p| (sealed(c.coverage.epoch), p.as_slice()))
        .collect();
    match checkpoint::assemble(&with).expect("its own parts assemble") {
        checkpoint::Assembled::Complete(back) => assert_eq!(*back, c, "it assembles into itself"),
        other => panic!("its own parts are complete: {other:?}"),
    }
    assert_eq!(c.coverage.follows(&c.coverage), Ok(()), "it follows itself");
    let bytes = c.coverage.encode().expect("its coverage encodes");
    assert_eq!(
        checkpoint::Coverage::parse(&bytes).expect("its coverage parses"),
        c.coverage
    );
    let wrong: Vec<(checkpoint::Sealed, &[u8])> = parts
        .iter()
        .map(|p| (sealed(c.coverage.epoch + 1), p.as_slice()))
        .collect();
    assert_eq!(
        checkpoint::assemble(&wrong),
        Err(checkpoint::Error::Epoch),
        "it is refused under another epoch"
    );
}
