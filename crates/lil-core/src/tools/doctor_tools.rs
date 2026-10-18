//! The doctor's workspace checks (ARCH-016, ARCH-085, CORE-026, CORE-038, CORE-040, QA-060),
//! module `workspace`: `lil doctor` calls `workspace.doctor {fix: true}` over a connection whose
//! hello names the default workspace and prints one line per check.
//!
//! | Tool | Effect | Arguments | `structured` result |
//! |---|---|---|---|
//! | `workspace.doctor` | write | `{fix, offers?}` | `{checks: [{id, state: "ok"\|"fixed"\|"problem"\|"info"\|"skipped", detail}]}` |
//!
//! The checks, in this order:
//! - `git.repository`: whether the workspace root holds `.git`. Without it the three git checks
//!   below are `ok`, "not a git repository; skipped".
//! - `git.core_longpaths` and `git.lfs_setlockablereadonly` (CORE-040, ARCH-085): `ok` when the
//!   repository's own config (`git config --local`) already says `true` and `false`; otherwise
//!   `fix` sets them there (`fixed`), and without `fix` they are a `problem`.
//! - `git.core_autocrlf`: report only. When the effective `core.autocrlf` is `true` or `input`
//!   it is a `problem`; this tool never changes it, whatever `fix` says and whoever calls it (an
//!   agent-callable tool doesn't change persistent git config without the person's
//!   confirmation; `lil doctor` asks, then sets it itself).
//! - `gitignore.entries` and `gitattributes.entries`: the five `.gitignore` entries and
//!   `projects/** -text`, appended through the store when missing (CORE-040 clarification).
//! - `sealed.readonly` (CORE-026): a sealed file (a `hashes` entry of a `v<semver>/` folder that
//!   holds `manifest.snapshot.json`) without the read-only attribute gets it back with `fix`
//!   when its SHA-256 matches the manifest (a clone or checkout drops the attribute); one that
//!   doesn't match is a human edit, listed and left as it is. A git-lfs pointer whose `oid` is
//!   the recorded hash is not fetched yet, not edited (ARCH-085): named and left writable. A
//!   cloud-only placeholder is never read (that would download it): named as cloud-only.
//! - `workspace.volume`: a network or cloud volume, or one without stable file IDs.
//! - `workspace.watcher`: a workspace served without a folder watch (Task 11).
//! - `workspace.orphans`, `workspace.needs_repair`, `workspace.missing` (CORE-038): listed,
//!   never fixed automatically.
//! - `legacy.names` (owner question 1): a workspace from before the rename that still uses the
//!   legacy workspace file (`LEGACY_WORKSPACE_FILE`), the legacy hint
//!   (`LEGACY_WORKSPACE_DIR`'s `state.json`) or legacy refs (`LEGACY_SCHEME`) in its asset
//!   manifests is a `problem`. Only the offer `legacy_names` renames them ([`rename_legacy`]):
//!   `lil doctor` lists it after the person typed `y`, and the core refuses it from an agent or a
//!   non-interactive session (`caller.not_allowed`). `fix` alone never renames anything.
//! - `legacy.sealed_refs`: sealed, read-only `manifest.snapshot.json` files that hold legacy
//!   refs are never rewritten (`info`; `skipped` right after a rename), and those refs still
//!   read.
//!
//! Git runs go through [`Store::git`](crate::store::Store::git) with the core's git
//! environment. A second run changes nothing (QA-060).

use std::ffi::OsString;
use std::path::Path;
use std::sync::Arc;

use lil_platform::fs::Probe;
use lil_platform::names::{
    LEGACY_WORKSPACE_DIR, LEGACY_WORKSPACE_FILE, WORKSPACE_DIR, WORKSPACE_FILE,
};
use lil_protocol::{CallerKind, EffectClass, ErrorBody, codes};
use lil_schema::{AssetRef, LEGACY_SCHEME, Manifest, SCHEME, Version, bare_sha256};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::registry::{CallCtx, Registry, typed_tool};
use super::workspace_tools::{
    GITATTRIBUTES_ENTRY, GITIGNORE_ENTRIES, WORKSPACE_MODULE, volume_warnings, with_lines,
};
use crate::index::FileState;
use crate::projects::active::STATE_JSON;
use crate::store::versions::SNAPSHOT;
use crate::store::write::Expect;
use crate::store::{Store, StoreError, WsPath, fsops};
use crate::workspace::OpenWorkspace;

const DOCTOR_DESCRIPTION: &str = "Check the session's workspace and repair what is safe to \
    repair: the git repository's core.longpaths=true and lfs.setlockablereadonly=false (in the \
    repository's own config), the .gitignore and .gitattributes entries, and the read-only \
    attribute of sealed files whose bytes still match their manifest. It also reports, without \
    changing anything, an inherited core.autocrlf, sealed files edited outside the suite, a \
    network, cloud or ID-less volume, a missing folder watch, orphans, manifests that need \
    repair and missing files. With fix false nothing is changed. Running it twice changes \
    nothing the second time.";

