//! `workspace.doctor` over the real pipe of an in-process core (ARCH-085, CORE-026, CORE-038,
//! CORE-040, QA-060): the repository's git config, the `.gitignore` and `.gitattributes`
//! entries, the read-only attribute of sealed files, and the problems it lists without fixing.
//!
//! Every core gets the test profile's git environment through `CoreConfig.git_env`, so its git
//! runs never read the system or user config (QA-038); every workspace is under the test's
//! `TestEnv` root. Durations are failure deadlines only (QA-031).

use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use lil_core::identity::{MCP_EXE, TrustedDirs};
use lil_core::store::FailPoints;
use lil_core::{CoreConfig, CoreThread, start_in_thread};
use lil_platform::fs::{FileIdentity, PLACEHOLDER_ATTRIBUTES, Probe, SystemProbe, VolumeInfo};
use lil_platform::process::{ProcessInfo, ProcessInspector, SystemInspector};
use lil_testkit::files::{sha256_bytes, sha256_file, write_file};
use lil_testkit::{RawPipe, TestEnv};
use serde_json::{Value, json};

/// The checks, in the order the tool reports them.
const CHECK_IDS: [&str; 12] = [
    "git.repository",
    "git.core_longpaths",
    "git.lfs_setlockablereadonly",
    "git.core_autocrlf",
    "gitignore.entries",
    "gitattributes.entries",
    "sealed.readonly",
    "workspace.volume",
    "workspace.watcher",
    "workspace.orphans",
    "workspace.needs_repair",
    "workspace.missing",
];

/// The five `.gitignore` entries (Global Constraints, CORE-040).
const GITIGNORE: [&str; 5] = [
    ".lilith/",
    ".trash/",
    "inbox/*",
    "projects/*/assets/*/candidates/",
    "projects/*/assets/*/work/",
];

/// The fixture asset with a sealed `v1.0.0/`.
const CRANE: &str = "projects/sketchbook/assets/chr_paper_crane";

/// Says this test process is `lil-mcp.exe` in a trusted MCP folder, so a hello that declares
/// `mcp` is granted it (ARCH-046); everything else is the real process.
#[derive(Debug)]
struct McpHere {
    dir: PathBuf,
}

impl ProcessInspector for McpHere {
    fn inspect(&self, pid: u32) -> io::Result<ProcessInfo> {
        let mut info = SystemInspector.inspect(pid)?;
        if pid == std::process::id() {
            info.image = self.dir.join(MCP_EXE);
        }
        Ok(info)
    }
}

/// An in-process core with the test profile's git environment and `fp`.
fn core_with(env: &TestEnv, fp: FailPoints) -> CoreThread {
    core_with_probe(env, fp, Arc::new(SystemProbe))
}

/// The real probe, except that files whose name ends in `suffix` carry the cloud placeholder
/// attributes (a cloud-only file, faked without a real sync root).
#[derive(Debug)]
struct CloudOnly {
    suffix: &'static str,
}

impl Probe for CloudOnly {
    fn identity(&self, path: &Path) -> io::Result<FileIdentity> {
        let mut id = SystemProbe.identity(path)?;
        if path.to_string_lossy().ends_with(self.suffix) {
            id.attributes |= PLACEHOLDER_ATTRIBUTES;
        }
        Ok(id)
    }

    fn volume_info(&self, path: &Path) -> io::Result<VolumeInfo> {
        SystemProbe.volume_info(path)
    }

    fn open_read(&self, path: &Path) -> io::Result<std::fs::File> {
        assert!(
            !path.to_string_lossy().ends_with(self.suffix),
            "a cloud-only file was read: {}",
            path.display()
        );
        SystemProbe.open_read(path)
    }
}

fn core_with_probe(env: &TestEnv, fp: FailPoints, probe: Arc<dyn Probe>) -> CoreThread {
    let mcp_dir = env.root().join("mcp bin");
    let base = CoreConfig::for_env(env.env.clone()).expect("core config");
    start_in_thread(CoreConfig {
        test_parent_pid: None,
        probe,
        failpoints: Arc::new(fp),
        git_env: env.git_env(),
        inspector: Arc::new(McpHere {
            dir: mcp_dir.clone(),
        }),
        trusted: TrustedDirs {
            app_dirs: base.trusted.app_dirs.clone(),
            mcp_dirs: vec![mcp_dir],
        },
        ..base
    })
}

fn core(env: &TestEnv) -> CoreThread {
    core_with(env, FailPoints::parse("").unwrap())
}

