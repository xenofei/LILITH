# Lilith.Web: the suite's web view and its WebEngine profile, as its own QML plugin DLL. It is the
# only code that links or imports QtWebEngine (APP-137, IMG-126): the exe and the shim never do,
# and the module loads only when a Loader first creates WebPanel. It stays dynamic on purpose,
# and deploys as Lilith\Web\ (qmldir and plugin DLL).
find_package(Qt6 REQUIRED COMPONENTS WebEngineQuick)

qt_add_qml_module(lil-web
    URI Lilith.Web
    VERSION 1.0
    PLUGIN_TARGET lil-web
    QML_FILES qml/SuiteWebProfile.qml qml/WebPanel.qml)
# CMAKE_RUNTIME_OUTPUT_DIRECTORY would put the DLL in bin\; it belongs beside its qmldir.
set_target_properties(lil-web PROPERTIES
    LIBRARY_OUTPUT_DIRECTORY ${QT_QML_OUTPUT_DIRECTORY}/Lilith/Web
    RUNTIME_OUTPUT_DIRECTORY ${QT_QML_OUTPUT_DIRECTORY}/Lilith/Web)
target_link_libraries(lil-web PRIVATE Qt6::Quick Qt6::WebEngineQuick)
# Task 25: the version alone, for LilShim.componentVersions; the shim links nothing of WebEngine.
target_compile_definitions(lil-qt-shim-qml PRIVATE LIL_WEBENGINE_VERSION="${Qt6WebEngineQuick_VERSION}")
