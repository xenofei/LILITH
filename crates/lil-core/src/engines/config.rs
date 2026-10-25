//! `suite.toml` `[engines.comfyui]` (Appendix F "ComfyUI engine"; CORE-007: machine-level), the
//! owned launch's arguments (ARCH-068) with its launch profile (ARCH-126; Appendix G "ComfyUI
//! launch profile") and the scrubbed engine environment (ARCH-063).

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use lil_protocol::{ErrorBody, codes};
use serde::Deserialize;

/// ComfyUI's default port (ARCH-070).
pub const DEFAULT_PORT: u16 = 8188;
/// An owned engine idle this long gets `/free` (Appendix F).
pub const DEFAULT_IDLE_FREE_AFTER_S: u64 = 300;
/// The only address an owned ComfyUI is told to listen on (ARCH-068).
pub const LOOPBACK: &str = "127.0.0.1";

/// `[engines.comfyui]`, with the defaults filled in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComfyConfig {
    /// The ComfyUI folder (the one holding `main.py`).
    pub root: Option<PathBuf>,
    /// The Python that runs it; `None`: the default search ([`ComfyConfig::python_path`]).
    pub python: Option<PathBuf>,
    pub port: u16,
    pub extra_args: Vec<String>,
    /// A job that needs the engine may start an owned one ([`super::Engines::ensure_ready`]).
    pub autostart: bool,
    pub idle_free_after_s: u64,
    /// The `comfy.exe` node installs run.
    pub comfy_cli: Option<PathBuf>,
    /// Environment variables the scrub lets through to the owned ComfyUI and comfy-cli (owner
    /// decision 2026-10-19): exact names, compared case-insensitively. Only you set it, in
    /// `suite.toml`; no tool writes it. The suite's own and paid-provider names, and names that
    /// can never match, are kept out ([`classify_pass_env`]) and reported in `pass_env_rejected`.
    pub pass_env: Vec<String>,
    /// The `pass_env` entries the scrub still refuses, or that can never name a variable
    /// (ruling P3-R31): `(entry, why)`. `lil doctor` reports them; they never go through.
    pub pass_env_rejected: Vec<(String, String)>,
    /// The owned start's launch profile (ARCH-126).
    pub profile: LaunchProfile,
}

/// An owned ComfyUI's launch profile (ARCH-126; Appendix G "ComfyUI launch profile"): the VRAM
/// it leaves to the studio, its default previews, ComfyUI-Manager, and whether it may use
/// dynamic VRAM.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaunchProfile {
    /// Gigabytes, a decimal string (`--reserve-vram`).
    pub reserve_vram_gb: String,
    /// `none`, `auto`, `latent2rgb` or `taesd` (`--preview-method`).
    pub preview_method: String,
    /// `--enable-manager` when true.
    pub manager: bool,
    /// `--disable-dynamic-vram` when false.
    pub dynamic_vram: bool,
}

impl Default for LaunchProfile {
    fn default() -> LaunchProfile {
        LaunchProfile {
            reserve_vram_gb: lil_schema::suite::DEFAULT_RESERVE_VRAM_GB.to_string(),
            preview_method: lil_schema::suite::DEFAULT_PREVIEW_METHOD.to_string(),
            manager: false,
            dynamic_vram: true,
        }
    }
}

impl LaunchProfile {
    /// Its flags in Appendix G's order: `--reserve-vram <gb> --preview-method <method>`, then
    /// `--enable-manager` when `manager`, then `--disable-dynamic-vram` unless `dynamic_vram`.
    pub fn flags(&self) -> Vec<String> {
        let mut flags = vec![
            "--reserve-vram".to_string(),
            self.reserve_vram_gb.clone(),
            "--preview-method".to_string(),
            self.preview_method.clone(),
        ];
        if self.manager {
            flags.push("--enable-manager".to_string());
        }
        if !self.dynamic_vram {
            flags.push("--disable-dynamic-vram".to_string());
        }
        flags
    }
}

/// The table as written; unknown keys are left alone.
#[derive(Debug, Deserialize)]
struct RawComfy {
    root: Option<PathBuf>,
    python: Option<PathBuf>,
    port: Option<u16>,
    extra_args: Option<Vec<String>>,
    autostart: Option<bool>,
    idle_free_after_s: Option<u64>,
    comfy_cli: Option<PathBuf>,
    pass_env: Option<Vec<String>>,
    reserve_vram_gb: Option<String>,
    preview_method: Option<String>,
    manager: Option<bool>,
    dynamic_vram: Option<bool>,
}

