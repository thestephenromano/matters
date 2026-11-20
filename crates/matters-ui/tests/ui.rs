//! QML interaction tests: drive the real `MainView` with simulated mouse and
//! keyboard input through Qt Quick Test (`tests/qml/tst_*.qml`).
//!
//! Runs on the main thread (`harness = false`), against an in-memory database,
//! on Qt's offscreen platform. Desktop sessions often export
//! `QT_QPA_PLATFORM` (e.g. `wayland;xcb`), which would put test windows on the
//! user's screen and make focus depend on the compositor, so it is overridden.
//! Set `MATTERS_UI_TEST_PLATFORM` (e.g. `wayland`) to watch the tests run.
//!
//! The desktop's look is not inherited either. The tests use the style the
//! app sets (`matters_ui::APP_STYLE`, Fusion) unless
//! `MATTERS_UI_TEST_STYLE` names another (e.g. `Basic`, or
//! `org.kde.desktop`, which KDE Plasma and COSMIC load), and the desktop
//! platform theme is ignored.
//! Extra arguments go to Qt Quick Test, e.g. `-- -functions` or `-- tst_x`.

fn main() {
    let platform = std::env::var("MATTERS_UI_TEST_PLATFORM").unwrap_or_else(|_| "offscreen".into());
    std::env::set_var("QT_QPA_PLATFORM", platform);
    match std::env::var("MATTERS_UI_TEST_STYLE") {
        Ok(style) => std::env::set_var("QT_QUICK_CONTROLS_STYLE", style),
        // No platform theme runs here (it is removed below), so the
        // variable is enough to give the tests the app's style.
        Err(_) => std::env::set_var("QT_QUICK_CONTROLS_STYLE", matters_ui::APP_STYLE),
    }
    std::env::remove_var("QT_QPA_PLATFORMTHEME");
    // Animations off by default, so tests don't race them; set
    // MATTERS_UI_TEST_MOTION=1 to run with motion (ci/check.sh does both).
    let motion = std::env::var_os("MATTERS_UI_TEST_MOTION").is_some_and(|v| v == "1");
    std::env::set_var("MATTERS_REDUCE_MOTION", if motion { "0" } else { "1" });
    let db = matters_core::Database::open_in_memory().expect("open in-memory database");
    matters_ui::install_database(db);
    // Settings live in memory only: the tests never read or write the
    // user's file.
    matters_ui::install_settings(None, None);
    matters_ui::init_qml();

    let source_dir = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/qml");
    std::process::exit(matters_ui::quick_test::run(std::env::args(), source_dir));
}
