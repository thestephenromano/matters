use std::path::{Path, PathBuf};
use std::process::Command;

use cxx_qt_build::{CxxQtBuilder, QmlModule};

fn main() {
    let mut builder = CxxQtBuilder::new_qml_module(
        QmlModule::new("matters")
            // TaskListModel derives from QAbstractListModel, which QML tooling
            // resolves from this module.
            .depend("QtQml.Models")
            .qml_files([
                "qml/Main.qml",
                "qml/MainView.qml",
                "qml/Icon.qml",
                "qml/ChecklistEditor.qml",
                "qml/DatePill.qml",
                "qml/TagField.qml",
                "qml/FocusRing.qml",
            ]),
    )
    .file("src/bridge.rs")
    .qt_module("Gui")
    .qt_module("Quick")
    .qt_module("QuickControls2");

    println!("cargo:rerun-if-changed=cpp/app_style.cpp");
    println!("cargo:rerun-if-changed=cpp/app_identity.cpp");
    println!("cargo:rerun-if-changed=cpp/translations.cpp");
    println!("cargo:rerun-if-changed=cpp/locale_format.cpp");
    // SAFETY: only adds source files; no compiler flags are changed.
    builder = unsafe {
        builder.cc_builder(|cc| {
            cc.file("cpp/app_style.cpp");
            cc.file("cpp/app_identity.cpp");
            cc.file("cpp/translations.cpp");
            cc.file("cpp/locale_format.cpp");
        })
    };

    let ui_tests = std::env::var_os("CARGO_FEATURE_UI_TESTS").is_some();
    catalogs(ui_tests);

    if ui_tests {
        println!("cargo:rerun-if-changed=cpp/quick_test.cpp");
        println!("cargo:rerun-if-changed=cpp/accessibility_probe.cpp");
        // What a screen reader is told, for the accessibility tests (#74).
        builder = builder
            .file("src/accessibility_probe.rs")
            // A desktop whose accent and contrast the tests set (#76).
            .file("src/fake_desktop.rs")
            // Pseudo-translations for the tests (#75).
            .file("src/test_translations.rs")
            .qt_module("QuickTest");
        // SAFETY: only adds source files; no compiler flags are changed.
        builder = unsafe {
            builder.cc_builder(|cc| {
                cc.file("cpp/quick_test.cpp");
                cc.file("cpp/accessibility_probe.cpp");
            })
        };
    }

    builder.build();
}

/// Compile the translation catalogs (#75) into `OUT_DIR` and list them in
/// `catalogs.rs` for `src/i18n.rs`: every `translations/matters_<lang>.ts`,
/// and for the UI tests two pseudo-translations of the template
/// (`translations/matters.ts`): `pseudo`, every string marked and made
/// longer, and `pseudo_rtl`, the same laid out right to left. Compiling
/// needs Qt's `lrelease` (qt6-linguist).
fn catalogs(ui_tests: bool) {
    let out = PathBuf::from(std::env::var_os("OUT_DIR").expect("OUT_DIR"));
    println!("cargo:rerun-if-changed=translations");
    println!("cargo:rerun-if-env-changed=LRELEASE");
    let mut sources: Vec<(String, PathBuf)> = std::fs::read_dir("translations")
        .map(|dir| {
            dir.filter_map(|entry| entry.ok().map(|e| e.path()))
                .filter_map(|path| {
                    let name = path.file_name()?.to_str()?;
                    let lang = name.strip_prefix("matters_")?.strip_suffix(".ts")?;
                    Some((lang.to_string(), path))
                })
                .collect()
        })
        .unwrap_or_default();
    sources.sort();
    if ui_tests {
        let template = std::fs::read_to_string("translations/matters.ts")
            .expect("translations/matters.ts, the catalog template (#75)");
        for (name, rtl) in [("pseudo", false), ("pseudo_rtl", true)] {
            let path = out.join(format!("matters_{name}.ts"));
            std::fs::write(&path, pseudo_catalog(&template, rtl)).expect("write a pseudo catalog");
            sources.push((name.to_string(), path));
        }
    }
    let mut list = String::from("pub static CATALOGS: &[(&str, &[u8])] = &[\n");
    for (lang, source) in &sources {
        let qm = out.join(format!("matters_{lang}.qm"));
        lrelease(source, &qm);
        list.push_str(&format!(
            "    ({lang:?}, include_bytes!({:?})),\n",
            qm.display().to_string()
        ));
    }
    list.push_str("];\n");
    std::fs::write(out.join("catalogs.rs"), list).expect("write catalogs.rs");
}

