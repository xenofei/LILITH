fn main() {
    // The Qt bridge is generated only with the `qt` feature. Without it this does nothing, so the
    // workspace builds and tests with no Qt installed (P4a-R1).
    #[cfg(feature = "qt")]
    cxx_qt_build::CxxQtBuilder::new_qml_module(cxx_qt_build::QmlModule::new("Lilith.Core"))
        .file("src/qt/mod.rs")
        .file("src/qt/core_object.rs")
        .file("src/qt/layout.rs")
        .cpp_file("src/qt/core_helpers.cpp")
        .file("src/qt/json_list_model.rs")
        .file("src/qt/preview_feed.rs")
        .cpp_file("src/qt/core_link.cpp")
        .cpp_file("src/qt/preview_sink.cpp")
        .build();
}
