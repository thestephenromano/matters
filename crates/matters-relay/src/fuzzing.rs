//! Fuzz entry points (#45): the bodies of the relay's cargo-fuzz targets in
//! `fuzz/`, shared with the stable corpus replay test
//! (`tests/fuzz_replay.rs`), for every relay parser of outside input:
//! request bodies, cursors (from the query), exports (from an operator's
//! file) and hint payloads (from PostgreSQL notifications).
//!
//! Each takes arbitrary bytes and must never panic. Beyond that, each
//! asserts its parser's invariant; a failed assertion is a found bug.

use crate::cbor;
use crate::store::Cursor;

/// The key sets of the API's CBOR request bodies.
const REQUESTS: [&[&str]; 2] = [
    &["route", "device"],
    &["route", "device", "challenge", "signature"],
];

/// A request body, read as each of the API's requests. An accepted one has
/// exactly its keys, each readable without panicking.
pub fn request(data: &[u8]) {
    for keys in REQUESTS {
        if let Some(req) = cbor::request(data, keys) {
            let _ = req.route("route");
            let _ = req.bytes::<48>("device");
            let _ = req.bytes::<32>("challenge");
            let _ = req.bytes::<{ matters_sync::wire::SIGNATURE_BYTES }>("signature");
            // A key outside the set is never found.
            assert!(req.bytes::<48>("other").is_none());
        }
    }
}

/// A page cursor. An accepted one is canonical: it re-encodes to exactly
/// its bytes.
pub fn cursor(data: &[u8]) {
    if let Ok(cursor) = Cursor::decode(data) {
        assert_eq!(cursor.encode(), data);
    }
}

/// An export file. An accepted one encodes again to bytes that parse to
/// the same route, authority objects and records.
pub fn export(data: &[u8]) {
    let Some(parsed) = crate::export::parse(data) else {
        return;
    };
    let records: Vec<(u64, Vec<u8>)> = parsed
        .records
        .iter()
        .enumerate()
        .map(|(n, bytes)| (n as u64 + 1, bytes.clone()))
        .collect();
    let again = crate::export::encode(&parsed.route, &[0; 16], &parsed.authority, &records);
    assert_eq!(crate::export::parse(&again), Some(parsed));
}

/// A hint payload. An accepted one prints, canonically, to a payload that
/// parses to the same hint.
pub fn hint(data: &[u8]) {
    let Ok(text) = std::str::from_utf8(data) else {
        return;
    };
    if let Some(hint) = crate::hints::parse(text) {
        let printed = crate::hints::payload(hint.route, hint.sequence, &hint.generation);
        assert_eq!(crate::hints::parse(&printed), Some(hint));
    }
}