/// The `ok` state.
const OK: &str = "ok";
/// The `fixed` state.
const FIXED: &str = "fixed";
/// The `problem` state.
const PROBLEM: &str = "problem";
/// The `info` state: worth knowing, nothing to do.
const INFO: &str = "info";
/// The `skipped` state: left as it is on purpose.
const SKIPPED: &str = "skipped";

/// `workspace.doctor`'s arguments.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DoctorArgs {
    /// Repair what is safe to repair (git config, ignore and attribute entries, the read-only
    /// attribute of unchanged sealed files). False only reports.
    pub fix: bool,
    /// Repairs that run only when listed here, which `lil doctor` does after the person answers
    /// y. Refused from an agent or a non-interactive session.
    #[serde(default)]
    pub offers: Vec<DoctorOffer>,
}

/// A repair `workspace.doctor` runs only when the call lists it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum DoctorOffer {
    /// Rename the workspace's names from before the rename ([`rename_legacy`]).
    LegacyNames,
}

/// One check's outcome.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct DoctorCheck {
    /// The check, for example `git.core_longpaths`.
    pub id: String,
    /// `ok`, `fixed` or `problem`.
    pub state: String,
    /// What was found or done, in plain words; a `problem` ends with ` — <what to do>`.
    pub detail: String,
}

impl DoctorCheck {
    fn new(id: &str, state: &str, detail: impl Into<String>) -> DoctorCheck {
        DoctorCheck {
            id: id.to_string(),
            state: state.to_string(),
            detail: detail.into(),
        }
    }
}

#[derive(Debug, Serialize, JsonSchema)]
struct DoctorResult {
    checks: Vec<DoctorCheck>,
}

/// What the tool keeps: the core's probe and git environment.
struct Doctor {
    probe: Arc<dyn Probe>,
    git_env: Arc<Vec<(OsString, OsString)>>,
}

/// Registers `workspace.doctor` on `reg`. `git_env` is added to every `git` it runs.
pub fn register_doctor_tools(
    reg: &mut Registry,
    probe: Arc<dyn Probe>,
    git_env: Arc<Vec<(OsString, OsString)>>,
) {
    let doctor = Arc::new(Doctor { probe, git_env });
    reg.register(typed_tool(
        "workspace.doctor",
        DOCTOR_DESCRIPTION,
        EffectClass::Write,
        WORKSPACE_MODULE,
        move |ctx, args: DoctorArgs| {
            let d = doctor.clone();
            Box::pin(async move { run(&d, ctx, args).await })
        },
    ));
}

async fn run(
    d: &Doctor,
    ctx: &CallCtx,
    args: DoctorArgs,
) -> Result<(DoctorResult, String), ErrorBody> {
    let open = ctx.workspace()?.clone();
    let fix = args.fix;
    let rename = args.offers.contains(&DoctorOffer::LegacyNames);
    if !args.offers.is_empty() {
        refuse_offers(ctx)?;
    }
    let mut checks = git_checks(d, &open.store, fix).await;
    checks.push(
        lines_check(
            &open.store,
            ".gitignore",
            "gitignore.entries",
            &GITIGNORE_ENTRIES,
            fix,
        )
        .await,
    );
    checks.push(
        lines_check(
            &open.store,
            ".gitattributes",
            "gitattributes.entries",
            &[GITATTRIBUTES_ENTRY],
            fix,
        )
        .await,
    );
    checks.push(sealed_check(d, &open, fix).await);
    checks.push(volume_check(d, &open.ws.path));
    checks.push(watcher_check(&open));
    checks.extend(listed_checks(&open).await);
    checks.extend(legacy_checks(&open, rename).await);
    let count = |state: &str| checks.iter().filter(|c| c.state == state).count();
    let summary = format!(
        "{} ok, {} fixed, {} problem(s)",
        count(OK),
        count(FIXED),
        count(PROBLEM)
    );
    Ok((DoctorResult { checks }, summary))
}

/// An offer runs only for the person: from `lil` (whose `lil doctor` asks first) or the app, in
/// an interactive session. An agent's call (`mcp`, `assistant`) or a non-interactive session
/// is `caller.not_allowed`, and nothing runs.
fn refuse_offers(ctx: &CallCtx) -> Result<(), ErrorBody> {
    let session = &ctx.session;
    let person = matches!(session.caller, CallerKind::Cli | CallerKind::Ui(_));
    if person && !session.non_interactive {
        return Ok(());
    }
    Err(ErrorBody::new(
        &codes::CALLER_NOT_ALLOWED,
        "workspace.doctor's offers run only from lil doctor in a terminal (or the app), after \
         you answer y; this session can't run them. Nothing was changed.",
        ctx.trace_id.clone(),
    ))
}

// ---------------------------------------------------------------------------------------------
// Git.
// ---------------------------------------------------------------------------------------------

