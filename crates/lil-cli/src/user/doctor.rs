//! `lil doctor` (CORE-004, CORE-040, ARCH-085, QA-060; owner decision 5): one line per check on
//! stdout, `ok: …`, `fixed: …` or `problem: … — <what to do>`; questions and explanations go to
//! stderr. Exit code 0 when no problem remains, 1 when one does, 2 when it couldn't run.
//!
//! In order:
//! 1. The suite `state.db` ([`inspect_suite_state`]). A damaged or newer one is explained and
//!    set aside as `state.db.damaged-<utc>` (with its `-wal`, `-shm` and `-journal`) only after
//!    the person types `y`; a hot rollback journal is offered to SQLite's own recovery first.
//!    `-wal`, `-shm` or `-journal` files left without their database are set aside the same way.
//!    Nothing is offered while a core runs on the channel (it holds the files open).
//!    With `--non-interactive` nothing is asked and nothing is moved. If a problem remains, the
//!    core can't start, so step 3 is skipped (`problem: workspace checks skipped — the core
//!    can't start`).
//! 2. `suite.toml` ([`inspect_suite_toml`]), never through the `cli` workspace chain, so a stale
//!    default is reported, not refused. Without a default workspace step 3 is skipped.
//!
//!    Step 2b: the default workspace's `state.db` in its local folder ([`workspace_state_db`];
//!    plan-2b Minor 7), checked and set aside exactly as in step 1. While a problem remains the
//!    core can't open the workspace, so step 3 is skipped.
//! 3. `workspace.doctor {fix: true}` over a connection whose hello names the default workspace.
//!    When `git.core_autocrlf` is a problem, `lil` asks, and only a `y` sets it to `false` in the
//!    repository's own config ([`set_repo_autocrlf_false`]); never with `--non-interactive`.
//!    When `legacy.names` is a problem (a workspace from before the rename), `lil` explains and
//!    asks, and only a `y` calls `workspace.doctor {fix: true, offers: ["legacy_names"]}`, so the
//!    core renames them through its store; never with `--non-interactive`. Lines may also be
//!    `info: …` and `skipped: …`. The `claude.*` checks are shown only when Claude Code is
//!    here (step 4); when `claude.rules` reports missing rules, `lil` asks, and only a `y` calls
//!    `workspace.doctor {fix: true, offers: ["claude_rules"]}`, so the core adds them to the
//!    workspace's `.claude/settings.json` through its store; never with `--non-interactive`.
//! 4. Claude Code (AI-114, AI-117, ARCH-104, ARCH-108): `claude.exe` on `PATH` (on a test channel
//!    only the test kit's, in this executable's folder); the plugin `worldwright@lilith` at
//!    1.3.0 in the default profile (the folder `CLAUDE_CONFIG_DIR` names, else Claude Code's
//!    own; the doctor never sets it), installed or updated in place with Claude Code's own
//!    commands only after the person types `y`, never removed; the plugin's options
//!    `lil_mcp_path` and `lil_path` (read from the profile's `settings.json`, never written)
//!    set to `lil-mcp.exe` and `lil.exe` in a folder the running channel trusts, else a
//!    `problem:` naming the path to enter; and the plugin's skills that still write workspace
//!    files directly, from its `skills-inventory.toml`.

use std::ffi::OsStr;
use std::io::{self, BufRead as _, Write as _};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use lil_client::blocking::BlockingClient;
use lil_client::doctor::{
    SUITE_STATE_SCHEMA, StateFileState, SuiteTomlState, WORKSPACE_STATE_SCHEMA, core_running,
    finish_recovery_at, inspect_state_db, inspect_suite_toml, set_aside_db, set_aside_suffix,
    set_repo_autocrlf_false, workspace_state_db,
};
use lil_client::{ClientError, StartPolicy, WorkspaceOrigin, WorkspaceSource};
use lil_platform::names::{
    LEGACY_WORKSPACE_DIR, LEGACY_WORKSPACE_FILE, WORKSPACE_DIR, WORKSPACE_FILE,
};
use lil_platform::paths::folder_key;
use lil_platform::process::{current_exe_dir, find_on_path, run_hidden};
use lil_platform::{Channel, Env};
use lil_protocol::Outcome;
use serde_json::{Map, Value};

use super::{EXIT_ERROR_RESULT, EXIT_OK, Exit, GlobalArgs, cannot_run, config, print_line};

