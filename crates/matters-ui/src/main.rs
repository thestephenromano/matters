use cxx_qt_lib::{QGuiApplication, QQmlApplicationEngine, QUrl};
use matters_core::calendar::{CalendarSource, FakeCalendarSource};
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
        Err(err) => matters_ui::install_settings(None, Some(err.to_string())),
    }
    // Reminders (#60) as desktop notifications.
    matters_ui::install_notifier(Box::new(matters_ui::notify::DesktopNotifier));
    // The desktop's accent colour and contrast preference (#76), followed
    // from the XDG desktop portal on a worker thread.
    matters_ui::appearance::start_portal();
    matters_ui::init_qml();

    // The desktop entry the window belongs to (#73).
    matters_ui::set_desktop_identity();
    let mut app = QGuiApplication::new();
    // After the application (and the desktop's platform theme) has started,
    // before any QML loads.
    matters_ui::apply_app_style(explicit_style.as_deref());
    // The interface in the user's language, when Matters has it (#75).
    matters_ui::i18n::register_catalogs();
    matters_ui::i18n::install_system_translation();
    // Calendar events (#62), read on a worker thread. Without Evolution
    // Data Server there are none, and Preferences says why (in the user's
    // language, so once the translation is in).
    let calendars: Box<dyn CalendarSource> = match matters_ui::eds::EdsSource::load() {
        Ok(source) => Box::new(source),
        Err(reason) => Box::new(FakeCalendarSource::unavailable(reason.0)),
    };
    matters_ui::install_calendar_source(calendars, true);
    let mut engine = QQmlApplicationEngine::new();

    if let Some(engine) = engine.as_mut() {
        engine.load(&QUrl::from("qrc:/qt/qml/matters/qml/Main.qml"));
    }

    if let Some(app) = app.as_mut() {
        app.exec();
    }
}
