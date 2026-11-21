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
        if let Ok(out) = Command::new(&qmake)
            .args(["-query", "QT_HOST_BINS"])
            .output()
        {
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
            let is_emoji =
                |c: char| matches!(c as u32, 0x1F000..=0x1FAFF | 0x2600..=0x27BF | 0x2B00..=0x2BFF);
            if line.chars().any(is_emoji) {
                found.push(format!("{}:{}: {}", path.display(), n + 1, line.trim()));
            }
        }
    }
    assert!(
        found.is_empty(),
        "emoji in QML (use Icon instead):\n{}",
        found.join("\n")
    );
}

/// A control that draws its own `contentItem` must not also set `text`: some
/// styles paint the text themselves, outside `contentItem` (the KDE desktop
/// style's ToolButton draws it in its native background, with an access-key
/// underline), so the label would appear twice. Such a control keeps its
/// label in its own property and gives it to `Accessible.name`.
#[test]
fn qml_custom_content_controls_leave_text_to_their_content() {
    const CONTROLS: &[&str] = &[
        "Button",
        "CheckBox",
        "DelayButton",
        "ItemDelegate",
        "MenuItem",
        "RadioButton",
        "RoundButton",
        "Switch",
        "TabButton",
        "ToolButton",
    ];
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("qml");
    let mut found = Vec::new();
    for entry in std::fs::read_dir(&dir).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().is_none_or(|e| e != "qml") {
            continue;
        }
        let text = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        for (start, line) in lines.iter().enumerate() {
            let Some(name) = line.trim().strip_suffix('{').map(str::trim) else {
                continue;
            };
            if !CONTROLS.contains(&name) {
                continue;
            }
            // Walk the control's block, looking only at its own members
            // (brace depth 1), not at those of the items inside it.
            let (mut depth, mut content, mut text_line) = (0i32, false, None);
            for (n, member) in lines.iter().enumerate().skip(start) {
                let m = member.trim();
                if depth == 1 {
                    content |= m.starts_with("contentItem:");
                    if m.starts_with("text:") {
                        text_line = Some(n);
                    }
                }
                depth += member.matches('{').count() as i32 - member.matches('}').count() as i32;
                if depth <= 0 && n > start {
                    break;
                }
            }
            if let (true, Some(n)) = (content, text_line) {
                found.push(format!("{}:{}: {}", path.display(), n + 1, lines[n].trim()));
            }
        }
    }
    assert!(
        found.is_empty(),
        "a control with its own contentItem sets text (styles may paint it twice):\n{}",
        found.join("\n")
    );
}

/// Every colour is a palette token (#74): a colour literal outside
/// MainView's palette block would escape the contrast check
/// (`tests/qml/tst_contrast.qml`), which reads the tokens.
#[test]
fn qml_colours_come_from_the_palette() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("qml");
    let literal = |line: &str| {
        line.match_indices("\"#").any(|(at, _)| {
            let hex = &line[at + 2..];
            let digits = hex.chars().take_while(char::is_ascii_hexdigit).count();
            matches!(digits, 3 | 6 | 8) && hex[digits..].starts_with('"')
        })
    };
    let mut found = Vec::new();
    let mut palettes = 0;
    for entry in std::fs::read_dir(&dir).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().is_none_or(|e| e != "qml") {
            continue;
        }
        let text = std::fs::read_to_string(&path).unwrap();
        let mut in_palette = false;
        for (n, line) in text.lines().enumerate() {
            if line.contains("// The palette (#74).") {
                in_palette = true;
                palettes += 1;
            } else if line.contains("// End of the palette.") {
                in_palette = false;
            } else if !in_palette && literal(line) {
                found.push(format!("{}:{}: {}", path.display(), n + 1, line.trim()));
            }
        }
    }
    assert_eq!(palettes, 1, "one palette block, in MainView.qml");
    assert!(
        found.is_empty(),
        "colour literals outside the palette (add a token):\n{}",
        found.join("\n")
    );
}