/// `git.repository`, `git.core_longpaths`, `git.lfs_setlockablereadonly`, `git.core_autocrlf`.
async fn git_checks(d: &Doctor, store: &Store, fix: bool) -> Vec<DoctorCheck> {
    const GIT_IDS: [&str; 3] = [
        "git.core_longpaths",
        "git.lfs_setlockablereadonly",
        "git.core_autocrlf",
    ];
    if !store.root().join(".git").exists() {
        let mut checks = vec![DoctorCheck::new(
            "git.repository",
            OK,
            "not a git repository; git checks skipped",
        )];
        checks.extend(
            GIT_IDS
                .iter()
                .map(|id| DoctorCheck::new(id, OK, "not a git repository; skipped")),
        );
        return checks;
    }
    vec![
        DoctorCheck::new("git.repository", OK, "the workspace is a git repository"),
        git_setting(
            d,
            store,
            "git.core_longpaths",
            "core.longpaths",
            "true",
            fix,
        )
        .await,
        git_setting(
            d,
            store,
            "git.lfs_setlockablereadonly",
            "lfs.setlockablereadonly",
            "false",
            fix,
        )
        .await,
        autocrlf_check(d, store).await,
    ]
}

/// One boolean of the repository's own config (`git config --local`): `ok` when it already
/// says `want`, else set with `fix` (`fixed`) or a `problem`.
async fn git_setting(
    d: &Doctor,
    store: &Store,
    id: &str,
    key: &str,
    want: &str,
    fix: bool,
) -> DoctorCheck {
    let env = d.git_env.as_slice();
    let current = match store
        .git(&["config", "--local", "--type=bool", "--get", key], env)
        .await
    {
        Ok(out) => out,
        Err(e) => return git_problem(id, key, &e),
    };
    if current.code == Some(0) && current.stdout.trim() == want {
        return DoctorCheck::new(
            id,
            OK,
            format!("{key} is {want} in this repository's config"),
        );
    }
    if !fix {
        return DoctorCheck::new(
            id,
            PROBLEM,
            format!("{key} isn't {want} in this repository's config — run lil doctor to set it"),
        );
    }
    match store.git(&["config", "--local", key, want], env).await {
        Ok(out) if out.code == Some(0) => DoctorCheck::new(
            id,
            FIXED,
            format!("{key} set to {want} in this repository's config"),
        ),
        Ok(out) => DoctorCheck::new(
            id,
            PROBLEM,
            format!(
                "`git config --local {key} {want}` failed: {} — check the repository's \
                 .git\\config",
                out.stderr.trim()
            ),
        ),
        Err(e) => git_problem(id, key, &e),
    }
}

fn git_problem(id: &str, key: &str, e: &StoreError) -> DoctorCheck {
    DoctorCheck::new(
        id,
        PROBLEM,
        format!("{key} can't be read: {e} — make sure git is installed and on PATH"),
    )
}

/// `git.core_autocrlf`: the effective value (every config the core's git reads), report only.
async fn autocrlf_check(d: &Doctor, store: &Store) -> DoctorCheck {
    const ID: &str = "git.core_autocrlf";
    let out = match store
        .git(&["config", "--get", "core.autocrlf"], d.git_env.as_slice())
        .await
    {
        Ok(out) => out,
        Err(e) => return git_problem(ID, "core.autocrlf", &e),
    };
    if out.code != Some(0) {
        return DoctorCheck::new(ID, OK, "core.autocrlf is not set");
    }
    let value = out.stdout.trim().to_ascii_lowercase();
    let effect = match value.as_str() {
        "true" => "git gives text files outside projects/ CRLF line endings on checkout",
        "input" => "git turns CRLF into LF in text files outside projects/ when they are committed",
        _ => return DoctorCheck::new(ID, OK, format!("core.autocrlf is {value}")),
    };
    DoctorCheck::new(
        ID,
        PROBLEM,
        format!(
            "core.autocrlf is {value}, so {effect} (projects/ itself is kept as is by \
             `projects/** -text`) — run lil doctor in a terminal; it asks before it sets \
             core.autocrlf=false for this repository only"
        ),
    )
}

// ---------------------------------------------------------------------------------------------
// Files.
// ---------------------------------------------------------------------------------------------

