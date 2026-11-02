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
# Task 19: the Opened card (P4a-R21), which sits in Home.
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

# Task 21: the Confirmations panel, the shared dialog, the cap prompt and the confirm window.
set_source_files_properties(qml/dialogs/OpenPrompts.qml PROPERTIES QT_QML_SINGLETON_TYPE TRUE)
qt_target_qml_sources(lil-ui-kit
    QML_FILES
        qml/dialogs/OpenPrompts.qml
        qml/dialogs/ConfirmText.js
        qml/dialogs/ApproveButton.qml
        qml/dialogs/DenyButton.qml
        qml/dialogs/AnswerBar.qml
        qml/dialogs/CardColumn.qml
        qml/dialogs/ConfirmationList.qml
        qml/dialogs/CapPrompt.qml
        qml/dialogs/ConfirmDialog.qml
        qml/panels/confirmations/ConfirmationsPanel.qml
        qml/panels/confirmations/ConfirmationCard.qml
        qml/panels/confirmations/HistoryList.qml
        qml/shell/ConfirmWindow.qml)

# Task 26: Home.
qt_target_qml_sources(lil-ui-kit
    QML_FILES
        qml/panels/home/HomePanel.qml
        qml/panels/home/ProjectsCard.qml
        qml/panels/home/RecentAssets.qml
        qml/panels/home/StatusCounts.qml
        qml/panels/home/OpenJobs.qml
        qml/panels/home/AwayCard.qml)

# Task 27: the Settings panel and the first-start card.
qt_target_qml_sources(lil-ui-kit
    QML_FILES
        qml/panels/settings/SettingsPanel.qml
        qml/panels/settings/SettingToggle.qml
        qml/panels/settings/NotificationToggles.qml
        qml/panels/settings/DesktopToggles.qml
        qml/panels/settings/NotificationSetup.qml
        qml/shell/FirstStartCard.qml)

# Task 29: the owner's budgets (test-params.toml's [budgets], protected) for the benches, as
# LIL_BUDGET_<NAME>=<value> environment entries: each `name = number` line of that table, its name
# upper-cased (gui_stall_p99_ms is LIL_BUDGET_GUI_STALL_P99_MS). LIL_BUDGET_ENV is the list, for
# the tests' ENVIRONMENT here, in tests/ and in apps/lil-image (bench_cold_start). A change to the
# file reconfigures at the next build.
set_property(DIRECTORY APPEND PROPERTY CMAKE_CONFIGURE_DEPENDS ${CMAKE_SOURCE_DIR}/test-params.toml)
file(STRINGS ${CMAKE_SOURCE_DIR}/test-params.toml lil_params_lines)
set(LIL_BUDGET_ENV "")
set(lil_in_budgets FALSE)
foreach(line IN LISTS lil_params_lines)
    if(line MATCHES "^[ \t]*\\[([A-Za-z0-9_.]+)\\]")
        if(CMAKE_MATCH_1 STREQUAL "budgets")
            set(lil_in_budgets TRUE)
        else()
            set(lil_in_budgets FALSE)
        endif()
    elseif(lil_in_budgets AND line MATCHES "^[ \t]*([a-z0-9_]+)[ \t]*=[ \t]*([0-9.]+)")
        string(TOUPPER "${CMAKE_MATCH_1}" lil_budget_name)
        list(APPEND LIL_BUDGET_ENV "LIL_BUDGET_${lil_budget_name}=${CMAKE_MATCH_2}")
    endif()
endforeach()
foreach(needed GUI_STALL_P99_MS EVENT_TO_UI_MS COLD_START_MS DEPLOY_FOLDER_MIB)
    if(NOT LIL_BUDGET_ENV MATCHES "LIL_BUDGET_${needed}=")
        message(FATAL_ERROR "test-params.toml's [budgets] has no ${needed} (lower case there)")
    endif()
endforeach()
set(LIL_BUDGET_ENV "${LIL_BUDGET_ENV}" PARENT_SCOPE)

add_subdirectory(tests)
