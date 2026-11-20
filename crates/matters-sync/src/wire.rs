//! The content-record codec (docs/SYNC_RECORD_CONTRACT.md, sync-record-v0.cddl).
//! Parsing checks structure, canonical encoding, sizes and the expected route
//! only: it never authorizes or opens a record. Ported from the approved
//! prototype unchanged in behavior; the prototype's independent golden
//! fixtures check it here too.

use minicbor::{Decoder, Encoder};
use std::convert::Infallible;

pub const SUITE: &str = "matters-sync-pq-v0";
pub const SIGNATURE_BYTES: usize = 3309;
pub const MAX_PLAINTEXT_BYTES: usize = 1024 * 1024;
pub const MAX_CIPHERTEXT_BYTES: usize = MAX_PLAINTEXT_BYTES + 16;
// Field-by-field size derivation from sync-record-v0.cddl. Text/byte strings
// share the same length-prefix size. Golden length AND hash are independent checks.
const MAX_HEADER_BYTES: usize = preferred_width(14) // H array
    + preferred_width(0) + string_size(SUITE.len())
    + 4 * string_size(16) // application, tenant, collection, document
    + string_size("snapshot".len()) + preferred_width(u64::MAX)
    + 2 * string_size(48) // manifest, author
    + 3 * string_size(32) // incarnation, record, batch
    + string_size(48); // present predecessor is larger than null
const MAX_SIGNED_BYTES: usize = preferred_width(8) // B array
    + preferred_width(0) + string_size(SUITE.len()) + string_size("snapshot".len())
    + 3 * string_size(16) + string_size(48) // route, signer
    + preferred_width(3) + MAX_HEADER_BYTES + string_size(MAX_CIPHERTEXT_BYTES)
    + string_size(48); // body author
pub const MAX_WIRE_BYTES: usize =
    preferred_width(2) + MAX_SIGNED_BYTES + string_size(SIGNATURE_BYTES);

const fn string_size(length: usize) -> usize {
    preferred_width(length as u64) + length
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    Malformed,
    NonCanonical,
    UnsupportedVersion,
    UnsupportedSuite,
    UnsupportedType,
    Length,
    ContextMismatch,
    Inconsistent,
    TooLarge,
    Trailing,
}
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "record format: {self:?}")
    }
}
impl std::error::Error for Error {}
impl From<minicbor::decode::Error> for Error {
    fn from(_: minicbor::decode::Error) -> Self {
        Self::Malformed
    }
}
impl From<minicbor::encode::Error<Infallible>> for Error {
    fn from(_: minicbor::encode::Error<Infallible>) -> Self {
        Self::Malformed
    }
}
type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Data,
    Snapshot,
}
impl Kind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Data => "data",
            Self::Snapshot => "snapshot",
        }
    }
    pub fn signature_context(self) -> &'static [u8] {
        match self {
            Self::Data => b"matters-sync/v0/data",
            Self::Snapshot => b"matters-sync/v0/snapshot",
        }
    }
}

/// Expected route, supplied by trusted local configuration, not by the record.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Context<'a> {
    pub application: &'a [u8; 16],
    pub tenant: &'a [u8; 16],
    pub collection: &'a [u8; 16],
    pub document: &'a [u8; 16],
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Header<'a> {
    pub context: Context<'a>,
    pub kind: Kind,
    pub epoch: u64,
    pub manifest: &'a [u8; 48],
    pub author: &'a [u8; 48],
    pub incarnation: &'a [u8; 32],
    pub record_id: &'a [u8; 32],
    pub logical_batch: &'a [u8; 32],
    pub previous: Option<&'a [u8; 48]>,
}

