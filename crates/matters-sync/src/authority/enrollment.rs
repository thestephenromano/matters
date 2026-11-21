//! Enrolling a new device through a one-time invitation (#37,
//! docs/SYNC_KEY_DISTRIBUTION.md "Enrollment"):
//!
//! ```text
//! invitation = [0, suite, "invitation", app16, tenant16, collection16, root48, manifest48, secret16]
//! offer      = [0, suite, "invitation_offer", app16, tenant16, collection16, root48, manifest48, tag48]
//! code       = Crockford base32 of secret16: 26 characters, the first 0-7
//! key(label) = HKDF-SHA384(salt = suite, ikm = secret16,
//!                          info = CBOR([label, app16, tenant16, collection16]))
//! lookup16   = key("matters-sync/v0/invitation-lookup")
//! tag        = HMAC-SHA384(key("matters-sync/v0/invitation-offer"), offer's first eight items)
//! binding    = HMAC-SHA384(key("matters-sync/v0/enrollment"),
//!                          CBOR(["matters-sync/v0/enrollment", suite, app16, tenant16, collection16,
//!                                root48, manifest48, device48, signing_key1952, kem_key1249]))
//! ```
//!
//! An admin device shows the invitation as a QR code, or shows its code and
//! publishes the offer under the lookup. The new device signs an
//! `enrollment_request` carrying both its public keys and the binding: the
//! signature proves it holds the signing key, the binding proves it saw the
//! secret. A server that resets an account or substitutes keys knows
//! neither, so it cannot enroll a device. Only symmetric primitives and
//! ML-DSA are involved.
//!
//! The secret is one-time: the admin's app forgets it after one accepted
//! request, or when it expires. This module holds no state.

use minicbor::{Decoder, Encoder};
use subtle::ConstantTimeEq;

use super::envelope::wipe;
use super::message::{sign, Body, EnrollmentRequest, Message, Reader, Route};
use super::{DeviceKeys, Error, Hash, Result};
use crate::wire::SUITE;

/// Bytes of an invitation secret: 128 bits of fresh OS entropy.
pub const INVITATION_SECRET_BYTES: usize = 16;
/// The largest invitation or offer accepted, checked before decoding.
const MAX_INVITATION_BYTES: usize = 256;
const LOOKUP_LABEL: &str = "matters-sync/v0/invitation-lookup";
const OFFER_LABEL: &str = "matters-sync/v0/invitation-offer";
pub(crate) const ENROLLMENT_LABEL: &str = "matters-sync/v0/enrollment";
/// Crockford's base32 alphabet: no I, L, O or U.
const CROCKFORD: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";
/// Characters of a typed invitation code.
pub const CODE_LENGTH: usize = 26;

/// A one-time invitation secret. Zeroed when dropped; never printed or
/// cloned.
pub struct InvitationSecret([u8; INVITATION_SECRET_BYTES]);

impl InvitationSecret {
    /// From 16 bytes of fresh OS entropy.
    pub fn new(bytes: [u8; INVITATION_SECRET_BYTES]) -> Self {
        Self(bytes)
    }

    /// The 26-character Crockford base32 code a user types.
    pub fn code(&self) -> String {
        let mut value = u128::from_be_bytes(self.0);
        let mut out = [0u8; CODE_LENGTH];
        for c in out.iter_mut().rev() {
            *c = CROCKFORD[(value & 31) as usize];
            value >>= 5;
        }
        // 26 characters carry 130 bits; the first holds the top 3.
        String::from_utf8(out.to_vec()).unwrap_or_default()
    }

    /// Parse a typed code. Case, spaces and hyphens are ignored, and I, L
    /// and O read as 1, 1 and 0, as Crockford specifies.
    pub fn from_code(code: &str) -> Result<Self> {
        let mut value: u128 = 0;
        let mut count = 0;
        for c in code.chars().filter(|c| !matches!(c, ' ' | '-')) {
            let c = match c.to_ascii_uppercase() {
                'I' | 'L' => '1',
                'O' => '0',
                c => c,
            };
            let digit = CROCKFORD
                .iter()
                .position(|d| char::from(*d) == c)
                .ok_or(Error::Format)?;
            count += 1;
            if count > CODE_LENGTH || (count == 1 && digit > 7) {
                return Err(Error::Format);
            }
            value = (value << 5) | digit as u128;
        }
        if count != CODE_LENGTH {
            return Err(Error::Format);
        }
        Ok(Self(value.to_be_bytes()))
    }

