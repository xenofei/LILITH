# Qt stack spike (ARCH-115, ARCH-116, APP-137).
# cxx-qt 0.10 (cxx_qt_import_crate, via Corrosion) + KDDockWidgets 2.4.1 (Qt Quick, FetchContent)
# + a QQuickRhiItem shim DLL + a lazily loaded QtWebEngine view.
#
#   . spikes/toolchain/env.ps1
#   cmake -S spikes/qt-stack -B spikes/qt-stack/build/cmake -G Ninja -DCMAKE_BUILD_TYPE=RelWithDebInfo
#   cmake --build spikes/qt-stack/build/cmake
#
# Warning settings are the toolchain defaults (ruling R3): nothing here adds or removes warning
# flags or warnings-as-errors.
cmake_minimum_required(VERSION 3.24)
project(qt_stack_spike LANGUAGES CXX)

# Rust always links the release MSVC runtime, so everything else must too.
if(CMAKE_CXX_COMPILER_ID STREQUAL "MSVC")
    set(CMAKE_MSVC_RUNTIME_LIBRARY "MultiThreadedDLL")
endif()
set(CMAKE_CXX_STANDARD 17)
set(CMAKE_CXX_STANDARD_REQUIRED ON)
set(CMAKE_AUTOMOC ON)
# One output folder for the exe and every DLL we build (shim, KDDockWidgets).
set(CMAKE_RUNTIME_OUTPUT_DIRECTORY ${CMAKE_BINARY_DIR}/bin)

# WebEngineQuick is found (proves the module is installed) but never linked (APP-137).
find_package(Qt6 REQUIRED COMPONENTS Core Gui Qml Quick QuickControls2 ShaderTools WebEngineQuick)
# QRhi (<rhi/qrhi.h>) is semi-public API: it needs the GuiPrivate target since Qt 6.6/6.10.
find_package(Qt6 REQUIRED COMPONENTS GuiPrivate)
message(STATUS "Qt ${Qt6_VERSION} at ${Qt6_DIR}; WebEngineQuick ${Qt6WebEngineQuick_VERSION}")

include(FetchContent)

# KDDockWidgets 2.4.1, Qt Quick frontend only.
set(KDDockWidgets_QT6 ON CACHE BOOL "" FORCE)
set(KDDockWidgets_FRONTENDS "qtquick" CACHE STRING "" FORCE)
FetchContent_Declare(KDDockWidgets
    GIT_REPOSITORY https://github.com/KDAB/KDDockWidgets.git
    GIT_TAG v2.4.1
    GIT_SHALLOW TRUE)
FetchContent_MakeAvailable(KDDockWidgets)

# cxx-qt's CMake helpers (they bring Corrosion), pinned to the cxx-qt release.
FetchContent_Declare(CxxQt
    GIT_REPOSITORY https://github.com/KDAB/cxx-qt-cmake.git
    GIT_TAG 0.10.0
    GIT_SHALLOW TRUE)
FetchContent_MakeAvailable(CxxQt)

# The Rust crate: creates the CMake target `spike_bridge` (static library).
cxx_qt_import_crate(
    MANIFEST_PATH rust/Cargo.toml
    CRATES spike_bridge
    LOCKED
    QT_MODULES Qt6::Core Qt6::Gui Qt6::Qml)
# Its QML module (SpikeBridge, with SpikeCounter).
cxx_qt_import_qml_module(spike_bridge_qml
    URI "SpikeBridge"
    SOURCE_CRATE spike_bridge)

# The C++ shim: a DLL with SpikeCanvas (QQuickRhiItem) and SpikeLog.
qt_add_library(spike_shim SHARED
    shim/spike_shim_global.h
    shim/SpikeCanvas.h shim/SpikeCanvas.cpp
    shim/SpikeLog.h shim/SpikeLog.cpp)
target_compile_definitions(spike_shim PRIVATE SPIKE_SHIM_BUILD)
target_include_directories(spike_shim PUBLIC ${CMAKE_CURRENT_SOURCE_DIR}/shim)
target_link_libraries(spike_shim PUBLIC Qt6::Core Qt6::Gui Qt6::Quick PRIVATE Qt6::GuiPrivate)
# .vert/.frag/.comp -> .qsb (SPIR-V, GLSL 430, HLSL 50 precompiled to DXBC, MSL 1.2),
# embedded in the shim at :/shaders/<name>.qsb.
qt_add_shaders(spike_shim "spike_shaders"
    BATCHABLE
    PRECOMPILE
    GLSL "430"
    HLSL 50
    MSL 12
    PREFIX "/shaders"
    BASE shaders
    FILES
        shaders/blit.vert
        shaders/blit.frag
        shaders/fill.comp)

# The app.
qt_add_executable(qt_stack_spike WIN32 main.cpp)
# QML is plain resources (no qmlcachegen): WebDock.qml's QtWebEngine import is resolved only at
# run time, when the Loader first creates it.
qt_add_resources(qt_stack_spike "spike_qml"
    PREFIX "/"
    FILES qml/Main.qml qml/WebDock.qml)
target_link_libraries(qt_stack_spike PRIVATE
    Qt6::Gui Qt6::Qml Qt6::Quick Qt6::QuickControls2
    KDAB::kddockwidgets
    spike_shim
    spike_bridge_qml)
