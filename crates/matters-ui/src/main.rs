use cxx_qt_lib::{QGuiApplication, QQmlApplicationEngine, QUrl};
use matters_core::Database;

fn main() {
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
    matters_ui::init_qml();

    let mut app = QGuiApplication::new();
    let mut engine = QQmlApplicationEngine::new();

    if let Some(engine) = engine.as_mut() {
        engine.load(&QUrl::from("qrc:/qt/qml/matters/qml/Main.qml"));
    }

    if let Some(app) = app.as_mut() {
        app.exec();
    }
}
