# Canvas compositing spike (Task 3: IMG-152, IMG-030-032, IMG-035, IMG-036).
# The Task 2 skeleton (cxx-qt 0.10 via Corrosion, KDDockWidgets 2.4.1 Qt Quick, a shim DLL) with the
# canvas dock only. The shim holds the QQuickRhiItem compositor; the Rust crate holds the CPU
# reference compositor.
#
#   . spikes/toolchain/env.ps1
#   cmake -S spikes/canvas -B spikes/canvas/build/cmake -G Ninja -DCMAKE_BUILD_TYPE=RelWithDebInfo
#   cmake --build spikes/canvas/build/cmake
#
# Warning settings are the toolchain defaults: nothing here adds or removes warning flags.
cmake_minimum_required(VERSION 3.24)
project(canvas_spike LANGUAGES CXX)

if(CMAKE_CXX_COMPILER_ID STREQUAL "MSVC")
    set(CMAKE_MSVC_RUNTIME_LIBRARY "MultiThreadedDLL")
endif()
set(CMAKE_CXX_STANDARD 17)
set(CMAKE_CXX_STANDARD_REQUIRED ON)
set(CMAKE_AUTOMOC ON)
set(CMAKE_RUNTIME_OUTPUT_DIRECTORY ${CMAKE_BINARY_DIR}/bin)

find_package(Qt6 REQUIRED COMPONENTS Core Gui Qml Quick QuickControls2 ShaderTools)
find_package(Qt6 REQUIRED COMPONENTS GuiPrivate)  # <rhi/qrhi.h>
message(STATUS "Qt ${Qt6_VERSION} at ${Qt6_DIR}")

include(FetchContent)
set(KDDockWidgets_QT6 ON CACHE BOOL "" FORCE)
set(KDDockWidgets_FRONTENDS "qtquick" CACHE STRING "" FORCE)
FetchContent_Declare(KDDockWidgets
    GIT_REPOSITORY https://github.com/KDAB/KDDockWidgets.git
    GIT_TAG v2.4.1
    GIT_SHALLOW TRUE)
FetchContent_MakeAvailable(KDDockWidgets)

FetchContent_Declare(CxxQt
    GIT_REPOSITORY https://github.com/KDAB/cxx-qt-cmake.git
    GIT_TAG 0.10.0
    GIT_SHALLOW TRUE)
FetchContent_MakeAvailable(CxxQt)

# The Rust crate (CPU reference compositor): CMake target `canvas_bridge` (static library).
cxx_qt_import_crate(
    MANIFEST_PATH rust/Cargo.toml
    CRATES canvas_bridge
    LOCKED
    QT_MODULES Qt6::Core)

# The shim DLL: document, compositor item/renderer, log.
qt_add_library(canvas_shim SHARED
    shim/spike_shim_global.h
    shim/CanvasDocument.h shim/CanvasDocument.cpp
    shim/CompositeRenderer.h shim/CompositeRenderer.cpp
    shim/SpikeLog.h shim/SpikeLog.cpp)
target_compile_definitions(canvas_shim PRIVATE SPIKE_SHIM_BUILD)
target_include_directories(canvas_shim PUBLIC ${CMAKE_CURRENT_SOURCE_DIR}/shim)
target_link_libraries(canvas_shim PUBLIC Qt6::Core Qt6::Gui Qt6::Quick PRIVATE Qt6::GuiPrivate)

# Shaders -> .qsb (SPIR-V, GLSL 430, HLSL 5.0 precompiled to DXBC by fxc, MSL 1.2) at :/shaders/.
# composite.comp is built twice: the group pass (writes the RGBA32F intermediate) and the parent pass
# (writes the RGBA8 projection).
qt_add_shaders(canvas_shim "canvas_shaders"
    PRECOMPILE
    GLSL "430"
    HLSL 50
    MSL 12
    PREFIX "/shaders"
    BASE shaders
    FILES
        shaders/downsample.comp
        shaders/display.vert
        shaders/display.frag)
qt_add_shaders(canvas_shim "canvas_shaders_composite_main"
    PRECOMPILE
    GLSL "430"
    HLSL 50
    MSL 12
    PREFIX "/shaders"
    BASE shaders
    FILES shaders/composite.comp
    OUTPUTS composite_main.comp.qsb)
qt_add_shaders(canvas_shim "canvas_shaders_composite_group"
    PRECOMPILE
    GLSL "430"
    HLSL 50
    MSL 12
    DEFINES COMPOSITE_GROUP=1
    PREFIX "/shaders"
    BASE shaders
    FILES shaders/composite.comp
    OUTPUTS composite_group.comp.qsb)
# Task 4 (R5) variants without the group pass (see composite.comp), and SM 6.6 builds of every
# composite shader for the D3D12 comparison:
#   composite_single[_sm66]   isolated group in registers, store-only;
#   composite_inplace         projection read-write, DXIL SM 6.6 from dxc (Windows SDK 10.0.26100);
#   composite_inplace_sm50    the same shader as DXBC SM 5.0 from fxc (fxc accepts the RGBA8 typed UAV
#                             load and flags it "Typed UAV Load Additional Formats"), usable on D3D11;
#   composite_main_sm66, composite_group_sm66   Task 3's two-pass shaders as DXIL SM 6.6.
# The *_sm66 / inplace packs hold DXIL only, so QRhi's D3D12 backend can only run them as DXIL.
function(ww_composite_shader name hlsl defines)
    qt_add_shaders(canvas_shim "canvas_shaders_${name}"
        PRECOMPILE
        GLSL "430"
        HLSL ${hlsl}
        MSL 12
        DEFINES ${defines}
        PREFIX "/shaders"
        BASE shaders
        FILES shaders/composite.comp
        OUTPUTS ${name}.comp.qsb)
endfunction()
ww_composite_shader(composite_single 50 "COMPOSITE_FLAT=1")
ww_composite_shader(composite_single_sm66 66 "COMPOSITE_FLAT=1")
ww_composite_shader(composite_inplace 66 "COMPOSITE_FLAT=1;COMPOSITE_INPLACE=1")
ww_composite_shader(composite_inplace_sm50 50 "COMPOSITE_FLAT=1;COMPOSITE_INPLACE=1")
ww_composite_shader(composite_main_sm66 66 "COMPOSITE_MAIN=1")
ww_composite_shader(composite_group_sm66 66 "COMPOSITE_GROUP=1")

# The app.
qt_add_executable(canvas WIN32 main.cpp CanvasController.h CanvasController.cpp)
qt_add_resources(canvas "canvas_qml"
    PREFIX "/"
    FILES qml/Main.qml)
target_link_libraries(canvas PRIVATE
    Qt6::Gui Qt6::Qml Qt6::Quick Qt6::QuickControls2
    KDAB::kddockwidgets
    canvas_shim
    canvas_bridge)