/// The entries of a line file (`.gitignore`, `.gitattributes`), appended through the store.
async fn lines_check(
    store: &Store,
    file: &str,
    id: &str,
    lines: &[&str],
    fix: bool,
) -> DoctorCheck {
    let rel = WsPath::parse(file).expect("the doctor's file names are valid");
    let text = match store.read(&rel).await {
        Ok(Some(bytes)) => match String::from_utf8(bytes) {
            Ok(text) => text,
            Err(_) => {
                return DoctorCheck::new(
                    id,
                    PROBLEM,
                    format!("{file} is not UTF-8 text — save it as UTF-8, then run lil doctor"),
                );
            }
        },
        Ok(None) => String::new(),
        Err(e) => {
            return DoctorCheck::new(
                id,
                PROBLEM,
                format!("{file} can't be read: {e} — check the file"),
            );
        }
    };
    let have: Vec<&str> = text.lines().map(str::trim_end).collect();
    let missing: Vec<&str> = lines
        .iter()
        .copied()
        .filter(|l| !have.contains(l))
        .collect();
    if missing.is_empty() {
        return DoctorCheck::new(id, OK, format!("{file} has its LILITH entries"));
    }
    let listed = missing.join(", ");
    if !fix {
        return DoctorCheck::new(
            id,
            PROBLEM,
            format!("{file} lacks {listed} — run lil doctor to add them"),
        );
    }
    let entries: Vec<String> = lines.iter().map(|l| (*l).to_string()).collect();
    let edit = move |current: &str| -> Result<String, StoreError> {
        let wanted: Vec<&str> = entries.iter().map(String::as_str).collect();
        Ok(with_lines(current, &wanted))
    };
    match store.edit_text(&rel, &edit).await {
        Ok(_) => DoctorCheck::new(id, FIXED, format!("added to {file}: {listed}")),
        Err(e) => DoctorCheck::new(
            id,
            PROBLEM,
            format!("{file} can't be written: {e} — add {listed} yourself"),
        ),
    }
}

/// `FILE_ATTRIBUTE_READONLY`.
const READONLY: u32 = 0x1;

/// `sealed.readonly` (CORE-026).
async fn sealed_check(d: &Doctor, open: &OpenWorkspace, fix: bool) -> DoctorCheck {
    const ID: &str = "sealed.readonly";
    let store = &open.store;
    let mut restored = Vec::new();
    let mut writable = Vec::new();
    let mut edited = Vec::new();
    let mut failed = Vec::new();
    let mut not_fetched = Vec::new();
    let mut cloud_only = Vec::new();
    for (rel, recorded) in sealed_files(open).await {
        let abs = rel.abs(store.root());
        let Ok(identity) = d.probe.identity(&abs) else {
            continue; // gone: `workspace.missing` reports it
        };
        if identity.attributes & READONLY != 0 {
            continue;
        }
        if identity.is_cloud_placeholder() {
            // Reading it would download it (ARCH-074): it is left as it is, unread.
            cloud_only.push(rel.as_str().to_string());
            continue;
        }
        match store.hash_and_oid(&rel).await {
            // A git-lfs pointer to the recorded bytes isn't fetched yet, not edited (ARCH-085):
            // git writes the real file over it, so it stays writable.
            Ok(Some((_, Some(oid)))) if oid == recorded => {
                not_fetched.push(rel.as_str().to_string());
            }
            Ok(Some((found, None))) if found == recorded => {
                if !fix {
                    writable.push(rel.as_str().to_string());
                    continue;
                }
                let set = tokio::task::spawn_blocking(move || fsops::set_readonly(&abs, true))
                    .await
                    .unwrap_or_else(|e| Err(std::io::Error::other(e.to_string())));
                match set {
                    Ok(()) => restored.push(rel.as_str().to_string()),
                    Err(e) => failed.push(format!("{rel} ({e})")),
                }
            }
            Ok(Some(_)) => edited.push(rel.as_str().to_string()),
            Ok(None) => {}
            Err(e) => failed.push(format!("{rel} ({e})")),
        }
    }
    let mut notes = Vec::new();
    if !not_fetched.is_empty() {
        notes.push(format!(
            "{} sealed file(s) are git-lfs pointers not fetched yet, left writable: {}",
            not_fetched.len(),
            not_fetched.join(", ")
        ));
    }
    if !cloud_only.is_empty() {
        notes.push(format!(
            "{} sealed file(s) are cloud-only and weren't read: {}",
            cloud_only.len(),
            cloud_only.join(", ")
        ));
    }
    let mut problems = Vec::new();
    if !edited.is_empty() {
        problems.push(format!(
            "{} sealed file(s) differ from the hash their manifest records and are left as they \
             are: {} — they were edited outside LILITH; compare them with the version's \
             manifest",
            edited.len(),
            edited.join(", ")
        ));
    }
    if !writable.is_empty() {
        problems.push(format!(
            "{} sealed file(s) lost their read-only attribute: {} — run lil doctor to set it \
             again",
            writable.len(),
            writable.join(", ")
        ));
    }
    if !failed.is_empty() {
        problems.push(format!(
            "the read-only attribute can't be checked or set on {} — check those files",
            failed.join(", ")
        ));
    }
    let fixed = (!restored.is_empty()).then(|| {
        format!(
            "read-only set again on {} sealed file(s) whose bytes match their manifest: {}",
            restored.len(),
            restored.join(", ")
        )
    });
    let (state, mut parts) = match (problems.is_empty(), fixed) {
        (true, None) if notes.is_empty() => {
            (OK, vec!["every sealed file is read-only".to_string()])
        }
        (true, None) => (OK, vec!["the other sealed files are read-only".to_string()]),
        (true, Some(fixed)) => (FIXED, vec![fixed]),
        (false, fixed) => (PROBLEM, fixed.into_iter().collect()),
    };
    // The notes go before the problems, so a problem's ` — <what to do>` stays last.
    parts.extend(notes);
    parts.extend(problems);
    DoctorCheck::new(ID, state, parts.join("; "))
}

