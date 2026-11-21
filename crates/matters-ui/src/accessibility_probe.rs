//! What a screen reader is told about the window (#74), for the QML tests
//! only (the `ui-tests` feature).
//!
//! `AccessibilityProbe.report()` walks Qt's accessibility tree, the one the
//! AT-SPI bridge gives Orca, from every visible window, and returns each
//! element that isn't hidden as JSON: `{elements: [{role, name, description, focusable, focused,
//! disabled, objectName, type, within, inside}], mouseOnly: [within]}`.
//! `role` and `inside` (the role of the nearest actionable element around
//! it) are `QAccessible::Role` numbers;
//! `within` is the nearest object name around the element, to say where
//! it is. Each report turns accessibility on for the walk, as a screen
//! reader starting would, so the controls fill in their roles and names,
//! and back off after it.

#[cxx_qt::bridge]
pub mod qobject {
    unsafe extern "C++" {
        include!("cxx-qt-lib/qstring.h");
        type QString = cxx_qt_lib::QString;
    }

    #[auto_cxx_name]
    extern "RustQt" {
        #[qobject]
        #[qml_element]
        type AccessibilityProbe = super::AccessibilityProbeRust;

        /// The visible accessibility tree, as JSON.
        #[qinvokable]
        fn report(self: &AccessibilityProbe) -> QString;
    }
}

use cxx_qt_lib::QString;
use std::ffi::{c_char, CStr};

extern "C" {
    // cpp/accessibility_probe.cpp
    fn matters_accessibility_report() -> *mut c_char;
    fn matters_accessibility_free(report: *mut c_char);
}

#[derive(Default)]
pub struct AccessibilityProbeRust;

impl qobject::AccessibilityProbe {
    pub fn report(&self) -> QString {
        // SAFETY: the callee returns a NUL-terminated string it allocated,
        // which is read once and handed back to be freed.
        unsafe {
            let report = matters_accessibility_report();
            let text = CStr::from_ptr(report).to_string_lossy().into_owned();
            matters_accessibility_free(report);
            QString::from(text.as_str())
        }
    }
}