    /// The 16-byte name the offer is published under for `route`. It
    /// reveals nothing usable about the secret.
    pub fn lookup(&self, route: &Route) -> Result<[u8; 16]> {
        let mut out = [0; 16];
        derive(&self.0, route, LOOKUP_LABEL, &mut out)?;
        Ok(out)
    }

    fn key(&self, route: &Route, label: &str) -> Result<[u8; 48]> {
        let mut out = [0; 48];
        derive(&self.0, route, label, &mut out)?;
        Ok(out)
    }
}

impl Drop for InvitationSecret {
    fn drop(&mut self) {
        wipe(&mut self.0);
    }
}

impl std::fmt::Debug for InvitationSecret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("InvitationSecret(redacted)")
    }
}

/// `HKDF-SHA384(salt = suite, ikm = secret, info = CBOR([label, route]))`.
fn derive(secret: &[u8], route: &Route, label: &str, out: &mut [u8]) -> Result<()> {
    let mut e = Encoder::new(Vec::new());
    e.array(4)?
        .str(label)?
        .bytes(&route.application)?
        .bytes(&route.tenant)?
        .bytes(&route.collection)?;
    libcrux_hkdf::hkdf(
        libcrux_hkdf::Algorithm::Sha384,
        out,
        SUITE.as_bytes(),
        secret,
        &e.into_writer(),
    )
    .map_err(|_| Error::Crypto)
}

fn hmac(key: &[u8; 48], data: &[u8]) -> Hash {
    let mut tag = [0; 48];
    libcrux_hmac::hmac(libcrux_hmac::Algorithm::Sha384, key, data, &mut tag);
    tag
}

/// Whether two tags match, in time independent of where they differ.
pub(crate) fn tags_match(a: &Hash, b: &Hash) -> bool {
    a.ct_eq(b).into()
}

/// A one-time invitation into one collection, at one manifest, under its
/// pinned root. Secret-bearing: not cloned, and Debug omits the secret.
#[derive(Debug)]
pub struct Invitation {
    pub route: Route,
    /// The root signer ID the new device pins.
    pub root: Hash,
    /// The manifest the invitation was issued under.
    pub manifest: Hash,
    pub secret: InvitationSecret,
}

fn write_head(
    e: &mut Encoder<Vec<u8>>,
    kind: &str,
    route: &Route,
    root: &Hash,
    manifest: &Hash,
) -> Result<()> {
    e.u8(0)?
        .str(SUITE)?
        .str(kind)?
        .bytes(&route.application)?
        .bytes(&route.tenant)?
        .bytes(&route.collection)?
        .bytes(root)?
        .bytes(manifest)?;
    Ok(())
}

/// Read `[0, suite, kind, app, tenant, collection, root, manifest, x]`.
fn read_head(bytes: &[u8], kind: &str) -> Result<(Route, Hash, Hash, Vec<u8>)> {
    if bytes.len() > MAX_INVITATION_BYTES {
        return Err(Error::Limit);
    }
    let mut r = Reader(Decoder::new(bytes));
    r.array(9)?;
    if r.0.u64()? != 0 || r.0.str()? != SUITE {
        return Err(Error::Unsupported);
    }
    if r.0.str()? != kind {
        return Err(Error::Unsupported);
    }
    let route = Route {
        application: r.fixed()?,
        tenant: r.fixed()?,
        collection: r.fixed()?,
    };
    let root = r.fixed()?;
    let manifest = r.fixed()?;
    let last = r.0.bytes()?.to_vec();
    if r.0.position() != bytes.len() {
        return Err(Error::Format);
    }
    Ok((route, root, manifest, last))
}

impl Invitation {
    /// The QR payload. It carries the secret: show it, never store or send
    /// it, and wipe the buffer after use.
    pub fn encode(&self) -> Result<Vec<u8>> {
        let mut e = Encoder::new(Vec::new());
        e.array(9)?;
        write_head(
            &mut e,
            "invitation",
            &self.route,
            &self.root,
            &self.manifest,
        )?;
        e.bytes(&self.secret.0)?;
        Ok(e.into_writer())
    }