/// Every sealed file the manifests record, with its recorded SHA-256 (bare hex): the `hashes`
/// entries under a `v<semver>/` folder that holds `manifest.snapshot.json` (a legacy version
/// folder isn't sealed).
async fn sealed_files(open: &OpenWorkspace) -> Vec<(WsPath, String)> {
    let mut found = Vec::new();
    if open.index.ready_result().await.is_err() {
        return found;
    }
    for project in projects(&open.ws.path) {
        let Ok(assets) = open.index.assets_of(&project).await else {
            continue;
        };
        for asset in assets.iter().filter(|a| !a.needs_repair) {
            let Ok(Some(bytes)) = open.store.read(&asset.manifest).await else {
                continue;
            };
            let Some((manifest, _)) = std::str::from_utf8(&bytes)
                .ok()
                .and_then(|t| Manifest::parse(t).ok())
            else {
                continue;
            };
            let Some(hashes) = manifest.get("hashes").and_then(|h| h.as_object()) else {
                continue;
            };
            let asset_dir = format!("projects/{}/assets/{}", asset.project, asset.asset_id);
            for (key, value) in hashes {
                let Some(sha) = value.as_str().and_then(bare_sha256) else {
                    continue;
                };
                let Some((folder, _)) = key.split_once('/') else {
                    continue;
                };
                let sealed_folder = folder
                    .strip_prefix('v')
                    .is_some_and(|v| v.parse::<Version>().is_ok());
                if !sealed_folder {
                    continue;
                }
                let Ok(snapshot) = WsPath::parse(&format!("{asset_dir}/{folder}/{SNAPSHOT}"))
                else {
                    continue;
                };
                if !snapshot.abs(open.store.root()).is_file() {
                    continue;
                }
                if let Ok(rel) = WsPath::parse(&format!("{asset_dir}/{key}")) {
                    found.push((rel, sha.to_string()));
                }
            }
        }
    }
    found.sort_by(|a, b| a.0.as_str().cmp(b.0.as_str()));
    found
}

/// The project folders under `<root>\projects`, by name.
fn projects(root: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(root.join("projects"))
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
        .filter_map(|e| e.file_name().to_str().map(str::to_string))
        .filter(|n| lil_schema::is_slug(n))
        .collect();
    names.sort();
    names
}

// ---------------------------------------------------------------------------------------------
// The workspace.
// ---------------------------------------------------------------------------------------------

/// `workspace.volume`.
fn volume_check(d: &Doctor, root: &Path) -> DoctorCheck {
    const ID: &str = "workspace.volume";
    let warnings = volume_warnings(d.probe.as_ref(), root);
    if warnings.is_empty() {
        return DoctorCheck::new(
            ID,
            OK,
            "the workspace is on a local volume with stable file IDs",
        );
    }
    DoctorCheck::new(
        ID,
        PROBLEM,
        format!(
            "{} — a workspace is safest on a local NTFS or ReFS disk outside synced folders",
            warnings.join(" ")
        ),
    )
}

/// `workspace.watcher`.
fn watcher_check(open: &OpenWorkspace) -> DoctorCheck {
    const ID: &str = "workspace.watcher";
    if open.watch.as_ref().is_some_and(|w| w.watching()) {
        return DoctorCheck::new(ID, OK, "the workspace folder is watched for outside edits");
    }
    DoctorCheck::new(
        ID,
        PROBLEM,
        "the workspace folder isn't watched, so outside edits are found only by `lil notify` and \
         at open — after changing files with another program, run `lil notify <path>`",
    )
}

