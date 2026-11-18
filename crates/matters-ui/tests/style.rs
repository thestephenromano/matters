//! The control style survives the desktop's Qt platform theme.
//!
//! Each case runs this binary again as a child (`--child`), which starts a
//! Qt application the way `src/main.rs` does, applies the style with
//! `matters_ui::apply_app_style`, and prints the style before and after.
//!
//! The COSMIC cases need COSMIC's Qt platform theme (cutecosmic), which sets
//! `org.kde.desktop` when the application starts. Where it is not installed
//! or does not act, they are reported as not run, unless
//! `MATTERS_REQUIRE_COSMIC_THEME=1`, which makes that a failure.

use std::process::Command;

fn child() {
    std::env::set_var("QT_QPA_PLATFORM", "offscreen");
    let explicit = std::env::var_os("QT_QUICK_CONTROLS_STYLE");
    let _app = cxx_qt_lib::QGuiApplication::new();
    let before = matters_ui::quick_style_name();
    matters_ui::apply_app_style(explicit.as_deref());
    println!("before={before} after={}", matters_ui::quick_style_name());
}

/// Run a child with `theme` as the platform theme ("" for none) and
/// `explicit` as `QT_QUICK_CONTROLS_STYLE`; return (before, after).
fn run(theme: &str, explicit: Option<&str>) -> (String, String) {
    let mut cmd = Command::new(std::env::current_exe().expect("test binary path"));
    cmd.arg("--child")
        .env_remove("QT_QUICK_CONTROLS_STYLE")
        .env_remove("QT_QPA_PLATFORMTHEME")
        .env("QT_FORCE_STDERR_LOGGING", "1");
    if !theme.is_empty() {
        cmd.env("QT_QPA_PLATFORMTHEME", theme);
    }
    if let Some(style) = explicit {
        cmd.env("QT_QUICK_CONTROLS_STYLE", style);
    }
    let out = cmd.output().expect("start the child");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "child failed ({}):\n{stdout}\n{}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
    let line = stdout
        .lines()
        .find(|l| l.starts_with("before="))
        .unwrap_or_else(|| panic!("child printed no result:\n{stdout}"));
    let (before, after) = line.split_once(" after=").expect("before=… after=…");
    (before.trim_start_matches("before=").to_string(), after.to_string())
}

fn main() {
    if std::env::args().nth(1).as_deref() == Some("--child") {
        child();
        return;
    }

    let mut failures = Vec::new();
    let mut check = |case: &str, got: &str, want: &str| {
        let ok = got == want;
        println!("{} {case}: {got}", if ok { "ok  " } else { "FAIL" });
        if !ok {
            failures.push(format!("{case}: got {got}, want {want}"));
        }
    };

    check("no theme, no style named", &run("", None).1, "Fusion");
    check("no theme, Material named", &run("", Some("Material")).1, "Material");

    let (cosmic_default, after) = run("cosmic", None);
    if cosmic_default == "org.kde.desktop" {
        check("COSMIC theme, no style named", &after, "Fusion");
        check("COSMIC theme, Fusion named", &run("cosmic", Some("Fusion")).1, "Fusion");
        check(
            "COSMIC theme, org.kde.desktop named",
            &run("cosmic", Some("org.kde.desktop")).1,
            "org.kde.desktop",
        );
    } else if std::env::var_os("MATTERS_REQUIRE_COSMIC_THEME").is_some_and(|v| v == "1") {
        failures.push(format!(
            "COSMIC theme required but not active (style at start: {cosmic_default:?})"
        ));
    } else {
        println!("not run: COSMIC cases (theme not active here; style at start: {cosmic_default:?})");
    }

    assert!(failures.is_empty(), "style cases failed:\n{}", failures.join("\n"));
}
