//! Replays the receive fuzz targets' corpus and regression inputs (fuzz/,
//! #45) through their bodies on stable, as
//! crates/matters-sync/tests/fuzz_replay.rs does for the parsers.

use std::path::{Path, PathBuf};

use matters_core::fuzzing;

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fuzz")
}

fn inputs(target: &str) -> Vec<Vec<u8>> {
    let mut out = Vec::new();
    for dir in ["corpus", "regressions"] {
        let Ok(entries) = std::fs::read_dir(root().join(dir).join(target)) else {
            continue;
        };
        let mut paths: Vec<PathBuf> = entries.map(|e| e.unwrap().path()).collect();
        paths.sort();
        for path in paths {
            out.push(std::fs::read(path).unwrap());
        }
    }
    out
}

#[test]
fn every_engine_import_input_passes() {
    let seeds = inputs("engine_import");
    assert!(seeds.len() >= 3, "its seeds are present");
    fuzzing::engine_import(&[]);
    fuzzing::engine_import(&[0xff; 64]);
    for bytes in &seeds {
        fuzzing::engine_import(bytes);
        // Every eighth prefix, and a flip of every eighth byte.
        for len in (0..bytes.len()).step_by(8) {
            fuzzing::engine_import(&bytes[..len]);
        }
        for at in (0..bytes.len()).step_by(8) {
            let mut flipped = bytes.clone();
            flipped[at] ^= 0x41;
            fuzzing::engine_import(&flipped);
        }
    }
}

#[test]
fn every_receive_input_passes() {
    let seeds = inputs("receive");
    assert!(seeds.len() >= 4, "its seeds are present");
    fuzzing::receive(&[]);
    for bytes in &seeds {
        fuzzing::receive(bytes);
    }
    fuzzing::remove_fixture();
}

/// Write the engine_import seeds: a snapshot, an update, and an update
/// that depends on operations it does not carry. Run by hand when they
/// change: `cargo test -p matters-core --test fuzz_replay_tests --
/// --ignored write_engine_import_seeds`.
#[test]
#[ignore]
fn write_engine_import_seeds() {
    let dir = root().join("corpus/engine_import");
    std::fs::create_dir_all(&dir).unwrap();
    let doc = loro::LoroDoc::new();
    doc.set_peer_id(7).unwrap();
    let tree = doc.get_tree("home");
    let node = tree.create(loro::TreeParentId::Root).unwrap();
    let meta = tree.get_meta(node).unwrap();
    meta.insert("id", "00000000-0000-0000-0000-000000000001")
        .unwrap();
    meta.insert("kind", "task").unwrap();
    meta.insert("title", "seed").unwrap();
    doc.commit();
    let before = doc.oplog_vv();
    std::fs::write(
        dir.join("update"),
        doc.export(loro::ExportMode::all_updates()).unwrap(),
    )
    .unwrap();
    doc.set_peer_id(8).unwrap();
    doc.get_map("x").insert("k", 1).unwrap();
    doc.commit();
    std::fs::write(
        dir.join("dependent-update"),
        doc.export(loro::ExportMode::updates(&before)).unwrap(),
    )
    .unwrap();
    std::fs::write(
        dir.join("snapshot"),
        doc.export(loro::ExportMode::Snapshot).unwrap(),
    )
    .unwrap();
}
