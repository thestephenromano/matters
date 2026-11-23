//! Checkpoint encoding, assembly and comparison (#40): a checkpoint has one
//! encoding, its parts must agree with each other and with the records that
//! carry them, its payload must match its coverage, and a coverage
//! statement that covers less than an accepted one is refused.

use matters_sync::checkpoint::{
    assemble, parse_part, Assembled, Checkpoint, Coverage, Cursor, Error, Sealed, Statement,
    Writer, CHUNK_BYTES, MAX_PARTS,
};
use matters_sync::record_hash;

fn writer(n: u8, records: u64) -> Writer {
    Writer {
        author: [n; 48],
        incarnation: [n; 32],
        records,
        head: [n.wrapping_add(100); 48],
    }
}

fn statement() -> Statement {
    Statement {
        engine: "loro-1".into(),
        schema: 17,
        epoch: 3,
        // Out of order on purpose: the encoder sorts.
        writers: vec![writer(2, 5), writer(1, 9)],
        version: vec![1, 2, 3],
        frontiers: vec![4, 5],
        cursor: Some(Cursor {
            generation: [7; 16],
            sequence: 42,
        }),
    }
}

fn sealed() -> Sealed {
    Sealed {
        author: [1; 48],
        incarnation: [1; 32],
        epoch: 3,
    }
}

fn checkpoint(snapshot_len: usize, pending: Vec<Vec<u8>>) -> Checkpoint {
    let snapshot = (0..snapshot_len).map(|i| (i % 251) as u8).collect();
    Checkpoint::new(statement(), snapshot, pending).unwrap()
}

fn with(parts: &[Vec<u8>]) -> Vec<(Sealed, &[u8])> {
    parts.iter().map(|p| (sealed(), p.as_slice())).collect()
}

#[test]
fn a_small_checkpoint_is_one_part_and_reassembles() {
    let c = checkpoint(1000, vec![b"pending b".to_vec(), b"pending a".to_vec()]);
    assert_eq!(
        c.coverage
            .writers
            .iter()
            .map(|w| w.incarnation[0])
            .collect::<Vec<_>>(),
        [1, 2],
        "writers in canonical order"
    );
    assert!(c.coverage.pending.windows(2).all(|p| p[0] < p[1]));
    let parts = c.parts().unwrap();
    assert_eq!(parts.len(), 1);
    assert_eq!(
        assemble(&with(&parts)).unwrap(),
        Assembled::Complete(Box::new(c.clone()))
    );
    let part = parse_part(&parts[0]).unwrap();
    assert_eq!(part.id, c.id().unwrap());
    assert_eq!(Coverage::parse(part.coverage.unwrap()).unwrap(), c.coverage);
}

#[test]
fn a_large_checkpoint_reassembles_from_parts_in_any_order() {
    let c = checkpoint(2 * CHUNK_BYTES + 77, vec![]);
    let parts = c.parts().unwrap();
    assert_eq!(parts.len(), 3);
    assert_eq!(c.coverage.parts, 3);
    // Without part 0, how much is missing is not known yet.
    assert_eq!(
        assemble(&with(&parts[1..])).unwrap(),
        Assembled::Incomplete { missing: None }
    );
    assert_eq!(
        assemble(&with(&[parts[2].clone(), parts[0].clone()])).unwrap(),
        Assembled::Incomplete { missing: Some(1) }
    );
    // A part delivered twice is the same part.
    let shuffled = vec![
        parts[2].clone(),
        parts[0].clone(),
        parts[2].clone(),
        parts[1].clone(),
    ];
    assert_eq!(
        assemble(&with(&shuffled)).unwrap(),
        Assembled::Complete(Box::new(c))
    );
}

