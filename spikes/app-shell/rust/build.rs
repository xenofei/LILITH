use cxx_qt_build::{CxxQtBuilder, QmlModule};

fn main() {
    // QML module "SpikeBridge": GridModel and SpikeCounter are QML elements; JobsFeed is created
    // in C++ (main.cpp) so its frame signal can be connected to the preview hub without QML.
    CxxQtBuilder::new_qml_module(QmlModule::new("SpikeBridge"))
        .qt_module("Qml")
        .files(["src/counter.rs", "src/grid.rs", "src/feed.rs"])
        .build();
}
