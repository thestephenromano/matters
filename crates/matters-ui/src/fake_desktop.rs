//! A desktop whose accent colour and contrast preference the QML tests set
//! (#76), in place of the XDG desktop portal; the `ui-tests` feature only.
//! `FakeDesktop.set("#rrggbb", highContrast)` publishes them as the portal
//! reader would (`crate::appearance::publish`); an empty accent is none.

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
        type FakeDesktop = super::FakeDesktopRust;

        /// Publish an accent (`#rrggbb`, or empty for none) and a contrast
        /// preference.
        #[qinvokable]
        fn set(self: &FakeDesktop, accent: &QString, high_contrast: bool);
    }
}

use cxx_qt_lib::QString;

#[derive(Default)]
pub struct FakeDesktopRust;

impl qobject::FakeDesktop {
    pub fn set(&self, accent: &QString, high_contrast: bool) {
        crate::appearance::publish(crate::appearance::Appearance {
            accent: crate::appearance::Rgb::from_hex(&String::from(accent)),
            high_contrast,
        });
    }
}
