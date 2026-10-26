# Makes the deploy folder (OPS-094, ARCH-114), run by the `deploy` target:
#   cmake -DDEPLOY_DIR=... -DBIN_DIR=... -DSOURCE_DIR=... -DWINDEPLOYQT=... -DKDDW_DLL=...
#         -DSHIM_DLL=... -DWEB_DLL=... -DKDDW_QML_DIR=... -DVC_INSTALL_DIR=... -P deploy.cmake
#
# The order is spike 10's (§7; Ruling Q8):
#   1. our files: the four programs, apps\image.toml, qt.conf, the shim DLL, the KDDockWidgets
#      DLL (windeployqt doesn't copy it) and the dynamic Lilith.Web module as Lilith\Web\;
#   2. windeployqt with Task 0's flags plus --no-compiler-runtime (no vc_redist.x64.exe), then
#      qtwebengine_devtools_resources.pak removed (no DevTools in the suite);
#   3. the whole Microsoft.VC14*.CRT folder of the pinned 14.44 toolset that built our binaries
#      (tools/dev-shell.ps1), <VCINSTALLDIR>\Redist\MSVC\14.44.*\x64, so the runtime is the
#      toolset's own; deps_walk checks its DLLs are 14.44.x. There is no fallback to another
#      redist: without a 14.44 one the deploy fails.
foreach(var DEPLOY_DIR BIN_DIR SOURCE_DIR WINDEPLOYQT KDDW_DLL SHIM_DLL WEB_DLL KDDW_QML_DIR VC_INSTALL_DIR)
    if(NOT ${var})
        message(FATAL_ERROR "deploy.cmake: ${var} isn't set")
    endif()
endforeach()
file(TO_CMAKE_PATH "${VC_INSTALL_DIR}" VC_INSTALL_DIR)
file(GLOB redist_dirs LIST_DIRECTORIES true "${VC_INSTALL_DIR}/Redist/MSVC/14.44.*")
list(LENGTH redist_dirs redist_count)
if(redist_count EQUAL 0)
    message(FATAL_ERROR "deploy.cmake: no 14.44 VC++ redist under ${VC_INSTALL_DIR}/Redist/MSVC (the pinned toolset's runtime)")
endif()
# The newest 14.44 redist, if there are several.
list(SORT redist_dirs COMPARE NATURAL ORDER DESCENDING)
list(GET redist_dirs 0 redist_dir)

file(REMOVE_RECURSE "${DEPLOY_DIR}")
file(MAKE_DIRECTORY "${DEPLOY_DIR}/apps" "${DEPLOY_DIR}/Lilith/Web")

# 1. Our files.
foreach(exe lil-image lil-core lil lil-mcp)
    file(COPY "${BIN_DIR}/${exe}.exe" DESTINATION "${DEPLOY_DIR}")
endforeach()
file(COPY "${SOURCE_DIR}/apps/lil-image/image.toml" DESTINATION "${DEPLOY_DIR}/apps")
file(COPY "${SOURCE_DIR}/apps/lil-image/qt.conf" DESTINATION "${DEPLOY_DIR}")
file(COPY "${SHIM_DLL}" "${KDDW_DLL}" DESTINATION "${DEPLOY_DIR}")
get_filename_component(web_dir "${WEB_DLL}" DIRECTORY)
file(GLOB web_files "${web_dir}/qmldir" "${web_dir}/*.qmltypes" "${web_dir}/*.dll")
file(COPY ${web_files} DESTINATION "${DEPLOY_DIR}/Lilith/Web")

# 2. windeployqt. --qmldir: the QML folders whose imports it deploys (the UI kit's, the web
# module's, KDDockWidgets' views).
execute_process(
    COMMAND "${WINDEPLOYQT}" --release
        --qmldir "${SOURCE_DIR}/apps/lil-ui-kit/qml"
        --qmldir "${SOURCE_DIR}/apps/lil-web/qml"
        --qmldir "${KDDW_QML_DIR}"
        --no-translations --no-opengl-sw --no-system-dxc-compiler --no-ffmpeg
        --no-compiler-runtime
        --skip-plugin-types designer,generic,help,networkinformation,position,qmllint,qmlls,qmltooling,sqldrivers,webview
        --dir "${DEPLOY_DIR}"
        "${DEPLOY_DIR}/lil-image.exe"
    RESULT_VARIABLE wdq_result
    OUTPUT_VARIABLE wdq_output
    ERROR_VARIABLE wdq_output)
file(WRITE "${DEPLOY_DIR}/../windeployqt.log" "${wdq_output}")
if(NOT wdq_result EQUAL 0)
    message(FATAL_ERROR "windeployqt exited ${wdq_result}; its output is in windeployqt.log beside the deploy folder")
endif()
file(REMOVE "${DEPLOY_DIR}/resources/qtwebengine_devtools_resources.pak")

# 3. The VC++ runtime, app-local.
file(GLOB crt_dirs LIST_DIRECTORIES true "${redist_dir}/x64/Microsoft.VC14*.CRT")
list(LENGTH crt_dirs crt_count)
if(NOT crt_count EQUAL 1)
    message(FATAL_ERROR "deploy.cmake: expected one Microsoft.VC14*.CRT folder under ${redist_dir}/x64, found ${crt_count}")
endif()
message(STATUS "VC++ runtime from ${crt_dirs}")
file(GLOB crt_dlls "${crt_dirs}/*.dll")
file(COPY ${crt_dlls} DESTINATION "${DEPLOY_DIR}")

file(GLOB_RECURSE deployed LIST_DIRECTORIES false "${DEPLOY_DIR}/*")
set(bytes 0)
foreach(f ${deployed})
    file(SIZE "${f}" size)
    math(EXPR bytes "${bytes} + ${size}")
endforeach()
list(LENGTH deployed count)
math(EXPR kib "${bytes} / 1024")
message(STATUS "Deployed ${count} files, ${kib} KiB, to ${DEPLOY_DIR}")
