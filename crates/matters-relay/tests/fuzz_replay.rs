//! Replays the relay's fuzz corpora and regression inputs (fuzz/, #45)
//! through the fuzz targets' bodies on stable, as matters-sync's
//! tests/fuzz_replay.rs does for the sync parsers. A regression is an
//! input that once failed: copy it from fuzz/artifacts/<target>/ into
//! fuzz/regressions/<target>/ with the fix.

use std::path::Path;

use matters_relay::fuzzing;

/// A target: its name, its body and how many seeds it has at least.
type Target = (&'static str, fn(&[u8]), usize);

const TARGETS: [Target; 4] = [
    ("relay-request", fuzzing::request, 2),
    ("relay-cursor", fuzzing::cursor, 2),
    ("relay-export", fuzzing::export, 2),
    ("relay-hint", fuzzing::hint, 2),
];

fn inputs(target: &str) -> Vec<Vec<u8>> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fuzz");
    let mut out = Vec::new();
    for dir in ["corpus", "regressions"] {
        let Ok(entries) = std::fs::read_dir(root.join(dir).join(target)) else {
            continue;
        };
        for entry in entries {
            out.push(std::fs::read(entry.unwrap().path()).unwrap());
        }
    }
    out
}

#[test]
fn every_corpus_and_regression_input_passes_its_target() {
    for (target, body, seeds) in TARGETS {
        let inputs = inputs(target);
        assert!(inputs.len() >= seeds, "{target}: its seeds are present");
        for input in inputs {
            body(&input);
        }
    }
}

#[test]
fn the_targets_take_garbage_truncations_and_flips_without_panicking() {
    for (target, body, _) in TARGETS {
        body(&[]);
        body(&[0xff; 64]);
        for bytes in inputs(target) {
            for len in 0..bytes.len().min(512) {
                body(&bytes[..len]);
            }
            for at in 0..bytes.len().min(64) {
                for bit in 0..8 {
                    let mut flipped = bytes.clone();
                    flipped[at] ^= 1 << bit;
                    body(&flipped);
                }
            }
        }
    }
}
