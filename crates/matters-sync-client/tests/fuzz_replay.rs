//! Replays the fuzz corpus and regression inputs of the client's relay
//! response parsers (fuzz/, #45) through the targets' bodies on stable, as
//! crates/matters-sync/tests/fuzz_replay.rs does for the sync parsers.

use std::path::Path;

use matters_sync_client::fuzzing;

/// A target's name, body and how many seeds it has at least.
type Target = (&'static str, fn(&[u8]), usize);

const TARGETS: [Target; 2] = [
    ("relay_page", fuzzing::page, 4),
    ("relay_snapshots", fuzzing::snapshots, 2),
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
fn the_page_seeds_are_pages_the_client_accepts() {
    let corpus = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fuzz/corpus/relay_page");
    for seed in ["complete", "partial", "empty"] {
        let bytes = std::fs::read(corpus.join(seed)).unwrap();
        assert!(fuzzing::page_accepted(&bytes), "{seed}");
    }
    assert!(!fuzzing::page_accepted(
        &std::fs::read(corpus.join("refusal")).unwrap()
    ));
}

#[test]
fn the_targets_reject_garbage_and_truncations_without_panicking() {
    for (target, body, _) in TARGETS {
        body(&[]);
        body(&[0xff; 64]);
        for bytes in inputs(target) {
            for len in 0..bytes.len().min(512) {
                body(&bytes[..len]);
            }
            for at in 0..bytes.len().min(256) {
                for bit in 0..8 {
                    let mut flipped = bytes.clone();
                    flipped[at] ^= 1 << bit;
                    body(&flipped);
                }
            }
        }
    }
}
