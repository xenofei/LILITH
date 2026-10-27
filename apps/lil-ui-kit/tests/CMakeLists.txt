# The Qt Quick Tests (QA-068): one executable, one CTest test per tst_*.qml (each in its own
# process), label qml. Later tasks append their tests here.
qt_add_executable(tst_lil_ui_kit main.cpp)
# A console program (qt_standard_project_setup makes WIN32 executables), so it runs from a
# terminal like any test.
set_target_properties(tst_lil_ui_kit PROPERTIES WIN32_EXECUTABLE OFF)
target_link_libraries(tst_lil_ui_kit PRIVATE
    Qt6::Gui Qt6::Qml Qt6::Quick Qt6::QuickTest
    KDAB::kddockwidgets
    lil-qt-shim
    lil-ui-kitplugin
    lil-qt-shim-qmlplugin
    lil_qt_qml)
# Lilith.Web is loaded at run time from bin/Lilith/Web.
add_dependencies(tst_lil_ui_kit lil-web)

# The tests run against the Qt pair's own DLLs.
set(LIL_QT_BIN_DIR "$<TARGET_FILE_DIR:Qt6::Core>")

function(lil_qml_test name file)
    add_test(NAME ${name} COMMAND tst_lil_ui_kit -input ${CMAKE_CURRENT_SOURCE_DIR}/${file})
    set_tests_properties(${name} PROPERTIES
        LABELS qml
        TIMEOUT 300
        # QtTest writes to the debugger, not stderr, when no console is attached (CTest, CI).
        ENVIRONMENT "QT_FORCE_STDERR_LOGGING=1"
        ENVIRONMENT_MODIFICATION "PATH=path_list_prepend:${LIL_QT_BIN_DIR}")
endfunction()

lil_qml_test(tst_boot tst_boot.qml)
lil_qml_test(lazy_web tst_lazy_web.qml)
lil_qml_test(tst_core_call tst_core_call.qml)

# lil_qt's e2e tests (crates/lil-qt/tests/e2e.rs) against a real test-channel core. Corrosion
# builds no test targets, so cargo builds and runs them, with QMAKE naming the Qt pair's qmake
# for cxx-qt-build (about 4 minutes the first time). Label qml, so the qml presets run it. One
# test at a time: each starts a core and a Qt event loop, and the progress test measures arrival
# times, which parallel tests would load (review M-7).
add_test(NAME lil_qt_e2e
    COMMAND cargo test --locked -p lil_qt --features qt --test e2e -- --test-threads=1
    WORKING_DIRECTORY ${CMAKE_SOURCE_DIR})
set_tests_properties(lil_qt_e2e PROPERTIES
    LABELS qml
    TIMEOUT 900
    ENVIRONMENT "QMAKE=$<TARGET_FILE:Qt6::qmake>"
    ENVIRONMENT_MODIFICATION "PATH=path_list_prepend:${LIL_QT_BIN_DIR}")
