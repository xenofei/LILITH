//! `suite.toml`, the workspace registry in the suite's roaming data folder (ARCH-090).
//!
//! Plan 2 reads two keys: `default_workspace = '<path>'` and `[[workspaces]] path = '<path>'`.
//! Every other key, at the top level or inside a `[[workspaces]]` entry, is kept in `other`,
//! so a parse followed by a write loses nothing.

use serde::{Deserialize, Serialize};

/// The suite `state.db` schema this build knows: its `PRAGMA user_version` after every suite
/// migration (CORE-004, QA-062). `lil-core` applies the migrations and checks its own count
/// against this; `lil doctor` reads it to tell a database from a newer version apart.
pub const SUITE_STATE_SCHEMA: i64 = 11;

/// The same for a workspace `state.db` (in the workspace's local folder): its `user_version`
/// after every workspace migration. `lil doctor` reads it when it checks the default
/// workspace's database.
pub const WORKSPACE_STATE_SCHEMA: i64 = 8;

/// The whole file. Unknown keys are preserved in `other`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SuiteToml {
    /// The workspace the core serves.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_workspace: Option<String>,
    /// Every registered workspace.
    #[serde(default)]
    pub workspaces: Vec<WorkspaceEntry>,
    #[serde(flatten)]
    pub other: toml::Table,
}

/// One `[[workspaces]]` entry. Unknown keys are preserved in `other`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorkspaceEntry {
    pub path: String,
    #[serde(flatten)]
    pub other: toml::Table,
}

impl SuiteToml {
    /// Parses the text of a `suite.toml`.
    pub fn parse(text: &str) -> Result<SuiteToml, toml::de::Error> {
        toml::from_str(text)
    }

    /// `[jobs] preempt_agent_batches` (CORE-125): `false` unless it is the boolean `true`.
    pub fn preempt_agent_batches(&self) -> bool {
        self.other
            .get("jobs")
            .and_then(|jobs| jobs.get("preempt_agent_batches"))
            .and_then(toml::Value::as_bool)
            .unwrap_or(false)
    }

    /// The `[power]` table (CORE-064): each value as the decimal text it was written as (a TOML
    /// string, integer or float); a key of another type is left out.
    pub fn power(&self) -> PowerToml {
        let power = self.other.get("power");
        let text = |table: Option<&toml::Value>, key: &str| match table?.get(key)? {
            toml::Value::String(s) => Some(s.clone()),
            toml::Value::Integer(n) => Some(n.to_string()),
            toml::Value::Float(f) => Some(f.to_string()),
            _ => None,
        };
        let electricity = power.and_then(|p| p.get("electricity"));
        PowerToml {
            psu_efficiency: text(power, "psu_efficiency"),
            rest_of_system_w: text(power, "rest_of_system_w"),
            cpu_tdp_w: text(power, "cpu_tdp_w"),
            electricity_rate: text(electricity, "rate"),
            electricity_currency: text(electricity, "currency"),
        }
    }
}

/// The `[power]` table of `suite.toml`, unparsed (`SuiteToml::power`): `psu_efficiency`,
/// `rest_of_system_w`, `cpu_tdp_w` and `electricity = {rate, currency}`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PowerToml {
    pub psu_efficiency: Option<String>,
    pub rest_of_system_w: Option<String>,
    pub cpu_tdp_w: Option<String>,
    pub electricity_rate: Option<String>,
    pub electricity_currency: Option<String>,
}

/// `[engines.comfyui] reserve_vram_gb` unless it is set (ARCH-126, P4a-R15; Appendix G
/// "ComfyUI launch profile").
pub const DEFAULT_RESERVE_VRAM_GB: &str = "1.5";
/// `[engines.comfyui] preview_method` unless it is set.
pub const DEFAULT_PREVIEW_METHOD: &str = "taesd";
/// The `preview_method` values ComfyUI's `--preview-method` takes.
pub const PREVIEW_METHODS: [&str; 4] = ["none", "auto", "latent2rgb", "taesd"];

/// The most `reserve_vram_gb` may be, in gigabytes (plan 4a ruling, 2026-10-24).
pub const MAX_RESERVE_VRAM_GB: u32 = 256;

