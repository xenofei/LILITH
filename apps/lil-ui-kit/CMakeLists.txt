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
# Task 19: the Opened card (P4a-R21), held by the shell until Task 26 moves it into Home.
qt_target_qml_sources(lil-ui-kit
    QML_FILES
        qml/panels/home/OpenedCard.qml)

# Task 24: the Usage & spend panel.
qt_target_qml_sources(lil-ui-kit
    QML_FILES
        qml/panels/usage/UsagePanel.qml
        qml/panels/usage/SpendBreakdown.qml
        qml/panels/usage/CapGauges.qml
        qml/panels/usage/AccountsList.qml
        qml/panels/usage/ClaudeCodeSection.qml
        qml/panels/usage/UsageText.js)

# Task 23: the Hardware panel.
qt_target_qml_sources(lil-ui-kit
    QML_FILES
        qml/panels/hardware/HardwarePanel.qml
        qml/panels/hardware/ChartTile.qml
        qml/panels/hardware/GpuUsers.qml
        qml/panels/hardware/EnergyTable.qml
        qml/panels/hardware/AlertsStrip.qml
        qml/panels/hardware/HardwareFormat.js)

# Task 22: the Jobs panel and the cost and time chip.
qt_target_qml_sources(lil-ui-kit
    QML_FILES
        qml/controls/CostChip.qml
        qml/panels/jobs/JobsPanel.qml
        qml/panels/jobs/JobRow.qml
        qml/panels/jobs/LaneHeader.qml
        qml/panels/jobs/QueueControls.qml)

# Task 20: the top bar, its switchers and the shell's connection and no-workspace banners.
qt_target_qml_sources(lil-ui-kit
    QML_FILES
        qml/shell/TopBar.qml
        qml/shell/TopBarJobs.qml
        qml/shell/StatusIndicator.qml
        qml/shell/SwitcherPopup.qml
        qml/shell/ProjectSwitcher.qml
        qml/shell/WorkspaceSwitcher.qml
        qml/shell/ConnectionBanner.qml
        qml/shell/NoWorkspaceBanner.qml
        qml/core/CoreGate.js
        qml/panels/jobs/PausedText.js)

# Task 25: the Diagnostics and Backups panels.
qt_target_qml_sources(lil-ui-kit
    QML_FILES
        qml/panels/diagnostics/DiagnosticsPanel.qml
        qml/panels/diagnostics/VersionsCard.qml
        qml/panels/diagnostics/EnginesCard.qml
        qml/panels/diagnostics/ProvidersCard.qml
        qml/panels/diagnostics/QueueSummary.qml
        qml/panels/diagnostics/RecentErrors.qml
        qml/panels/diagnostics/EventLog.qml
        qml/panels/diagnostics/TelemetrySummary.qml
        qml/panels/diagnostics/ModelsReport.qml
        qml/panels/diagnostics/NotificationPathCard.qml
        qml/panels/diagnostics/StatusLines.qml
        qml/panels/diagnostics/LineText.qml
        qml/panels/diagnostics/ToolRead.qml
        qml/panels/diagnostics/DiagnosticsText.js
        qml/panels/diagnostics/NotificationText.js
        qml/panels/backups/BackupsPanel.qml)

add_subdirectory(tests)
