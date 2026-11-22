//! A control message's signature, checked with a key the caller chose
//! (#45). For a relay that stores writer claims and key envelopes without
//! admitting them (clients do), but must not store forgeries of them.
//! Everything else about a control message is checked where it is
//! admitted, in [`crate::authority`].

use crate::authority::{ParsedControl, Result};
use crate::VerificationKey;

/// Verify `parsed`'s signature, in its kind's signature context, with
/// `key`. The caller takes `key` from authenticated state, such as a
/// verified roster, never from the message itself. A valid signature
/// admits nothing by itself: it says only that the key's holder signed
/// these bytes.
pub fn verify_signature(parsed: &ParsedControl<'_>, key: &VerificationKey) -> Result<()> {
    parsed.verify(key)
}

#[cfg(test)]
mod tests {
    use super::verify_signature;
    use crate::authority::{self, Body, DeviceKeys, Error, Message, Route, WriterStart};

    #[test]
    fn only_the_signers_key_verifies_a_control_message() {
        let route = Route {
            application: [1; 16],
            tenant: [2; 16],
            collection: [3; 16],
        };
        let a = DeviceKeys::generate([1; 32], [2; 32]).unwrap();
        let b = DeviceKeys::generate([3; 32], [4; 32]).unwrap();
        let message = Message {
            route,
            signer: a.id(),
            body: Body::WriterStart(WriterStart {
                manifest: [5; 48],
                document: [6; 16],
                device: a.id(),
                incarnation: [7; 32],
                actor: 8,
                previous: None,
            }),
        };
        let mut bytes = authority::sign(&message, a.signing_key(), [9; 32]).unwrap();
        let parsed = authority::parse(&bytes).unwrap();
        assert_eq!(verify_signature(&parsed, a.verification_key()), Ok(()));
        assert_eq!(
            verify_signature(&parsed, b.verification_key()),
            Err(Error::Signature)
        );
        // The signed bytes, not just the signature, are what is checked.
        let at = bytes.windows(16).position(|w| w == [6; 16]).unwrap();
        bytes[at] ^= 1;
        let tampered = authority::parse(&bytes).unwrap();
        assert_eq!(
            verify_signature(&tampered, a.verification_key()),
            Err(Error::Signature)
        );
    }
}