/// Whether `s` is a valid `reserve_vram_gb`: a decimal, digits with at most one `.`, from 0 to
/// [`MAX_RESERVE_VRAM_GB`], as ComfyUI's `--reserve-vram` reads it (a float, in gigabytes).
pub fn is_reserve_vram_gb(s: &str) -> bool {
    s.bytes().any(|b| b.is_ascii_digit())
        && s.bytes().all(|b| b.is_ascii_digit() || b == b'.')
        && s.bytes().filter(|b| *b == b'.').count() <= 1
        && s.parse::<f64>()
            .is_ok_and(|gb| gb <= f64::from(MAX_RESERVE_VRAM_GB))
}

// ---------------------------------------------------------------------------------------------
// The desktop settings (APP-146, CORE-160, ARCH-127; Appendix G "Notifications").

/// The notification categories plan 4a knows, each on by default (owner decision 2026-10-22),
/// in `suite.toml`'s `[notifications]` by name. The set is open: a later plan adds a category
/// here; any other key under `[notifications]` (an old `job_progress` among them) is kept and
/// ignored.
pub const NOTIFICATION_CATEGORIES: [&str; 6] = [
    "job_finished",
    "job_failed",
    "confirmation_waiting",
    "cap_alert",
    "director_decision",
    "agent_activity",
];
/// `[notifications] quiet_while_fullscreen` unless it is set.
pub const DEFAULT_QUIET_WHILE_FULLSCREEN: bool = true;
/// `[jobs] fullscreen_autopause` unless it is set.
pub const DEFAULT_FULLSCREEN_AUTOPAUSE: bool = false;
/// `[power] keep_awake_gpu` unless it is set.
pub const DEFAULT_KEEP_AWAKE_GPU: bool = true;

/// The desktop settings of `suite.toml` (`SuiteToml::desktop`): each known category in
/// [`NOTIFICATION_CATEGORIES`]' order, and the three switches. A value that isn't a boolean
/// counts as not set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesktopToml {
    pub notifications: Vec<(&'static str, bool)>,
    pub quiet_while_fullscreen: bool,
    pub fullscreen_autopause: bool,
    pub keep_awake_gpu: bool,
}

impl Default for DesktopToml {
    fn default() -> Self {
        DesktopToml {
            notifications: NOTIFICATION_CATEGORIES.iter().map(|c| (*c, true)).collect(),
            quiet_while_fullscreen: DEFAULT_QUIET_WHILE_FULLSCREEN,
            fullscreen_autopause: DEFAULT_FULLSCREEN_AUTOPAUSE,
            keep_awake_gpu: DEFAULT_KEEP_AWAKE_GPU,
        }
    }
}

