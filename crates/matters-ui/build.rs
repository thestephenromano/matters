use cxx_qt_build::{CxxQtBuilder, QmlModule};

fn main() {
    let mut builder = CxxQtBuilder::new_qml_module(
        QmlModule::new("matters")
            // TaskListModel derives from QAbstractListModel, which QML tooling
            // resolves from this module.
            .depend("QtQml.Models")
            .qml_files([
                "qml/Main.qml",
                "qml/MainView.qml",
                "qml/Icon.qml",
                "qml/ChecklistEditor.qml",
            ]),
    )
    .file("src/bridge.rs")
    .qt_module("Gui")
    .qt_module("Quick")
    .qt_module("QuickControls2");

    println!("cargo:rerun-if-changed=cpp/app_style.cpp");
    // SAFETY: only adds a source file; no compiler flags are changed.
    builder = unsafe {
        builder.cc_builder(|cc| {
            cc.file("cpp/app_style.cpp");
        })
    };

    if std::env::var_os("CARGO_FEATURE_UI_TESTS").is_some() {
        println!("cargo:rerun-if-changed=cpp/quick_test.cpp");
        builder = builder.qt_module("QuickTest");
        // SAFETY: only adds a source file; no compiler flags are changed.
        builder = unsafe {
            builder.cc_builder(|cc| {
                cc.file("cpp/quick_test.cpp");
            })
        };
    }

    builder.build();
}