/// `workspace.orphans`, `workspace.needs_repair` and `workspace.missing` (CORE-038): listed,
/// never fixed.
async fn listed_checks(open: &OpenWorkspace) -> Vec<DoctorCheck> {
    let orphans = match open.store.orphans().await {
        Ok(found) => listed(
            "workspace.orphans",
            "no orphans",
            "orphan(s), never deleted",
            found.iter().map(|o| o.as_str().to_string()).collect(),
            "look at them, keep what you need and delete the rest yourself",
        ),
        Err(e) => DoctorCheck::new(
            "workspace.orphans",
            PROBLEM,
            format!("the orphans can't be listed: {e} — check the workspace folder"),
        ),
    };
    let mut needs_repair = Vec::new();
    let mut missing = Vec::new();
    let mut failed = None;
    match open.index.ready_result().await {
        Err(e) => failed = Some(e.to_string()),
        Ok(()) => {
            for project in projects(&open.ws.path) {
                match open.index.assets_of(&project).await {
                    Ok(assets) => needs_repair.extend(
                        assets
                            .iter()
                            .filter(|a| a.needs_repair)
                            .map(|a| a.manifest.as_str().to_string()),
                    ),
                    Err(e) => failed = Some(e.to_string()),
                }
                match open
                    .index
                    .files_in_states(&project, &[FileState::NeedsRepair, FileState::Missing])
                    .await
                {
                    Ok(files) => {
                        for file in files {
                            let path = file.path.as_str().to_string();
                            if file.state == FileState::Missing {
                                missing.push(path);
                            } else {
                                needs_repair.push(path);
                            }
                        }
                    }
                    Err(e) => failed = Some(e.to_string()),
                }
            }
        }
    }
    if let Some(why) = failed {
        let detail = format!("the index can't be read: {why} — run lil doctor again later");
        return vec![
            orphans,
            DoctorCheck::new("workspace.needs_repair", PROBLEM, detail.clone()),
            DoctorCheck::new("workspace.missing", PROBLEM, detail),
        ];
    }
    needs_repair.sort();
    needs_repair.dedup();
    missing.sort();
    vec![
        orphans,
        listed(
            "workspace.needs_repair",
            "no manifest or file needs repair",
            "manifest(s) or file(s) need repair",
            needs_repair,
            "fix them by hand; LILITH never rewrites them",
        ),
        listed(
            "workspace.missing",
            "no expected file is missing",
            "expected file(s) are missing",
            missing,
            "put them back (the same bytes are recognised) or record their removal",
        ),
    ]
}

/// `ok` with `none` when `paths` is empty, else a `problem` listing them.
fn listed(id: &str, none: &str, what: &str, paths: Vec<String>, todo: &str) -> DoctorCheck {
    if paths.is_empty() {
        return DoctorCheck::new(id, OK, none);
    }
    DoctorCheck::new(
        id,
        PROBLEM,
        format!("{} {what}: {} — {todo}", paths.len(), paths.join(", ")),
    )
}

// ---------------------------------------------------------------------------------------------
// Legacy names (owner question 1).
// ---------------------------------------------------------------------------------------------

/// The names from before the rename that a workspace still uses.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LegacyNames {
    /// Its workspace file is the legacy one (`LEGACY_WORKSPACE_FILE`): there is no
    /// [`WORKSPACE_FILE`].
    pub workspace_file: bool,
    /// Its active-project hint is the legacy folder's `state.json` (`LEGACY_WORKSPACE_DIR`):
    /// [`STATE_JSON`] is missing.
    pub state_json: bool,
    /// The unsealed asset manifests that hold a legacy ref (`LEGACY_SCHEME`),
    /// workspace-relative, sorted.
    pub legacy_refs: Vec<String>,
}

impl LegacyNames {
    fn is_empty(&self) -> bool {
        !self.workspace_file && !self.state_json && self.legacy_refs.is_empty()
    }

    /// The names, in plain words.
    fn describe(&self) -> String {
        let mut parts = Vec::new();
        if self.workspace_file {
            parts.push(format!(
                "the workspace file {LEGACY_WORKSPACE_FILE} (becomes {WORKSPACE_FILE})"
            ));
        }
        if self.state_json {
            parts.push(format!(
                "the active-project hint {LEGACY_WORKSPACE_DIR}\\state.json (copied to \
                 {WORKSPACE_DIR}\\state.json; the old folder stays)"
            ));
        }
        if !self.legacy_refs.is_empty() {
            parts.push(format!(
                "{LEGACY_SCHEME} refs (rewritten to {SCHEME}) in {} manifest(s): {}",
                self.legacy_refs.len(),
                self.legacy_refs.join(", ")
            ));
        }
        parts.join("; ")
    }
}

/// The legacy hint, in the legacy workspace data folder.
fn legacy_state_json() -> WsPath {
    WsPath::from_disk(format!("{LEGACY_WORKSPACE_DIR}/state.json"))
}

/// Whether there is something at `rel` (attributes only; a cloud-only file counts).
fn present(root: &Path, rel: &WsPath) -> bool {
    std::fs::symlink_metadata(lil_platform::fs::long_path(&rel.abs(root))).is_ok()
}

/// The asset manifests and the sealed snapshots of the workspace, workspace-relative:
/// `projects/<p>/assets/<a>/<a>.manifest.json` and `.../v<semver>/manifest.snapshot.json`.
/// Folders are listed without following links.
fn manifests_and_snapshots(root: &Path) -> (Vec<WsPath>, Vec<WsPath>) {
    let folders = |dir: &Path| -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(lil_platform::fs::long_path(dir))
            .into_iter()
            .flatten()
            .filter_map(Result::ok)
            .filter(|e| e.file_type().is_ok_and(|t| t.is_dir() && !t.is_symlink()))
            .filter_map(|e| e.file_name().to_str().map(str::to_string))
            .collect();
        names.sort();
        names
    };
    let (mut manifests, mut snapshots) = (Vec::new(), Vec::new());
    for project in projects(root) {
        let assets = format!("projects/{project}/assets");
        for asset in folders(&root.join("projects").join(&project).join("assets")) {
            let dir = format!("{assets}/{asset}");
            let manifest = WsPath::from_disk(format!("{dir}/{asset}.manifest.json"));
            if present(root, &manifest) {
                manifests.push(manifest);
            }
            let abs = WsPath::from_disk(dir.clone()).abs(root);
            for version in folders(&abs) {
                let sealed = version
                    .strip_prefix('v')
                    .is_some_and(|v| v.parse::<Version>().is_ok());
                let snapshot = WsPath::from_disk(format!("{dir}/{version}/{SNAPSHOT}"));
                if sealed && present(root, &snapshot) {
                    snapshots.push(snapshot);
                }
            }
        }
    }
    (manifests, snapshots)
}