    /// Parse a scanned QR payload: structure and canonical bytes only.
    pub fn parse(bytes: &[u8]) -> Result<Self> {
        let (route, root, manifest, mut secret) = read_head(bytes, "invitation")?;
        let fixed: Result<[u8; INVITATION_SECRET_BYTES]> =
            secret.as_slice().try_into().map_err(|_| Error::Format);
        wipe(&mut secret);
        let invitation = Self {
            route,
            root,
            manifest,
            secret: InvitationSecret(fixed?),
        };
        let mut canonical = invitation.encode()?;
        let matches = canonical == bytes;
        wipe(&mut canonical);
        if !matches {
            return Err(Error::NonCanonical);
        }
        Ok(invitation)
    }

    fn offer_head(&self) -> Result<Vec<u8>> {
        let mut e = Encoder::new(Vec::new());
        e.array(8)?;
        write_head(
            &mut e,
            "invitation_offer",
            &self.route,
            &self.root,
            &self.manifest,
        )?;
        Ok(e.into_writer())
    }

    /// The offer to publish under [`InvitationSecret::lookup`] when the user
    /// types the code instead of scanning: the route, root and manifest,
    /// authenticated under the secret. It does not carry the secret.
    pub fn offer(&self) -> Result<Vec<u8>> {
        let mut key = self.secret.key(&self.route, OFFER_LABEL)?;
        let tag = hmac(&key, &self.offer_head()?);
        wipe(&mut key);
        let mut e = Encoder::new(Vec::new());
        e.array(9)?;
        write_head(
            &mut e,
            "invitation_offer",
            &self.route,
            &self.root,
            &self.manifest,
        )?;
        e.bytes(&tag)?;
        Ok(e.into_writer())
    }

    /// Rebuild the invitation from a typed code and the offer fetched under
    /// its lookup, for the route the device signed in to. The offer must be
    /// canonical and authenticated under the code's secret.
    pub fn from_offer(secret: InvitationSecret, route: &Route, offer: &[u8]) -> Result<Self> {
        let (offered, root, manifest, tag) = read_head(offer, "invitation_offer")?;
        if &offered != route {
            return Err(Error::Context);
        }
        let tag: Hash = tag.as_slice().try_into().map_err(|_| Error::Format)?;
        let invitation = Self {
            route: offered,
            root,
            manifest,
            secret,
        };
        let mut key = invitation.secret.key(route, OFFER_LABEL)?;
        let expected = hmac(&key, &invitation.offer_head()?);
        wipe(&mut key);
        if !tags_match(&expected, &tag) {
            return Err(Error::Unauthorized);
        }
        if invitation.offer()? != offer {
            return Err(Error::NonCanonical);
        }
        Ok(invitation)
    }

    /// The binding a request for `device`'s keys must carry.
    pub(crate) fn binding(&self, device: &Hash, request: &EnrollmentRequest) -> Result<Hash> {
        let mut e = Encoder::new(Vec::new());
        e.array(10)?
            .str(ENROLLMENT_LABEL)?
            .str(SUITE)?
            .bytes(&self.route.application)?
            .bytes(&self.route.tenant)?
            .bytes(&self.route.collection)?
            .bytes(&self.root)?
            .bytes(&request.manifest)?
            .bytes(device)?
            .bytes(request.signing_key.as_bytes())?
            .bytes(request.kem_key.as_bytes())?;
        let mut key = self.secret.key(&self.route, ENROLLMENT_LABEL)?;
        let tag = hmac(&key, &e.into_writer());
        wipe(&mut key);
        Ok(tag)
    }
}

/// The new device's signed request to join under `invitation`.
/// `signing_randomness` must be fresh OS entropy; retries resend the bytes
/// returned here.
pub fn request_enrollment(
    invitation: &Invitation,
    device: &DeviceKeys,
    signing_randomness: [u8; 32],
) -> Result<Vec<u8>> {
    let mut request = EnrollmentRequest {
        manifest: invitation.manifest,
        signing_key: device.verification_key().clone(),
        kem_key: device.kem_public_key().clone(),
        binding: [0; 48],
    };
    request.binding = invitation.binding(&device.id(), &request)?;
    let message = Message {
        route: invitation.route.clone(),
        signer: device.id(),
        body: Body::EnrollmentRequest(Box::new(request)),
    };
    sign(&message, device.signing_key(), signing_randomness)
}