fn key() -> String {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    format!(
        "doctor-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    )
}

/// A connection whose hello names `ws`, as `caller` (`cli` or `mcp`), checked to be granted.
fn session_as(env: &TestEnv, ws: &Path, caller: &str) -> RawPipe {
    let mut pipe = RawPipe::connect(env);
    let hello = pipe.hello(json!({"workspace": env.workspace_id(ws), "caller_kind": caller}));
    assert_eq!(hello["result"]["status"], "ok", "{hello}");
    assert_eq!(hello["result"]["caller_kind"], caller, "{hello}");
    pipe
}

fn session(env: &TestEnv, ws: &Path) -> RawPipe {
    session_as(env, ws, "cli")
}

/// `workspace.doctor {fix}`: the checks by id, in order.
fn doctor(pipe: &mut RawPipe, fix: bool) -> Vec<(String, String, String)> {
    let out = pipe.tool_ok("workspace.doctor", json!({"fix": fix}), Some(&key()));
    let checks: Vec<(String, String, String)> = out["checks"]
        .as_array()
        .unwrap_or_else(|| panic!("no checks: {out}"))
        .iter()
        .map(|c| {
            (
                c["id"].as_str().unwrap().to_string(),
                c["state"].as_str().unwrap().to_string(),
                c["detail"].as_str().unwrap().to_string(),
            )
        })
        .collect();
    let ids: Vec<&str> = checks.iter().map(|c| c.0.as_str()).collect();
    assert_eq!(ids, CHECK_IDS, "{out}");
    checks
}

/// The `(state, detail)` of check `id`.
fn check<'a>(checks: &'a [(String, String, String)], id: &str) -> (&'a str, &'a str) {
    let c = checks
        .iter()
        .find(|c| c.0 == id)
        .unwrap_or_else(|| panic!("no {id} in {checks:?}"));
    (c.1.as_str(), c.2.as_str())
}

fn lines_of(path: &Path) -> Vec<String> {
    std::fs::read_to_string(path)
        .unwrap_or_else(|e| panic!("{}: {e}", path.display()))
        .lines()
        .map(|l| l.trim_end_matches('\r').to_string())
        .collect()
}

fn file(ws: &Path, rel: &str) -> PathBuf {
    ws.join(rel.replace('/', "\\"))
}

/// Removes a file as an outside program would.
#[allow(clippy::disallowed_methods)]
fn outside_remove(path: &Path) {
    std::fs::remove_file(path).unwrap_or_else(|e| panic!("cannot remove {}: {e}", path.display()));
}

fn readonly(path: &Path) -> bool {
    std::fs::metadata(path)
        .unwrap_or_else(|e| panic!("{}: {e}", path.display()))
        .permissions()
        .readonly()
}

/// The fixture workspace as the default, a git repository.
fn git_fixture(env: &TestEnv, name: &str) -> PathBuf {
    let ws = env.copy_fixture_workspace(name);
    env.register_workspace(&ws, true);
    env.git_init(&ws);
    ws
}

#[test] // spec: CORE-040, ARCH-085, QA-060
fn doctor_sets_git_config_and_gitignore_then_changes_nothing() {
    let env = TestEnv::with_label("Ünïcödé space");
    let ws = git_fixture(&env, "doctor");
    let _core = core(&env);
    let mut pipe = session(&env, &ws);

    let first = doctor(&mut pipe, true);
    for id in [
        "git.core_longpaths",
        "git.lfs_setlockablereadonly",
        "gitignore.entries",
        "gitattributes.entries",
        "sealed.readonly",
    ] {
        assert_eq!(check(&first, id).0, "fixed", "{id}: {first:?}");
    }
    for (id, state, _) in &first {
        assert_ne!(state, "problem", "{id}: {first:?}");
    }
    assert_eq!(
        env.git(&ws, &["config", "--local", "core.longpaths"])
            .trim(),
        "true"
    );
    assert_eq!(
        env.git(&ws, &["config", "--local", "lfs.setlockablereadonly"])
            .trim(),
        "false"
    );
    let ignored = lines_of(&ws.join(".gitignore"));
    for entry in GITIGNORE {
        assert!(ignored.iter().any(|l| l == entry), "{entry}: {ignored:?}");
    }
    assert!(lines_of(&ws.join(".gitattributes")).contains(&"projects/** -text".to_string()));
    assert!(readonly(&file(&ws, &format!("{CRANE}/v1.0.0/crane_b.png"))));

    let watched = [
        ws.join(".gitignore"),
        ws.join(".gitattributes"),
        ws.join(".git").join("config"),
    ];
    let hashes: Vec<String> = watched.iter().map(|p| sha256_file(p)).collect();
    let second = doctor(&mut pipe, true);
    for (id, state, detail) in &second {
        assert_eq!(state, "ok", "{id}: {detail}");
    }
    for (path, before) in watched.iter().zip(&hashes) {
        assert_eq!(&sha256_file(path), before, "{} changed", path.display());
    }
}

