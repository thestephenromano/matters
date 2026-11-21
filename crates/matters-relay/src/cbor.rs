//! The API's CBOR bodies (sync-relay-v0.cddl), in RFC 8949 core
//! deterministic encoding: definite lengths, shortest forms, and map keys
//! in the bytewise order of their encodings (shorter text keys first).

use minicbor::data::Type;
use minicbor::{Decoder, Encoder};

/// A value of the few shapes the API uses.
pub enum Value<'a> {
    Uint(u64),
    Bytes(&'a [u8]),
    Bool(bool),
    Text(&'a str),
    /// `[[uint, bytes], ...]`, as a page's entries.
    Entries(&'a [(u64, Vec<u8>)]),
}

/// Encode a map with text keys, ordered as core deterministic CBOR
/// requires: by encoded length, then bytewise.
pub fn map(mut entries: Vec<(&str, Value<'_>)>) -> Vec<u8> {
    entries.sort_by(|(a, _), (b, _)| (a.len(), a.as_bytes()).cmp(&(b.len(), b.as_bytes())));
    let mut e = Encoder::new(Vec::new());
    // Writing to a Vec cannot fail.
    let _ = e.map(entries.len() as u64);
    for (key, value) in entries {
        let _ = e.str(key);
        let _ = match value {
            Value::Uint(v) => e.u64(v).map(|_| ()),
            Value::Bytes(b) => e.bytes(b).map(|_| ()),
            Value::Bool(b) => e.bool(b).map(|_| ()),
            Value::Text(t) => e.str(t).map(|_| ()),
            Value::Entries(list) => (|| {
                e.array(list.len() as u64)?;
                for (seq, bytes) in list {
                    e.array(2)?.u64(*seq)?.bytes(bytes)?;
                }
                Ok::<_, minicbor::encode::Error<std::convert::Infallible>>(())
            })(),
        };
    }
    e.into_writer()
}

/// A decoded request map: its keys in order and their raw values.
pub struct Request<'a> {
    fields: Vec<(&'a str, Decoder<'a>)>,
}

/// Decode a map whose keys are exactly `keys`, each once; anything else is
/// malformed. Values are read with [`Request::bytes`] and friends.
pub fn request<'a>(bytes: &'a [u8], keys: &[&str]) -> Option<Request<'a>> {
    let mut d = Decoder::new(bytes);
    let n = d.map().ok()??;
    if n != keys.len() as u64 {
        return None;
    }
    let mut fields = Vec::new();
    for _ in 0..n {
        let key = d.str().ok()?;
        if !keys.contains(&key) || fields.iter().any(|(k, _)| *k == key) {
            return None;
        }
        let start = d.position();
        d.skip().ok()?;
        let end = d.position();
        fields.push((key, Decoder::new(&bytes[start..end])));
    }
    if d.position() != bytes.len() {
        return None;
    }
    Some(Request { fields })
}

impl<'a> Request<'a> {
    fn field(&self, key: &str) -> Option<Decoder<'a>> {
        self.fields
            .iter()
            .find(|(k, _)| *k == key)
            .map(|(_, d)| d.clone())
    }

    pub fn bytes<const N: usize>(&self, key: &str) -> Option<[u8; N]> {
        let mut d = self.field(key)?;
        if d.datatype().ok()? != Type::Bytes {
            return None;
        }
        d.bytes().ok()?.try_into().ok()
    }

    /// `[id16, id16, id16]`, a route.
    pub fn route(&self, key: &str) -> Option<[[u8; 16]; 3]> {
        let mut d = self.field(key)?;
        if d.array().ok()? != Some(3) {
            return None;
        }
        let mut out = [[0; 16]; 3];
        for part in &mut out {
            *part = d.bytes().ok()?.try_into().ok()?;
        }
        Some(out)
    }
}