impl ComfyConfig {
    /// The `[engines.comfyui]` table of a `suite.toml` text: `None` when there is none.
    ///
    /// # Errors
    /// The text is not TOML, or a key has the wrong type (a port outside 1–65535, say).
    pub fn from_suite_toml(text: &str) -> Result<Option<ComfyConfig>, String> {
        let suite = lil_schema::SuiteToml::parse(text).map_err(|e| e.to_string())?;
        let Some(table) = suite
            .other
            .get("engines")
            .and_then(|engines| engines.get("comfyui"))
        else {
            return Ok(None);
        };
        let json = serde_json::to_value(table).map_err(|e| e.to_string())?;
        let raw: RawComfy = serde_json::from_value(json)
            .map_err(|e| format!("[engines.comfyui] is not valid: {e}"))?;
        if raw.port == Some(0) {
            return Err("[engines.comfyui] port must be 1–65535".to_string());
        }
        let defaults = LaunchProfile::default();
        let profile = LaunchProfile {
            reserve_vram_gb: raw.reserve_vram_gb.unwrap_or(defaults.reserve_vram_gb),
            preview_method: raw.preview_method.unwrap_or(defaults.preview_method),
            manager: raw.manager.unwrap_or(defaults.manager),
            dynamic_vram: raw.dynamic_vram.unwrap_or(defaults.dynamic_vram),
        };
        if !lil_schema::suite::is_reserve_vram_gb(&profile.reserve_vram_gb) {
            return Err(format!(
                "[engines.comfyui] reserve_vram_gb = {:?} is not a number of gigabytes such as \
                 \"1.5\"",
                profile.reserve_vram_gb
            ));
        }
        if !lil_schema::suite::PREVIEW_METHODS.contains(&profile.preview_method.as_str()) {
            return Err(format!(
                "[engines.comfyui] preview_method = {:?} is not one of {}",
                profile.preview_method,
                lil_schema::suite::PREVIEW_METHODS.join(", ")
            ));
        }
        let mut pass_env = Vec::new();
        let mut pass_env_rejected = Vec::new();
        for entry in raw.pass_env.unwrap_or_default() {
            match classify_pass_env(&entry) {
                Ok(()) => pass_env.push(entry),
                Err(why) => pass_env_rejected.push((entry, why)),
            }
        }
        Ok(Some(ComfyConfig {
            root: raw.root,
            python: raw.python,
            port: raw.port.unwrap_or(DEFAULT_PORT),
            extra_args: raw.extra_args.unwrap_or_default(),
            autostart: raw.autostart.unwrap_or(true),
            idle_free_after_s: raw.idle_free_after_s.unwrap_or(DEFAULT_IDLE_FREE_AFTER_S),
            comfy_cli: raw.comfy_cli,
            pass_env,
            pass_env_rejected,
            profile,
        }))
    }

    /// The environment of an engine process this core starts (the owned ComfyUI, comfy-cli):
    /// this process's, scrubbed, with `pass_env` let through ([`scrubbed_env`]).
    pub fn engine_env(&self) -> Vec<(OsString, OsString)> {
        scrubbed_env(std::env::vars_os(), &self.pass_env)
    }

    /// `python`, else the first of `<root>\..\.venv\Scripts\python.exe`,
    /// `<root>\venv\Scripts\python.exe` and `<root>\..\python_embeded\python.exe` that exists.
    pub fn python_path(&self) -> Option<PathBuf> {
        if let Some(python) = &self.python {
            return Some(python.clone());
        }
        let root = self.root.as_deref()?;
        let parent = root
            .parent()
            .map_or_else(|| root.join(".."), Path::to_path_buf);
        [
            parent.join(r".venv\Scripts\python.exe"),
            root.join(r"venv\Scripts\python.exe"),
            parent.join(r"python_embeded\python.exe"),
        ]
        .into_iter()
        .find(|p| p.is_file())
    }

    /// `http://127.0.0.1:<port>`.
    pub fn url(&self) -> String {
        format!("http://{LOOPBACK}:{}", self.port)
    }