/// Qt's lrelease: $LRELEASE, or the one with the Qt this builds against
/// (`qmake6 -query QT_HOST_BINS`, as Fedora's qt6-linguist installs it),
/// or `lrelease-qt6` or `lrelease` on the PATH (the KDE Flatpak SDK's).
fn lrelease_tool() -> PathBuf {
    if let Some(tool) = std::env::var_os("LRELEASE") {
        return PathBuf::from(tool);
    }
    let host_bins = Command::new("qmake6")
        .args(["-query", "QT_HOST_BINS"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| PathBuf::from(String::from_utf8_lossy(&o.stdout).trim()).join("lrelease"))
        .filter(|p| p.exists());
    host_bins.unwrap_or_else(|| {
        let on_path = |name: &str| {
            std::env::var_os("PATH")
                .map(|path| std::env::split_paths(&path).any(|dir| dir.join(name).exists()))
                .unwrap_or(false)
        };
        PathBuf::from(if on_path("lrelease-qt6") {
            "lrelease-qt6"
        } else {
            "lrelease"
        })
    })
}

fn lrelease(source: &Path, qm: &Path) {
    let tool = lrelease_tool();
    let status = Command::new(&tool)
        .arg("-silent")
        .arg(source)
        .arg("-qm")
        .arg(qm)
        .status()
        .unwrap_or_else(|err| {
            panic!(
                "{} (Qt's lrelease, from qt6-linguist; or set LRELEASE) can't run: {err}",
                tool.display()
            )
        });
    assert!(status.success(), "lrelease failed on {}", source.display());
}

/// The template with every message translated by [`pseudo`], for the
/// English plural rules; `rtl` also lays the interface out right to left.
fn pseudo_catalog(template: &str, rtl: bool) -> String {
    let mut out = String::with_capacity(template.len() * 2);
    let mut rest = template;
    while let Some(start) = rest.find("<message") {
        let end = rest[start..].find("</message>").expect("a closed message") + start;
        out.push_str(&rest[..start]);
        let message = &rest[start..end];
        let source = between(message, "<source>", "</source>").expect("a message has a source");
        let text = escape(&pseudo(&unescape(source)));
        let translation = if message.contains("numerus=\"yes\"") {
            format!("<translation><numerusform>{text}</numerusform><numerusform>{text}</numerusform></translation>")
        } else {
            format!("<translation>{text}</translation>")
        };
        let open = message.find("<translation").expect("a translation");
        out.push_str(&message[..open]);
        out.push_str(&translation);
        out.push('\n');
        rest = &rest[end..];
    }
    out.push_str(rest);
    let mut out = out.replacen(
        "<TS version=\"2.1\">",
        "<TS version=\"2.1\" language=\"en\">",
        1,
    );
    if rtl {
        let direction = "<context>\n    <name>QGuiApplication</name>\n    <message>\n        \
            <source>QT_LAYOUT_DIRECTION</source>\n        <translation>RTL</translation>\n    \
            </message>\n</context>\n</TS>";
        out = out.replacen("</TS>", direction, 1);
    }
    out
}

fn between<'a>(text: &'a str, open: &str, close: &str) -> Option<&'a str> {
    let start = text.find(open)? + open.len();
    let end = text[start..].find(close)? + start;
    Some(&text[start..end])
}

fn unescape(text: &str) -> String {
    text.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
}

fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

/// A pseudo-translation: the letters accented, a third longer, between
/// ⟦ and ⟧, so an untranslated string shows plainly and long translations
/// are tried. `%1`, `%n` and `%L1` stay as they are.
fn pseudo(text: &str) -> String {
    const PLAIN: &str = "aceinorsuyACEINORSUY";
    const ACCENTED: [char; 20] = [
        'á', 'ç', 'é', 'í', 'ñ', 'ó', 'ř', 'š', 'ú', 'ý', 'Á', 'Ç', 'É', 'Í', 'Ñ', 'Ó', 'Ř', 'Š',
        'Ú', 'Ý',
    ];
    let mut out = String::from("⟦");
    let mut chars = text.chars().peekable();
    let mut letters: usize = 0;
    while let Some(c) = chars.next() {
        if c == '%' {
            out.push(c);
            if chars.peek() == Some(&'L') {
                out.push(chars.next().unwrap_or('L'));
            }
            while let Some(&d) = chars.peek() {
                if d.is_ascii_digit() || d == 'n' {
                    out.push(d);
                    chars.next();
                } else {
                    break;
                }
            }
            continue;
        }
        if c.is_alphabetic() {
            letters += 1;
        }
        out.push(PLAIN.find(c).map_or(c, |i| ACCENTED[i]));
    }
    for _ in 0..letters.div_ceil(3) {
        out.push('·');
    }
    out.push('⟧');
    out
}
