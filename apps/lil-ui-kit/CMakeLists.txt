# Lilith.UiKit: every QML file of the apps but the web view (module Lilith.Web). A static module
# (backing library and plugin), linked into lil-image.exe and the test executable, so nothing of
# it is deployed (Ruling Q8). Later tasks append their files to QML_FILES.
set_source_files_properties(qml/Theme.qml PROPERTIES QT_QML_SINGLETON_TYPE TRUE)

qt_add_qml_module(lil-ui-kit
    URI Lilith.UiKit
    VERSION 1.0
    STATIC
    QML_FILES
        qml/Theme.qml
        qml/shell/ShellWindow.qml
        qml/core/CoreCall.js
        qml/core/OwnerWatch.qml)
target_link_libraries(lil-ui-kit PRIVATE Qt6::Quick)

add_subdirectory(tests)