/// The check whose fix only the person may confirm.
const AUTOCRLF_CHECK: &str = "git.core_autocrlf";
/// The check whose rename runs only as an offer, after the person's `y` (owner question 1).
const LEGACY_CHECK: &str = "legacy.names";
/// The checks the legacy-name offer reports on.
const LEGACY_CHECKS: &str = "legacy.";

/// What has been printed so far.
#[derive(Debug, Default)]
struct Report {
    problems: usize,
}

impl Report {
    fn line(&mut self, state: &str, detail: &str) {
        if state == "problem" {
            self.problems += 1;
        }
        let _ = print_line(&format!("{state}: {detail}"));
    }

    fn ok(&mut self, detail: &str) {
        self.line("ok", detail);
    }

    fn fixed(&mut self, detail: &str) {
        self.line("fixed", detail);
    }

    fn problem(&mut self, detail: &str) {
        self.line("problem", detail);
    }
}

/// `lil doctor`.
pub(super) fn run(global: &GlobalArgs) -> Result<i32, Exit> {
    let env = Env::from_process()
        .map_err(|e| cannot_run(format!("Cannot tell which LILITH folders to use: {e}")))?;
    let interactive = !global.non_interactive;
    let mut report = Report::default();
    let claude = find_claude(&env);

    // 1.
    let blocked = state_db(&env, &Db::suite(&env), interactive, &mut report);

    // 2.
    let workspace = match inspect_suite_toml(&env) {
        SuiteTomlState::Missing | SuiteTomlState::NoDefault => {
            report.ok("no default workspace; workspace checks skipped");
            None
        }
        SuiteTomlState::Unreadable(why) => {
            report.problem(&format!(
                "the workspace registry can't be used: {why} — fix the file, or make a \
                 workspace the default with lil workspace use \"<path>\""
            ));
            None
        }
        SuiteTomlState::DefaultMissing(path) => {
            report.problem(&format!(
                "the default workspace {} no longer exists — run lil workspace use \"<path>\" \
                 with the workspace to use",
                path.display()
            ));
            None
        }
        SuiteTomlState::Ok { default, id } => {
            report.ok(&format!("default workspace: {}", default.display()));
            Some((default, id))
        }
    };
    if let Some(skipped) = blocked {
        report.problem(&skipped);
        return Ok(EXIT_ERROR_RESULT);
    }

    // 2b.
    if let Some((default, _)) = &workspace {
        let blocked = match workspace_state_db(&env, default) {
            Ok(path) => state_db(&env, &Db::workspace(path), interactive, &mut report),
            Err(e) => {
                report.problem(&format!(
                    "the local folder of the workspace {} can't be found: {e} — check that \
                     the folder is there",
                    default.display()
                ));
                Some("workspace checks skipped — the core can't open the workspace".to_string())
            }
        };
        if let Some(skipped) = blocked {
            report.problem(&skipped);
            return Ok(EXIT_ERROR_RESULT);
        }
    }

    // 3.
    if let Some((default, id)) = workspace {
        let claude_here = claude.is_some();
        workspace_checks(global, interactive, &default, id, claude_here, &mut report)?;
    }

    // 4.
    claude_code(&env, claude.as_deref(), interactive, &mut report);
    Ok(if report.problems == 0 {
        EXIT_OK
    } else {
        EXIT_ERROR_RESULT
    })
}

/// A state database a step checks: the suite one (step 1) or the default workspace's (step 2b).
struct Db {
    /// "the suite state database" or "the workspace state database".
    label: &'static str,
    path: PathBuf,
    /// The schema this build knows for it.
    known: i64,
    /// Why step 3 is skipped while it can't be used.
    cant_use: &'static str,
    /// Why step 3 is skipped while side files are left without it.
    leftovers_skip: &'static str,
    /// What setting it aside means, in plain words.
    explained: &'static str,
    /// What happens after it was set aside.
    fresh: &'static str,
    /// The `ok` line when there is none yet.
    missing: &'static str,
}

impl Db {
    /// The suite `state.db` (step 1).
    fn suite(env: &Env) -> Db {
        Db {
            label: "the suite state database",
            path: env.suite_state_db(),
            known: SUITE_STATE_SCHEMA,
            cant_use: "workspace checks skipped — the core can't start",
            leftovers_skip: "workspace checks skipped — the leftover files must be set aside \
                 before the core starts",
            explained: SET_ASIDE_EXPLAINED,
            fresh: "the core starts with a fresh database",
            missing: "no suite state database yet; the core creates one when it starts",
        }
    }

