//! `lil workspace init|use` against the real `lil` and `lil-core` (owner decision 1, ARCH-090,
//! CORE-007), and the `--no-start` probe with a workspace that can't be resolved (ARCH-018).
//!
//! Every test runs on its own `TestEnv` channel; any core `lil` starts is drained when the env
//! drops (QA-041). Every workspace is under the env's root. Durations are failure deadlines
//! only, never measurements (QA-031).

use std::path::{Path, PathBuf};

use lil_testkit::{CoreFixture, LilOutput, TestEnv, ensure_built};

/// The canonical spelling of `path` without `\\?\`, as the core shows workspace paths.
fn canonical(path: &Path) -> String {
    let c = std::fs::canonicalize(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    let text = c.to_string_lossy().into_owned();
    text.strip_prefix(r"\\?\").unwrap_or(&text).to_string()
}

fn arg(path: &Path) -> &str {
    path.to_str().expect("a UTF-8 test path")
}

fn same(a: &Path, b: &Path) -> bool {
    a.exists() && b.exists() && canonical(a).eq_ignore_ascii_case(&canonical(b))
}

/// The default workspace of the test channel's `suite.toml`, and whether `ws` is listed.
fn registered(env: &TestEnv, ws: &Path) -> (Option<PathBuf>, bool) {
    let (default, listed) = env.registered();
    (default, listed.iter().any(|p| same(p, ws)))
}

fn ok(out: &LilOutput) {
    assert_eq!(out.code, 0, "{out:?}");
}

#[test] // spec: ARCH-003, ARCH-098, CORE-007
fn init_creates_and_prints_the_default() {
    let env = TestEnv::with_label("Ünïcödé space");

    // The first workspace becomes the default.
    let first = env.root().join("Ünïcödé first");
    let out = env.lil(&["workspace", "init", arg(&first)]);
    ok(&out);
    let shown = canonical(&first);
    assert_eq!(
        out.stdout,
        format!("workspace created at {shown}\ndefault workspace: {shown}\n"),
        "{out:?}"
    );
    assert!(first.join("lilith.yaml").is_file());
    let (default, listed) = registered(&env, &first);
    assert!(listed);
    assert!(default.is_some_and(|d| same(&d, &first)));

    // A second one, relative to the current folder and named, is registered but isn't the
    // default.
    let out = env.lil_in(
        env.root(),
        &["workspace", "init", "second ws", "--name", "Ünïcödé second"],
    );
    ok(&out);
    let second = env.root().join("second ws");
    assert_eq!(
        out.stdout,
        format!("workspace created at {}\n", canonical(&second)),
        "{out:?}"
    );
    let yaml = std::fs::read_to_string(second.join("lilith.yaml")).unwrap();
    assert!(yaml.lines().any(|l| l == "name: Ünïcödé second"), "{yaml}");
    let (default, listed) = registered(&env, &second);
    assert!(listed);
    assert!(default.is_some_and(|d| same(&d, &first)));

    // Again, with --default: an existing workspace is registered and made the default.
    let out = env.lil(&["workspace", "init", arg(&second), "--default"]);
    ok(&out);
    let shown = canonical(&second);
    assert_eq!(
        out.stdout,
        format!("workspace registered at {shown}\ndefault workspace: {shown}\n"),
        "{out:?}"
    );
    let (default, _) = registered(&env, &second);
    assert!(default.is_some_and(|d| same(&d, &second)));

    // A file is no workspace: the core's message, exit 1.
    let file = env.root().join("a file.txt");
    std::fs::write(&file, b"x").unwrap();
    let out = env.lil(&["workspace", "init", arg(&file)]);
    assert_eq!(out.code, 1, "{out:?}");
    assert_eq!(out.stdout, "", "{out:?}");
    assert!(
        out.stderr.contains("can't be used as a workspace"),
        "{out:?}"
    );
}

#[test] // spec: ARCH-090, CORE-007
fn use_works_with_a_stale_default() {
    let env = TestEnv::new();
    let gone = env.make_workspace("gone");
    let next = env.make_workspace("Ünïcödé next");
    env.register_workspace(&gone, true);
    std::fs::remove_dir_all(&gone).unwrap();
    // The stale default stops commands that name a workspace...
    let status = env.lil(&["status"]);
    assert_eq!(status.code, 2, "{status:?}");

    // ...but never the command that repairs it.
    let out = env.lil(&["workspace", "use", arg(&next)]);
    ok(&out);
    assert_eq!(
        out.stdout,
        format!("default workspace: {}\n", canonical(&next)),
        "{out:?}"
    );
    let (default, listed) = registered(&env, &next);
    assert!(listed);
    assert!(default.is_some_and(|d| same(&d, &next)));
    let status = env.lil(&["status"]);
    ok(&status);
    assert!(
        status
            .stdout
            .contains(&format!("\nworkspace: {}\n", canonical(&next))),
        "{status:?}"
    );

    // A folder that isn't a workspace can't be the default.
    let plain = env.root().join("plain");
    std::fs::create_dir_all(&plain).unwrap();
    let out = env.lil(&["workspace", "use", arg(&plain)]);
    assert_eq!(out.code, 1, "{out:?}");
    assert!(out.stderr.contains("lilith.yaml"), "{out:?}");
}

#[test] // spec: ARCH-018, ARCH-030
fn no_start_with_an_unresolvable_workspace_while_a_core_runs() {
    let env = TestEnv::new();
    let plain = env.root().join("not a workspace");
    std::fs::create_dir_all(&plain).unwrap();

    // Without a core, `--no-start` stays a silent no-op.
    let quiet = env.lil(&[
        "--no-start",
        "--workspace",
        arg(&plain),
        "call",
        "test.echo",
        "{\"text\":\"x\"}",
    ]);
    assert_eq!(
        (quiet.code, quiet.stdout.as_str(), quiet.stderr.as_str()),
        (0, "", ""),
        "{quiet:?}"
    );

    // With one running, the workspace can't be resolved: exit 2 with the plain message.
    let _core = CoreFixture::start(&env);
    let out = env.lil(&[
        "--no-start",
        "--workspace",
        arg(&plain),
        "call",
        "test.echo",
        "{\"text\":\"x\"}",
    ]);
    assert_eq!(out.code, 2, "{out:?}");
    assert_eq!(out.stdout, "", "{out:?}");
    assert!(out.stderr.contains("lilith.yaml"), "{out:?}");
    assert!(out.stderr.contains("not a workspace"), "{out:?}");
}

/// The `--no-start` probe for a running core reports its own failure: a core this client
/// doesn't trust is named, with the way to stop it, not hidden behind the workspace message.
#[test] // spec: ARCH-018, ARCH-027
fn no_start_probe_reports_an_untrusted_core() {
    let env = TestEnv::with_label("Ünïcödé space");
    let plain = env.root().join("not a workspace");
    std::fs::create_dir_all(&plain).unwrap();
    let other = env.root().join("other bin");
    std::fs::create_dir_all(&other).unwrap();
    let copy = other.join("lil-core.exe");
    std::fs::copy(&ensure_built().lil_core, &copy).unwrap();
    let mut fixture = CoreFixture::start_exe(&env, &copy);

    let out = env.lil(&[
        "--no-start",
        "--workspace",
        arg(&plain),
        "call",
        "test.echo",
        "{\"text\":\"x\"}",
    ]);
    assert_eq!(out.code, 2, "{out:?}");
    assert_eq!(out.stdout, "", "{out:?}");
    let forms = [copy.display().to_string(), canonical(&copy)];
    assert!(forms.iter().any(|p| out.stderr.contains(p)), "{out:?}");
    assert!(out.stderr.contains("lil core stop"), "{out:?}");

    let stop = env.lil(&["core", "stop"]);
    assert_eq!(stop.code, 0, "{stop:?}");
    assert_eq!(
        fixture.wait_exit(std::time::Duration::from_secs(30)),
        Some(0)
    );
}
