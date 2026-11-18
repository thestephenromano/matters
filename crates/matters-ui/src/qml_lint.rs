//! The QML must agree with the types the Rust bridge actually exports.
//!
//! `qmllint` checks every property, method and type the QML uses against the
//! `plugin.qmltypes` that cxx-qt generated for this build, so a renamed or
//! mis-cased bridge member fails here instead of silently evaluating to
//! `undefined` at runtime. Any warning in any `qml/*.qml` fails the test.
//!
//! The linter is found through `$QMLLINT`, or in the Qt host binaries reported
//! by `qmake6`/`qmake` (the same Qt the build used).

use std::path::{Path, PathBuf};
use std::process::Command;

fn qt_host_bins() -> Option<PathBuf> {
    let qmake = std::env::var_os("QMAKE");
    let candidates = qmake
        .iter()
        .map(PathBuf::from)
        .chain(["qmake6", "qmake"].map(PathBuf::from));
    for qmake in candidates {
        if let Ok(out) = Command::new(&qmake).args(["-query", "QT_HOST_BINS"]).output() {
            if out.status.success() {
                let dir = String::from_utf8_lossy(&out.stdout).trim().to_string();
                if !dir.is_empty() {
                    return Some(PathBuf::from(dir));
                }
            }
        }
    }
    None
}

fn qmllint() -> PathBuf {
    if let Some(path) = std::env::var_os("QMLLINT") {
        return PathBuf::from(path);
    }
    qt_host_bins()
        .map(|bins| bins.join("qmllint"))
        .filter(|p| p.exists())
        .expect("qmllint not found: install Qt's QML tooling or set QMLLINT=/path/to/qmllint")
}

fn copy_into(src: &Path, dst: &Path) {
    std::fs::create_dir_all(dst.parent().unwrap()).unwrap();
    std::fs::copy(src, dst).unwrap_or_else(|e| panic!("copy {}: {e}", src.display()));
}

#[test]
fn qml_matches_the_generated_bridge_types() {
    let crate_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let generated = Path::new(env!("OUT_DIR")).join("qt-build-utils/qml_modules/matters");

    // Lay the module out as it is installed: qmldir + qmltypes + the QML files
    // at the paths the qmldir names.
    let import_root = tempfile::tempdir().unwrap();
    let module = import_root.path().join("matters");
    for file in ["qmldir", "plugin.qmltypes"] {
        copy_into(&generated.join(file), &module.join(file));
    }
    let mut qml_files: Vec<PathBuf> = std::fs::read_dir(crate_dir.join("qml"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "qml"))
        .collect();
    qml_files.sort();
    assert!(!qml_files.is_empty(), "no QML files found");

    let mut copies = Vec::new();
    for source in &qml_files {
        let copy = module.join("qml").join(source.file_name().unwrap());
        copy_into(source, &copy);
        copies.push(copy);
    }

    let output = Command::new(qmllint())
        .arg("-I")
        .arg(import_root.path())
        .args(&copies)
        .output()
        .expect("run qmllint");
    let report = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
    .replace(&module.display().to_string(), "");

    assert!(
        output.status.success() && report.trim().is_empty(),
        "qmllint reported problems:\n{report}"
    );
}

/// The interface draws its own symbolic icons (`qml/Icon.qml`). Emoji render
/// differently from one font to another and don't follow the palette, so none
/// may come back into the QML.
#[test]
fn qml_uses_matters_icons_not_emoji() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("qml");
    let mut found = Vec::new();
    for entry in std::fs::read_dir(&dir).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().is_none_or(|e| e != "qml") {
            continue;
        }
        let text = std::fs::read_to_string(&path).unwrap();
        for (n, line) in text.lines().enumerate() {
            let is_emoji = |c: char| matches!(c as u32, 0x1F000..=0x1FAFF | 0x2600..=0x27BF | 0x2B00..=0x2BFF);
            if line.chars().any(is_emoji) {
                found.push(format!("{}:{}: {}", path.display(), n + 1, line.trim()));
            }
        }
    }
    assert!(found.is_empty(), "emoji in QML (use Icon instead):\n{}", found.join("\n"));
}