#[test] // spec: CORE-040
fn doctor_reports_a_missing_lilith_gitignore_line() {
    use lil_platform::names::{LEGACY_WORKSPACE_DIR, LEGACY_WORKSPACE_FILE, WORKSPACE_FILE};
    let env = TestEnv::new();
    let ws = env.make_legacy_workspace("plugin era");
    // The `.gitignore` a workspace from before the rename has: the legacy folder's line.
    let legacy_line = format!("{LEGACY_WORKSPACE_DIR}/");
    let mut old = vec![legacy_line.clone()];
    old.extend(GITIGNORE[1..].iter().map(|l| (*l).to_string()));
    write_file(
        &ws.join(".gitignore"),
        format!("{}\n", old.join("\n")).as_bytes(),
    );
    let before = sha256_file(&ws.join(".gitignore"));
    env.register_workspace(&ws, true);
    let _core = core(&env);
    let mut pipe = session(&env, &ws);

    // Reported, never fixed without being asked.
    let checks = doctor(&mut pipe, false);
    let (state, detail) = check(&checks, "gitignore.entries");
    assert_eq!(state, "problem", "{detail}");
    assert!(detail.contains(".lilith/"), "{detail}");
    assert!(!detail.contains(&legacy_line), "{detail}");
    assert_eq!(sha256_file(&ws.join(".gitignore")), before);

    // Asked to fix, it adds the line and keeps the legacy one.
    let checks = doctor(&mut pipe, true);
    assert_eq!(check(&checks, "gitignore.entries").0, "fixed");
    let ignored = lines_of(&ws.join(".gitignore"));
    assert_eq!(ignored[0], legacy_line, "{ignored:?}");
    assert!(ignored.iter().any(|l| l == ".lilith/"), "{ignored:?}");
    let checks = doctor(&mut pipe, false);
    assert_eq!(
        check(&checks, "gitignore.entries"),
        ("ok", ".gitignore has its LILITH entries")
    );
    assert!(ws.join(LEGACY_WORKSPACE_FILE).is_file());
    assert!(!ws.join(WORKSPACE_FILE).exists());
}

#[test] // spec: CORE-040
fn doctor_without_git_skips_with_a_note() {
    let env = TestEnv::with_label("Ünïcödé space");
    let ws = env.make_workspace("Ünïcödé no git");
    env.register_workspace(&ws, true);
    let _core = core(&env);
    let mut pipe = session(&env, &ws);

    let checks = doctor(&mut pipe, true);
    assert_eq!(
        check(&checks, "git.repository"),
        ("ok", "not a git repository; git checks skipped")
    );
    for id in &CHECK_IDS[1..4] {
        assert_eq!(check(&checks, id), ("ok", "not a git repository; skipped"));
    }
    assert!(!ws.join(".git").exists(), "the doctor made a repository");
    // The ignore and attribute entries are still the store's to add.
    assert_eq!(check(&checks, "gitignore.entries").0, "fixed");
}

