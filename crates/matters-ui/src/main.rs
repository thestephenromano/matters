use cxx_qt_lib::{QGuiApplication, QQmlApplicationEngine, QUrl};
use matters_core::{Database, SettingsFile};

fn main() {
    // Read before the application starts: the user's explicit style choice.
    let explicit_style = std::env::var_os("QT_QUICK_CONTROLS_STYLE");

    // Open the user's database before the UI exists. If it cannot be opened,
    // refuse to start rather than run against a store that would not persist.
    let db = match Database::open_default() {
        Ok(db) => db,
        Err(err) => {
            let location = Database::default_path()
                .map(|p| p.display().to_string())
                .unwrap_or_else(|_| "<unknown>".to_string());
            eprintln!("matters: cannot open database at {location}: {err}");
            std::process::exit(1);
        }
    };
    matters_ui::install_database(db);
    // This device's settings (#72). An unreadable file gives the defaults,
    // and the app says so once it is up.
    match SettingsFile::in_config_dir() {
        Ok(file) => matters_ui::install_settings(Some(file), None),
        Err(err) => {
            matters_ui::install_settings(None, Some(format!("Your settings can't be kept: {err}")))
        }
    }
    matters_ui::init_qml();

    let mut app = QGuiApplication::new();
    // After the application (and the desktop's platform theme) has started,
    // before any QML loads.
    matters_ui::apply_app_style(explicit_style.as_deref());
    let mut engine = QQmlApplicationEngine::new();

    if let Some(engine) = engine.as_mut() {
        engine.load(&QUrl::from("qrc:/qt/qml/matters/qml/Main.qml"));
    }

    if let Some(app) = app.as_mut() {
        app.exec();
    }
}
