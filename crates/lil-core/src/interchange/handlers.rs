//! The app handler registry (APP-061, APP-066): which program can open what.
//!
//! An app registers with `apps\<app>.toml`: `app`, `exe`, `args?`, `opens` (a list of
//! `{types, formats, intents}`) and `send_to?`. Installed apps ship theirs beside the core
//! executable; external programs register the same way in `%APPDATA%\LILITH\apps\`
//! ([`AppRegistration::external`]). A file that doesn't parse is skipped and logged, so one
//! bad file never hides the others. An installed app wins over an external one of the same
//! name, so a program in the user's folder can't stand in for an app the suite ships, and an
//! external registration whose `exe` is in the core's folder is skipped, so it can't start a
//! suite program with arguments of its own (ruling I1 of the Task 11 review). The one
//! exception is the test app (`lil-test-app.exe`) on a test channel, the channels whose core
//! accepts it as `ui:test` ([`crate::identity::trusts_test_app`]): tests register it from
//! outside, and it sits beside the test core.
//!
//! A [`Target`] is what is to be opened: a file path, a `lil://` ref, a candidate
//! (`@candidate/<run>/<file>`, APP-057) or a document (`doc:<doc_id>/…`). Its type and format
//! decide which registrations handle it. A part that can't be told (a ref's file format) is not
//! held against an app, but a target of which neither is known has no handler.

use std::path::{Path, PathBuf};

use lil_protocol::CallerKind;
use lil_schema::refs::AssetRef;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// What the requester wants done (APP-066). The set is closed.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum Intent {
    /// View it.
    Open,
    /// Create or resume a working document.
    Edit,
    /// Use it as an input of what the app makes.
    UseAsInput,
}

/// One kind of thing an app opens.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpensEntry {
    /// Asset types (`image`, ...); empty means any.
    #[serde(default)]
    pub types: Vec<String>,
    /// File formats, by extension without the dot (`png`, ...); empty means any.
    #[serde(default)]
    pub formats: Vec<String>,
    pub intents: Vec<Intent>,
}

/// A registered app or program.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppRegistration {
    /// The app's name: the `<app>` of its `ui:<app>` connections and of `open_requests:<app>`.
    pub app: String,
    pub exe: PathBuf,
    pub args: Vec<String>,
    pub opens: Vec<OpensEntry>,
    /// Registered in `%APPDATA%\LILITH\apps\` rather than shipped with the suite.
    pub external: bool,
}

/// What `handlers.for` says about one app.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct HandlerView {
    pub app: String,
    /// The intents the app handles for the target, in the order `open`, `edit`, `use_as_input`.
    pub intents: Vec<Intent>,
    pub external: bool,
}

/// The file `apps\<app>.toml` as written. Unknown keys (`send_to`, later ones) are accepted.
#[derive(Deserialize)]
struct RawRegistration {
    app: String,
    exe: PathBuf,
    #[serde(default)]
    args: Vec<String>,
    #[serde(default)]
    opens: Vec<OpensEntry>,
}

/// Loads the registrations in `<core_dir>\apps` (installed apps) and `<data_dir>\apps`
/// (external programs), by file name. A missing folder has none. `test_app`: the test app may
/// be registered from outside although it is in the core's folder (module docs).
pub fn load_registrations(
    core_dir: &Path,
    data_dir: &Path,
    test_app: bool,
) -> Vec<AppRegistration> {
    let mut found: Vec<AppRegistration> = Vec::new();
    for (dir, external) in [(core_dir, false), (data_dir, true)] {
        for reg in load_folder(&dir.join("apps"), core_dir, external, test_app) {
            if found.iter().any(|f| f.app == reg.app) {
                tracing::warn!(app = %reg.app, external, "an app of this name is registered already; this registration is skipped");
            } else {
                found.push(reg);
            }
        }
    }
    found
}

fn load_folder(
    folder: &Path,
    core_dir: &Path,
    external: bool,
    test_app: bool,
) -> Vec<AppRegistration> {
    let Ok(entries) = std::fs::read_dir(folder) else {
        return Vec::new();
    };
    let mut files: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| {
            p.extension()
                .is_some_and(|x| x.eq_ignore_ascii_case("toml"))
                && p.is_file()
        })
        .collect();
    files.sort();
    files
        .into_iter()
        .filter_map(
            |file| match read_registration(&file, core_dir, external, test_app) {
                Ok(reg) => Some(reg),
                Err(why) => {
                    tracing::warn!(file = %file.display(), %why, "an app registration is skipped");
                    None
                }
            },
        )
        .collect()
}