#[test] // spec: CORE-038
fn doctor_lists_orphans_and_needs_repair_without_fixing() {
    let env = TestEnv::with_label("Ünïcödé space");
    let ws = env.copy_fixture_workspace("listed");
    env.register_workspace(&ws, true);
    // An orphan run folder and a manifest that doesn't parse.
    let orphan = format!("{CRANE}/candidates/r_20260101_0000");
    write_file(&file(&ws, &format!("{orphan}/stray.png")), b"stray");
    let lamp = "projects/sketchbook/assets/prop_signal_lamp/prop_signal_lamp.manifest.json";
    write_file(&file(&ws, lamp), b"{ not json");
    let lamp_before = sha256_file(&file(&ws, lamp));

    let _core = core(&env);
    let mut pipe = session(&env, &ws);
    // An expected file removed outside the suite while the workspace is watched.
    pipe.fence(&ws);
    let gone = format!("{CRANE}/v1.0.0/crane_b.png");
    outside_remove(&file(&ws, &gone));
    pipe.fence(&ws);
    for fix in [false, true] {
        let checks = doctor(&mut pipe, fix);
        let (state, detail) = check(&checks, "workspace.orphans");
        assert_eq!(state, "problem", "{detail}");
        assert!(detail.contains(&orphan), "{detail}");
        let (state, detail) = check(&checks, "workspace.needs_repair");
        assert_eq!(state, "problem", "{detail}");
        assert!(detail.contains(lamp), "{detail}");
        let (state, detail) = check(&checks, "workspace.missing");
        assert_eq!(state, "problem", "{detail}");
        assert!(detail.contains(&gone), "{detail}");
        assert!(
            detail.contains(" — "),
            "a problem says what to do: {detail}"
        );
    }
    // Nothing was fixed.
    assert_eq!(
        std::fs::read(file(&ws, &format!("{orphan}/stray.png"))).unwrap(),
        b"stray"
    );
    assert_eq!(sha256_file(&file(&ws, lamp)), lamp_before);
    assert!(!file(&ws, &gone).exists());
}

#[test] // spec: ARCH-079, CORE-038
fn doctor_reports_a_missing_watcher() {
    let env = TestEnv::with_label("Ünïcödé space");
    let ws = env.make_workspace("Ünïcödé unwatched");
    env.register_workspace(&ws, true);
    let _core = core_with(&env, FailPoints::parse("watch.start=unsupported").unwrap());
    let mut pipe = session(&env, &ws);

    let checks = doctor(&mut pipe, true);
    let (state, detail) = check(&checks, "workspace.watcher");
    assert_eq!(state, "problem", "{detail}");
    assert!(
        detail.contains("outside edits are found only by `lil notify` and at open"),
        "{detail}"
    );
}

#[test] // spec: QA-038, CORE-040
fn doctor_git_calls_use_the_configured_git_env() {
    let env = TestEnv::with_label("Ünïcödé space");
    let ws = env.make_workspace("Ünïcödé git env");
    env.register_workspace(&ws, true);
    env.git_init(&ws);
    let profile_config = env.git_profile().join(".gitconfig");
    let base = std::fs::read_to_string(&profile_config).unwrap();
    let _core = core(&env);
    let mut pipe = session(&env, &ws);

    // The test profile, which only CoreConfig.git_env points at, says autocrlf=true.
    write_file(
        &profile_config,
        format!("{base}[core]\n\tautocrlf = true\n").as_bytes(),
    );
    let checks = doctor(&mut pipe, true);
    let (state, detail) = check(&checks, "git.core_autocrlf");
    assert_eq!(state, "problem", "{detail}");
    assert!(detail.contains("core.autocrlf is true"), "{detail}");

    // Without it the setting is unset, whatever this machine's system or user config says.
    write_file(&profile_config, base.as_bytes());
    let checks = doctor(&mut pipe, true);
    assert_eq!(
        check(&checks, "git.core_autocrlf"),
        ("ok", "core.autocrlf is not set")
    );
}

#[test] // spec: CORE-040
fn doctor_tool_reports_autocrlf_and_never_sets_it() {
    let env = TestEnv::with_label("Ünïcödé space");
    let ws = env.make_workspace("Ünïcödé autocrlf");
    env.register_workspace(&ws, true);
    env.git_init(&ws);
    env.git(&ws, &["config", "--local", "core.autocrlf", "true"]);
    env.git(&ws, &["config", "--local", "core.longpaths", "true"]);
    env.git(
        &ws,
        &["config", "--local", "lfs.setlockablereadonly", "false"],
    );
    let config = ws.join(".git").join("config");
    let before = sha256_file(&config);
    let _core = core(&env);

    for caller in ["cli", "mcp"] {
        let mut pipe = session_as(&env, &ws, caller);
        let checks = doctor(&mut pipe, true);
        let (state, detail) = check(&checks, "git.core_autocrlf");
        assert_eq!(state, "problem", "{caller}: {detail}");
        assert!(detail.contains(" — "), "{caller}: {detail}");
        assert_eq!(
            sha256_file(&config),
            before,
            "{caller}: .git\\config changed"
        );
        assert_eq!(
            env.git(&ws, &["config", "--local", "core.autocrlf"]).trim(),
            "true"
        );
    }
    assert!(lines_of(&ws.join(".gitattributes")).contains(&"projects/** -text".to_string()));
}

