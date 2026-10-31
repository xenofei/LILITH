//! lil_qt: the Rust side of the LILITH apps (P4a-R1).
//!
//! Everything outside [`qt`] is Qt-free, so it builds and tests with default features and no Qt
//! installed. [`plan_boot`] settles a start before Qt does: `apps/lil-image/main.cpp` calls it
//! first, and Qt sees only what the plan hands it (Checkpoint A I-3):
//! - **argv:** [`BootPlan::qt_args`] is the program name alone, whatever the command line says, so
//!   Qt never parses an option such as `-platformpluginpath` from a crafted link. LILITH's own
//!   arguments come only from the raw command line (P4a-R3).
//! - **The command line:** its rest must be the grammar of [`args`], or the start is refused
//!   before Qt starts.
//! - **One instance** ([`single_instance`], APP-013, APP-014): a second launch hands its
//!   arguments to the running instance over the app's pipe and exits 0, before Qt starts.
//! - **The environment:** [`qt_env_to_clear`] names the variables, by their Qt, QML, scene-graph,
//!   QML-engine, WebEngine and Vulkan-loader prefixes, that choose where Qt loads code, QML or data
//!   from, which exe WebEngine starts, or that open debugging; `main.cpp` removes them before Qt
//!   is initialised, and refuses to start if one is still there. (Qt also reads a few names
//!   outside these prefixes, such as `QQC2_STYLE` and `QMLSCENE_DEVICE`; they choose among what
//!   the app folder holds and load nothing from elsewhere.)
//! - **The library path:** [`app_dir`] is the exe's folder, the only one Qt loads plugins from; a
//!   start without one is refused.
//!
//! With the `qt` feature, [`qt`] is the cxx-qt bridge: the `Lilith.Core` QML module and the
//! boot call `main.cpp` makes. The bridge's `Core` object stands on four Qt-free parts:
//! [`requests`] (the request ids, APP-028), [`throttle`] (the leading-edge update throttle,
//! APP-027), [`topics`] (QML's counted subscriptions) and [`retry`] (the automatic retry after
//! a confirmation, APP-029). Its list models stand on [`paging`] (pages and rows, APP-025), and
//! its live previews on [`preview`] (a frame out of its event, APP-031, APP-037).
//!
//! The shell's Qt-free parts: [`layout_file`] (the layout file and the window clamp, APP-002,
//! APP-003), [`shortcuts`] (the shortcut map, APP-011, APP-143) and [`app_state`] (the app's
//! own state, P4a-R16). The bridge's `ShellState` object serves the first two to QML.
//!
//! The Hardware panel's Qt-free part: [`series`] (chart resolutions, per-pixel downsampling, the
//! chart buffer and the energy table's periods, APP-129, APP-131), served to QML by the
//! bridge's `Hardware` singleton and `ChartSeries` objects.
//!
//! The Jobs panel's Qt-free parts: [`format`](mod@format) (the cost and time chip's and the
//! ETAs' words, APP-114, APP-140) and [`live_jobs`] (the running and queued jobs in run order,
//! with their places, ETAs and times); the bridge's `JobText` and `LiveJobsModel` serve them to
//! QML.

#[cfg(feature = "qt")]
extern crate cxx_qt_lib;

#[cfg(feature = "qt")]
pub mod qt;

pub mod app_state;
pub mod args;
pub mod counts;
pub mod csv;
pub mod format;
pub mod layout_file;
pub mod live_jobs;
pub mod money;
pub mod paging;
pub mod preview;
pub mod requests;
pub mod retry;
pub mod series;
pub mod shortcuts;
pub mod single_instance;
pub mod throttle;
pub mod topics;

pub use args::{
    APP, BootPlan, Intent, Mode, NO_ACTIVATE, Opened, QT_ARGV0, SMOKE_EXIT, plan_args, plan_boot,
};

/// The variables removed by prefix, compared without regard to ASCII case (Windows' rule): Qt's
/// (`QT_PLUGIN_PATH`, `QT_QPA_*`, `QT_OPENGL`, `QT_OPENGL_DLL`, ...), QML's (`QML_IMPORT_PATH`,
/// `QML2_IMPORT_PATH`), the scene graph's (`QSG_RHI_BACKEND`, ...), the QML engine's (`QV4_*`),
/// WebEngine's (`QTWEBENGINEPROCESS_PATH`, `QTWEBENGINE_DISABLE_SANDBOX`, `QTWEBENGINE_*_PATH`,
/// `QTWEBENGINE_CHROMIUM_FLAGS`, `QTWEBENGINE_REMOTE_DEBUGGING`) and the Vulkan loader's own.
const CLEAR_PREFIXES: [&str; 7] = [
    "QT_",
    "QML_",
    "QML2_",
    "QSG_",
    "QV4_",
    "QTWEBENGINE",
    "VK_LOADER_",
];