#[test]
fn parts_that_disagree_are_refused() {
    let c = checkpoint(CHUNK_BYTES + 10, vec![]);
    let parts = c.parts().unwrap();
    // Another writer, or another epoch, sealed one of the parts.
    let mut other = with(&parts);
    other[1].0.author = [9; 48];
    assert_eq!(assemble(&other), Err(Error::Inconsistent));
    let mut other = with(&parts);
    other[1].0.incarnation = [9; 32];
    assert_eq!(assemble(&other), Err(Error::Inconsistent));
    // Every part sealed under an epoch other than the coverage's.
    let wrong: Vec<(Sealed, &[u8])> = parts
        .iter()
        .map(|p| {
            (
                Sealed {
                    epoch: 4,
                    ..sealed()
                },
                p.as_slice(),
            )
        })
        .collect();
    assert_eq!(assemble(&wrong), Err(Error::Epoch));
    // A part of another checkpoint.
    let d = Checkpoint::new(
        Statement {
            schema: 18,
            ..statement()
        },
        vec![1; CHUNK_BYTES + 10],
        vec![],
    )
    .unwrap();
    let foreign = d.parts().unwrap();
    assert_eq!(
        assemble(&with(&[parts[0].clone(), foreign[1].clone()])),
        Err(Error::Inconsistent)
    );
}

#[test]
fn a_tampered_payload_or_coverage_is_refused() {
    let c = checkpoint(CHUNK_BYTES + 10, vec![b"held back".to_vec()]);
    let parts = c.parts().unwrap();
    // One byte of a chunk changed, in place, keeping the encoding valid.
    let mut changed = parts.clone();
    let at = changed[1].len() - 3;
    changed[1][at] ^= 1;
    assert_eq!(assemble(&with(&changed)), Err(Error::Digest));
    // Coverage claiming one more record for a writer, under its old ID.
    let mut claimed = c.coverage.clone();
    claimed.writers[0].records += 1;
    let forged = forge_part0(&parts[0], &claimed.encode().unwrap(), None);
    assert_eq!(
        assemble(&with(&[forged, parts[1].clone()])),
        Err(Error::Digest)
    );
    // The same forgery under its own ID no longer matches the other part.
    let forged = forge_part0(
        &parts[0],
        &claimed.encode().unwrap(),
        Some(claimed.id().unwrap()),
    );
    assert_eq!(
        assemble(&with(&[forged, parts[1].clone()])),
        Err(Error::Inconsistent)
    );
    // Coverage listing a pending record the payload does not carry.
    let mut claimed = c.coverage.clone();
    claimed.pending = vec![record_hash(b"something else")];
    let one = Checkpoint {
        coverage: claimed.clone(),
        ..c.clone()
    };
    let forged = one.parts().unwrap();
    assert_eq!(assemble(&with(&forged)), Err(Error::Digest));
}

/// Part 0 with other coverage bytes and, optionally, another ID.
fn forge_part0(part: &[u8], coverage: &[u8], id: Option<[u8; 48]>) -> Vec<u8> {
    let p = parse_part(part).unwrap();
    let mut e = minicbor::Encoder::new(Vec::new());
    e.array(6)
        .unwrap()
        .str("matters-checkpoint-part")
        .unwrap()
        .u8(0)
        .unwrap()
        .bytes(&id.unwrap_or(p.id))
        .unwrap()
        .u64(0)
        .unwrap()
        .bytes(p.chunk)
        .unwrap()
        .bytes(coverage)
        .unwrap();
    e.into_writer()
}

