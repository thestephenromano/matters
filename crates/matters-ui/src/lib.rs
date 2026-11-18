//! Qt 6 / QML frontend for Matters.
//!
//! The binary (`src/main.rs`) opens the database and shows `qml/Main.qml`;
//! this library holds everything else so tests can link against it.

pub mod bridge;
#[cfg(test)]
mod qml_lint;

pub use bridge::install_database;

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
        fn matters_quick_test_main(argc: c_int, argv: *mut *mut c_char, source_dir: *const c_char) -> c_int;
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