    /// The default workspace's `state.db`, in its local folder (step 2b).
    fn workspace(path: PathBuf) -> Db {
        Db {
            label: "the workspace state database",
            path,
            known: WORKSPACE_STATE_SCHEMA,
            cant_use: "workspace checks skipped — the core can't open the workspace",
            leftovers_skip: "workspace checks skipped — the leftover files must be set aside \
                 before the core opens the workspace",
            explained: WORKSPACE_SET_ASIDE_EXPLAINED,
            fresh: "the core makes a fresh one when it opens the workspace",
            missing: "no workspace state database yet; the core creates one when it opens the \
                 workspace",
        }
    }
}

/// Steps 1 and 2b: the database `db`. A damaged or newer one is explained and set aside as
/// `state.db.damaged-<utc>` (with its `-wal`, `-shm` and `-journal`) only after the person
/// types `y`; a hot rollback journal is offered to SQLite's own recovery first; side files left
/// without their database are set aside the same way. Nothing is offered while a core runs on
/// the channel, and nothing is asked or moved with `--non-interactive`. Returns why step 3 is
/// skipped when a problem remains.
fn state_db(env: &Env, db: &Db, interactive: bool, report: &mut Report) -> Option<String> {
    let path = &db.path;
    let label = db.label;
    // A running core may be writing it: then never `immutable=1`.
    let mut state = inspect_state_db(path, !core_running(env), db.known);
    if state == StateFileState::HotJournal {
        let what = format!(
            "{label} at {} was left mid-write by a crash",
            path.display()
        );
        if !interactive {
            report.problem(&format!(
                "{what} — run lil doctor interactively to let SQLite finish its recovery"
            ));
            return Some(db.cant_use.to_string());
        }
        if core_running(env) {
            report.problem(&format!("{what} — {STOP_THE_CORE}"));
            return Some(db.cant_use.to_string());
        }
        say(&format!(
            "{} at {} was left mid-write by a crash. Nothing has been changed yet. SQLite can \
             finish its recovery: it undoes the write the crash cut short.",
            capitalised(label),
            path.display()
        ));
        if !ask("Let SQLite finish its recovery? [y/N] ") {
            report.problem(&format!(
                "{what} — run lil doctor again and answer y to let SQLite finish its recovery"
            ));
            return Some(db.cant_use.to_string());
        }
        match finish_recovery_at(path, db.known) {
            Ok(StateFileState::Healthy) => {
                report.fixed(&format!("SQLite finished the recovery of {label}"));
                return None;
            }
            Ok(after) => {
                say("SQLite finished its recovery, but the database still can't be used.");
                state = after;
            }
            Err(e) => {
                report.problem(&format!(
                    "{what}, and the recovery couldn't run: {e} — close the LILITH apps, \
                     then run lil doctor again"
                ));
                return Some(db.cant_use.to_string());
            }
        }
    }
    let (what, skip, explain, question, them) = match &state {
        StateFileState::Missing => {
            report.ok(db.missing);
            return None;
        }
        StateFileState::Healthy => {
            report.ok(&format!("{label} is healthy"));
            return None;
        }
        StateFileState::Leftovers(left) => {
            let names: Vec<String> = left
                .iter()
                .filter_map(|p| p.file_name())
                .map(|n| n.to_string_lossy().into_owned())
                .collect();
            (
                format!(
                    "{label} at {} is gone, but SQLite files it left are still there ({})",
                    path.display(),
                    left.iter()
                        .map(|p| p.display().to_string())
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
                db.leftovers_skip.to_string(),
                "A fresh database created beside them could take an old write-ahead log for its \
                 own. Setting them aside renames them; nothing is deleted.",
                format!(
                    "Set them aside as {} so the core starts clean? [y/N] ",
                    names
                        .iter()
                        .map(|n| format!("{n}{{suffix}}"))
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
                "them",
            )
        }
        StateFileState::Damaged(why) => (
            format!("{label} at {} is damaged ({why})", path.display()),
            db.cant_use.to_string(),
            db.explained,
            SET_ASIDE_QUESTION.to_string(),
            "it",
        ),
        StateFileState::TooNew { found, known } => (
            format!(
                "{label} at {} is from a newer version of LILITH (schema {found}; this lil \
                 knows {known})",
                path.display()
            ),
            db.cant_use.to_string(),
            db.explained,
            SET_ASIDE_QUESTION.to_string(),
            "it",
        ),
        StateFileState::HotJournal => (
            format!(
                "{label} at {} still has a hot rollback journal after the recovery",
                path.display()
            ),
            db.cant_use.to_string(),
            db.explained,
            SET_ASIDE_QUESTION.to_string(),
            "it",
        ),
    };
    if !interactive {
        report.problem(&format!(
            "{what} — run lil doctor interactively to set {them} aside"
        ));
        return Some(skip);
    }
    // A running core holds the files open: nothing is offered while one does.
    if core_running(env) {
        report.problem(&format!("{what} — {STOP_THE_CORE}"));
        return Some(skip);
    }
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX));
    say(&format!(
        "{}. Nothing has been changed. {explain}",
        capitalised(&what)
    ));
    if !ask(&question.replace("{suffix}", &set_aside_suffix(now))) {
        report.problem(&format!(
            "{what} — run lil doctor again and answer y to set {them} aside"
        ));
        return Some(skip);
    }
    match set_aside_db(path, now) {
        Ok(moved) => {
            let names: Vec<String> = moved
                .iter()
                .filter_map(|p| p.file_name())
                .map(|n| n.to_string_lossy().into_owned())
                .collect();
            report.fixed(&format!(
                "{what}; set aside as {}, and {}",
                names.join(", "),
                db.fresh
            ));
            None
        }
        Err(e) => {
            report.problem(&format!("{what}, and it couldn't be set aside: {e}"));
            Some(skip)
        }
    }
}

