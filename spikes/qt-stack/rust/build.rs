use cxx_qt_build::{CxxQtBuilder, QmlModule};

fn main() {
    // QML module "SpikeBridge" with the SpikeCounter element; CMake imports it with
    // cxx_qt_import_qml_module.
    CxxQtBuilder::new_qml_module(QmlModule::new("SpikeBridge"))
        .qt_module("Qml")
        .files(["src/lib.rs"])
        .build();
}