#[test]
fn only_the_canonical_encoding_parses() {
    let c = checkpoint(100, vec![]);
    let coverage = c.coverage.encode().unwrap();
    // Writers out of order.
    let mut unordered = c.coverage.clone();
    unordered.writers.reverse();
    let bytes = encode_unchecked(&unordered);
    assert_eq!(Coverage::parse(&bytes), Err(Error::Format));
    // A writer with no records.
    let mut empty = c.coverage.clone();
    empty.writers[0].records = 0;
    assert_eq!(
        Coverage::parse(&encode_unchecked(&empty)),
        Err(Error::Format)
    );
    // A part count that does not match the length.
    let mut count = c.coverage.clone();
    count.parts = 2;
    assert_eq!(
        Coverage::parse(&encode_unchecked(&count)),
        Err(Error::Format)
    );
    // A non-minimal integer: the schema 17 as a two-byte head.
    let mut wide = coverage.clone();
    let at = wide.windows(2).position(|w| w == [0x11, 0x03]).unwrap();
    wide.splice(at..at + 1, [0x18, 0x11]);
    assert_eq!(Coverage::parse(&wide), Err(Error::NonCanonical));
    // Trailing bytes.
    let mut trailing = coverage;
    trailing.push(0);
    assert_eq!(Coverage::parse(&trailing), Err(Error::Format));
    // A part whose index is non-minimal.
    let part = c.parts().unwrap().remove(0);
    let p = parse_part(&part).unwrap();
    let mut e = minicbor::Encoder::new(Vec::new());
    e.array(6)
        .unwrap()
        .str("matters-checkpoint-part")
        .unwrap()
        .u8(0)
        .unwrap()
        .bytes(&p.id)
        .unwrap();
    let mut bytes = e.into_writer();
    bytes.extend([0x18, 0x00]);
    let mut e = minicbor::Encoder::new(Vec::new());
    e.bytes(p.chunk)
        .unwrap()
        .bytes(p.coverage.unwrap())
        .unwrap();
    bytes.extend(e.into_writer());
    assert_eq!(parse_part(&bytes).map(|_| ()), Err(Error::NonCanonical));
    // Coverage on a part other than 0, or missing from part 0.
    let big = checkpoint(CHUNK_BYTES + 1, vec![]).parts().unwrap();
    let p1 = parse_part(&big[1]).unwrap();
    let mut e = minicbor::Encoder::new(Vec::new());
    e.array(6)
        .unwrap()
        .str("matters-checkpoint-part")
        .unwrap()
        .u8(0)
        .unwrap()
        .bytes(&p1.id)
        .unwrap()
        .u64(0)
        .unwrap()
        .bytes(p1.chunk)
        .unwrap()
        .null()
        .unwrap();
    assert_eq!(parse_part(&e.into_writer()).map(|_| ()), Err(Error::Format));
}

/// The coverage encoding without the encoder's own checks, for building
/// invalid statements.
fn encode_unchecked(c: &Coverage) -> Vec<u8> {
    let mut e = minicbor::Encoder::new(Vec::new());
    e.array(13)
        .unwrap()
        .str("matters-checkpoint")
        .unwrap()
        .u8(0)
        .unwrap()
        .str(&c.engine)
        .unwrap()
        .u64(c.schema)
        .unwrap()
        .u64(c.epoch)
        .unwrap()
        .array(c.writers.len() as u64)
        .unwrap();
    for w in &c.writers {
        e.array(4)
            .unwrap()
            .bytes(&w.author)
            .unwrap()
            .bytes(&w.incarnation)
            .unwrap()
            .u64(w.records)
            .unwrap()
            .bytes(&w.head)
            .unwrap();
    }
    e.bytes(&c.version)
        .unwrap()
        .bytes(&c.frontiers)
        .unwrap()
        .array(c.pending.len() as u64)
        .unwrap();
    for p in &c.pending {
        e.bytes(p).unwrap();
    }
    e.null()
        .unwrap()
        .bytes(&c.payload)
        .unwrap()
        .u64(c.length)
        .unwrap()
        .u64(c.parts)
        .unwrap();
    e.into_writer()
}

#[test]
fn a_checkpoint_covering_less_than_an_accepted_one_is_refused() {
    let known = checkpoint(10, vec![]).coverage;
    // The same statement follows itself; more records, a later epoch or
    // cursor, or an extra writer follow it.
    assert_eq!(known.follows(&known), Ok(()));
    let mut later = known.clone();
    later.writers[0].records += 3;
    later.writers[0].head = [0; 48];
    later.epoch += 1;
    later.cursor.as_mut().unwrap().sequence += 1;
    later.writers.push(writer(3, 1));
    assert_eq!(later.follows(&known), Ok(()));
    // Fewer records for a writer.
    let mut fewer = known.clone();
    fewer.writers[1].records -= 1;
    assert_eq!(fewer.follows(&known), Err(Error::Behind));
    // A writer left out.
    let mut missing = known.clone();
    missing.writers.remove(0);
    assert_eq!(missing.follows(&known), Err(Error::Behind));
    // An earlier epoch.
    let mut earlier = known.clone();
    earlier.epoch -= 1;
    assert_eq!(earlier.follows(&known), Err(Error::Behind));
    // An earlier cursor in the same generation; another generation's
    // cursor is not comparable.
    let mut cursor = known.clone();
    cursor.cursor.as_mut().unwrap().sequence -= 1;
    assert_eq!(cursor.follows(&known), Err(Error::Behind));
    cursor.cursor.as_mut().unwrap().generation = [8; 16];
    assert_eq!(cursor.follows(&known), Ok(()));
    // Another head at the same position, or another author for a chain.
    let mut fork = known.clone();
    fork.writers[0].head = [0; 48];
    assert_eq!(fork.follows(&known), Err(Error::Fork));
    let mut author = known.clone();
    author.writers[0].author = [0; 48];
    assert_eq!(author.follows(&known), Err(Error::Fork));
}

