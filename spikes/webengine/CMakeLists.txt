# WebEngine <-> ComfyUI frontend scripting spike (IMG-159, IMG-127, IMG-129, ARCH-070).
# The Task 2 app (spikes/qt-stack) cut down to the Web dock, plus PageDriver in the shim.
# No Rust crate: nothing here needs cxx-qt (the counter dock it served is gone).
#
#   . spikes/toolchain/env.ps1
#   cmake -S spikes/webengine -B spikes/webengine/build/cmake -G Ninja -DCMAKE_BUILD_TYPE=RelWithDebInfo
#   cmake --build spikes/webengine/build/cmake
#
# Warning settings are the toolchain defaults (ruling R3).
cmake_minimum_required(VERSION 3.24)
project(webengine_spike LANGUAGES CXX)

if(CMAKE_CXX_COMPILER_ID STREQUAL "MSVC")
    set(CMAKE_MSVC_RUNTIME_LIBRARY "MultiThreadedDLL")
endif()
set(CMAKE_CXX_STANDARD 17)
set(CMAKE_CXX_STANDARD_REQUIRED ON)
set(CMAKE_AUTOMOC ON)
set(CMAKE_RUNTIME_OUTPUT_DIRECTORY ${CMAKE_BINARY_DIR}/bin)

find_package(Qt6 REQUIRED COMPONENTS Core Gui Qml Quick QuickControls2 WebEngineCore WebEngineQuick)
message(STATUS "Qt ${Qt6_VERSION} at ${Qt6_DIR}; WebEngineQuick ${Qt6WebEngineQuick_VERSION}")

include(FetchContent)
set(KDDockWidgets_QT6 ON CACHE BOOL "" FORCE)
set(KDDockWidgets_FRONTENDS "qtquick" CACHE STRING "" FORCE)
FetchContent_Declare(KDDockWidgets
    GIT_REPOSITORY https://github.com/KDAB/KDDockWidgets.git
    GIT_TAG v2.4.1
    GIT_SHALLOW TRUE)
FetchContent_MakeAvailable(KDDockWidgets)

# The shim: SpikeLog (JSON-lines log) and PageDriver (runJavaScript helpers, the hidden
# conversion page, download blocking, navigation pinning). Unlike Task 2's shim it links
# QtWebEngine, so this spike does not keep WebEngine off the start-up path (APP-137 is Task 2's).
qt_add_library(spike_shim SHARED
    shim/spike_shim_global.h
    shim/SpikeLog.h shim/SpikeLog.cpp
    shim/PageDriver.h shim/PageDriver.cpp)
target_compile_definitions(spike_shim PRIVATE SPIKE_SHIM_BUILD)
target_include_directories(spike_shim PUBLIC ${CMAKE_CURRENT_SOURCE_DIR}/shim)
target_link_libraries(spike_shim
    PUBLIC Qt6::Core Qt6::Gui Qt6::Qml Qt6::Quick Qt6::WebEngineCore Qt6::WebEngineQuick)

qt_add_executable(webengine WIN32 main.cpp)
# QML and the page scripts are plain resources (:/qml/..., :/js/...).
qt_add_resources(webengine "spike_qml"
    PREFIX "/"
    FILES
        qml/Main.qml
        qml/WebDock.qml
        js/load_graph.js
        js/read_graph.js
        js/convert.js
        js/poll_flag.js
        js/take_result.js
        js/app_ready.js
        js/tabs_state.js
        js/export_capture.js
        js/nav_probe.js)
target_link_libraries(webengine PRIVATE
    Qt6::Gui Qt6::Qml Qt6::Quick Qt6::QuickControls2
    KDAB::kddockwidgets
    spike_shim)
