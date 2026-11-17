use cxx_qt_build::{CxxQtBuilder, QmlModule};

fn main() {
    CxxQtBuilder::new_qml_module(
        QmlModule::new("matters")
            .qml_files([
                "qml/Main.qml",
            ])
    )
    .file("src/bridge.rs")
    .qt_module("Gui")
    .qt_module("Quick")
    .qt_module("QuickControls2")
    .build();
}