#[test]
fn limits_are_checked_before_work() {
    // A statement of more parts than allowed.
    let mut huge = checkpoint(10, vec![]).coverage;
    huge.parts = MAX_PARTS + 1;
    huge.length = huge.parts * CHUNK_BYTES as u64;
    assert_eq!(
        Coverage::parse(&encode_unchecked(&huge)),
        Err(Error::Format)
    );
    // An empty snapshot is not a checkpoint.
    assert_eq!(
        Checkpoint::new(statement(), vec![], vec![]).map(|_| ()),
        Err(Error::Format)
    );
    // A part index beyond the most parts.
    let c = checkpoint(10, vec![]);
    let p = parse_part(&c.parts().unwrap()[0]).unwrap().id;
    let mut e = minicbor::Encoder::new(Vec::new());
    e.array(6)
        .unwrap()
        .str("matters-checkpoint-part")
        .unwrap()
        .u8(0)
        .unwrap()
        .bytes(&p)
        .unwrap()
        .u64(MAX_PARTS)
        .unwrap()
        .bytes(&[1])
        .unwrap()
        .null()
        .unwrap();
    assert_eq!(parse_part(&e.into_writer()).map(|_| ()), Err(Error::Format));
    // Coverage beyond its byte limit is refused before it is decoded.
    assert_eq!(Coverage::parse(&vec![0; 256 * 1024 + 1]), Err(Error::Limit));
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[test]
fn the_encoding_matches_the_independent_oracle() {
    // tests/vectors/checkpoint_kat.py: the same checkpoints encoded by hand
    // in Python's standard library.
    let small = checkpoint(1000, vec![b"pending b".to_vec(), b"pending a".to_vec()]);
    assert_eq!(
        hex(&small.id().unwrap()),
        "ba0ac258bdb84f9da6b4110b1b5df819d05cb527e3db1e61c738be617a1830e3e141a1c807456504cc18a359f62d07d2"
    );
    assert_eq!(
        hex(&record_hash(&small.parts().unwrap()[0])),
        "4b647d012f4e2a5b7bd454bf40ca2f27dd6ba473eae5a0857719bd842c8f4bf34ebb90e92a84c4e6e7f16b8b8e0bad63"
    );
    let large = checkpoint(CHUNK_BYTES + 77, vec![]);
    assert_eq!(
        hex(&large.id().unwrap()),
        "bb3916ada7802e1625898171470222ec3d3a8949d05245aea677745da36fdf166e2717cc13a37287614a884aba713ffa"
    );
    let parts: Vec<String> = large
        .parts()
        .unwrap()
        .iter()
        .map(|p| hex(&record_hash(p)))
        .collect();
    assert_eq!(
        parts,
        [
            "d23bc97a3d6a11b88e03176efbe4ad8620755bba250360bf738e66de141e3cd54a394b20cfdb70073a25567f4b38eec2",
            "de6c7b3991a9ead2ce26969b03e2b39e0df060aed26d608de0e93c04ba14fd3c0a0796598728c88dd8af3031580089f9",
        ]
    );
}

#[test]
fn a_relay_cursor_reads_back_exactly() {
    let at = Cursor {
        generation: [7; 16],
        sequence: 300,
    };
    let bytes = at.relay_cursor();
    assert_eq!(Cursor::from_relay_cursor(&bytes), Some(at));
    let mut longer = bytes.clone();
    longer.push(0);
    assert_eq!(Cursor::from_relay_cursor(&longer), None);
    assert_eq!(Cursor::from_relay_cursor(&bytes[1..]), None);
    assert_eq!(Cursor::from_relay_cursor(&[0x83, 0, 0, 0]), None);
}
