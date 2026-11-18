//! QML interaction tests: drive the real `MainView` with simulated mouse and
//! keyboard input through Qt Quick Test (`tests/qml/tst_*.qml`).
//!
//! Runs on the main thread (`harness = false`), against an in-memory database,
//! on Qt's offscreen platform. Desktop sessions often export
//! `QT_QPA_PLATFORM` (e.g. `wayland;xcb`), which would put test windows on the
//! user's screen and make focus depend on the compositor, so it is overridden.
//! Set `MATTERS_UI_TEST_PLATFORM` (e.g. `wayland`) to watch the tests run.
//!
//! The desktop's look is not inherited either: the Qt Quick Controls style
//! changes control sizes and dialog transitions, so tests that pass under one
//! desktop theme could fail on another machine or in CI. The tests use Qt's
//! built-in Basic style unless `MATTERS_UI_TEST_STYLE` names another (e.g.
//! `Fusion`), and the desktop platform theme is ignored.
//! Extra arguments go to Qt Quick Test, e.g. `-- -functions` or `-- tst_x`.

fn main() {
    let platform = std::env::var("MATTERS_UI_TEST_PLATFORM").unwrap_or_else(|_| "offscreen".into());
    std::env::set_var("QT_QPA_PLATFORM", platform);
    let style = std::env::var("MATTERS_UI_TEST_STYLE").unwrap_or_else(|_| "Basic".into());
    std::env::set_var("QT_QUICK_CONTROLS_STYLE", style);
    std::env::remove_var("QT_QPA_PLATFORMTHEME");
    let db = matters_core::Database::open_in_memory().expect("open in-memory database");
    matters_ui::install_database(db);
    matters_ui::init_qml();

    let source_dir = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/qml");
    std::process::exit(matters_ui::quick_test::run(std::env::args(), source_dir));
}