/// The set-aside question for the database itself.
const SET_ASIDE_QUESTION: &str =
    "Set it aside as state.db{suffix} and start with a fresh database? [y/N] ";

/// What setting a damaged or newer suite database aside means, in plain words.
const SET_ASIDE_EXPLAINED: &str = "The core can't start with it. Setting it aside renames it \
    (and its -wal, -shm and -journal files); nothing is deleted. The core then starts with a \
    fresh database: idempotency records of earlier calls stay behind in the set-aside file.";

/// The same for a workspace's database.
const WORKSPACE_SET_ASIDE_EXPLAINED: &str = "The core can't open the workspace with it. Setting \
    it aside renames it (and its -wal, -shm and -journal files); nothing is deleted, and no \
    workspace file is touched. The core then makes a fresh one: the records of earlier calls \
    and of the files the suite last wrote stay behind in the set-aside file, so a file the \
    suite wrote may be reported once as changed outside it.";

/// What to do while a core still holds a state database.
const STOP_THE_CORE: &str = "a LILITH core is running and holds it open; stop it first \
    (lil core stop), then run lil doctor again";

/// Step 3: `workspace.doctor {fix: true}` in the default workspace, then the `core.autocrlf`,
/// legacy-name and Claude Code rules questions. The `claude.*` checks are left out when
/// Claude Code isn't here (`claude_here`).
fn workspace_checks(
    global: &GlobalArgs,
    interactive: bool,
    default: &Path,
    id: String,
    claude_here: bool,
    report: &mut Report,
) -> Result<(), Exit> {
    let mut cfg = config(global, StartPolicy::StartIfNeeded)?;
    cfg.workspace = id;
    cfg.workspace_origin = Some(WorkspaceOrigin {
        path: default.to_path_buf(),
        source: WorkspaceSource::Default,
    });
    let client = match BlockingClient::connect(&cfg) {
        Ok(client) => client,
        Err(ClientError::NotRunning) => {
            report
                .ok("workspace checks skipped: no core runs, and --no-start forbids starting one");
            return Ok(());
        }
        Err(e) => return Err(cannot_run(e)),
    };
    let mut args = Map::new();
    args.insert("fix".into(), Value::Bool(true));
    let output = match client
        .call_tool("workspace.doctor", args, None)
        .map_err(cannot_run)?
    {
        Outcome::Ok(output) => output,
        Outcome::Error { error } => return Err(cannot_run(error.message)),
        Outcome::PendingConfirmation(_) => {
            return Err(cannot_run(
                "workspace.doctor is waiting for a confirmation in the LILITH app.",
            ));
        }
    };
    let mut autocrlf_problem = false;
    let mut legacy_problem = false;
    let mut rules_missing = false;
    for check in output.structured["checks"].as_array().into_iter().flatten() {
        let state = check["state"].as_str().unwrap_or("problem");
        let detail = check["detail"].as_str().unwrap_or_default();
        let id = check["id"].as_str().unwrap_or_default();
        if id.starts_with(CLAUDE_CHECKS) && !claude_here {
            continue;
        }
        if id == CLAUDE_RULES_CHECK && state == "info" {
            rules_missing = true;
        }
        if check["id"] == AUTOCRLF_CHECK && state == "problem" {
            autocrlf_problem = true;
        }
        if check["id"] == LEGACY_CHECK && state == "problem" {
            legacy_problem = true;
        }
        report.line(state, detail);
    }
    if autocrlf_problem && interactive {
        say(
            "Git converts line endings of text files outside projects/ because core.autocrlf \
             is set (often by the git installer, for every repository). lil can set \
             core.autocrlf=false in this repository's own config; your other repositories keep \
             their setting.",
        );
        if ask("Set core.autocrlf=false for this repository only? [y/N] ") {
            match set_repo_autocrlf_false(default) {
                Ok(()) => {
                    // The problem printed above is resolved.
                    report.problems -= 1;
                    report.fixed("core.autocrlf set to false for this repository only");
                }
                Err(e) => report.problem(&format!(
                    "core.autocrlf couldn't be set: {e} — run git config --local core.autocrlf \
                     false in the workspace yourself"
                )),
            }
        }
    }
    if legacy_problem && interactive {
        legacy_names(&client, report)?;
    }
    if rules_missing && interactive {
        claude_rules(&client, report)?;
    }
    Ok(())
}

