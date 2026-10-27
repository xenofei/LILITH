# The C++ shim (APP-030): window activation, the dock factory, the live-preview image provider and
# the Qt log bridge, as a DLL beside the exe. GuiPrivate stays private to it; KDDockWidgets' Qt
# Quick frontend uses QuickPrivate. Both bind to the exact Qt build, which QA-077's pair check
# pins.
find_package(Qt6 REQUIRED COMPONENTS GuiPrivate)

qt_add_library(lil-qt-shim SHARED
    include/lilshim/shim_global.h
    include/lilshim/Activation.h src/Activation.cpp
    include/lilshim/DockFactory.h src/DockFactory.cpp
    include/lilshim/LogBridge.h src/LogBridge.cpp
    include/lilshim/PreviewProvider.h src/PreviewProvider.cpp)
target_compile_definitions(lil-qt-shim PRIVATE LILSHIM_BUILD)
target_include_directories(lil-qt-shim PUBLIC ${CMAKE_CURRENT_SOURCE_DIR}/include)
target_link_libraries(lil-qt-shim
    PUBLIC Qt6::Core Qt6::Gui Qt6::Quick KDAB::kddockwidgets
    PRIVATE Qt6::GuiPrivate)

# Lilith.Shim: the LilShim singleton for QML. A static module (backing library and plugin),
# linked into lil-image.exe and the test executable, so nothing of it is deployed (Ruling Q8).
qt_add_qml_module(lil-qt-shim-qml
    URI Lilith.Shim
    VERSION 1.0
    STATIC
    SOURCES src/LilShim.h src/LilShim.cpp)
target_link_libraries(lil-qt-shim-qml PRIVATE lil-qt-shim Qt6::Qml)
target_include_directories(lil-qt-shim-qml PRIVATE ${CMAKE_CURRENT_SOURCE_DIR}/src)
# Task 18: the GUI-thread stall watchdog and the font warm-up (in the DLL), and DockLayout,
# KDDockWidgets' LayoutSaver for QML (APP-003), in Lilith.Shim; it reads the platform's normal
# window geometry (GuiPrivate).
target_sources(lil-qt-shim PRIVATE include/lilshim/StallWatchdog.h src/StallWatchdog.cpp)
target_sources(lil-qt-shim-qml PRIVATE src/LayoutBridge.h src/LayoutBridge.cpp)
target_link_libraries(lil-qt-shim-qml PRIVATE Qt6::GuiPrivate KDAB::kddockwidgets)
