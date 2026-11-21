//! Qt 6 / QML frontend for Matters.
//!
//! The binary (`src/main.rs`) opens the database and shows `qml/Main.qml`;
//! this library holds everything else so tests can link against it.

#[cfg(feature = "ui-tests")]
pub mod accessibility_probe;
pub mod appearance;
pub mod bridge;
pub mod calendar;
pub mod eds;
#[cfg(feature = "ui-tests")]
pub mod fake_desktop;
mod motion;
pub mod notify;
#[cfg(test)]
mod qml_lint;
pub mod reminders;

pub use bridge::{install_database, install_settings};
pub use calendar::install_calendar_source;
pub use reminders::install_notifier;

/// The Qt Quick Controls style Matters uses unless `QT_QUICK_CONTROLS_STYLE`
/// names another: Fusion, a built-in style drawn entirely from the palette,
/// which Matters sets from its own colours. Desktop styles (for example `org.kde.desktop`, which KDE Plasma
/// and COSMIC load) draw controls natively, with their own palette and
/// sizes, beside Matters' own drawing.
pub const APP_STYLE: &str = "Fusion";

/// The style Matters sets: the one the user named in `QT_QUICK_CONTROLS_STYLE`
/// (`explicit`), or else `APP_STYLE`.
pub fn style_to_set(explicit: Option<&std::ffi::OsStr>) -> String {
    match explicit.and_then(|name| name.to_str()) {
        Some(name) if !name.is_empty() => name.to_string(),
        _ => APP_STYLE.to_string(),
    }
}

mod quick_style {
    use std::ffi::{c_char, c_int, CStr, CString};

    extern "C" {
        // cpp/app_style.cpp
        fn matters_set_quick_style(name: *const c_char);
        fn matters_quick_style_name(out: *mut c_char, capacity: c_int);
    }

    pub fn set(name: &str) {
        let name = CString::new(name).expect("style name without NUL");
        // SAFETY: `name` is a valid NUL-terminated string for the call.
        unsafe { matters_set_quick_style(name.as_ptr()) }
    }

    pub fn name() -> String {
        let mut buf = [0 as c_char; 128];
        // SAFETY: the buffer holds `capacity` bytes; the callee writes at
        // most that many, NUL included.
        unsafe { matters_quick_style_name(buf.as_mut_ptr(), buf.len() as c_int) };
        // SAFETY: the callee always NUL-terminates within the buffer.
        unsafe { CStr::from_ptr(buf.as_ptr()) }
            .to_string_lossy()
            .into_owned()
    }
}

/// Apply the control style (`style_to_set`). Call after creating the
/// application and before loading QML: a desktop's Qt platform theme sets a
/// style while the application starts (COSMIC's replaces anything but Basic,
/// even a style named in `QT_QUICK_CONTROLS_STYLE`), so only a choice made
/// afterwards holds.
pub fn apply_app_style(explicit: Option<&std::ffi::OsStr>) {
    quick_style::set(&style_to_set(explicit));
}

/// The Qt Quick Controls style in effect (for tests).
pub fn quick_style_name() -> String {
    quick_style::name()
}

#[cfg(test)]
mod style_tests {
    use super::*;
    use std::ffi::OsStr;

    #[test]
    fn matters_uses_fusion_unless_a_style_is_named() {
        assert_eq!(style_to_set(None), "Fusion");
        assert_eq!(style_to_set(Some(OsStr::new(""))), "Fusion");
        assert_eq!(style_to_set(Some(OsStr::new("Basic"))), "Basic");
        assert_eq!(
            style_to_set(Some(OsStr::new("org.kde.desktop"))),
            "org.kde.desktop"
        );
    }
}

/// Matters' application ID (#73): it names the desktop entry, the icon, the
/// AppStream metainfo and the Flatpak (`packaging/`).
pub const APP_ID: &str = "io.github.thestephenromano.Matters";

/// Tell the desktop which entry the window belongs to ([`APP_ID`]), so it
/// shows Matters' icon and name for it.
pub fn set_desktop_identity() {
    extern "C" {
        // cpp/app_identity.cpp
        fn matters_set_desktop_file_name(name: *const std::ffi::c_char);
    }
    let name = std::ffi::CString::new(APP_ID).expect("application ID without NUL");
    // SAFETY: `name` is a valid NUL-terminated string for the call; Qt
    // copies it.
    unsafe { matters_set_desktop_file_name(name.as_ptr()) }
}

#[cfg(test)]
mod identity_tests {
    use super::APP_ID;

    fn packaging(path: &str) -> String {
        let path = format!("{}/../../packaging/{path}", env!("CARGO_MANIFEST_DIR"));
        std::fs::read_to_string(&path).unwrap_or_else(|err| panic!("{path}: {err}"))
    }

    #[test]
    fn the_packaging_names_the_app_by_its_id() {
        let desktop = packaging(&format!("linux/{APP_ID}.desktop"));
        assert!(desktop.lines().any(|l| l == format!("Icon={APP_ID}")));
        let metainfo = packaging(&format!("linux/{APP_ID}.metainfo.xml"));
        assert!(metainfo.contains(&format!("<id>{APP_ID}</id>")));
        assert!(metainfo.contains(&format!(
            "<launchable type=\"desktop-id\">{APP_ID}.desktop</launchable>"
        )));
        let manifest = packaging(&format!("flatpak/{APP_ID}.yml"));
        assert!(manifest.lines().any(|l| l == format!("app-id: {APP_ID}")));
        packaging(&format!("linux/icons/hicolor/scalable/apps/{APP_ID}.svg"));
        packaging(&format!(
            "linux/icons/hicolor/symbolic/apps/{APP_ID}-symbolic.svg"
        ));
    }
}

/// Register cxx-qt and the `matters` QML module. Call once, before creating
/// any QML engine.
pub fn init_qml() {
    cxx_qt::init_crate!(cxx_qt);
    cxx_qt::init_qml_module!("matters");
}

/// Qt Quick Test runner for the QML interaction tests (`tests/ui.rs`).
#[cfg(feature = "ui-tests")]
pub mod quick_test {
    use std::ffi::{c_char, c_int, CString};

    extern "C" {
        // cpp/quick_test.cpp
        fn matters_quick_test_main(
            argc: c_int,
            argv: *mut *mut c_char,
            source_dir: *const c_char,
        ) -> c_int;
    }

    /// Run every `tst_*.qml` in `source_dir`, forwarding `args` (program name
    /// first) to Qt Quick Test. Returns the process exit code.
    pub fn run(args: impl IntoIterator<Item = String>, source_dir: &str) -> i32 {
        let args: Vec<CString> = args
            .into_iter()
            .map(|a| CString::new(a).expect("argument without NUL"))
            .collect();
        let mut argv: Vec<*mut c_char> = args.iter().map(|a| a.as_ptr().cast_mut()).collect();
        argv.push(std::ptr::null_mut());
        let source_dir = CString::new(source_dir).expect("path without NUL");
        let argc = c_int::try_from(args.len()).expect("argument count fits in int");
        // SAFETY: argv holds `argc` valid NUL-terminated strings plus a null
        // terminator, all outliving the call.
        unsafe { matters_quick_test_main(argc, argv.as_mut_ptr(), source_dir.as_ptr()) }
    }
}