/// The legacy-name question (owner question 1): only a `y` lists the offer, and the core then
/// renames through its store. The lines of the offer's legacy checks replace the problem.
fn legacy_names(client: &BlockingClient, report: &mut Report) -> Result<(), Exit> {
    say(&format!(
        "This workspace still uses names from before LILITH's rename. Nothing has been changed. \
         The core can rename them: {LEGACY_WORKSPACE_FILE} becomes {WORKSPACE_FILE}, the \
         active-project hint is copied into {WORKSPACE_DIR}\\, and the old-style refs in asset \
         manifests are rewritten to lil://. Nothing is deleted: the {LEGACY_WORKSPACE_DIR}\\ \
         folder and every sealed version stay as they are."
    ));
    if !ask("Rename them now? [y/N] ") {
        return Ok(());
    }
    let mut args = Map::new();
    args.insert("fix".into(), Value::Bool(true));
    args.insert(
        "offers".into(),
        Value::Array(vec![Value::from("legacy_names")]),
    );
    let output = match client
        .call_tool("workspace.doctor", args, None)
        .map_err(cannot_run)?
    {
        Outcome::Ok(output) => output,
        Outcome::Error { error } => {
            report.problem(&format!(
                "the legacy names couldn't be renamed: {} — run lil doctor again",
                error.message
            ));
            return Ok(());
        }
        Outcome::PendingConfirmation(_) => {
            return Err(cannot_run(
                "workspace.doctor is waiting for a confirmation in the LILITH app.",
            ));
        }
    };
    for check in output.structured["checks"].as_array().into_iter().flatten() {
        let Some(id) = check["id"]
            .as_str()
            .filter(|id| id.starts_with(LEGACY_CHECKS))
        else {
            continue;
        };
        if id == LEGACY_CHECK {
            // The problem printed above is answered by this line.
            report.problems -= 1;
        }
        let state = check["state"].as_str().unwrap_or("problem");
        report.line(state, check["detail"].as_str().unwrap_or_default());
    }
    Ok(())
}

