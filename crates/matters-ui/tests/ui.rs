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

use matters_core::calendar::{Calendar, Event, FakeCalendarSource};

/// Two calendars, with events today and in the coming days.
fn fake_calendar() -> FakeCalendarSource {
    use jiff::ToSpan;
    let today = matters_core::local_today();
    let day = |n: i64| today.checked_add(n.days()).expect("a near day");
    let calendar = |id: &str, name: &str, color: &str| Calendar {
        id: id.into(),
        name: name.into(),
        color: Some(color.into()),
    };
    let timed = |id: &str, calendar: &str, title: &str, on: i64, from: i8, to: i8| Event {
        id: id.into(),
        calendar_id: calendar.into(),
        title: title.into(),
        start: day(on).at(from, 0, 0, 0),
        end: day(on).at(to, 15, 0, 0),
        all_day: false,
    };
    let all_day = |id: &str, calendar: &str, title: &str, first: i64, last: i64| Event {
        id: id.into(),
        calendar_id: calendar.into(),
        title: title.into(),
        start: day(first).at(0, 0, 0, 0),
        end: day(last + 1).at(0, 0, 0, 0),
        all_day: true,
    };
    FakeCalendarSource {
        calendars: vec![
            calendar("work", "Work", "#3584e4"),
            calendar("home", "Home", "#e66100"),
        ],
        events: vec![
            timed("standup", "work", "Team standup", 0, 9, 9),
            all_day("holiday", "home", "Public holiday", 0, 0),
            timed("dentist", "home", "Dentist", 2, 15, 16),
            all_day("conference", "work", "Conference", 4, 5),
        ],
        unavailable: None,
    }
}

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
    // A fake calendar, read at once rather than on a worker (#62). The tests
    // start with events hidden (`init()`), and those about events show them.
    matters_ui::install_calendar_source(Box::new(fake_calendar()), false);
    // Reminders are kept, never shown on the desktop (#60).
    matters_ui::install_notifier(Box::new(matters_ui::notify::RecordingNotifier::default()));
    // The pseudo-translations the translation tests switch to (#75).
    matters_ui::i18n::register_catalogs();
    matters_ui::init_qml();

    let source_dir = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/qml");
    std::process::exit(matters_ui::quick_test::run(std::env::args(), source_dir));
}