    /// The owned start's arguments after the Python: `main.py --listen 127.0.0.1 --port <port>
    /// <profile flags> <extra_args>` ([`LaunchProfile::flags`]).
    ///
    /// # Errors
    /// `comfyui.not_loopback` when `extra_args` holds `--listen` (or a prefix of it that
    /// Python's argparse would take for it, `--listen=…` included): ComfyUI takes the last one.
    /// `engine.not_configured` for `--port` in the same forms: the port is `port`, which the
    /// suite also talks to, so another one would only make the start time out; and for
    /// `--enable-assets` ([`refuse_enable_assets`]).
    pub fn launch_args(&self) -> Result<Vec<OsString>, ErrorBody> {
        refuse_enable_assets(&self.extra_args)?;
        if let Some(bad) = self.extra_args.iter().find(|a| is_flag(a, "--port")) {
            return Err(ErrorBody::new(
                &codes::ENGINE_NOT_CONFIGURED,
                format!(
                    "[engines.comfyui] extra_args holds {bad:?}: the suite sets --port itself \
                     from [engines.comfyui] port. Set the port there and remove it from \
                     extra_args."
                ),
                "",
            ));
        }
        if let Some(bad) = self.extra_args.iter().find(|a| is_flag(a, "--listen")) {
            return Err(ErrorBody::new(
                &codes::COMFYUI_NOT_LOOPBACK,
                format!(
                    "[engines.comfyui] extra_args holds {bad:?}: ComfyUI listens on {LOOPBACK} \
                     only (ARCH-068), so the suite sets --listen itself. Remove it from \
                     extra_args."
                ),
                "",
            ));
        }
        let mut args: Vec<OsString> = ["main.py", "--listen", LOOPBACK, "--port"]
            .into_iter()
            .map(OsString::from)
            .collect();
        args.push(self.port.to_string().into());
        args.extend(self.profile.flags().into_iter().map(OsString::from));
        args.extend(self.extra_args.iter().map(OsString::from));
        Ok(args)
    }
}

/// `engine.not_configured` naming the first `extra_args` entry that is `--enable-assets` or a
/// prefix of it from `--enable-as` on, with or without `=<value>` (ARCH-065, ARCH-126): the
/// suite never turns ComfyUI's asset catalogue on.
///
/// # Errors
/// That refusal.
pub fn refuse_enable_assets(extra_args: &[String]) -> Result<(), ErrorBody> {
    const FLAG: &str = "--enable-assets";
    const SHORTEST: usize = "--enable-as".len();
    let refused = extra_args.iter().find(|arg| {
        let name = arg.split_once('=').map_or(arg.as_str(), |(name, _)| name);
        name.len() >= SHORTEST && FLAG.starts_with(name)
    });
    match refused {
        Some(bad) => Err(ErrorBody::new(
            &codes::ENGINE_NOT_CONFIGURED,
            format!(
                "[engines.comfyui] extra_args holds {bad:?}: the suite never turns on ComfyUI's \
                 asset catalogue (--enable-assets, ARCH-065). Remove it from extra_args."
            ),
            "",
        )),
        None => Ok(()),
    }
}

/// `flag` (`--listen`), `flag=…`, or an abbreviation of it (`--lis`): argparse accepts any
/// unique prefix of a long option.
fn is_flag(arg: &str, flag: &str) -> bool {
    let name = arg.split_once('=').map_or(arg, |(name, _)| name);
    name.len() >= 3 && name.starts_with("--") && flag.starts_with(name)
}

/// Names that contain one of these (upper-cased) are secrets (ARCH-063): Appendix F's list
/// (`API_KEY`, `APIKEY`, `_TOKEN`, `SECRET`, `PASSWORD`, `CREDENTIAL`), widened by ruling
/// P3-R19 to `KEY`, `TOKEN`, `PASSWD` and `AUTH`, which cover the first list.
const SECRET_PARTS: [&str; 7] = [
    "KEY",
    "TOKEN",
    "SECRET",
    "PASSWORD",
    "PASSWD",
    "CREDENTIAL",
    "AUTH",
];

/// Names that start with one of these (upper-cased) are provider or suite settings.
const SECRET_PREFIXES: [&str; 19] = [
    "ANTHROPIC_",
    "OPENAI_",
    "OPENROUTER_",
    "NANOGPT_",
    "GEMINI_",
    "FAL_",
    "CIVITAI_",
    "COMFY_API",
    "DEEPSEEK_",
    "MOONSHOT_",
    "DASHSCOPE_",
    "ZAI_",
    "ZHIPU",
    "MINIMAX_",
    "XAI_",
    "MISTRAL_",
    "REPLICATE_",
    "LIL_",
    "CLAUDE_",
];