/// Structural validity only. Callers must still verify membership, signatures,
/// epoch/chain/actor admission, AEAD and domain semantics before any state change.
#[derive(Debug)]
pub struct ParsedRecord<'a> {
    pub header: Header<'a>,
    header_bytes: &'a [u8],
    pub signed_bytes: &'a [u8],
    pub ciphertext: &'a [u8],
    pub signature: &'a [u8; SIGNATURE_BYTES],
}
impl<'a> ParsedRecord<'a> {
    pub fn header_bytes(&self) -> &'a [u8] {
        self.header_bytes
    }

    /// Receiver transcript: append the already validated original H item.
    /// No header normalization or re-encoding is involved.
    pub fn record_key_info(&self) -> Result<Vec<u8>> {
        let mut info = record_key_info_prefix()?;
        info.extend_from_slice(self.header_bytes);
        Ok(info)
    }
}

// This checks RFC 8949 shortest argument widths; minicbor owns binary decoding.
const fn preferred_width(value: u64) -> usize {
    match value {
        0..=23 => 1,
        24..=0xff => 2,
        0x100..=0xffff => 3,
        0x10000..=0xffffffff => 5,
        _ => 9,
    }
}
struct Reader<'a>(Decoder<'a>);
impl<'a> Reader<'a> {
    fn width(&self, start: usize, payload: usize, argument: u64) -> Result<()> {
        if self.0.position() - start - payload != preferred_width(argument) {
            return Err(Error::NonCanonical);
        }
        Ok(())
    }
    fn array(&mut self, size: u64) -> Result<()> {
        let start = self.0.position();
        let actual = self.0.array()?.ok_or(Error::NonCanonical)?;
        self.width(start, 0, actual)?;
        if actual != size {
            return Err(Error::Length);
        }
        Ok(())
    }
    fn uint(&mut self) -> Result<u64> {
        let start = self.0.position();
        let n = self.0.u64()?;
        self.width(start, 0, n)?;
        Ok(n)
    }
    fn text(&mut self) -> Result<&'a str> {
        let start = self.0.position();
        let s = self.0.str()?;
        self.width(start, s.len(), s.len() as u64)?;
        Ok(s)
    }
    fn bytes(&mut self) -> Result<&'a [u8]> {
        let start = self.0.position();
        let b = self.0.bytes()?;
        self.width(start, b.len(), b.len() as u64)?;
        Ok(b)
    }
    fn fixed<const N: usize>(&mut self) -> Result<&'a [u8; N]> {
        self.bytes()?.try_into().map_err(|_| Error::Length)
    }
    fn version_suite(&mut self) -> Result<()> {
        if self.uint()? != 0 {
            return Err(Error::UnsupportedVersion);
        }
        if self.text()? != SUITE {
            return Err(Error::UnsupportedSuite);
        }
        Ok(())
    }
    fn kind(&mut self) -> Result<Kind> {
        match self.text()? {
            "data" => Ok(Kind::Data),
            "snapshot" => Ok(Kind::Snapshot),
            _ => Err(Error::UnsupportedType),
        }
    }
    fn header(&mut self) -> Result<Header<'a>> {
        self.array(14)?;
        self.version_suite()?;
        let context = Context {
            application: self.fixed()?,
            tenant: self.fixed()?,
            collection: self.fixed()?,
            document: self.fixed()?,
        };
        Ok(Header {
            context,
            kind: self.kind()?,
            epoch: self.uint()?,
            manifest: self.fixed()?,
            author: self.fixed()?,
            incarnation: self.fixed()?,
            record_id: self.fixed()?,
            logical_batch: self.fixed()?,
            previous: if self.0.datatype()? == minicbor::data::Type::Null {
                self.0.null()?;
                None
            } else {
                Some(self.fixed()?)
            },
        })
    }
}

