//! QML interaction tests: drive the real `MainView` with simulated mouse and
//! keyboard input through Qt Quick Test (`tests/qml/tst_*.qml`).
//!
//! Runs on the main thread (`harness = false`), against an in-memory database,
//! on Qt's offscreen platform unless `QT_QPA_PLATFORM` is already set.
//! Extra arguments go to Qt Quick Test, e.g. `-- -functions` or `-- tst_x`.

fn main() {
    if std::env::var_os("QT_QPA_PLATFORM").is_none() {
        std::env::set_var("QT_QPA_PLATFORM", "offscreen");
    }
    let db = matters_core::Database::open_in_memory().expect("open in-memory database");
    matters_ui::install_database(db);
    matters_ui::init_qml();

    let source_dir = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/qml");
    std::process::exit(matters_ui::quick_test::run(std::env::args(), source_dir));
}
