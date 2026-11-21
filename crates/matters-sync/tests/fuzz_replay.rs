//! Replays every fuzz corpus and regression input (fuzz/, #36) through the
//! fuzz targets' bodies on stable, so an input the fuzzer once found keeps
//! being checked by the ordinary test suite. A regression is an input that
//! once failed: copy it from fuzz/artifacts/<target>/ into
//! fuzz/regressions/<target>/ with the fix.

use std::path::Path;

use matters_sync::fuzzing;

fn replay(target: &str, body: fn(&[u8])) -> usize {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fuzz");
    let mut count = 0;
    for dir in ["corpus", "regressions"] {
        let Ok(entries) = std::fs::read_dir(root.join(dir).join(target)) else {
            continue;
        };
        for entry in entries {
            let path = entry.unwrap().path();
            body(&std::fs::read(&path).unwrap());
            count += 1;
        }
    }
    count
}

#[test]
fn every_corpus_and_regression_input_passes_its_target() {
    for (target, body, seeds) in [
        ("record", fuzzing::record as fn(&[u8]), 2),
        ("control", fuzzing::control, 6),
        ("invitation", fuzzing::invitation, 2),
        ("typed", fuzzing::typed, 5),
    ] {
        assert!(
            replay(target, body) >= seeds,
            "{target}: its seeds are present"
        );
    }
}

#[test]
fn the_targets_reject_garbage_and_truncations_without_panicking() {
    let seeds = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fuzz/corpus");
    for (target, body) in [
        ("record", fuzzing::record as fn(&[u8])),
        ("control", fuzzing::control),
        ("invitation", fuzzing::invitation),
        ("typed", fuzzing::typed),
    ] {
        body(&[]);
        body(&[0xff; 64]);
        for entry in std::fs::read_dir(seeds.join(target)).unwrap() {
            let bytes = std::fs::read(entry.unwrap().path()).unwrap();
            // Every eighth prefix, and every single-byte flip of the head.
            for len in (0..bytes.len()).step_by(8) {
                body(&bytes[..len]);
            }
            for at in 0..bytes.len().min(64) {
                let mut flipped = bytes.clone();
                flipped[at] ^= 0x01;
                body(&flipped);
            }
        }
    }
}