pub fn parse<'a>(bytes: &'a [u8], expected: Context<'_>) -> Result<ParsedRecord<'a>> {
    if bytes.len() > MAX_WIRE_BYTES {
        return Err(Error::TooLarge);
    }
    let mut r = Reader(Decoder::new(bytes));
    r.array(2)?;
    let signed_start = r.0.position();
    r.array(8)?;
    r.version_suite()?;
    let kind = r.kind()?;
    let application: &[u8; 16] = r.fixed()?;
    let tenant: &[u8; 16] = r.fixed()?;
    let collection: &[u8; 16] = r.fixed()?;
    let signer: &[u8; 48] = r.fixed()?;
    r.array(3)?;
    let header_start = r.0.position();
    let header = r.header()?;
    let header_end = r.0.position();
    let ciphertext = r.bytes()?;
    check_ciphertext(ciphertext)?;
    let author: &[u8; 48] = r.fixed()?;
    let signed_end = r.0.position();
    let signature = r.fixed()?;
    if r.0.position() != bytes.len() {
        return Err(Error::Trailing);
    }
    if kind != header.kind
        || application != header.context.application
        || tenant != header.context.tenant
        || collection != header.context.collection
        || signer != header.author
        || author != header.author
    {
        return Err(Error::Inconsistent);
    }
    if header.context != expected {
        return Err(Error::ContextMismatch);
    }
    Ok(ParsedRecord {
        header,
        header_bytes: &bytes[header_start..header_end],
        signed_bytes: &bytes[signed_start..signed_end],
        ciphertext,
        signature,
    })
}

fn check_ciphertext(bytes: &[u8]) -> Result<()> {
    if bytes.len() < 16 {
        return Err(Error::Length);
    }
    if bytes.len() > MAX_CIPHERTEXT_BYTES {
        return Err(Error::TooLarge);
    }
    Ok(())
}
fn write_header(e: &mut Encoder<Vec<u8>>, h: Header<'_>) -> Result<()> {
    e.array(14)?
        .u8(0)?
        .str(SUITE)?
        .bytes(h.context.application)?
        .bytes(h.context.tenant)?
        .bytes(h.context.collection)?
        .bytes(h.context.document)?
        .str(h.kind.label())?
        .u64(h.epoch)?
        .bytes(h.manifest)?
        .bytes(h.author)?
        .bytes(h.incarnation)?
        .bytes(h.record_id)?
        .bytes(h.logical_batch)?;
    match h.previous {
        Some(hash) => {
            e.bytes(hash)?;
        }
        None => {
            e.null()?;
        }
    }
    Ok(())
}
fn write_body(e: &mut Encoder<Vec<u8>>, h: Header<'_>, ciphertext: &[u8]) -> Result<()> {
    check_ciphertext(ciphertext)?;
    e.array(8)?
        .u8(0)?
        .str(SUITE)?
        .str(h.kind.label())?
        .bytes(h.context.application)?
        .bytes(h.context.tenant)?
        .bytes(h.context.collection)?
        .bytes(h.author)?
        .array(3)?;
    write_header(e, h)?;
    e.bytes(ciphertext)?.bytes(h.author)?;
    Ok(())
}
pub fn header_bytes(header: Header<'_>) -> Result<Vec<u8>> {
    let mut e = Encoder::new(Vec::new());
    write_header(&mut e, header)?;
    Ok(e.into_writer())
}
fn record_key_info_prefix() -> Result<Vec<u8>> {
    let mut e = Encoder::new(Vec::new());
    e.array(2)?.str("matters-sync/v0/record-key")?;
    Ok(e.into_writer())
}
/// Sender transcript: a new typed header has no received wire bytes yet.
pub fn record_key_info(header: Header<'_>) -> Result<Vec<u8>> {
    let mut e = Encoder::new(record_key_info_prefix()?);
    write_header(&mut e, header)?;
    Ok(e.into_writer())
}
pub fn signing_bytes(header: Header<'_>, ciphertext: &[u8]) -> Result<Vec<u8>> {
    let mut e = Encoder::new(Vec::new());
    write_body(&mut e, header, ciphertext)?;
    Ok(e.into_writer())
}
/// Packages an externally produced signature; this function does not sign or seal.
pub fn encode(
    header: Header<'_>,
    ciphertext: &[u8],
    signature: &[u8; SIGNATURE_BYTES],
) -> Result<Vec<u8>> {
    let mut e = Encoder::new(Vec::new());
    e.array(2)?;
    write_body(&mut e, header, ciphertext)?;
    e.bytes(signature)?;
    Ok(e.into_writer())
}