/// The legacy refs (`LEGACY_SCHEME`) among the string values of `value`, at any depth: strings
/// that read as a ref ([`AssetRef`]). Keys and strings that only mention one are not refs.
fn find_legacy_refs(value: &Value, out: &mut Vec<String>) {
    match value {
        Value::String(s) if s.starts_with(LEGACY_SCHEME) && s.parse::<AssetRef>().is_ok() => {
            out.push(s.clone());
        }
        Value::Array(items) => items.iter().for_each(|v| find_legacy_refs(v, out)),
        Value::Object(map) => map.values().for_each(|v| find_legacy_refs(v, out)),
        _ => {}
    }
}

/// The JSON of `text` (a leading byte-order mark is skipped), or `None`.
fn json_of(text: &str) -> Option<Value> {
    serde_json::from_str(text.strip_prefix('\u{feff}').unwrap_or(text)).ok()
}

/// `value` with every legacy ref string written with [`SCHEME`] instead.
fn with_new_scheme(value: &Value) -> Value {
    match value {
        Value::String(s) if s.starts_with(LEGACY_SCHEME) && s.parse::<AssetRef>().is_ok() => {
            Value::String(format!("{SCHEME}{}", &s[LEGACY_SCHEME.len()..]))
        }
        Value::Array(items) => Value::Array(items.iter().map(with_new_scheme).collect()),
        Value::Object(map) => Value::Object(
            map.iter()
                .map(|(k, v)| (k.clone(), with_new_scheme(v)))
                .collect(),
        ),
        other => other.clone(),
    }
}

/// `text` (a manifest) with its legacy refs rewritten by targeted edits: each ref's quoted
/// JSON string is replaced where it stands, so every other byte (spacing, line endings, key
/// order, a note that only mentions a legacy ref) stays as it is. The result must read as the
/// same JSON with only those strings changed; otherwise nothing is written.
fn rewrite_legacy_refs(rel: &WsPath, text: &str) -> Result<String, StoreError> {
    let refuse = |why: &str| StoreError::Io {
        path: rel.as_str().to_string(),
        why: why.to_string(),
    };
    let Some(before) = json_of(text) else {
        return Err(refuse(
            "it isn't valid JSON, so its refs can't be rewritten",
        ));
    };
    let mut refs = Vec::new();
    find_legacy_refs(&before, &mut refs);
    refs.sort();
    refs.dedup();
    let mut out = text.to_string();
    for old in &refs {
        let new = format!("{SCHEME}{}", &old[LEGACY_SCHEME.len()..]);
        let (old, new) = (Value::String(old.clone()), Value::String(new));
        out = out.replace(&old.to_string(), &new.to_string());
    }
    if json_of(&out).as_ref() != Some(&with_new_scheme(&before)) {
        return Err(refuse(
            "a legacy ref in it is also written another way (escaped, or as a key), so it \
             can't be rewritten with targeted edits; rewrite it by hand",
        ));
    }
    Ok(out)
}

/// The names from before the rename that `open`'s workspace still uses (owner question 1).
///
/// # Errors
/// `store.io` when a manifest can't be read.
pub async fn legacy_names(open: &OpenWorkspace) -> Result<LegacyNames, ErrorBody> {
    let store = &open.store;
    let root = store.root();
    let file = |name: &str| WsPath::from_disk(name.to_string());
    let workspace_file =
        present(root, &file(LEGACY_WORKSPACE_FILE)) && !present(root, &file(WORKSPACE_FILE));
    let new_hint = WsPath::parse(STATE_JSON).expect("the state.json path is valid");
    let state_json = present(root, &legacy_state_json()) && !present(root, &new_hint);
    let mut legacy_refs = Vec::new();
    for manifest in manifests_and_snapshots(root).0 {
        let bytes = match store.read(&manifest).await {
            Ok(Some(bytes)) => bytes,
            Ok(None) => continue,
            Err(e) => return Err(e.to_error("")),
        };
        let mut refs = Vec::new();
        if let Some(value) = std::str::from_utf8(&bytes).ok().and_then(json_of) {
            find_legacy_refs(&value, &mut refs);
        }
        if !refs.is_empty() {
            legacy_refs.push(manifest.as_str().to_string());
        }
    }
    Ok(LegacyNames {
        workspace_file,
        state_json,
        legacy_refs,
    })
}

