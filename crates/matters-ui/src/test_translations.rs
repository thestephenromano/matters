//! Pseudo-translations for the QML tests (#75), the `ui-tests` feature
//! only. `TestTranslations.use("pseudo")` translates the open windows with
//! the pseudo catalog `build.rs` makes from the template: every string
//! marked ⟦…⟧ and a third longer; `"pseudo_rtl"` also lays them out right
//! to left; `""` goes back to the untranslated strings.

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
        type TestTranslations = super::TestTranslationsRust;

        /// Translate with catalog `name` (`""`: none). False when there is
        /// no such catalog.
        #[qinvokable]
        fn use_catalog(self: &TestTranslations, name: &QString) -> bool;
    }
}

use cxx_qt_lib::QString;

#[derive(Default)]
pub struct TestTranslationsRust;

impl qobject::TestTranslations {
    pub fn use_catalog(&self, name: &QString) -> bool {
        crate::i18n::use_catalog(&String::from(name))
    }
}
