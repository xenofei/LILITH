# LILITH's CMake build (ARCH-114): Corrosion and cxx-qt build the Rust crates, KDDockWidgets
# comes from source, and the C++ shim, the QML modules and lil-image.exe are built against the
# Qt pair (Qt 6.12.0 with Qt WebEngine 6.140.0, QA-077). Every exe and DLL lands in one bin/
# folder; `cmake --build --preset dev --target deploy` makes build/dev/deploy.
#
#   pwsh -NoProfile -File tools/dev-shell.ps1 cmake --preset dev
#   pwsh -NoProfile -File tools/dev-shell.ps1 cmake --build --preset dev
#   pwsh -NoProfile -File tools/dev-shell.ps1 ctest --preset qml
#
# Warning settings are the toolchain defaults: nothing here adds or removes a warning flag.
cmake_minimum_required(VERSION 3.24)
project(lilith LANGUAGES CXX)

# The pinned toolset, MSVC 14.44 (cl 19.44.x), the one Qt 6.12 supports for x64;
# tools/dev-shell.ps1 selects it. Another compiler fails here rather than building something else.
if(NOT CMAKE_CXX_COMPILER_ID STREQUAL "MSVC" OR NOT CMAKE_CXX_COMPILER_VERSION MATCHES "^19\\.44\\.")
    message(FATAL_ERROR "The compiler is ${CMAKE_CXX_COMPILER_ID} ${CMAKE_CXX_COMPILER_VERSION}, not MSVC 19.44.x (14.44): configure through tools/dev-shell.ps1.")
endif()

# Rust always links the release MSVC runtime, so everything else does too, in every
# configuration, and Qt's Debug libraries are never mixed in.
set(CMAKE_MSVC_RUNTIME_LIBRARY "MultiThreadedDLL")
set(CMAKE_MAP_IMPORTED_CONFIG_DEBUG Release)
set(CMAKE_CXX_STANDARD 17)
set(CMAKE_CXX_STANDARD_REQUIRED ON)
set(CMAKE_AUTOMOC ON)
# One output folder for every exe, DLL and dynamic QML module (Lilith/Web/), set before the
# Rust import so Corrosion's binaries land there too.
set(CMAKE_RUNTIME_OUTPUT_DIRECTORY ${CMAKE_BINARY_DIR}/bin)
set(QT_QML_OUTPUT_DIRECTORY ${CMAKE_BINARY_DIR}/bin)

# WebEngineQuick is found only in apps/lil-web/ (APP-137).
find_package(Qt6 6.12.0 EXACT REQUIRED COMPONENTS Core Gui Qml Quick QuickControls2 QuickTest)
message(STATUS "Qt ${Qt6_VERSION} at ${Qt6_DIR}")
qt_standard_project_setup(REQUIRES 6.12)

include(FetchContent)

# Corrosion v0.5.2, declared and made available before cxx-qt-cmake (which asks for 0.5.2), so
# this pinned commit wins over a Corrosion installed on the machine: after MakeAvailable,
# cxx-qt-cmake's find_package(Corrosion) finds this one.
FetchContent_Declare(Corrosion
    GIT_REPOSITORY https://github.com/corrosion-rs/corrosion.git
    GIT_TAG a1a1aaa057a5da656c06c3d8505b767a4e941709) # v0.5.2
FetchContent_MakeAvailable(Corrosion)

# cxx-qt's CMake helpers, pinned to the cxx-qt release the crates use (0.10.0).
FetchContent_Declare(CxxQt
    GIT_REPOSITORY https://github.com/KDAB/cxx-qt-cmake.git
    GIT_TAG 64e63f560737b3b7f85ef8267c5918e4b8176d5a) # 0.10.0
FetchContent_MakeAvailable(CxxQt)

# KDDockWidgets v2.5.0 (APP-005), Qt Quick frontend only, without spdlog, examples or tests.
# Licence: GPL-2.0-only OR GPL-3.0-only; LILITH takes GPL-3.0-only (NOTICE, section 6).
set(KDDockWidgets_QT6 ON CACHE BOOL "" FORCE)
set(KDDockWidgets_FRONTENDS "qtquick" CACHE STRING "" FORCE)
set(KDDockWidgets_NO_SPDLOG ON CACHE BOOL "" FORCE)
set(KDDockWidgets_EXAMPLES OFF CACHE BOOL "" FORCE)
set(KDDockWidgets_TESTS OFF CACHE BOOL "" FORCE)
FetchContent_Declare(KDDockWidgets
    GIT_REPOSITORY https://github.com/KDAB/KDDockWidgets.git
    GIT_TAG 884b00a36528552060aae01ee80cbb6e0df4b5ef) # v2.5.0
FetchContent_MakeAvailable(KDDockWidgets)
# Its Qt Quick views' QML folder, for windeployqt's import scan.
set(LIL_KDDW_QML_DIR ${kddockwidgets_SOURCE_DIR}/src/qtquick/views/qml)

# The Rust programs the deploy folder carries (Corrosion names them after their [[bin]]: lil,
# not lil-cli), built against the workspace's Cargo.lock.
# LIL_RUST_BIN_DIR (the ci preset sets it to target/release): take the three programs from that
# folder, where the same CI job's `cargo build --workspace --release --locked` step has just
# built them, instead of building them a second time through Corrosion in its own target folder
# (Corrosion always passes --target and its own --target-dir, so it can't reuse that build). They
# then carry the workspace's unified features rather than each package's own. The targets keep
# Corrosion's names, so the deploy target depends on them either way. lil_qt is still built
# below, by cxx-qt-cmake, which has to drive its build.
set(LIL_RUST_BIN_DIR "" CACHE PATH "Folder with release builds of lil-core.exe, lil.exe and lil-mcp.exe to use instead of building them (empty: Corrosion builds them)")
if(LIL_RUST_BIN_DIR)
    foreach(bin lil-core lil lil-mcp)
        if(NOT EXISTS "${LIL_RUST_BIN_DIR}/${bin}.exe")
            message(FATAL_ERROR "${LIL_RUST_BIN_DIR}/${bin}.exe doesn't exist: run `cargo build --workspace --release --locked` first, or leave LIL_RUST_BIN_DIR empty.")
        endif()
        add_custom_target(cargo-build_${bin} ALL
            COMMAND ${CMAKE_COMMAND} -E copy_if_different
                "${LIL_RUST_BIN_DIR}/${bin}.exe" "${CMAKE_RUNTIME_OUTPUT_DIRECTORY}/${bin}.exe"
            COMMENT "Copying the prebuilt ${bin}.exe from ${LIL_RUST_BIN_DIR}"
            VERBATIM)
    endforeach()
else()
    corrosion_import_crate(
        MANIFEST_PATH ${CMAKE_SOURCE_DIR}/Cargo.toml
        CRATES lil-core lil-cli lil-mcp
        LOCKED)
endif()

# lil_qt with its `qt` feature: the static library and the Lilith.Core QML module (P4a-R1).
cxx_qt_import_crate(
    MANIFEST_PATH ${CMAKE_SOURCE_DIR}/Cargo.toml
    CRATES lil_qt
    LOCKED
    FEATURES qt
    QT_MODULES Qt6::Core Qt6::Qml)
cxx_qt_import_qml_module(lil_qt_qml
    URI "Lilith.Core"
    SOURCE_CRATE lil_qt)

enable_testing()

add_subdirectory(apps/lil-qt-shim)
add_subdirectory(apps/lil-web)
add_subdirectory(apps/lil-ui-kit)
add_subdirectory(apps/lil-image)
