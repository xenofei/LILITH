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
        qml/core/OwnerWatch.qml
        qml/core/PreviewImage.qml)
target_link_libraries(lil-ui-kit PRIVATE Qt6::Quick)
# Task 18: the base controls (APP-156), the shell's docks and panel placeholders, and A11yWalk.
qt_target_qml_sources(lil-ui-kit
    QML_FILES
        qml/controls/LButton.qml
        qml/controls/LIconButton.qml
        qml/controls/LToggle.qml
        qml/controls/LTextField.qml
        qml/controls/LBanner.qml
        qml/controls/LBadge.qml
        qml/controls/LGauge.qml
        qml/controls/LCard.qml
        qml/shell/DockedPanel.qml
        qml/shell/DockTitleBar.qml
        qml/shell/DockTabBar.qml
        qml/shell/Panels.js
        qml/shell/PlaceholderPanel.qml
        qml/a11y/A11yWalk.qml)

# Task 24: the Usage & spend panel.
qt_target_qml_sources(lil-ui-kit
    QML_FILES
        qml/panels/usage/UsagePanel.qml
        qml/panels/usage/SpendBreakdown.qml
        qml/panels/usage/CapGauges.qml
        qml/panels/usage/AccountsList.qml
        qml/panels/usage/ClaudeCodeSection.qml
        qml/panels/usage/UsageText.js)

add_subdirectory(tests)