impl SuiteToml {
    /// `[notifications]`, `[jobs] fullscreen_autopause` and `[power] keep_awake_gpu`, with
    /// their defaults where a key is missing or not a boolean.
    pub fn desktop(&self) -> DesktopToml {
        let flag = |table: &str, key: &str, default: bool| {
            self.other
                .get(table)
                .and_then(|t| t.get(key))
                .and_then(toml::Value::as_bool)
                .unwrap_or(default)
        };
        DesktopToml {
            notifications: NOTIFICATION_CATEGORIES
                .iter()
                .map(|c| (*c, flag("notifications", c, true)))
                .collect(),
            quiet_while_fullscreen: flag(
                "notifications",
                "quiet_while_fullscreen",
                DEFAULT_QUIET_WHILE_FULLSCREEN,
            ),
            fullscreen_autopause: flag(
                "jobs",
                "fullscreen_autopause",
                DEFAULT_FULLSCREEN_AUTOPAUSE,
            ),
            keep_awake_gpu: flag("power", "keep_awake_gpu", DEFAULT_KEEP_AWAKE_GPU),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test] // spec: APP-146, CORE-160, ARCH-127
    fn desktop_settings_default_and_ignore_unknown_or_odd_keys() {
        assert_eq!(
            SuiteToml::parse("").unwrap().desktop(),
            DesktopToml::default()
        );
        let d = DesktopToml::default();
        assert!(d.notifications.iter().all(|(_, on)| *on));
        assert_eq!(d.notifications.len(), 6);
        assert!(!d.notifications.iter().any(|(c, _)| *c == "job_progress"));
        assert!(d.quiet_while_fullscreen && !d.fullscreen_autopause && d.keep_awake_gpu);

        let suite = SuiteToml::parse(
            "[notifications]\njob_finished = false\njob_progress = true\nfuture_thing = false\n\
             quiet_while_fullscreen = false\ncap_alert = \"no\"\n\
             [jobs]\nfullscreen_autopause = true\n[power]\nkeep_awake_gpu = false\n",
        )
        .unwrap();
        let got = suite.desktop();
        let on = |c: &str| got.notifications.iter().find(|(k, _)| *k == c).unwrap().1;
        assert!(!on("job_finished"));
        assert!(
            on("cap_alert"),
            "a value that isn't a boolean is the default"
        );
        assert!(on("job_failed"));
        assert_eq!(got.notifications.len(), 6, "unknown keys are ignored");
        assert!(!got.quiet_while_fullscreen);
        assert!(got.fullscreen_autopause);
        assert!(!got.keep_awake_gpu);
    }

    const TEXT: &str = r#"
default_workspace = 'C:\work\Ünïcödé space'
theme = "dark"

[[workspaces]]
path = 'C:\work\Ünïcödé space'
pinned = true

[[workspaces]]
path = 'D:\other'

[future]
answer = 42
"#;

    #[test] // spec: ARCH-090
    fn suite_toml_round_trips_unknown_keys() {
        let suite = SuiteToml::parse(TEXT).expect("parse");
        assert_eq!(
            suite.default_workspace.as_deref(),
            Some(r"C:\work\Ünïcödé space")
        );
        let paths: Vec<&str> = suite.workspaces.iter().map(|w| w.path.as_str()).collect();
        assert_eq!(paths, [r"C:\work\Ünïcödé space", r"D:\other"]);
        assert_eq!(
            suite.workspaces[0].other["pinned"],
            toml::Value::Boolean(true)
        );
        assert!(suite.workspaces[1].other.is_empty());
        assert_eq!(suite.other["theme"].as_str(), Some("dark"));
        assert_eq!(suite.other["future"]["answer"].as_integer(), Some(42));
        assert!(!suite.other.contains_key("default_workspace"));
        assert!(!suite.other.contains_key("workspaces"));

        // Written back and read again, nothing is lost or changed.
        let written = toml::to_string(&suite).expect("serialise");
        assert_eq!(SuiteToml::parse(&written).expect("reparse"), suite);
        let original: toml::Table = TEXT.parse().unwrap();
        let rewritten: toml::Table = written.parse().unwrap();
        assert_eq!(rewritten, original, "{written}");
    }

    #[test] // spec: ARCH-090
    fn empty_suite_toml_has_no_workspaces() {
        let suite = SuiteToml::parse("").expect("parse");
        assert_eq!(suite.default_workspace, None);
        assert!(suite.workspaces.is_empty());
        assert!(suite.other.is_empty());
        assert!(SuiteToml::parse("[[workspaces]]\npinned = true\n").is_err());
        assert!(SuiteToml::parse("default_workspace = 3\n").is_err());
    }

    #[test] // spec: CORE-064
    fn power_keeps_each_value_as_written_and_skips_other_types() {
        let suite = SuiteToml::parse(
            "[power]\npsu_efficiency = \"0.9\"\nrest_of_system_w = 40\ncpu_tdp_w = 65.5\n\
             electricity = {rate = \"0.25\", currency = \"EUR\"}\n",
        )
        .unwrap();
        assert_eq!(
            suite.power(),
            PowerToml {
                psu_efficiency: Some("0.9".into()),
                rest_of_system_w: Some("40".into()),
                cpu_tdp_w: Some("65.5".into()),
                electricity_rate: Some("0.25".into()),
                electricity_currency: Some("EUR".into()),
            }
        );
        let odd = SuiteToml::parse("[power]\npsu_efficiency = true\nelectricity = 3\n").unwrap();
        assert_eq!(odd.power(), PowerToml::default());
        assert_eq!(SuiteToml::parse("").unwrap().power(), PowerToml::default());
    }
}