/// The sealed snapshots that hold a legacy ref, workspace-relative: never rewritten.
async fn sealed_legacy_refs(open: &OpenWorkspace) -> Vec<String> {
    let mut found = Vec::new();
    for snapshot in manifests_and_snapshots(open.store.root()).1 {
        let Ok(Some(bytes)) = open.store.read(&snapshot).await else {
            continue;
        };
        let mut refs = Vec::new();
        if let Some(value) = std::str::from_utf8(&bytes).ok().and_then(json_of) {
            find_legacy_refs(&value, &mut refs);
        }
        if !refs.is_empty() {
            found.push(snapshot.as_str().to_string());
        }
    }
    found
}

/// Renames what [`legacy_names`] finds, through the store, in this order: the hint (the legacy
/// `state.json`'s bytes are written to [`STATE_JSON`], a new file; the legacy folder is left as
/// it is), the refs (each unsealed manifest by targeted, hash-checked edits), and last the
/// workspace file (moved to [`WORKSPACE_FILE`], never over a file). Nothing is deleted. A
/// sealed snapshot is never touched: it is returned as skipped.
///
/// Each step is one store write, and until the last one the workspace still reads as it did
/// (its file is the legacy one, a missing new hint falls back to the legacy one, and legacy refs
/// still parse), so an interruption leaves a workspace the core serves; the next call finds
/// what is left and finishes it.
///
/// # Errors
/// The first store error; the steps before it stay done.
pub async fn rename_legacy(open: &OpenWorkspace) -> Result<(LegacyNames, Vec<String>), ErrorBody> {
    let found = legacy_names(open).await?;
    let store = &open.store;
    let mut renamed = LegacyNames::default();
    if found.state_json {
        let legacy = legacy_state_json();
        if let Some(bytes) = store.read(&legacy).await.map_err(|e| e.to_error(""))? {
            let new_hint = WsPath::parse(STATE_JSON).expect("the state.json path is valid");
            store
                .write(&new_hint, bytes, Expect::New)
                .await
                .map_err(|e| e.to_error(""))?;
            renamed.state_json = true;
        }
    }
    for manifest in &found.legacy_refs {
        let rel = WsPath::parse(manifest).map_err(|e| e.to_error(""))?;
        let edit_rel = rel.clone();
        let edit = move |current: &str| rewrite_legacy_refs(&edit_rel, current);
        store
            .edit_text(&rel, &edit)
            .await
            .map_err(|e| e.to_error(""))?;
        renamed.legacy_refs.push(manifest.clone());
    }
    if found.workspace_file {
        let (from, to) = (
            WsPath::from_disk(LEGACY_WORKSPACE_FILE.to_string()),
            WsPath::from_disk(WORKSPACE_FILE.to_string()),
        );
        store
            .rename_new(&from, &to)
            .await
            .map_err(|e| e.to_error(""))?;
        renamed.workspace_file = true;
    }
    tracing::info!(workspace = %open.ws.path.display(), ?renamed, "legacy names renamed");
    Ok((renamed, sealed_legacy_refs(open).await))
}

/// `legacy.names` and, when sealed snapshots hold legacy refs, `legacy.sealed_refs`.
async fn legacy_checks(open: &OpenWorkspace, rename: bool) -> Vec<DoctorCheck> {
    const ID: &str = "legacy.names";
    let found = match legacy_names(open).await {
        Ok(found) => found,
        Err(e) => {
            return vec![DoctorCheck::new(
                ID,
                PROBLEM,
                format!(
                    "the names from before the rename can't be checked: {} — run lil doctor \
                     again",
                    e.message
                ),
            )];
        }
    };
    let names = if found.is_empty() {
        DoctorCheck::new(ID, OK, "the workspace uses LILITH's names")
    } else if !rename {
        DoctorCheck::new(
            ID,
            PROBLEM,
            format!(
                "legacy names from before the rename: {} — run lil doctor in a terminal; it \
                 asks before it renames them",
                found.describe()
            ),
        )
    } else {
        match rename_legacy(open).await {
            Ok((renamed, _)) => DoctorCheck::new(
                ID,
                FIXED,
                format!("legacy names renamed: {}", renamed.describe()),
            ),
            Err(e) => DoctorCheck::new(
                ID,
                PROBLEM,
                format!(
                    "legacy names from before the rename: {}; the rename stopped: {} — run \
                     lil doctor again; it finishes what is left",
                    found.describe(),
                    e.message
                ),
            ),
        }
    };
    let mut checks = vec![names];
    let sealed = sealed_legacy_refs(open).await;
    if !sealed.is_empty() {
        let state = if rename && !found.is_empty() {
            SKIPPED
        } else {
            INFO
        };
        checks.push(DoctorCheck::new(
            "legacy.sealed_refs",
            state,
            format!(
                "{} sealed snapshot(s) keep their {LEGACY_SCHEME} refs and are left as they are \
                 (a sealed version is never rewritten, and those refs still read): {}",
                sealed.len(),
                sealed.join(", ")
            ),
        ));
    }
    checks
}
