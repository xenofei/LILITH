# Clipboard and drag-out formats spike (APP-071, APP-068, APP-069).
# A minimal Qt Quick app plus a shim DLL (ImageMime, ClipboardBridge, SpikeLog). Same build
# pattern as spikes/qt-stack, without cxx-qt, KDDockWidgets, WebEngine or GuiPrivate.
#
#   . spikes/toolchain/env.ps1
#   cmake -S spikes/clipboard -B spikes/clipboard/build/cmake -G Ninja -DCMAKE_BUILD_TYPE=RelWithDebInfo
#   cmake --build spikes/clipboard/build/cmake
#
# Warning settings are the toolchain defaults (ruling R3).
cmake_minimum_required(VERSION 3.24)
project(clipboard_spike LANGUAGES CXX)

if(CMAKE_CXX_COMPILER_ID STREQUAL "MSVC")
    set(CMAKE_MSVC_RUNTIME_LIBRARY "MultiThreadedDLL")
endif()
set(CMAKE_CXX_STANDARD 17)
set(CMAKE_CXX_STANDARD_REQUIRED ON)
set(CMAKE_AUTOMOC ON)
set(CMAKE_RUNTIME_OUTPUT_DIRECTORY ${CMAKE_BINARY_DIR}/bin)

find_package(Qt6 REQUIRED COMPONENTS Core Gui Qml Quick QuickControls2)
message(STATUS "Qt ${Qt6_VERSION} at ${Qt6_DIR}")

qt_add_library(spike_shim SHARED
    shim/spike_shim_global.h
    shim/SpikeLog.h shim/SpikeLog.cpp
    shim/ImageMime.h shim/ImageMime.cpp
    shim/ClipboardBridge.h shim/ClipboardBridge.cpp)
target_compile_definitions(spike_shim PRIVATE SPIKE_SHIM_BUILD)
target_include_directories(spike_shim PUBLIC ${CMAKE_CURRENT_SOURCE_DIR}/shim)
target_link_libraries(spike_shim PUBLIC Qt6::Core Qt6::Gui Qt6::Quick)

qt_add_executable(clipboard_spike WIN32 main.cpp)
# QML and the source PNG are plain resources: qrc:/qml/Main.qml, qrc:/assets/source.png.
qt_add_resources(clipboard_spike "spike_res"
    PREFIX "/"
    FILES qml/Main.qml assets/source.png)
target_link_libraries(clipboard_spike PRIVATE
    Qt6::Gui Qt6::Qml Qt6::Quick Qt6::QuickControls2 spike_shim)