fn read_registration(
    file: &Path,
    core_dir: &Path,
    external: bool,
    test_app: bool,
) -> Result<AppRegistration, String> {
    let text = std::fs::read_to_string(file).map_err(|e| format!("cannot read it: {e}"))?;
    let raw: RawRegistration = toml::from_str(&text).map_err(|e| format!("not valid: {e}"))?;
    if format!("ui:{}", raw.app).parse::<CallerKind>().is_err() {
        return Err(format!(
            "`{}` is not an app name (a-z, then a-z, 0-9 or -)",
            raw.app
        ));
    }
    let exe = if raw.exe.is_absolute() {
        raw.exe
    } else if external {
        return Err("an external program's exe must be an absolute path".to_string());
    } else {
        core_dir.join(raw.exe)
    };
    let the_test_app = test_app
        && exe
            .file_name()
            .is_some_and(|n| n.eq_ignore_ascii_case(crate::identity::TEST_APP_EXE.0));
    if external && !the_test_app {
        let core = lil_platform::paths::folder_key(core_dir);
        if exe
            .ancestors()
            .skip(1)
            .any(|dir| lil_platform::paths::folder_key(dir) == core)
        {
            return Err("an external program's exe can't be in the suite's folder".to_string());
        }
    }
    Ok(AppRegistration {
        app: raw.app,
        exe,
        args: raw.args,
        opens: raw.opens,
        external,
    })
}

/// The longest target accepted, in bytes of UTF-8: the longest an `open/` launch URL carries
/// (Appendix G "`lil:` URLs").
const MAX_TARGET: usize = 4096;

/// What is to be opened, as far as handlers care.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    /// As given.
    pub text: String,
    /// The asset type, when it can be told.
    pub kind: Option<String>,
    /// The file format (extension, lower case), when it can be told.
    pub format: Option<String>,
}

impl Target {
    /// Reads `text` as a target. It is untrusted input: it is checked, never executed.
    ///
    /// # Errors
    /// Why `text` is not a target: empty, longer than 4096 bytes of UTF-8, a control
    /// character, a `lil://` ref that doesn't parse.
    pub fn parse(text: &str) -> Result<Target, String> {
        if text.is_empty() || text.len() > MAX_TARGET {
            return Err(format!("a target is 1 to {MAX_TARGET} bytes of UTF-8"));
        }
        if text.chars().any(char::is_control) {
            return Err("a target holds no control characters".to_string());
        }
        let (kind, format) = if text.starts_with("doc:") {
            (Some("document".to_string()), None)
        } else if let Some(candidate) = text.strip_prefix("@candidate/") {
            let format = extension(candidate);
            (kind_of_format(format.as_deref()), format)
        } else if text.starts_with(lil_schema::SCHEME)
            || text.starts_with(lil_schema::LEGACY_SCHEME)
        {
            match text.parse::<AssetRef>()? {
                AssetRef::Candidate { file, .. } => {
                    let format = extension(&file);
                    (kind_of_format(format.as_deref()), format)
                }
                AssetRef::Sealed { .. } => (None, None),
            }
        } else {
            let format = extension(text);
            (kind_of_format(format.as_deref()), format)
        };
        Ok(Target {
            text: text.to_string(),
            kind,
            format,
        })
    }
}

/// The lower-case extension of the last path component of `path`, if it has one.
fn extension(path: &str) -> Option<String> {
    let name = path.rsplit(['/', '\\']).next()?;
    let (stem, ext) = name.rsplit_once('.')?;
    (!stem.is_empty() && !ext.is_empty()).then(|| ext.to_ascii_lowercase())
}

/// The asset type a file format is, for the formats the suite knows.
fn kind_of_format(format: Option<&str>) -> Option<String> {
    let kind = match format? {
        "png" | "jpg" | "jpeg" | "webp" | "gif" | "bmp" | "tif" | "tiff" | "exr" | "tga" => "image",
        "wav" | "mp3" | "ogg" | "flac" => "audio",
        "mp4" | "webm" | "mov" => "video",
        "glb" | "gltf" | "fbx" | "obj" => "model3d",
        "lildoc" => "document",
        _ => return None,
    };
    Some(kind.to_string())
}

fn lists(values: &[String], wanted: Option<&str>) -> bool {
    values.is_empty() || wanted.is_none_or(|w| values.iter().any(|v| v.eq_ignore_ascii_case(w)))
}

impl OpensEntry {
    /// Whether this entry covers `target` (the part of it that is known).
    fn covers(&self, target: &Target) -> bool {
        lists(&self.types, target.kind.as_deref()) && lists(&self.formats, target.format.as_deref())
    }
}

impl AppRegistration {
    /// The intents `self` handles for `target`, in the order of [`Intent`].
    pub fn intents_for(&self, target: &Target) -> Vec<Intent> {
        // Nothing is known of the target (a ref of a workspace that isn't open): no app is
        // offered, rather than every app.
        if target.kind.is_none() && target.format.is_none() {
            return Vec::new();
        }
        let mut intents: Vec<Intent> = self
            .opens
            .iter()
            .filter(|e| e.covers(target))
            .flat_map(|e| e.intents.iter().copied())
            .collect();
        intents.sort();
        intents.dedup();
        intents
    }
}

/// The apps in `registrations` that can open `target`, installed apps first, then by name.
pub fn handlers_for_target(registrations: &[AppRegistration], target: &Target) -> Vec<HandlerView> {
    let mut views: Vec<HandlerView> = registrations
        .iter()
        .filter_map(|r| {
            let intents = r.intents_for(target);
            (!intents.is_empty()).then(|| HandlerView {
                app: r.app.clone(),
                intents,
                external: r.external,
            })
        })
        .collect();
    views.sort_by(|a, b| (a.external, &a.app).cmp(&(b.external, &b.app)));
    views
}