/// Names dropped as they are.
const SECRET_NAMES: [&str; 2] = ["HF_TOKEN", "HUGGING_FACE_HUB_TOKEN"];

/// `Ok` when a `pass_env` entry may be let through, `Err(why)` when the scrub refuses it even
/// though it is listed (ruling P3-R31) or it can never name a variable. The rule lives in
/// `lil-schema` so `lil doctor` shares it; `refused_prefixes_match_the_scrub` below pins that its
/// refusal list is this module's [`SECRET_PREFIXES`] without `CIVITAI_`.
pub use lil_schema::pass_env::classify as classify_pass_env;

/// Whether an environment variable of this name stays out of an engine's environment.
fn is_secret(name: &str) -> bool {
    let name = name.to_ascii_uppercase();
    SECRET_PARTS.iter().any(|part| name.contains(part))
        || SECRET_PREFIXES
            .iter()
            .any(|prefix| name.starts_with(prefix))
        || SECRET_NAMES.contains(&name.as_str())
}

/// `vars` without every provider key, secret and suite setting (Appendix F's list, ARCH-063),
/// except the names in `pass` (`[engines.comfyui] pass_env`: exact names, compared
/// case-insensitively); everything else (`PATH`, `HF_HOME`, `SystemRoot`, …) is kept in order.
pub fn scrubbed_env(
    vars: impl IntoIterator<Item = (OsString, OsString)>,
    pass: &[String],
) -> Vec<(OsString, OsString)> {
    vars.into_iter()
        .filter(|(name, _)| {
            let name = name.to_string_lossy();
            !is_secret(&name) || pass.iter().any(|p| p.eq_ignore_ascii_case(&name))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test] // spec: ARCH-068
    fn listen_abbreviations_count_as_listen() {
        for arg in ["--listen", "--listen=0.0.0.0", "--lis", "--liste", "--l"] {
            assert!(is_flag(arg, "--listen"), "{arg}");
        }
        for arg in ["--lowvram", "--port", "--", "-l", "listen", "--listening"] {
            assert!(!is_flag(arg, "--listen"), "{arg}");
        }
        for arg in ["--port", "--port=8189", "--po"] {
            assert!(is_flag(arg, "--port"), "{arg}");
        }
    }

    #[test] // spec: CORE-007
    fn the_table_gets_the_documented_defaults() {
        let cfg = ComfyConfig::from_suite_toml("[engines.comfyui]\nroot = 'C:\\comfy'\n")
            .unwrap()
            .unwrap();
        assert_eq!(
            cfg,
            ComfyConfig {
                root: Some(PathBuf::from(r"C:\comfy")),
                python: None,
                port: 8188,
                extra_args: Vec::new(),
                autostart: true,
                idle_free_after_s: 300,
                comfy_cli: None,
                pass_env: Vec::new(),
                pass_env_rejected: Vec::new(),
                profile: LaunchProfile::default(),
            }
        );
        assert_eq!(ComfyConfig::from_suite_toml("theme = 'dark'\n"), Ok(None));
        for bad in [
            "[engines.comfyui]\nport = 0\n",
            "[engines.comfyui]\nport = 70000\n",
            "[engines.comfyui]\nextra_args = '--lowvram'\n",
        ] {
            assert!(ComfyConfig::from_suite_toml(bad).is_err(), "{bad}");
        }
    }

    /// The shared `pass_env` refusal list (lil-schema) and the scrub's own `SECRET_PREFIXES` must
    /// not drift: every prefix but `CIVITAI_` is refused, and `CIVITAI_` stays listable.
    #[test] // spec: ARCH-063
    fn refused_prefixes_match_the_scrub() {
        for prefix in SECRET_PREFIXES {
            let name = format!("{prefix}X");
            if prefix == "CIVITAI_" {
                assert!(classify_pass_env(&name).is_ok(), "{name}");
            } else {
                assert!(classify_pass_env(&name).is_err(), "{name}");
            }
        }
        for hub in ["HF_TOKEN", "HUGGING_FACE_HUB_TOKEN"] {
            assert!(classify_pass_env(hub).is_ok(), "{hub}");
        }
    }
}
