# lil-image.exe, the image app. It links lil_qt (the static library and the Lilith.Core module),
# the shim, KDDockWidgets and the static Lilith.UiKit and Lilith.Shim modules; never WebEngine
# (APP-137, checked by deps_walk).
qt_add_executable(lil-image WIN32 main.cpp)
target_link_libraries(lil-image PRIVATE
    Qt6::Gui Qt6::Qml Qt6::Quick
    KDAB::kddockwidgets
    lil-qt-shim
    lil-ui-kitplugin
    lil-qt-shim-qmlplugin
    lil_qt_qml)
# No QML debugging in shipped builds (Checkpoint A I-3); main.cpp fails to build if it is on.
set_target_properties(lil-image PROPERTIES QT_ENABLE_QML_DEBUG OFF)

# apps\image.toml beside the core (the core reads <its folder>\apps\*.toml).
add_custom_command(TARGET lil-image POST_BUILD
    COMMAND ${CMAKE_COMMAND} -E copy_if_different
        ${CMAKE_CURRENT_SOURCE_DIR}/image.toml $<TARGET_FILE_DIR:lil-image>/apps/image.toml
    VERBATIM)

# The deploy folder, build/<preset>/deploy (OPS-094): our files first, then windeployqt, then the
# VC++ runtime of the pinned 14.44 toolset that built them (cmake/deploy.cmake). Its redist is
# found under the Visual Studio install, not through VCToolsRedistDir, which names the install's
# default redist whatever toolset vcvars64 -vcvars_ver picked.
set(LIL_DEPLOY_DIR ${CMAKE_BINARY_DIR}/deploy)
file(TO_CMAKE_PATH "$ENV{VCINSTALLDIR}" LIL_VC_INSTALL_DIR)
if(NOT LIL_VC_INSTALL_DIR)
    message(FATAL_ERROR "VCINSTALLDIR isn't set: configure through tools/dev-shell.ps1.")
endif()
add_custom_target(deploy
    COMMAND ${CMAKE_COMMAND}
        -DDEPLOY_DIR=${LIL_DEPLOY_DIR}
        -DBIN_DIR=${CMAKE_RUNTIME_OUTPUT_DIRECTORY}
        -DSOURCE_DIR=${CMAKE_SOURCE_DIR}
        -DWINDEPLOYQT=$<TARGET_FILE:Qt6::windeployqt>
        -DKDDW_DLL=$<TARGET_FILE:KDAB::kddockwidgets>
        -DSHIM_DLL=$<TARGET_FILE:lil-qt-shim>
        -DWEB_DLL=$<TARGET_FILE:lil-web>
        -DKDDW_QML_DIR=${LIL_KDDW_QML_DIR}
        -DVC_INSTALL_DIR=${LIL_VC_INSTALL_DIR}
        -P ${CMAKE_SOURCE_DIR}/cmake/deploy.cmake
    COMMENT "Deploying to ${LIL_DEPLOY_DIR}"
    VERBATIM)
add_dependencies(deploy lil-image lil-qt-shim lil-web cargo-build_lil-core cargo-build_lil cargo-build_lil-mcp)

# The deploy checks (label deploy): run them after the deploy target (the build presets build it).
find_program(LIL_PWSH pwsh REQUIRED)
add_test(NAME deps_walk
    COMMAND ${LIL_PWSH} -NoProfile -ExecutionPolicy Bypass -File ${CMAKE_SOURCE_DIR}/tools/deploy/deps-walk.ps1
        -Deploy ${LIL_DEPLOY_DIR}
        -Exe $<TARGET_FILE:lil-image>
        -Shim $<TARGET_FILE:lil-qt-shim>
        -RustLibrary $<TARGET_FILE:lil_qt-static>)
add_test(NAME deploy_runs_off_path
    COMMAND ${LIL_PWSH} -NoProfile -ExecutionPolicy Bypass -File ${CMAKE_SOURCE_DIR}/tools/deploy/check-deploy.ps1
        -Deploy ${LIL_DEPLOY_DIR})
set_tests_properties(deps_walk deploy_runs_off_path PROPERTIES LABELS deploy TIMEOUT 300)