/// The Vulkan loader's layer and driver variables, removed by name, compared the same way.
const CLEAR_EXACT: [&str; 6] = [
    "VK_INSTANCE_LAYERS",
    "VK_LAYER_PATH",
    "VK_ADD_LAYER_PATH",
    "VK_ICD_FILENAMES",
    "VK_DRIVER_FILES",
    "TZ",
];

/// The environment variables to remove before Qt is initialised (Checkpoint A I-3, as ruled at
/// review): every name starting with `QT_`, `QML_`, `QML2_`, `QSG_`, `QV4_`, `QTWEBENGINE` or
/// `VK_LOADER_`, and `VK_INSTANCE_LAYERS`, `VK_LAYER_PATH`, `VK_ADD_LAYER_PATH`,
/// `VK_ICD_FILENAMES` and `VK_DRIVER_FILES`, and `TZ`, from `names` (the process's variable
/// names), keeping each name as it was spelled. `TZ` is there because the C runtime gives QML's
/// local time from it, and the core's and the bridge's calendar come from Windows (CORE-012).
pub fn qt_env_to_clear<'a>(names: impl IntoIterator<Item = &'a str>) -> Vec<String> {
    names
        .into_iter()
        .filter(|name| {
            let upper = name.to_ascii_uppercase();
            CLEAR_EXACT.contains(&upper.as_str())
                || CLEAR_PREFIXES.iter().any(|p| upper.starts_with(p))
        })
        .map(str::to_string)
        .collect()
}

/// The folder of `exe` (the running exe): Qt's only library path. `None`, which refuses the
/// start, when there is no exe or its folder is empty or not absolute: an empty library path
/// would make Qt look for its plugins under the current drive's root.
pub fn app_dir(exe: Option<&std::path::Path>) -> Option<String> {
    let dir = exe?.parent()?;
    (dir.is_absolute() && !dir.as_os_str().is_empty()).then(|| dir.display().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test] // spec: APP-020
    fn qt_env_to_clear_names_the_load_and_debug_variables() {
        // Every name that loads code or data, starts another exe or opens a debugger.
        let cleared = [
            "QT_PLUGIN_PATH",
            "qt_plugin_path",
            "QT_QPA_PLATFORM",
            "QT_QPA_PLATFORM_PLUGIN_PATH",
            "QT_OPENGL",
            "QT_OPENGL_DLL",
            "QT_QUICK_BACKEND",
            "QT_QML_DEBUG",
            "Qt_Qml_Something",
            "QT_LOGGING_RULES",
            "QML_IMPORT_PATH",
            "QML2_IMPORT_PATH",
            "QML_DISK_CACHE_PATH",
            "QSG_RHI_BACKEND",
            "QSG_INFO",
            "QV4_FORCE_INTERPRETER",
            "QTWEBENGINEPROCESS_PATH",
            "QTWEBENGINE_DISABLE_SANDBOX",
            "QTWEBENGINE_RESOURCES_PATH",
            "QTWEBENGINE_LOCALES_PATH",
            "QTWEBENGINE_DICTIONARIES_PATH",
            "QTWEBENGINE_CHROMIUM_FLAGS",
            "QTWEBENGINE_REMOTE_DEBUGGING",
            "VK_INSTANCE_LAYERS",
            "VK_LAYER_PATH",
            "VK_ADD_LAYER_PATH",
            "vk_icd_filenames",
            "VK_DRIVER_FILES",
            "VK_LOADER_DEBUG",
            "VK_LOADER_LAYERS_ENABLE",
            "TZ",
            "tz",
        ];
        let kept = [
            "TZDIR",
            "XTZ",
            "PATH",
            "QTDIR",
            "XQT_QPA_PLATFORM",
            "LIL_CHANNEL",
            "VK_SDK_PATH",
            "VULKAN_SDK",
            "QMAKE",
        ];
        let names: Vec<&str> = cleared.iter().chain(kept.iter()).copied().collect();
        assert_eq!(qt_env_to_clear(names), cleared);
    }

    #[test] // spec: APP-020
    fn app_dir_is_the_exe_folder_or_refuses() {
        assert_eq!(
            app_dir(Some(Path::new(r"C:\lil\deploy\lil-image.exe"))).as_deref(),
            Some(r"C:\lil\deploy")
        );
        for exe in [
            None,
            Some(Path::new("lil-image.exe")),
            Some(Path::new(r"deploy\lil-image.exe")),
        ] {
            assert_eq!(app_dir(exe), None, "{exe:?}");
        }
    }
}