#[test] // spec: CORE-026
fn doctor_restores_read_only_on_matching_sealed_files() {
    let env = TestEnv::with_label("Ünïcödé space");
    let ws = env.copy_fixture_workspace("sealed");
    env.register_workspace(&ws, true);
    // A second sealed file the manifest records, and the first one edited outside the suite.
    // Neither is read-only, as after a clone or `git checkout`.
    let extra = format!("{CRANE}/v1.0.0/crane_extra.png");
    let extra_bytes = "Ünïcödé extra".as_bytes();
    write_file(&file(&ws, &extra), extra_bytes);
    let manifest_path = file(&ws, &format!("{CRANE}/chr_paper_crane.manifest.json"));
    let mut manifest: Value =
        serde_json::from_str(&std::fs::read_to_string(&manifest_path).unwrap()).unwrap();
    manifest["hashes"]["v1.0.0/crane_extra.png"] =
        json!(format!("sha256:{}", sha256_bytes(extra_bytes)));
    write_file(
        &manifest_path,
        format!("{}\n", serde_json::to_string_pretty(&manifest).unwrap()).as_bytes(),
    );
    // A git-lfs pointer to a sealed file's recorded bytes (not fetched yet, ARCH-085), and a
    // cloud-only sealed file (never read).
    let pointer = format!("{CRANE}/v1.0.0/crane_lfs.png");
    let behind = sha256_bytes(b"the real bytes behind the pointer");
    write_file(
        &file(&ws, &pointer),
        format!("version https://git-lfs.github.com/spec/v1\noid sha256:{behind}\nsize 33\n")
            .as_bytes(),
    );
    let cloud = format!("{CRANE}/v1.0.0/crane_cloud.png");
    write_file(&file(&ws, &cloud), b"cloud bytes");
    manifest["hashes"]["v1.0.0/crane_lfs.png"] = json!(format!("sha256:{behind}"));
    manifest["hashes"]["v1.0.0/crane_cloud.png"] =
        json!(format!("sha256:{}", sha256_bytes(b"cloud bytes")));
    write_file(
        &manifest_path,
        format!("{}\n", serde_json::to_string_pretty(&manifest).unwrap()).as_bytes(),
    );
    let edited = format!("{CRANE}/v1.0.0/crane_b.png");
    write_file(&file(&ws, &edited), b"edited outside the suite");
    for rel in [&extra, &edited, &pointer, &cloud] {
        assert!(!readonly(&file(&ws, rel)), "{rel}");
    }

    let _core = core_with_probe(
        &env,
        FailPoints::parse("").unwrap(),
        Arc::new(CloudOnly {
            suffix: "crane_cloud.png",
        }),
    );
    let mut pipe = session(&env, &ws);
    let report = doctor(&mut pipe, false);
    let (state, detail) = check(&report, "sealed.readonly");
    assert_eq!(state, "problem", "{detail}");
    assert!(
        !readonly(&file(&ws, &extra)),
        "fix false changed an attribute"
    );

    let fixed = doctor(&mut pipe, true);
    let (state, detail) = check(&fixed, "sealed.readonly");
    assert_eq!(
        state, "problem",
        "the edited file stays a problem: {detail}"
    );
    assert!(
        detail.contains(&extra),
        "the restored file is named: {detail}"
    );
    assert!(
        detail.contains(&edited),
        "the edited file is listed: {detail}"
    );
    assert!(
        readonly(&file(&ws, &extra)),
        "the matching file is read-only again"
    );
    assert!(
        !readonly(&file(&ws, &edited)),
        "the edited file was touched"
    );
    assert_eq!(
        std::fs::read(file(&ws, &edited)).unwrap(),
        b"edited outside the suite"
    );
    // The pointer is not fetched, not edited: named as such and left writable.
    assert!(
        detail.contains(&format!("not fetched yet, left writable: {pointer}")),
        "{detail}"
    );
    assert!(
        !readonly(&file(&ws, &pointer)),
        "the pointer was made read-only"
    );
    // The cloud-only file is reported as such, never as unreadable, and never read.
    assert!(
        detail.contains(&format!("cloud-only and weren't read: {cloud}")),
        "{detail}"
    );
    assert!(!detail.contains("can't be checked"), "{detail}");
    assert!(!readonly(&file(&ws, &cloud)));
    // The edited file is the only problem left.
    let differ = detail
        .split("; ")
        .find(|p| p.contains("differ from the hash"))
        .unwrap_or_else(|| panic!("{detail}"));
    assert!(
        !differ.contains(&pointer) && !differ.contains(&cloud),
        "{differ}"
    );
}