/// `text` with its first letter upper-cased.
fn capitalised(text: &str) -> String {
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

/// Writes `text` and a line end to stderr.
fn say(text: &str) {
    let mut err = io::stderr().lock();
    let _ = writeln!(err, "{text}");
    let _ = err.flush();
}

/// Asks `question` on stderr and reads one line from stdin: `true` only for `y` or `yes` (any
/// case). An empty answer, end of input or a read error is no.
fn ask(question: &str) -> bool {
    {
        let mut err = io::stderr().lock();
        let _ = write!(err, "{question}");
        let _ = err.flush();
    }
    let mut answer = String::new();
    if io::stdin().lock().read_line(&mut answer).is_err() {
        return false;
    }
    let answer = answer.trim().to_ascii_lowercase();
    answer == "y" || answer == "yes"
}

// ---------------------------------------------------------------------------------------------
// Step 4: Claude Code (AI-114, AI-117, ARCH-104, ARCH-108; Appendix F).
// ---------------------------------------------------------------------------------------------

/// The plugin as Claude Code names it, `worldwright@lilith`: the plugin's name, then its
/// marketplace's.
const PLUGIN: &str = "worldwright@lilith";
/// The plugin version this suite ships.
const PLUGIN_VERSION: &str = "1.3.0";
/// How long one `claude` command may take.
const CLAUDE_LIMIT: Duration = Duration::from_secs(120);
/// The workspace check whose rules only the person may add.
const CLAUDE_RULES_CHECK: &str = "claude.rules";
/// The workspace checks only shown when Claude Code is here.
const CLAUDE_CHECKS: &str = "claude.";

/// `claude.exe` on `PATH`, when Claude Code is installed. On a test channel only one in this
/// executable's own folder (the test kit's fake) counts, so a test never runs a real Claude
/// Code (QA-040).
fn find_claude(env: &Env) -> Option<PathBuf> {
    let claude = find_on_path("claude.exe")?;
    if matches!(env.channel(), Channel::Test(_)) {
        let own = current_exe_dir().ok()?;
        let dir = claude.parent()?;
        if folder_key(dir) != folder_key(&own) {
            return None;
        }
    }
    Some(claude)
}

/// The folders the running channel trusts for `lil-mcp.exe` and `lil.exe` (ARCH-047), and the
/// one whose paths the doctor names: `%LOCALAPPDATA%\LILITH\bin\` (or the install folder) on
/// stable, the build folder otherwise.
fn trusted_folders(env: &Env) -> Option<(Vec<PathBuf>, PathBuf)> {
    match env.install_dir() {
        Some(install) => {
            let bin = env.local_dir().join("bin");
            Some((vec![bin.clone(), install], bin))
        }
        None => {
            let own = current_exe_dir().ok()?;
            Some((vec![own.clone()], own))
        }
    }
}

/// The plugin's marketplace folder in the checkout this `lil` was built from.
fn marketplace_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("plugin")
}

/// Runs `claude <args>` (the profile is the one `CLAUDE_CONFIG_DIR` names, or the default
/// folder when it isn't set: the doctor never sets it, AI-089). `Err` says what went wrong.
fn run_claude(claude: &Path, args: &[&str]) -> Result<String, String> {
    let os: Vec<&OsStr> = args.iter().map(OsStr::new).collect();
    let cwd = claude.parent().unwrap_or(Path::new("."));
    let shown = format!("claude {}", args.join(" "));
    match run_hidden(claude, &os, cwd, &[], CLAUDE_LIMIT) {
        Ok(out) if out.code == Some(0) => Ok(out.stdout),
        Ok(out) => Err(format!(
            "`{shown}` failed ({}): {}",
            out.code.map_or_else(
                || "it ran too long".to_string(),
                |c| format!("exit code {c}")
            ),
            out.stderr.trim()
        )),
        Err(e) => Err(format!("`{shown}` couldn't run: {e}")),
    }
}

/// The plugin's name (before the `@`), which another marketplace's copy shares.
fn plugin_name() -> &'static str {
    PLUGIN.split_once('@').map_or(PLUGIN, |(name, _)| name)
}

/// `a` is an older version than `b` (dotted numbers; anything else counts as older).
fn older(a: &str, b: &str) -> bool {
    let parse = |v: &str| -> Option<Vec<u64>> { v.split('.').map(|p| p.parse().ok()).collect() };
    match (parse(a), parse(b)) {
        (Some(a), Some(b)) => a < b,
        _ => true,
    }
}

/// Step 4.
fn claude_code(env: &Env, claude: Option<&Path>, interactive: bool, report: &mut Report) {
    let Some(claude) = claude else {
        report.ok("Claude Code (claude.exe) isn't on PATH; Claude Code checks skipped");
        return;
    };
    report.ok(&format!("Claude Code: {}", claude.display()));
    if plugin(claude, interactive, report) {
        plugin_options(env, report);
    }
    skills_inventory(report);
}

