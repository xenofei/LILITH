# App-shell spike (plan 4a, Task 0): KDDockWidgets (Qt Quick, FetchContent) with a themed dock
# factory and LayoutSaver; a cxx-qt bridge (paged list model, fake job feed, telemetry, counter);
# a separate QML plugin DLL (SpikeWeb) that is the only code importing QtWebEngine; a shim DLL
# (dock factory, preview image provider, heartbeat and frame logger, start-up timer, layouts);
# and a Qt Quick Test for the offscreen check (S0.9).
#
#   . spikes/toolchain/env.ps1
#   cmake -S spikes/app-shell -B spikes/app-shell/build/cmake -G Ninja -DCMAKE_BUILD_TYPE=RelWithDebInfo
#   cmake --build spikes/app-shell/build/cmake
#
# Warning settings are the toolchain defaults: nothing here adds or removes a warning flag or
# warnings-as-errors (S0.1).
cmake_minimum_required(VERSION 3.24)
project(app_shell_spike LANGUAGES CXX)

set(SPIKE_KDDW_TAG "v2.5.0" CACHE STRING "KDDockWidgets git tag (S0.1: v2.5.0, fallback v2.4.1)")

# Rust always links the release MSVC runtime, so everything else must too.
if(CMAKE_CXX_COMPILER_ID STREQUAL "MSVC")
    set(CMAKE_MSVC_RUNTIME_LIBRARY "MultiThreadedDLL")
endif()
set(CMAKE_CXX_STANDARD 17)
set(CMAKE_CXX_STANDARD_REQUIRED ON)
set(CMAKE_AUTOMOC ON)
# One output folder for the exe and every DLL we build (shim, KDDockWidgets).
set(CMAKE_RUNTIME_OUTPUT_DIRECTORY ${CMAKE_BINARY_DIR}/bin)

# WebEngineQuick is linked only by the SpikeWeb plugin, never by the exe or the shim (APP-137).
find_package(Qt6 REQUIRED COMPONENTS Core Gui Qml Quick QuickControls2 QuickShapes QuickTest WebEngineQuick)
message(STATUS "Qt ${Qt6_VERSION} at ${Qt6_DIR}; WebEngineQuick ${Qt6WebEngineQuick_VERSION}")
qt_standard_project_setup(REQUIRES 6.12)

include(FetchContent)

# KDDockWidgets, Qt Quick frontend only.
set(KDDockWidgets_QT6 ON CACHE BOOL "" FORCE)
set(KDDockWidgets_FRONTENDS "qtquick" CACHE STRING "" FORCE)
set(KDDockWidgets_EXAMPLES OFF CACHE BOOL "" FORCE)
set(KDDockWidgets_TESTS OFF CACHE BOOL "" FORCE)
FetchContent_Declare(KDDockWidgets
    GIT_REPOSITORY https://github.com/KDAB/KDDockWidgets.git
    GIT_TAG ${SPIKE_KDDW_TAG}
    GIT_SHALLOW TRUE)
FetchContent_MakeAvailable(KDDockWidgets)

# cxx-qt's CMake helpers (they bring Corrosion), pinned to the cxx-qt release.
FetchContent_Declare(CxxQt
    GIT_REPOSITORY https://github.com/KDAB/cxx-qt-cmake.git
    GIT_TAG 0.10.0
    GIT_SHALLOW TRUE)
FetchContent_MakeAvailable(CxxQt)

# The Rust crate: the CMake target `spike_shell_bridge` (static library) and its QML module.
cxx_qt_import_crate(
    MANIFEST_PATH rust/Cargo.toml
    CRATES spike_shell_bridge
    LOCKED
    QT_MODULES Qt6::Core Qt6::Gui Qt6::Qml)
cxx_qt_import_qml_module(spike_shell_bridge_qml
    URI "SpikeBridge"
    SOURCE_CRATE spike_shell_bridge)

# The C++ shim DLL.
qt_add_library(spike_shell_shim SHARED
    shim/spike_shim_global.h
    shim/SpikeLog.h shim/SpikeLog.cpp
    shim/SpikeDockFactory.h shim/SpikeDockFactory.cpp
    shim/PreviewProvider.h shim/PreviewProvider.cpp
    shim/Heartbeat.h shim/Heartbeat.cpp
    shim/SpikeLayout.h shim/SpikeLayout.cpp
    shim/NoActivate.h shim/NoActivate.cpp
    shim/StartupClock.h shim/StartupClock.cpp
    shim/ExitWatchdog.h shim/ExitWatchdog.cpp)
target_compile_definitions(spike_shell_shim PRIVATE SPIKE_SHIM_BUILD)
target_include_directories(spike_shell_shim PUBLIC ${CMAKE_CURRENT_SOURCE_DIR}/shim)
target_link_libraries(spike_shell_shim PUBLIC Qt6::Core Qt6::Gui Qt6::Qml Qt6::Quick KDAB::kddockwidgets PRIVATE dbghelp)
# The themed title bar the dock factory hands to KDDockWidgets.
qt_add_resources(spike_shell_shim "spike_shim_qml"
    PREFIX "/spike"
    BASE shim
    FILES shim/SpikeTitleBar.qml)

# SpikeWeb: a separate QML plugin DLL, the only code that links QtWebEngine (APP-137, IMG-126).
# It lands in bin/SpikeWeb/ (qmldir + spike_web.dll); the engine finds it through the
# application directory, which is on the QML import path.
set_source_files_properties(web/SpikeWebView.qml PROPERTIES QT_RESOURCE_ALIAS SpikeWebView.qml)
qt_add_qml_module(spike_web
    URI SpikeWeb
    VERSION 1.0
    PLUGIN_TARGET spike_web
    OUTPUT_DIRECTORY ${CMAKE_BINARY_DIR}/bin/SpikeWeb
    QML_FILES web/SpikeWebView.qml)
set_target_properties(spike_web PROPERTIES
    LIBRARY_OUTPUT_DIRECTORY ${CMAKE_BINARY_DIR}/bin/SpikeWeb
    RUNTIME_OUTPUT_DIRECTORY ${CMAKE_BINARY_DIR}/bin/SpikeWeb)
target_link_libraries(spike_web PRIVATE Qt6::Quick Qt6::WebEngineQuick)

# The app. QML is plain resources (no qmlcachegen), so imports are resolved only at run time and
# Qt's QML disk cache is exercised (S0.8).
qt_add_executable(app_shell_spike WIN32 main.cpp)
qt_add_resources(app_shell_spike "spike_qml"
    PREFIX "/"
    FILES
        qml/Main.qml
        qml/JobsSim.qml
        qml/GridSim.qml
        qml/ChartsSim.qml
        qml/WebPanel.qml)
target_link_libraries(app_shell_spike PRIVATE
    Qt6::Gui Qt6::Qml Qt6::Quick Qt6::QuickControls2
    KDAB::kddockwidgets
    spike_shell_shim
    spike_shell_bridge_qml)
add_dependencies(app_shell_spike spike_web)

# S0.9: the offscreen Qt Quick Test.
qt_add_executable(tst_app_shell tests/tst_main.cpp)
target_compile_definitions(tst_app_shell PRIVATE
    QUICK_TEST_SOURCE_DIR="${CMAKE_CURRENT_SOURCE_DIR}/tests"
    SPIKE_BIN_DIR="${CMAKE_BINARY_DIR}/bin")
target_link_libraries(tst_app_shell PRIVATE
    Qt6::Gui Qt6::Qml Qt6::Quick Qt6::QuickTest
    KDAB::kddockwidgets
    spike_shell_shim)
add_dependencies(tst_app_shell spike_web)