/// The plugin in the default profile: installed or updated in place only after `y`, never
/// removed. Whether it is (now) installed at this suite's version.
fn plugin(claude: &Path, interactive: bool, report: &mut Report) -> bool {
    let listed = match run_claude(claude, &["plugin", "list", "--json"]) {
        Ok(text) => text,
        Err(e) => {
            report.problem(&format!(
                "the Claude Code plugins can't be listed: {e} — check that Claude Code works"
            ));
            return false;
        }
    };
    let plugins: Vec<Value> = serde_json::from_str(listed.trim()).unwrap_or_default();
    let ours = plugins.iter().find(|p| p["id"] == PLUGIN);
    for p in &plugins {
        let Some(other) = p["id"].as_str() else {
            continue;
        };
        if other == PLUGIN || other.split('@').next() != Some(plugin_name()) {
            continue;
        }
        // Two enabled copies clash (the same skills and server names); the doctor never
        // removes a plugin, so it names the commands that do.
        let scope = p["scope"].as_str().unwrap_or("user");
        let place = p["projectPath"]
            .as_str()
            .map(|dir| format!(" in {dir}"))
            .unwrap_or_default();
        let detail = format!(
            "another copy of the plugin, {other} {}, is installed in the default Claude Code \
             profile ({scope} scope) and clashes with {PLUGIN} — lil doctor never removes a \
             plugin; remove it yourself{place} with claude plugin uninstall {other} --scope \
             {scope} (or turn it off with claude plugin disable {other} --scope {scope})",
            p["version"].as_str().unwrap_or("unknown")
        );
        if p["enabled"] == false {
            report.line("info", &detail);
        } else {
            report.problem(&detail);
        }
    }
    let update = match ours {
        None => false,
        Some(p) => {
            let version = p["version"].as_str().unwrap_or("unknown");
            if !older(version, PLUGIN_VERSION) {
                report.ok(&format!(
                    "the plugin {PLUGIN} {version} is installed in the default Claude Code profile"
                ));
                if p["enabled"] == false {
                    report.problem(&format!(
                        "the plugin {PLUGIN} is turned off in the default Claude Code profile — \
                         turn it on in Claude Code (/plugin)"
                    ));
                }
                return true;
            }
            true
        }
    };
    let what = match ours {
        None => format!("the plugin {PLUGIN} isn't installed in the default Claude Code profile"),
        Some(p) => format!(
            "the plugin {PLUGIN} in the default Claude Code profile is at {}; this suite's is \
             {PLUGIN_VERSION}",
            p["version"].as_str().unwrap_or("unknown")
        ),
    };
    if !interactive {
        report.problem(&format!(
            "{what} — run lil doctor in a terminal; it asks before it installs or updates it"
        ));
        return false;
    }
    let source = marketplace_dir();
    if !source
        .join(".claude-plugin")
        .join("marketplace.json")
        .is_file()
    {
        report.problem(&format!(
            "{what} — the plugin's files aren't at {}, so lil doctor can't install it",
            source.display()
        ));
        return false;
    }
    let verb = if update { "Update" } else { "Install" };
    say(&format!(
        "{}. Nothing has been changed. lil doctor can add the plugin's marketplace ({}) to the \
         default Claude Code profile and {} {PLUGIN} {PLUGIN_VERSION} there with Claude Code's \
         own commands. It never removes a plugin, and it doesn't fill in the plugin's options: \
         you enter them in Claude Code.",
        capitalised(&what),
        source.display(),
        verb.to_lowercase()
    ));
    if !ask(&format!(
        "{verb} the plugin {PLUGIN} {PLUGIN_VERSION} in the default Claude Code profile? [y/N] "
    )) {
        report.problem(&format!(
            "{what} — run lil doctor again and answer y to {} it",
            verb.to_lowercase()
        ));
        return false;
    }
    let source = source.display().to_string();
    let mut steps: Vec<Vec<&str>> = vec![
        vec!["plugin", "marketplace", "add", &source],
        vec!["plugin", "install", PLUGIN],
    ];
    if update {
        // An install leaves an installed plugin as it is; the update brings it to this version.
        steps.push(vec!["plugin", "update", PLUGIN]);
    }
    for step in steps {
        if let Err(e) = run_claude(claude, &step) {
            report.problem(&format!("{what}, and {e} — run lil doctor again"));
            return false;
        }
    }
    report.fixed(&format!(
        "the plugin {PLUGIN} {PLUGIN_VERSION} is {} in the default Claude Code profile; it \
         loads in the next Claude Code session",
        if update { "updated" } else { "installed" }
    ));
    true
}

/// The plugin's options `lil_mcp_path` and `lil_path`, from the default profile's
/// `settings.json` (`pluginConfigs`): set, and pointing into a folder the running channel
/// trusts. The doctor only reads them; the person enters them in Claude Code (owner question 9,
/// ruling P3-R6).
fn plugin_options(env: &Env, report: &mut Report) {
    let Some((trusted, folder)) = trusted_folders(env) else {
        report.problem(
            "the folder lil runs from can't be found, so the plugin's options can't be checked",
        );
        return;
    };
    let profile = std::env::var_os("CLAUDE_CONFIG_DIR")
        .filter(|v| !v.is_empty())
        .map_or_else(|| env.claude_default_profile_dir(), PathBuf::from);
    let settings: Value = std::fs::read_to_string(profile.join("settings.json"))
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or(Value::Null);
    let options = &settings["pluginConfigs"][PLUGIN]["options"];
    let trusted: Vec<String> = trusted.iter().map(|d| folder_key(d)).collect();
    for (key, exe) in [("lil_mcp_path", "lil-mcp.exe"), ("lil_path", "lil.exe")] {
        let wanted = folder.join(exe);
        let enter = format!(
            "enter {} in that field (in Claude Code: /plugin, {}, its options)",
            wanted.display(),
            plugin_name()
        );
        let Some(value) = options[key].as_str().filter(|v| !v.trim().is_empty()) else {
            report.problem(&format!("the plugin's option {key} isn't set — {enter}"));
            continue;
        };
        let path = Path::new(value.trim());
        let in_trusted = path
            .parent()
            .is_some_and(|d| trusted.contains(&folder_key(d)));
        let named = path
            .file_name()
            .is_some_and(|n| n.to_string_lossy().eq_ignore_ascii_case(exe));
        if in_trusted && named {
            report.ok(&format!("the plugin's option {key} is {value}"));
        } else {
            report.problem(&format!(
                "the plugin's option {key} is {value}, which is not {exe} in a folder this \
                 channel trusts — {enter}"
            ));
        }
    }
}

/// The plugin's skills that still write workspace files directly (ARCH-104), from its
/// `skills-inventory.toml`.
fn skills_inventory(report: &mut Report) {
    let file = marketplace_dir()
        .join(plugin_name())
        .join("skills-inventory.toml");
    let Some(table) = std::fs::read_to_string(&file)
        .ok()
        .and_then(|t| t.parse::<toml::Table>().ok())
    else {
        report.line(
            "info",
            &format!(
                "the plugin's skills inventory ({}) can't be read; the check of skills writing \
                 workspace files directly is skipped",
                file.display()
            ),
        );
        return;
    };
    let direct: Vec<&str> = table
        .get("skill")
        .and_then(toml::Value::as_array)
        .into_iter()
        .flatten()
        .filter(|s| s.get("writes_directly").and_then(toml::Value::as_bool) == Some(true))
        .filter_map(|s| s.get("name").and_then(toml::Value::as_str))
        .collect();
    if direct.is_empty() {
        report.ok("no plugin skill writes workspace files directly");
    } else {
        report.line(
            "info",
            &format!(
                "plugin skills that still write workspace files directly: {} — while a core \
                 runs, the suite's tools are the only writers; these skills move to them in \
                 the skills port",
                direct.join(", ")
            ),
        );
    }
}

/// The Claude Code rules question (AI-114): only a `y` lists the offer, and the core then
/// writes the missing rules into the workspace's `.claude/settings.json` through its store.
fn claude_rules(client: &BlockingClient, report: &mut Report) -> Result<(), Exit> {
    say(
        "The workspace's .claude/settings.json lacks Claude Code permission rules for the \
         suite's tools. Nothing has been changed. The core can add them: allow rules for the \
         tools that read and write, ask rules for the destructive and exec ones, in both names \
         Claude Code gives them. Rules already there, yours included, stay as they are.",
    );
    if !ask("Add the rules to .claude/settings.json? [y/N] ") {
        return Ok(());
    }
    let mut args = Map::new();
    args.insert("fix".into(), Value::Bool(true));
    args.insert(
        "offers".into(),
        Value::Array(vec![Value::from("claude_rules")]),
    );
    let output = match client
        .call_tool("workspace.doctor", args, None)
        .map_err(cannot_run)?
    {
        Outcome::Ok(output) => output,
        Outcome::Error { error } => {
            report.problem(&format!(
                "the Claude Code rules couldn't be added: {} — run lil doctor again",
                error.message
            ));
            return Ok(());
        }
        Outcome::PendingConfirmation(_) => {
            return Err(cannot_run(
                "workspace.doctor is waiting for a confirmation in the LILITH app.",
            ));
        }
    };
    for check in output.structured["checks"].as_array().into_iter().flatten() {
        if check["id"] == CLAUDE_RULES_CHECK {
            let state = check["state"].as_str().unwrap_or("problem");
            report.line(state, check["detail"].as_str().unwrap_or_default());
        }
    }
    Ok(())
}
