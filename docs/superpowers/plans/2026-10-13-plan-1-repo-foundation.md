# Plan 1 — Repo Foundation and Process Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Set up the Rust workspace and the process machinery that keeps AI-written code honest:
- the environment provider;
- the protected-test check;
- `ww dev merge` with its git hooks;
- the reviewer App;
- the CI tiers;
- the post-merge backstop.

Every later plan builds inside this machinery.

**Architecture:** A Cargo workspace (Rust 2024 edition, pinned toolchain) of small crates. Dev tooling lives in the `ww` binary under `ww dev …`:
- `guard`, `merge`, `approve`, `hook`, `install-hooks`, `reviewer-setup`, `spec-tests` and `spec-report`.

The tooling is built on two pure, testable libraries:
- the test-diff guard verdict;
- the merge conditions.

These sit behind a `GitHubApi` trait that has a fake for tests. GitHub Actions run four workflows: every-push, PR-ready, nightly and post-merge. There's no Qt yet; Qt arrives in plan 4.

**Tech Stack:**
- Rust 1.99.0 (MSVC), Cargo workspace, `cargo-nextest`, `cargo-deny`, `cargo-audit`, `cargo-llvm-cov`.
- Crates: `clap`, `serde`, `toml`, `serde_json`, `globset`, `reqwest` (blocking, rustls), `jsonwebtoken`, `age` (passphrase), `rpassword`, `sha2`, `tempfile`, `windows` (in `ww-platform` only), `thiserror`, `anyhow`.
- GitHub Actions on GitHub-hosted Windows runners. Git hooks run through Git for Windows' `sh`.

**Spec:** `docs/superpowers/specs/2026-10-12-lilith-design.md`. The rules this plan implements:
- §1.8: ARCH-109–113;
- §1.6: ARCH-095, ARCH-097;
- §6.2: QA-017–QA-021, QA-024, QA-027–QA-031, QA-033–QA-037;
- §6.3: QA-038, QA-040;
- §6.4: QA-061 — only the project-settings pattern part, here for QA-036;
- §6.5: QA-068;
- §6.6: QA-069–QA-073 tier skeletons, QA-079 as amended in Task 1.

Roadmap: `docs/superpowers/plans/2026-10-12-roadmap.md` (plan 1). M0 results: `docs/spikes/README.md`.

## Global Constraints

- **Toolchain:** Rust **1.99.0**, `x86_64-pc-windows-msvc`, edition **2024**, pinned in `rust-toolchain.toml` with `rustfmt` and `clippy`. MSVC 19.44 (VS 2022 Build Tools). Load the shell environment with `. spikes/toolchain/env.ps1` (from M0).
- **Repo:** a local clone (`<repo>`) of the suite repository on GitHub. The private repository is the development archive; Task 15 creates the public repository from this branch's tree (Amendment A). Work on branch `plan-1-foundation`, cut from `main` after the plan-1 docs PR merges. Commit after every task; every message ends with `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`. Push the branch when the plan is complete. Merging happens through the owner (Task 15), never by an implementer.
- **Crates in this plan** (ARCH-110):

  | Crate | Purpose |
  |---|---|
  | `ww-schema` | skeleton |
  | `ww-protocol` | skeleton |
  | `ww-platform` | the only crate that calls Windows APIs (ARCH-111) |
  | `ww-testkit` | |
  | `ww-core` | skeleton plus the `store` module stub |
  | `ww-cli` | binary named `ww` |
  | `ww-spec-tests` | protected tests |

  Nothing else.
- **Dependency boundaries** (ARCH-111, QA-027): `ww-schema` and `ww-protocol` have no `windows*`, `winapi*`, `cxx*` or `qt*` crate anywhere in their normal dependency closure.
- **Tests** (QA-031): no wall-clock time, randomness or network in unit or integration tests. GitHub is reached only through the `GitHubApi` trait, which has a fake in tests. Filesystem tests use `tempfile` dirs and `ww-testkit`'s `TestEnv`.
- **Isolation** (QA-040, QA-041): nothing in this plan's tests reads or writes:
  - `%APPDATA%\Worldwright*` or `%LOCALAPPDATA%\Worldwright*`;
  - any `<volume>:\.worldwright-local`;
  - `HKCU\Software\Worldwright`;
  - real Credential Manager targets;
  - the user's workspace (`<workspace>`).
- **Names that are contracts** (other plans and CI depend on them):
  - Pipe name: `\\.\pipe\worldwright-core-<userSID>-<channel>` (ARCH-024).
  - Channels: `stable`, `dev`, or `test-<id>`.
  - Data folders: channel `stable` uses `%APPDATA%\Worldwright` and `%LOCALAPPDATA%\Worldwright`. Every other channel appends `-<channel>`, e.g. `%APPDATA%\Worldwright-dev`.
  - Registry root: `HKCU\Software\Worldwright` for stable, `HKCU\Software\Worldwright-<channel>` otherwise. Tests use `HKCU\Software\Worldwright-test-<id>`.
  - Credential prefix: `Worldwright/` for stable, `Worldwright-<channel>/` otherwise.
  - Notification AUMID: `Worldwright.Suite.<Channel>`.
  - Workspace hash: the first **16 lowercase hex chars of SHA-256** over the canonical workspace path. Canonical means: `std::fs::canonicalize`, `\\?\` stripped, ASCII-lowercased, using `\` separators.
- **CI check names** (exact strings `ww dev merge` requires):
  - `every-push`: job in `.github/workflows/ci-push.yml`;
  - `pr-ready`: job in `ci-pr-ready.yml`;
  - `protected-test-check`: job in `ci-guard.yml`.
- **Merge guard configuration** lives in `dev/merge-guard.toml` (protected):
  - `owner = "xenofei"`
  - `repo = "Worldwright-Suite"`
  - `base_branch = "main"`
  - `reviewer_app_slug = "worldwright-reviewer"`
  - `required_checks = ["every-push", "pr-ready", "protected-test-check"]`
  - `hooks_path = "tools/git-hooks"`
- **Reviewer key** (owner-only): `%USERPROFILE%\.worldwright\reviewer\app-key.age`, which is the GitHub App private key encrypted with an `age` passphrase. It's never in the repo, never in suite data folders, and never decrypted without an interactive passphrase.
- **GitHub access** for agents and tooling: `GITHUB_TOKEN` if set, otherwise the output of `gh auth token`. Never log or print a token.
- **Protected-path patterns** (QA-019), in `dev/protected-paths.txt`, gitignore-style, one per line:
  - `crates/ww-spec-tests/**` (spec and golden tests, their fixtures and data);
  - `**/proptest-regressions/**`;
  - `test-params.toml`;
  - `tools/git-hooks/**`;
  - `crates/ww-cli/src/dev/**`;
  - `crates/ww-dev-guard/**` — reserved: no such crate exists in this plan, but the pattern stays in the list;
  - `dev/protected-paths.txt`;
  - `dev/merge-guard.toml`;
  - `.github/workflows/ci-guard.yml`;
  - `.github/workflows/post-merge.yml`.

## Review Focus

1. **A protected file is changed by a rename or a mode-only change**, e.g. a spec test moved to another folder.
   - **Expected:** treated as a modification needing approval, never as an "add".
   - **Covered by:** Task 7, `rename_out_of_protected_needs_approval` and `rename_within_protected_needs_approval`.
2. **The PR's head moves between the check and the merge**, because an agent pushes again.
   - **Expected:** the merge fails, and nothing unchecked slips in.
   - **Covered by:** Task 9, `merge_is_pinned_to_checked_sha`; Task 8's fake returns 409 for a stale sha.
3. **CI hasn't reported yet, or reported only some checks**, for the head commit.
   - **Expected:** a refusal that names the missing checks in plain language. A missing check is never treated as passing.
   - **Covered by:** Task 9, `missing_check_refuses` and `pending_check_refuses`.
4. **`ww dev approve` is run non-interactively** (an agent pipes input) or with a wrong passphrase.
   - **Expected:** refusal with no review posted.
   - **Covered by:** Task 11, `approve_refuses_without_tty` and `wrong_passphrase_posts_nothing`.
5. **A test resolves to the owner's real Worldwright folders**, because a channel defaulted to `stable` inside a test build.
   - **Expected:** an immediate panic naming the path.
   - **Covered by:** Task 5, `guard_panics_on_real_appdata` and `guard_panics_on_stable_channel_in_tests`.

---

### Task 1: Spec changes first (QA-029)

**Files:**
- Modify: `docs/superpowers/specs/2026-10-12-lilith-design.md` (no renumbering; ID check stays `922 922 []`)

**Interfaces:**
- Produces: the spec text later tasks implement.

- [ ] **Step 1: Apply these edits, each marked "(owner decision 2026-10-13)" or "(plan 1 clarification)":**
  - **QA-020:** the separate reviewer identity is a **GitHub App** named `worldwright-reviewer`, installed on the repo with *Pull requests: write* and *Contents: read*. Approvals are posted only by `ww dev approve <PR>`, which needs the owner's passphrase for the App key (`%USERPROFILE%\.worldwright\reviewer\app-key.age`), typed interactively. An approval counts only when it is the App bot's `APPROVED` review on the PR's exact head commit.
  - **QA-079:** (superseded by Amendment A; the waiver below is withdrawn and agents use a restricted token, Task 13) project decision: agents keep using the maintainer's GitHub login, which can change workflow files. The token restriction is waived. The repository's default workflow token stays read-only. The post-merge check (QA-035) and the protected listing of `ci-guard.yml` and `post-merge.yml` (QA-019) are the backstop. Add a sentence that this weakens QA-079, and that moving agents to a fine-grained token without the *Workflows* permission restores it.
  - **ARCH-111 and QA-027:** "checked by `cargo-deny`" becomes "checked by a dependency-boundary test over `cargo metadata`, alongside `cargo-deny`'s advisory, licence and duplicate checks". The reason: transitive Windows dependencies of common crates make `cargo-deny` wrappers brittle.
  - **QA-038 / ARCH-095:** add the data-folder and registry naming per channel exactly as in this plan's Global Constraints, and the workspace-hash definition (first 16 lowercase hex chars of SHA-256 of the canonical path). Mark it as a contract.
  - **QA-021:** protected tests are the `ww-spec-tests` crate. Expected-to-fail spec tests are listed in `crates/ww-spec-tests/expected-fail.toml`. A change may remove entries freely. It may add an entry only for a test file the same change adds; any other addition needs approval.

- [ ] **Step 2: Verify the IDs.**

Run: `python -I -c "import re;t=open('docs/superpowers/specs/2026-10-12-lilith-design.md',encoding='utf-8').read();ids=re.findall(r'(?m)^- \*\*([A-Z]+-\d{3})\*\* \[',t);refs=set(re.findall(r'\b(?:ARCH|CORE|APP|AI|IMG|QA|OPS)-\d{3}\b',t));print(len(ids),len(set(ids)),sorted(refs-set(ids)))"`
Expected: `925 925 []` (922 when Task 1 ran; Task 16 added QA-086, QA-087 and OPS-108)

- [ ] **Step 3: Commit.**

```bash
git add docs/superpowers/specs/2026-10-12-lilith-design.md
git commit -m "spec: plan-1 owner decisions (reviewer App, agent token) and clarifications"
```

---

### Task 2: Cargo workspace, pins and the dependency-boundary test (ARCH-110, ARCH-111, QA-027)

**Files:**
- Create: `Cargo.toml` — workspace, `resolver = "3"`, `[workspace.package] edition = "2024"`, `rust-version = "1.99"`, shared `[workspace.dependencies]`, and `[workspace.lints.rust] unsafe_op_in_unsafe_fn = "deny"`.
- Create: `rust-toolchain.toml` — `channel = "1.99.0"`, `components = ["rustfmt", "clippy"]`.
- Create: `rustfmt.toml` — `edition = "2024"`.
- Create: `deny.toml` — advisories (deny vulnerabilities and unmaintained), licences (allow MIT, Apache-2.0, BSD-2/3, ISC, Unicode-3.0, Zlib, MPL-2.0), and bans (`multiple-versions = "warn"`).
- Create: `crates/ww-schema/{Cargo.toml,src/lib.rs}`, `crates/ww-protocol/{Cargo.toml,src/lib.rs}` — doc-comment-only libs naming their role from the ARCH-110 table.
- Create: `crates/ww-testkit/tests/dependency_boundaries.rs`.
- Modify: `.gitignore` — add `/target/`.

**Interfaces:**
- Produces: the workspace that all later crates join; `cargo metadata` resolves.

- [ ] **Step 1: Write the failing test.** `dependency_boundaries.rs` runs `cargo metadata --format-version 1` (using `env!("CARGO")`), walks the resolve graph from each package, and asserts:

```rust
#[test] // spec: ARCH-111, QA-027
fn schema_and_protocol_have_no_platform_deps() {
    for root in ["ww-schema", "ww-protocol"] {
        let closure = normal_dependency_closure(root);
        let bad: Vec<_> = closure.iter()
            .filter(|n| n.starts_with("windows") || n.starts_with("winapi")
                     || n.starts_with("cxx") || n.starts_with("qt"))
            .collect();
        assert!(bad.is_empty(), "{root} depends on {bad:?}");
    }
}
```

`normal_dependency_closure(root: &str) -> BTreeSet<String>` follows only `dep_kinds` with `kind == null` (normal), over the target `x86_64-pc-windows-msvc`.

- [ ] **Step 2: Run it and watch it fail.**

Run: `cargo nextest run -p ww-testkit --test dependency_boundaries`
Expected: FAIL. `ww-testkit` doesn't exist yet. Create its `Cargo.toml` with only `[dev-dependencies] serde_json`, and an empty `src/lib.rs`. Then the test fails on a missing helper, not on the boundary.

- [ ] **Step 3: Implement `normal_dependency_closure`** in the test file, and create the crates listed above.

- [ ] **Step 4: Run the checks.**

Run: `cargo nextest run -p ww-testkit --test dependency_boundaries && cargo fmt --check && cargo clippy --workspace -- -D warnings && cargo deny check`
Expected: PASS everywhere.

- [ ] **Step 5: Commit.**

```bash
git add Cargo.toml Cargo.lock rust-toolchain.toml rustfmt.toml deny.toml .gitignore crates/
git commit -m "build: cargo workspace, toolchain pin, dependency-boundary test"
```

---

### Task 3: `ww-core` skeleton with the single file-write allowance (ARCH-112, QA-024)

**Files:**
- Create: `crates/ww-core/Cargo.toml`, `crates/ww-core/src/lib.rs` (`pub mod store;`), `crates/ww-core/src/store.rs`.
- Create: `crates/ww-core/clippy.toml`. It lists `disallowed-methods` for:
  - `std::fs::write`, `std::fs::rename`, `std::fs::remove_file`, `std::fs::remove_dir`, `std::fs::remove_dir_all`, `std::fs::copy`, `std::fs::create_dir`, `std::fs::create_dir_all`;
  - `std::fs::File::create`, `std::fs::File::create_new`, `std::fs::OpenOptions::open`;
  - `tokio::fs::write`, `tokio::fs::rename`, `tokio::fs::remove_file`, `tokio::fs::remove_dir_all`.

  Each entry has a `reason` naming ARCH-112.
- Create: `crates/ww-testkit/tests/single_write_allowance.rs`.

**Interfaces:**
- Produces: `ww_core::store::write_file_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()>`. It's a stub that plan 2 completes. In this plan it writes `<path>.tmp-<pid>`, `sync_all`s, then renames, and it's the only place with `#[allow(clippy::disallowed_methods)]`.

- [ ] **Step 1: Write the failing tests.**
  - `single_write_allowance.rs`: scan every `*.rs` under `crates/ww-core/src`. Assert that `allow(clippy::disallowed_methods)` and `expect(clippy::disallowed_methods)` occur **only** in `src/store.rs`, at most once per function.
  - In `store.rs`: a unit test `atomic_write_replaces_contents`. In a tempdir, write "a", then "b"; read back "b"; no `.tmp-` file remains.

- [ ] **Step 2: Run them.**

Run: `cargo nextest run -p ww-core -p ww-testkit`
Expected: FAIL. `write_file_atomic` isn't defined.

- [ ] **Step 3: Implement `write_file_atomic`** with the allowance on that function only. Add a deliberate check: in a scratch copy, a temporary `std::fs::write` in `lib.rs` must make `cargo clippy -p ww-core -- -D warnings` fail. Run it once, record the output in the report, then remove the line.

- [ ] **Step 4: Run all three checks.**

Run: `cargo nextest run -p ww-core -p ww-testkit && cargo clippy -p ww-core -- -D warnings`
Expected: PASS. The report includes the deliberate-violation clippy failure.

- [ ] **Step 5: Commit.**

```bash
git add crates/ww-core crates/ww-testkit/tests/single_write_allowance.rs Cargo.toml Cargo.lock
git commit -m "core: store-only file writes enforced by clippy (ARCH-112)"
```

---

### Task 4: Environment provider (ARCH-095, ARCH-097, QA-038)

**Files:**
- Create: `crates/ww-platform/Cargo.toml` (deps: `windows` with `Win32_UI_Shell`, `Win32_Storage_FileSystem`, `Win32_Security`, `Win32_System_Threading`; `sha2`).
- Create: `crates/ww-platform/src/{lib.rs,env.rs,known_folders.rs,volume.rs,sid.rs}`.
- Test: unit tests in `env.rs`.

**Interfaces:**

Produces:

```rust
pub enum Channel { Stable, Dev, Test(String) }        // Display: "stable" | "dev" | "test-<id>"
pub struct Roots { pub roaming: PathBuf, pub local: PathBuf, pub user_profile: PathBuf }
pub struct Env { /* private */ }
impl Env {
    pub fn from_system(channel: Channel) -> Result<Env, EnvError>;     // known folders via SHGetKnownFolderPath
    pub fn with_roots(channel: Channel, roots: Roots, user_sid: String) -> Env; // used by ww-testkit
    pub fn channel(&self) -> &Channel;
    pub fn data_dir(&self) -> PathBuf;          // <roaming>\Worldwright[-<channel>]
    pub fn local_dir(&self) -> PathBuf;         // <local>\Worldwright[-<channel>]
    pub fn logs_dir(&self) -> PathBuf;          // local_dir\logs
    pub fn workspace_hash(canonical: &Path) -> String;                  // 16 hex, per Global Constraints
    pub fn workspace_local_dir(&self, workspace: &Path) -> Result<PathBuf, EnvError>;
    pub fn registry_root(&self) -> String;      // "HKCU\\Software\\Worldwright[-<channel>]"
    pub fn credential_prefix(&self) -> String;  // "Worldwright[-<channel>]/"
    pub fn pipe_name(&self) -> String;          // \\.\pipe\worldwright-core-<sid>-<channel>
    pub fn notification_aumid(&self) -> String; // "Worldwright.Suite.<Channel>"
    pub fn claude_default_profile_dir(&self) -> PathBuf; // <user_profile>\.claude
    pub fn claude_profiles_dir(&self) -> PathBuf;        // local_dir\claude-profiles
}
```

`workspace_local_dir` compares volume roots using `GetVolumePathNameW`:
- same volume as `local_dir` → `local_dir\workspaces\<hash>`;
- otherwise → `<volume root>\.worldwright-local\<hash>`.

`ww-platform` exposes `pub fn volume_root(path: &Path) -> io::Result<PathBuf>` behind a `VolumeResolver` trait, so tests can inject one.

- [ ] **Step 1: Write the failing unit tests** in `env.rs`, using `with_roots` and an injected fake `VolumeResolver`:
  - `stable_paths`: data_dir is `R\Worldwright`; registry root is `HKCU\Software\Worldwright`; credential prefix is `Worldwright/`.
  - `dev_paths_are_suffixed`: `R\Worldwright-dev`; `HKCU\Software\Worldwright-dev`; `Worldwright-dev/`.
  - `pipe_name_format`: `\\.\pipe\worldwright-core-S-1-5-21-1-2-3-1001-dev`.
  - `workspace_hash_is_16_hex_and_case_insensitive`: `C:\Ws\A` and `c:\ws\a` give the same hash, 16 chars long, `[0-9a-f]`.
  - `same_volume_goes_under_local`: workspace `C:\x`, local `C:\L` → `C:\L\Worldwright\workspaces\<h>`.
  - `other_volume_goes_to_worldwright_local`: workspace `D:\x` → `D:\.worldwright-local\<h>`.

- [ ] **Step 2: Run them.**

Run: `cargo nextest run -p ww-platform`
Expected: FAIL. Nothing is implemented.

- [ ] **Step 3: Implement it.** Use `SHGetKnownFolderPath` for `FOLDERID_RoamingAppData`, `FOLDERID_LocalAppData` and `FOLDERID_Profile`. Get the SID from the process token (`GetTokenInformation(TokenUser)` + `ConvertSidToStringSidW`).

- [ ] **Step 4: Run the tests and lint.**

Run: `cargo nextest run -p ww-platform && cargo clippy -p ww-platform -- -D warnings`
Expected: PASS.

- [ ] **Step 5: Commit.**

```bash
git add crates/ww-platform Cargo.toml Cargo.lock
git commit -m "platform: environment provider keyed by channel (ARCH-095/097, QA-038)"
```

---

### Task 5: `TestEnv` and the harness guard (QA-038, QA-040)

**Files:**
- Create: `crates/ww-testkit/src/{lib.rs,env.rs}`.
- Modify: `crates/ww-platform/src/env.rs` to add the guard hook.

**Interfaces:**

Consumes:
- `ww_platform::Env::with_roots`
- `Channel`

Produces:

```rust
// ww-platform
pub fn arm_test_guard(real: RealLocations);            // process-global; once armed, every Env path method checks
pub struct RealLocations { pub roaming: PathBuf, pub local: PathBuf, pub registry_root: String, pub credential_prefix: String }
// ww-testkit
pub struct TestEnv { pub env: Env, _dir: tempfile::TempDir }
impl TestEnv { pub fn new() -> TestEnv }                  // channel Test(<8 hex>), roots under a fresh tempdir, arms the guard with the real locations from Env::from_system(Channel::Stable)
```

Once armed, any `Env` method whose result is in a real location panics with `"test resolved a real Worldwright location: <value>"`:
- under the real roaming or local `Worldwright*` folders;
- any `.worldwright-local`;
- the real registry root;
- the real credential prefix.

Constructing a `Stable` or `Dev` channel `Env` after arming also panics.

- [ ] **Step 1: Write the failing tests** in `ww-testkit/src/env.rs`:
  - `testenv_paths_are_under_tempdir`
  - `guard_panics_on_real_appdata`: arm, then build an `Env` whose roaming root is the real one, call `data_dir()`, and expect `#[should_panic(expected = "real Worldwright location")]`.
  - `guard_panics_on_stable_channel_in_tests`
  - `registry_and_credentials_are_test_scoped`: the values start with `HKCU\Software\Worldwright-test-` and `Worldwright-test-`.

- [ ] **Step 2: Run them.**

Run: `cargo nextest run -p ww-testkit`
Expected: FAIL. `TestEnv` doesn't exist.

- [ ] **Step 3: Implement it.**

- [ ] **Step 4: Run them again.**

Run: `cargo nextest run -p ww-testkit`
Expected: PASS.

- [ ] **Step 5: Commit.**

```bash
git add crates/ww-testkit crates/ww-platform
git commit -m "testkit: TestEnv and the real-location harness guard (QA-040)"
```

---

### Task 6: Test parameters, nextest profiles and the spec-test runner (QA-017, QA-019, QA-030)

**Files:**
- Create: `test-params.toml`, with keys:
  - `[timeouts] unit_s = 30, integration_s = 120, spec_s = 300`
  - `[proptest] cases = 256`
  - `[images] max_channel_diff = 1` (IMG-036)
- Create: `.config/nextest.toml`.
  - Profile `default`: `retries = 1`, `slow-timeout = { period = "30s", terminate-after = 4 }`, `fail-fast = false`.
  - Profile `spec`: JUnit output at `target/nextest/spec/junit.xml`.
- Create: `crates/ww-spec-tests/{Cargo.toml,src/lib.rs,tests/README.md,expected-fail.toml}`. The tests README says what spec tests may use: only the `ww` CLI, file formats, events, `ww-protocol` descriptors and the `ww-testkit` API (QA-018). `expected-fail.toml` contains `tests = []` (each entry is `{ name = "...", rule = "QA-xxx", file = "tests/....rs" }`).
- Create: `crates/ww-spec-tests/tests/smoke.rs` with one passing test `spec_harness_runs`, citing `// spec: QA-017`.
- Create: `crates/ww-cli/{Cargo.toml,src/main.rs,src/dev/mod.rs,src/dev/spec_tests.rs}`, with a `clap` derive: `ww dev spec-tests`.

**Interfaces:**

Produces:

```rust
// ww-cli/src/dev/spec_tests.rs
pub struct JunitCase { pub name: String, pub failed: bool }
pub fn parse_junit(xml: &str) -> Vec<JunitCase>;
pub struct ExpectedFail { pub name: String, pub rule: String, pub file: String }
pub enum SpecVerdict { Pass, UnexpectedFailures(Vec<String>), NowPassing(Vec<String>) }
pub fn evaluate(cases: &[JunitCase], expected: &[ExpectedFail]) -> SpecVerdict;
```

`ww dev spec-tests` runs `cargo nextest run -p ww-spec-tests --profile spec`, parses the JUnit file and evaluates it.

| Verdict | Meaning | Exit code |
|---|---|---|
| `UnexpectedFailures` | a test fails that isn't listed | 1 |
| `NowPassing` | a listed test now passes ("remove it from expected-fail.toml") | 1 |
| `Pass` | | 0 |

- [ ] **Step 1: Write the failing unit tests** in `spec_tests.rs`:
  - `unlisted_failure_is_unexpected`
  - `listed_failure_is_tolerated`
  - `listed_but_passing_is_reported`
  - `parse_junit_reads_failures` (uses a fixed XML string with one `<failure>`)

- [ ] **Step 2: Run them.**

Run: `cargo nextest run -p ww-cli`
Expected: FAIL. Nothing is implemented.

- [ ] **Step 3: Implement it.**

- [ ] **Step 4: Run the tests, then the runner.**

Run: `cargo nextest run -p ww-cli && cargo run -q -p ww-cli -- dev spec-tests`
Expected: PASS. The runner prints `spec tests: 1 passed, 0 expected-fail` and exits 0.

- [ ] **Step 5: Commit.**

```bash
git add test-params.toml .config crates/ww-spec-tests crates/ww-cli Cargo.toml Cargo.lock
git commit -m "test: params, nextest profiles, spec-test runner with expected-fail list"
```

---

### Task 7: Protected paths and the test-diff guard verdict (QA-019, QA-021)

**Files:**
- Create: `dev/protected-paths.txt` (patterns exactly as in Global Constraints) and `dev/merge-guard.toml` (keys exactly as in Global Constraints).
- Create: `crates/ww-cli/src/dev/guard.rs`.

**Interfaces:**

Produces:

```rust
pub enum ChangeKind { Added, Modified, Deleted, Renamed { from: String } }
pub struct FileChange { pub path: String, pub kind: ChangeKind }
pub struct Protected(globset::GlobSet);
impl Protected { pub fn load(repo: &Path) -> anyhow::Result<Protected>; pub fn is_protected(&self, path: &str) -> bool; }
pub struct Hunk { pub path: String, pub added: Vec<String>, pub removed: Vec<String> }
pub enum Blocker { ProtectedChanged { path: String, kind: String }, ExpectedFailAdditionWithoutNewTest { name: String } }
pub enum Warning { RemovedTest { path: String, line: String }, IgnoredTest { path: String, line: String }, NewSleepOrRetry { path: String, line: String }, NewLintSuppression { path: String, line: String } }
pub struct Verdict { pub needs_approval: Vec<Blocker>, pub warnings: Vec<Warning> }
pub fn evaluate(protected: &Protected, changes: &[FileChange], hunks: &[Hunk], expected_fail_added: &[String], spec_files_added: &[String]) -> Verdict;
pub fn collect_changes(repo: &Path, base: &str, head: &str) -> anyhow::Result<(Vec<FileChange>, Vec<Hunk>)>; // git diff --name-status -M base...head and -U0 hunks
pub fn render_plain(v: &Verdict) -> String;  // plain-language lines for reports
```

**Rules:**
- `Added` never needs approval.
- `Modified` and `Deleted` of a protected path need approval.
- `Renamed` needs approval when the *from* path or the *to* path is protected.
- `expected-fail.toml` additions are allowed only when each added entry's `file` is in `spec_files_added`. Removing entries is allowed.

**Warning patterns** (`.rs` hunks only):
- removed lines containing `#[test]`, `#[tokio::test]` or `proptest!`;
- added `#[ignore`;
- added `sleep(` or `retry` in files under `tests/` or containing `mod tests`;
- added `#[allow(`, `#![allow(` or `#[expect(`.

- [ ] **Step 1: Write the failing unit tests:**
  - `added_protected_file_is_allowed`
  - `modified_protected_file_needs_approval`
  - `deleted_protected_file_needs_approval`
  - `rename_out_of_protected_needs_approval`
  - `rename_within_protected_needs_approval`
  - `unprotected_change_is_clean`
  - `guard_files_are_protected` (`tools/git-hooks/pre-push`, `crates/ww-cli/src/dev/guard.rs`, `dev/protected-paths.txt`)
  - `expected_fail_addition_needs_matching_new_test`
  - `expected_fail_removal_is_free`
  - `warns_on_removed_ignored_sleep_and_allow`

  Plus an integration test `collect_changes_in_temp_repo`. It runs `git init` in a tempdir, commits a protected file, modifies it on a branch, and asserts one `Modified` change (it uses the real `git` binary, with no network).

- [ ] **Step 2: Run them.**

Run: `cargo nextest run -p ww-cli guard`
Expected: FAIL. Nothing is implemented.

- [ ] **Step 3: Implement it** (`globset` for the patterns; parse `git diff` output).

- [ ] **Step 4: Run them again.**

Run: `cargo nextest run -p ww-cli guard`
Expected: PASS.

- [ ] **Step 5: Commit.**

```bash
git add dev crates/ww-cli
git commit -m "dev: protected paths and the test-diff guard verdict (QA-019/021)"
```

---

### Task 8: The `GitHubApi` trait, its real client and its fake (QA-021, QA-033)

**Files:**
- Create: `crates/ww-cli/src/dev/github.rs` and `crates/ww-cli/src/dev/github_fake.rs` (`#[cfg(test)]` plus a `pub` test-support module for `ww-spec-tests`).

**Interfaces:**

Produces:

```rust
pub struct PrInfo { pub number: u64, pub head_sha: String, pub base_ref: String, pub state: String, pub draft: bool }
pub enum CheckState { Success, Pending, Failure(String), Missing }
pub struct Review { pub login: String, pub state: String, pub commit_id: String }
pub trait GitHubApi {
    fn pr(&self, number: u64) -> anyhow::Result<PrInfo>;
    fn check(&self, sha: &str, name: &str) -> anyhow::Result<CheckState>;   // latest check run with that name on sha
    fn reviews(&self, number: u64) -> anyhow::Result<Vec<Review>>;
    fn merge(&self, number: u64, expected_head_sha: &str) -> anyhow::Result<MergeOutcome>; // PUT .../merge with "sha", merge_method "merge"
    fn post_review_as_app(&self, number: u64, sha: &str, body: &str, installation_token: &str) -> anyhow::Result<()>;
}
pub enum MergeOutcome { Merged { sha: String }, HeadMoved, NotMergeable(String) }
pub struct RestClient { /* reqwest blocking, base https://api.github.com, token from GITHUB_TOKEN or `gh auth token` */ }
pub fn token_from_env_or_gh() -> anyhow::Result<String>;
```

The fake holds maps of PRs, checks and reviews in memory. `merge` returns `HeadMoved` when `expected_head_sha` doesn't equal the current head; this mirrors GitHub's 409.

- [ ] **Step 1: Write the failing tests** (against the fake, plus `RestClient` request building checked through a pure `fn build_merge_request(...) -> (Method, String, serde_json::Value)`):
  - `merge_request_pins_sha`: the body contains `"sha":"<expected>"` and `"merge_method":"merge"`.
  - `fake_merge_detects_head_move`
  - `check_missing_when_no_run`
  - `token_never_in_debug_output`: `format!("{:?}", client)` doesn't contain the token.

- [ ] **Step 2: Run them.**

Run: `cargo nextest run -p ww-cli github`
Expected: FAIL. Nothing is implemented.

- [ ] **Step 3: Implement it.** `RestClient` sets `User-Agent: ww-dev`, `Accept: application/vnd.github+json` and `X-GitHub-Api-Version: 2022-11-28`. Its `Debug` impl redacts the token.

- [ ] **Step 4: Run them again.**

Run: `cargo nextest run -p ww-cli github`
Expected: PASS.

- [ ] **Step 5: Commit.**

```bash
git add crates/ww-cli
git commit -m "dev: GitHubApi trait, REST client and in-memory fake"
```

---

### Task 9: `ww dev guard` and `ww dev merge` (QA-021, QA-033, QA-034 refusal rule)

**Files:**
- Create: `crates/ww-cli/src/dev/merge.rs`.
- Modify: `crates/ww-cli/src/dev/mod.rs` to add the subcommands `guard` and `merge`.

**Interfaces:**

Consumes:
- `guard::{collect_changes, evaluate, Protected}` (Task 7)
- `GitHubApi` (Task 8)
- `MergeGuardConfig`, loaded from `dev/merge-guard.toml`

Produces:

```rust
pub struct MergeGuardConfig { pub owner: String, pub repo: String, pub base_branch: String, pub reviewer_app_slug: String, pub required_checks: Vec<String>, pub hooks_path: String }
pub enum Refusal { HooksNotInstalled, CheckNotGreen { name: String, state: String }, ProtectedNeedsApproval(Vec<String>), PrNotOpen, HeadMoved, NotMergeable(String) }
pub fn merge_conditions(cfg: &MergeGuardConfig, gh: &dyn GitHubApi, pr: u64, verdict_for: &dyn Fn(&str, &str) -> anyhow::Result<guard::Verdict>, hooks_path_configured: Option<String>) -> Result<String /* checked head sha */, Vec<Refusal>>;
pub fn explain(refusals: &[Refusal]) -> String; // plain language, one line each
```

- `ww dev guard --base <ref> --head <ref> [--pr <n>]` prints `render_plain(verdict)`. With `--pr`, approval counts only from `<reviewer_app_slug>[bot]` with `state == "APPROVED"` and `commit_id == head`. It exits with 1 on unapproved blockers.
- `ww dev merge <pr>` does three things in order:
  1. checks `git config core.hooksPath` equals `tools/git-hooks`;
  2. evaluates all conditions;
  3. calls `gh.merge(pr, checked_sha)`.

  It prints `explain(...)` on any refusal, and also on `HeadMoved` from the merge call.

- [ ] **Step 1: Write the failing tests** (fake GitHub, injected verdict closure):
  - `all_green_merges_at_checked_sha`
  - `missing_check_refuses`: the message includes "pr-ready hasn't reported for <sha7>".
  - `pending_check_refuses`
  - `failed_check_refuses_with_its_name`
  - `protected_change_without_app_approval_refuses`
  - `approval_on_older_commit_does_not_count`
  - `approval_by_human_does_not_count` (login `xenofei`)
  - `hooks_not_installed_refuses_before_anything`
  - `merge_is_pinned_to_checked_sha` (the fake moves the head after the conditions → `HeadMoved` → refusal explained)

- [ ] **Step 2: Run them.**

Run: `cargo nextest run -p ww-cli merge`
Expected: FAIL. Nothing is implemented.

- [ ] **Step 3: Implement it.**

- [ ] **Step 4: Run the tests, then check the CLI help.**

Run: `cargo nextest run -p ww-cli merge && cargo run -q -p ww-cli -- dev merge --help`
Expected: PASS. The help text describes the three conditions.

- [ ] **Step 5: Commit.**

```bash
git add crates/ww-cli
git commit -m "dev: ww dev guard and ww dev merge with pinned-sha merges (QA-033)"
```

---

### Task 10: Git hooks and `ww dev install-hooks` (QA-034)

**Files:**
- Create: `tools/git-hooks/pre-push`. POSIX `sh`; it runs `exec cargo run --quiet --locked -p ww-cli --bin ww -- dev hook pre-push "$@"` and passes stdin through.
- Create: `tools/git-hooks/pre-merge-commit`, which runs `... dev hook pre-merge-commit`.
- Create: `crates/ww-cli/src/dev/hooks.rs`.

**Interfaces:**

Produces:

```rust
pub fn pre_push(stdin_lines: &str, cfg: &MergeGuardConfig, verdict_for: &dyn Fn(&str,&str)->anyhow::Result<guard::Verdict>, gh: &dyn GitHubApi) -> Result<(), String>;
pub fn pre_merge_commit(current_branch: &str, merge_head: &str, base_sha: &str, cfg: &MergeGuardConfig, verdict_for: ..., gh: &dyn GitHubApi) -> Result<(), String>;
pub fn install_hooks(repo: &Path) -> anyhow::Result<()>; // git config core.hooksPath tools/git-hooks
```

**Rules:**
- **`pre_push`:** for each stdin line whose remote ref is `refs/heads/main`, refuse when either:
  - the range `remote_sha..local_sha` has protected blockers (no approval is possible on a direct push);
  - `every-push` isn't `Success` on `local_sha`.

  The refusal says: "Pushes to main go through `ww dev merge <PR>`". Lines for other branches pass.
- **`pre_merge_commit`:** when the current branch is `main`, it applies the same rule to `base..MERGE_HEAD`. On other branches it passes.

- [ ] **Step 1: Write the failing tests:**
  - `push_to_feature_branch_passes`
  - `push_to_main_with_protected_change_refused`
  - `push_to_main_without_ci_refused`
  - `merge_into_main_locally_refused_when_blocked`
  - `merge_on_feature_branch_passes`
  - `install_hooks_sets_config` (in a temp repo)

- [ ] **Step 2: Run them.**

Run: `cargo nextest run -p ww-cli hooks`
Expected: FAIL. Nothing is implemented.

- [ ] **Step 3: Implement it.** Then install the hooks in this repo.

- [ ] **Step 4: Run the tests and the live hook check.**

Run: `cargo nextest run -p ww-cli hooks && cargo run -q -p ww-cli -- dev install-hooks && git config core.hooksPath`
Expected: PASS, then `tools/git-hooks`.

Then a live check in a throwaway clone. Clone the repo into a tempdir with `git clone --no-local`, install the hooks, commit a change to `test-params.toml` on `main`, and run `git push origin main` to the temp clone's bare origin (a local bare repo, not GitHub). Expected: the push is refused with the message above. Record the output in the report.

- [ ] **Step 5: Commit.**

```bash
git add tools/git-hooks crates/ww-cli
git commit -m "dev: pre-push and pre-merge-commit hooks guard main (QA-034)"
```

---

### Task 11: The reviewer App — `ww dev reviewer-setup` and `ww dev approve` (QA-020 as amended)

**Files:**
- Create: `crates/ww-cli/src/dev/reviewer.rs`.

**Interfaces:**

Produces:

```rust
pub struct ReviewerKeyFile { pub app_id: u64, pub installation_id: u64, pub encrypted_pem: Vec<u8> } // serialized as TOML with base64 field; stored at %USERPROFILE%\.worldwright\reviewer\app-key.age
pub fn encrypt_key(pem: &[u8], passphrase: &str) -> anyhow::Result<Vec<u8>>;   // age scrypt
pub fn decrypt_key(blob: &[u8], passphrase: &str) -> anyhow::Result<Vec<u8>>;
pub fn app_jwt(app_id: u64, pem: &[u8], now_unix: i64) -> anyhow::Result<String>; // RS256, iat=now-60, exp=now+540, iss=app_id
pub trait Prompt { fn is_interactive(&self) -> bool; fn passphrase(&self, label: &str) -> String; fn confirm(&self, question: &str) -> bool; }
pub fn approve(pr: u64, cfg: &MergeGuardConfig, gh: &dyn GitHubApi, prompt: &dyn Prompt, key_file: &ReviewerKeyFile, token_exchange: &dyn Fn(&str /*jwt*/, u64) -> anyhow::Result<String>, summary: &str, now_unix: i64) -> anyhow::Result<()>;
```

**`approve` steps:**
1. Refuse unless `prompt.is_interactive()`. Real check: `std::io::stdin().is_terminal() && std::io::stderr().is_terminal()`.
2. Print the PR's protected-change summary (the guard verdict) and ask "Approve these protected changes on <sha7>? (y/N)".
3. Read the passphrase.
4. Decrypt the key, mint the JWT, and exchange it for an installation token.
5. Post an `APPROVE` review pinned to the current head sha. The body lists the approved protected paths.

**`reviewer-setup`:**
1. Ask for the App id, the installation id and the path to the downloaded `.pem`.
2. Ask for the passphrase twice.
3. Write the key file.
4. Tell the owner to delete the plaintext `.pem` themselves. The tool never deletes it.

- [ ] **Step 1: Write the failing tests.** Use a throwaway RSA key generated at test time and a fake `Prompt`:
  - `encrypt_decrypt_round_trip`
  - `wrong_passphrase_posts_nothing`
  - `approve_refuses_without_tty`
  - `declined_confirmation_posts_nothing`
  - `jwt_has_expected_claims`: decode without verification and check iss, iat and exp.
  - `review_is_pinned_to_head_sha`
  - `key_path_is_outside_repo_and_suite_data`

- [ ] **Step 2: Run them.**

Run: `cargo nextest run -p ww-cli reviewer`
Expected: FAIL. Nothing is implemented.

- [ ] **Step 3: Implement it** with `age`, `jsonwebtoken` and `rpassword`. The token exchange is `POST https://api.github.com/app/installations/{id}/access_tokens` with `Authorization: Bearer <jwt>`.

- [ ] **Step 4: Run them again.**

Run: `cargo nextest run -p ww-cli reviewer`
Expected: PASS.

- [ ] **Step 5: Commit.**

```bash
git add crates/ww-cli
git commit -m "dev: reviewer GitHub App setup and passphrase-gated approvals (QA-020)"
```

---

### Task 12: CI workflows (QA-035, QA-069–QA-073, QA-079 as amended)

**Files:**
- Create: `.github/workflows/ci-push.yml`
  - Triggers: `push` to branches other than `main`.
  - Job **`every-push`** on `windows-2025` with `concurrency: { group: push-${{ github.ref }}, cancel-in-progress: true }`. Steps:
    - checkout;
    - Rust from `rust-toolchain.toml`;
    - `sccache`;
    - install `cargo-nextest` and `cargo-deny` (via `taiki-e/install-action`, pinned by SHA);
    - `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo deny check`, `cargo nextest run --workspace`, `cargo run -p ww-cli -- dev spec-tests`.
- Create: `.github/workflows/ci-pr-ready.yml`
  - Triggers: `pull_request` (`opened`, `synchronize`, `reopened`, `ready_for_review`).
  - Job **`pr-ready`**, gated with `if: github.event.pull_request.draft == false`. It runs:
    - everything in `every-push`;
    - `cargo audit` (installed by SHA-pinned action);
    - `cargo build --workspace --release`.
  - Leave a comment marking where plans 2–6 add QtQuickTest, E2E and the real-NTFS tests.
- Create: `.github/workflows/ci-guard.yml`
  - Triggers: `pull_request` (same types) and `pull_request_review` (`submitted`).
    (Later, R40: `pull_request_review` and `ready_for_review` were dropped and concurrency is per head without cancelling, because each run is its own check suite; `ww dev approve` re-runs the red runs instead.)
  - Job **`protected-test-check`**: `cargo run -p ww-cli -- dev guard --base origin/main --head ${{ github.event.pull_request.head.sha }} --pr ${{ github.event.pull_request.number }}`, then `dev spec-tests`.
  - Uses the default token, read-only.
  - The workflow file is protected.
- Create: `.github/workflows/nightly.yml`
  - Triggers: `schedule` at 08:00 UTC daily, plus `workflow_dispatch`, on `main` only.
  - Steps: a release build and coverage with `cargo llvm-cov nextest --workspace --lcov --output-path lcov.info`, uploaded as an artifact.
- Create: `.github/workflows/post-merge.yml`
  - Triggers: `push` to `main`. No concurrency cancel.
  - Job **`post-merge-check`**. It's the only job with `permissions: { contents: write, pull-requests: write, issues: write }`. Steps:
    1. Run `every-push`'s test steps.
    2. Run `ww dev guard --post-merge --before ${{ github.event.before }} --after ${{ github.sha }}`.
    3. On failure, run `ww dev revert-pr --before ... --after ...`.
- Create: `crates/ww-cli/src/dev/post_merge.rs`.
  - `--post-merge` mode: for each commit in the range, find its PR via `GET /repos/{o}/{r}/commits/{sha}/pulls`. Flag the commit when either:
    - its protected changes lack an App approval on that PR's final head;
    - it has no PR at all (a direct push).
  - `revert-pr`: creates a branch `revert/<after7>`, runs `git revert --no-edit <before>..<after>`, pushes it, and opens a PR and an issue assigned to `owner` with the plain-language verdict. Use the `gh` CLI with `GH_TOKEN=${{ secrets.GITHUB_TOKEN }}`.
- Every workflow sets top-level `permissions: { contents: read }`.
- Every third-party action is pinned to a full commit SHA, with the version in a trailing comment. Record each one in the report.

**Interfaces:**
- Consumes: Tasks 6–9.
- Produces: check runs named exactly `every-push`, `pr-ready` and `protected-test-check`.

- [ ] **Step 1: Write the failing tests:**
  - `post_merge_flags_direct_push` (fake GitHub: the commit has no PR)
  - `post_merge_flags_unapproved_protected_change`
  - `post_merge_passes_approved_pr`
  - `workflow_files_parse_and_name_jobs`: parse each YAML with `serde_yaml`, then assert the job ids above, top-level `contents: read`, write permissions only on `post-merge-check`, and every `uses:` pinned to a 40-hex SHA.

- [ ] **Step 2: Run them.**

Run: `cargo nextest run -p ww-cli post_merge workflow`
Expected: FAIL. Nothing is implemented.

- [ ] **Step 3: Implement it.**

- [ ] **Step 4: Run them again.**

Run: `cargo nextest run -p ww-cli post_merge workflow`
Expected: PASS.

- [ ] **Step 5: Commit.**

```bash
git add .github crates/ww-cli
git commit -m "ci: every-push, pr-ready, guard, nightly and post-merge workflows (QA-035, QA-069-073)"
```

---

### Task 13: Claude Code project settings for the suite repo (QA-036)

**Files:**
- Create: `.claude/settings.json` in the suite repo, with `permissions.deny` entries:
  - `Bash(git push origin main:*)`, `Bash(git push * main:*)`, `Bash(git push --force*)`;
  - `Bash(git * --no-verify*)`, `Bash(git commit -n*)`;
  - `Bash(gh pr merge:*)`, `Bash(gh api *merge*)`, `Bash(git config core.hooksPath*)`.

  Also the PowerShell equivalents with the same shapes: `PowerShell(git push origin main*)` and so on.
- Create: `crates/ww-testkit/tests/claude_settings_patterns.rs`.

**Interfaces:**
- Produces: the deny list. Plan 3's plugin hook-pattern tests (QA-061) extend the same test table.

- [ ] **Step 1: Write the failing test.** It holds a table of commands that **must** match some deny rule and commands that **must not**:
  - Must match: `git push origin main`, `git push -u origin main`, `git merge x --no-verify`, `gh pr merge 3`, `gh api -X PUT repos/x/y/pulls/3/merge`.
  - Must not match: `git push origin plan-1-foundation`, `cargo run -p ww-cli -- dev merge 3`.

  It implements Claude Code's documented prefix and wildcard matching for `Bash(...)` rules in a small helper. The test file's header comment states the known limits of pattern guards (e.g. shell aliases, `git -c` forms).

- [ ] **Step 2: Run it.**

Run: `cargo nextest run -p ww-testkit --test claude_settings_patterns`
Expected: FAIL, because the settings file is missing.

- [ ] **Step 3: Create the settings file.**

- [ ] **Step 4: Run it again.**

Run: the same command.
Expected: PASS.

- [ ] **Step 5: Commit.**

```bash
git add .claude/settings.json crates/ww-testkit/tests/claude_settings_patterns.rs
git commit -m "dev: Claude Code project settings deny direct merges and hook bypass (QA-036)"
```

---

### Task 14: Spec traceability report (QA-028)

**Files:**
- Create: `crates/ww-cli/src/dev/spec_report.rs`.
- Create: `dev/rule-classification.toml`. It starts as `default = "unclassified"` with an empty `[rules]` table. Later plans classify each rule as `test`, `lint`, `manual` or `n/a`.

**Interfaces:**

Produces:

```rust
pub fn spec_rule_ids(spec_markdown: &str) -> BTreeSet<String>;           // same regex as the ID check
pub fn cited_ids(rust_sources: &[(String /*path*/, String /*text*/)]) -> BTreeMap<String, Vec<String /*path:line*/>>; // `// spec: A-001, B-002`
pub struct SpecReport { pub dangling: Vec<(String, String)>, pub test_rules_without_tests: Vec<String>, pub counts: BTreeMap<String, usize> }
pub fn report(ids: &BTreeSet<String>, cited: &BTreeMap<String, Vec<String>>, classification: &Classification) -> SpecReport;
```

`ww dev spec-report` prints the report.

| Condition | Exit code |
|---|---|
| A dangling citation exists (a test cites an ID not in the spec) | 1 |
| Otherwise | 0 |

`test_rules_without_tests` is informational in this plan.

- [ ] **Step 1: Write the failing tests:**
  - `extracts_all_ids_from_spec` (reads the real spec file and asserts the current total, `EXPECTED_RULE_COUNT`: 925 since Task 16; it was 922 and named `extracts_922_ids_from_spec` before)
  - `parses_multi_id_citations`
  - `dangling_citation_reported`
  - `test_classified_rule_without_test_listed`

- [ ] **Step 2: Run them.**

Run: `cargo nextest run -p ww-cli spec_report`
Expected: FAIL. Nothing is implemented.

- [ ] **Step 3: Implement it.** Add `cargo run -p ww-cli -- dev spec-report` to `every-push` as an extra step.

- [ ] **Step 4: Run the tests, then the report.**

Run: `cargo nextest run -p ww-cli spec_report && cargo run -q -p ww-cli -- dev spec-report`
Expected: PASS. The report lists 925 rules (922 before Task 16), 0 dangling citations, and per-classification counts (all `unclassified`).

- [ ] **Step 5: Commit.**

```bash
git add crates/ww-cli dev/rule-classification.toml .github/workflows/ci-push.yml
git commit -m "dev: spec traceability report (QA-028)"
```

---

### Task 15: Owner setup and end-to-end dry run — owner-assisted

**Files:**
- Create: `docs/dev/merge-guard.md`, the owner's guide: what the guard does, the one-time setup below, everyday use, and what to do when CI opens a revert PR.

**Interfaces:**
- Consumes: everything above.

- [ ] **Step 1: Write the guide.** It covers:
  - **Creating the GitHub App.** Settings → Developer settings → GitHub Apps → New:
    - name `worldwright-reviewer`;
    - webhook off;
    - permissions *Pull requests: Read and write* and *Contents: Read-only*;
    - "Only on this account";
    - install it on `xenofei/Worldwright-Suite`;
    - generate a private key.
  - Running `ww dev reviewer-setup` and deleting the downloaded `.pem` afterwards.
  - Making the default workflow token read-only: `gh api -X PUT repos/xenofei/Worldwright-Suite/actions/permissions/workflow -f default_workflow_permissions=read -F can_approve_pull_request_reviews=false`.
  - Everyday flow: agents run `ww dev merge <PR>`; the owner runs `ww dev approve <PR>` only when asked to approve protected changes.

- [ ] **Step 2: The controller asks the owner** to do the App creation and `reviewer-setup` following the guide, and to confirm the workflow-permission command, which the controller then runs after a yes. Record each outcome.

- [ ] **Step 3: First PR (the plan-1 branch itself).**
  1. Push `plan-1-foundation` and open a PR.
  2. Expected: `every-push`, `pr-ready` and `protected-test-check` all run and pass. Every protected file in this PR is newly *added*, and additions never need approval (QA-020).
  3. Run `ww dev merge <PR>`. Expected: merged, pinned to the checked head sha.
  4. Expected: `post-merge-check` runs on `main` and passes.
  5. Record the check URLs and outcomes in the "First run" section of `docs/dev/merge-guard.md`. That section lives on a follow-up branch, since `main` now holds the guard.

- [ ] **Step 4: The approval path.** On a branch `guard-dry-run`, change only a comment in `test-params.toml`, which is a protected modification, and open a PR.
  1. Expected: `protected-test-check` fails.
  2. Run `ww dev merge <PR>`. Expected: it's refused with "protected file test-params.toml modified — needs the reviewer App's approval on <sha7>".
  3. The owner runs `ww dev approve <PR>`. Expected: the check re-runs on the review and passes.
  4. Run `ww dev merge <PR>`. Expected: merged.
  5. Push one more commit to a second throwaway PR after approving it. Expected: the old approval no longer counts, and the merge is refused.
  6. Close or delete the throwaway branches.
  7. Record each outcome.

- [ ] **Step 5: Commit the guide.** The guide goes on `plan-1-foundation` before Step 3; the "First run" record is added afterwards on a branch `docs/merge-guard-first-run`, merged with `ww dev merge`.

```bash
git add docs/dev/merge-guard.md
git commit -m "docs: merge-guard owner guide"
# after steps 3–4, on docs/merge-guard-first-run:
git commit -am "docs: merge-guard first-run record"
```

---

## Amendment A (2026-10-13): public release

**Decision.** After plan 1, the suite is published as a **new public repository** under the same GitHub account. Task 15 creates it from this branch's cleaned tree as a **single initial commit**. This private repository becomes the archive. Rewriting history in place is not enough, because pull-request refs on GitHub keep old commits for good.

Other settings:
- **Licence:** GPL-3.0-or-later.
- **Agent access:** a fine-grained token limited to the public repository (Task 13), with no administration permission and no workflow-file permission. This restores QA-079's original intent; its 2026-10-13 waiver is withdrawn.
- **CI minutes:** hosted minutes are free and unlimited on public repositories, so the minute trims of Ruling R22 are dropped (R25).

**Task order from here:** 12 (fix round) → 13 → 14 → 16 → 17 → 15.

### Global constraint added: nothing personal enters the repository

From this amendment on, every commit is treated as public. Repository files never contain:
- a Windows user-profile path (write `%USERPROFILE%`);
- an e-mail address other than a GitHub no-reply address or `noreply@anthropic.com` trailers;
- running-process lists;
- machine-identifying details beyond what a test needs;
- statements about the owner's accounts, habits, plans or preferences.

The owner's decisions and personal context live in the controller's private memory, not in repository docs. Task 17 enforces this mechanically.

### Task 13 (expanded): granular agent access (QA-036, QA-079)

Everything in the original Task 13 stays. In addition:

**Files:**
- Create `crates/ww-platform/src/credentials.rs`. It wraps Windows Credential Manager (`CredReadW`, `CredWriteW`, `CredDeleteW`) behind `trait CredentialStore { get, set, delete }`, with an in-memory fake for tests.
  - ww-platform is the only crate that calls Windows APIs (ARCH-111).
  - The real store refuses targets outside the `Worldwright-dev/` prefix.
- Create `crates/ww-cli/src/dev/agent_access.rs`.

**Interfaces (produces):**
- `ww dev agent-token set|status|clear`.
  - `set` is TTY-only: it reads the token with a hidden prompt and stores it at target `Worldwright-dev/github/agent-token`.
  - `status` prints only whether a token is present, its first 4 characters, and the expiry the owner entered. It never prints the token.
- `ww dev git-credential <get|store|erase>`, a git credential helper.
  - `get` for `host=github.com` answers `username=x-access-token` and the stored token.
  - Every other host gets an empty reply.
  - `store` and `erase` are no-ops.
- `ww dev gh -- <args...>` runs `gh` with `GH_TOKEN` set to the agent token in the child process only.
- Token resolution for every `ww dev` GitHub call:
  1. `GITHUB_TOKEN` (CI);
  2. otherwise the Credential Manager agent token;
  3. otherwise a refusal explaining `ww dev agent-token set`.

  The `gh auth token` fallback is removed: it returned a full-access login.
- `ww dev install-hooks` also writes these repository-local credential entries:
  1. `credential.helper` set to an empty value, which resets inherited helpers such as Git Credential Manager;
  2. `credential.helper` set to the `ww dev git-credential` helper.

  `ww dev merge`'s hooks check also requires these exact helper entries.

**Deny patterns.** Add these to `.claude/settings.json`, with tests in the same pattern table:
- `gh auth token*`
- `gh auth status*--show-token*`
- `gh auth login*`
- `git credential fill*`
- `git credential-manager*`
- `cmdkey*`
- `git config*credential*`

**Rule.** No full-access GitHub login is stored on the machine agents run on. Task 15 removes the owner's full-access login from that machine once setup is done, and admin work is done in the browser. The deny patterns alone are a leaky layer.

**Tests** (none of them touches the real Credential Manager):
- the credential helper protocol round trip;
- a non-github.com host gets nothing;
- `status` never prints the token;
- `set` refuses without a TTY;
- token resolution order;
- a target outside the `Worldwright-dev/` prefix is refused;
- `install-hooks` writes the helper reset and the helper, in a temp repo with isolated git config.

### Task 16: Privacy cleanup of the tree

All edits are ordinary commits on `plan-1-foundation`. The inputs are the private audit reports the controller hands over. Never copy personal values from them into the repo.

1. **VTuber research.** Remove `docs/research/vtuber/**`; it stays in the private archive. Reword the roadmap's back-burner row to say only that research is kept privately and the direction is VRM-first.
2. **Raw spike outputs.** Remove those that record the machine: result JSONs, logs, ComfyUI logs and process lists under `spikes/**/results/**`. Keep the 16 images. Keep the `docs/spikes/*.md` summaries, with paths rewritten.
3. **User-profile paths.** Replace every Windows user-profile path in the remaining files with `%USERPROFILE%`. This includes the plan's Global Constraints and the spec.
4. **Neutral wording about the owner.**
   - Rewrite the multi-account section (AI §4.6a) as neutral feature text: several Claude Code profiles, switched manually. Leave out any narrative about usage limits and any example account labels.
   - Remove quotes of the owner and statements about the owner (habits, intentions, equipment, affiliation, accounts) from the spec, plans, roadmap and spike docs.
   - Reword "no content rules" passages to describe the design: the suite adds no content filter of its own, and each provider's policies still apply.
5. **Licence and README.**
   - Add `LICENSE` with the full GPL-3.0 text. State the `-or-later` grant in the README and in `license = "GPL-3.0-or-later"` in `[workspace.package]`, which every crate inherits.
   - Add `NOTICE` listing third-party material, e.g. the MIT-licensed ComfyUI template workflows used by the spikes.
   - Update `deny.toml` so the workspace's own crates pass.
   - Rewrite `README.md` for a public audience.
6. **Spec rules.**
   - Update QA-069, QA-070 and QA-075: public repository, hosted runners, no minute budget.
   - Restore QA-079: agents' token has no workflow or administration permission.
   - Update QA-019 and QA-020 for server-side rulesets on the public repository.
   - Add new rules with the next free IDs, never renumbering existing ones: the privacy rule (Task 17's checks), the licence rule and the agent-token rule.
   - Update the ID-check count in the plan and in Task 14's `extracts_*_ids_from_spec` test to the new total.
7. **Verification.** `ww dev privacy-scan --tree HEAD` (from Task 17; until then, the controller runs the audit's scan scripts) reports nothing, and the test suite stays green.

### Task 17: Privacy guard (new rule from Task 16)

**Files:**
- Create `crates/ww-cli/src/dev/privacy.rs`. It is protected via `crates/ww-cli/src/dev/**`.

**Interfaces.** `ww dev privacy-scan` has three modes:
- `--range <base>..<head>` scans added and changed lines, paths and commit messages, plus commit author and committer identities;
- `--tree <rev>` scans every file;
- `--commits <range>` checks identities only.

**It blocks:**
1. **User-profile paths.** Any `X:\Users\<name>`, also written as `/x/Users/<name>`, `C:\\Users\\<name>` or `C:/Users/<name>`, where `<name>` isn't an allowlisted placeholder: `%USERPROFILE%`, `<user>`, `<name>`, `Fake` (case-insensitive), `runneradmin`, `Public` or `Default` (ruling R30). The path pattern requires a single-letter drive (`X:`, or the `/x/` form) followed by `Users` and a non-empty segment that contains no quote character, so URL paths such as `/v1/users/me/balance` and the quoted `":\\\\users"` pattern strings in `crates/ww-testkit/tests/claude_settings_patterns.rs` don't match.
2. **Identities.** A commit author or committer e-mail that isn't `*@users.noreply.github.com` or `noreply@github.com`, and trailer e-mails other than `noreply@anthropic.com`.
3. **A private denylist** of case-insensitive literal strings.
   - It is read from `%USERPROFILE%\.worldwright\privacy-denylist.txt` locally, or from the `WW_PRIVACY_DENYLIST` environment variable in CI (from an Actions secret).
   - A missing denylist is a refusal, never an empty pass.
   - Findings name the entry number and file:line, never the matched text.

**Hooks.** `pre-push` runs the range scan for every pushed ref, not only main. This updates R17: the shell pre-filter now always hands off when any ref is pushed.

**CI.** A job `privacy-check` in `ci-push.yml` and `ci-pr-ready.yml` runs the range scan. Add `privacy-check` to `dev/merge-guard.toml`'s `required_checks` and to the rulesets in Task 15.

**Tests:**
- each path form is blocked or allowed as specified;
- placeholders are allowed;
- non-no-reply e-mails are blocked, while no-reply addresses pass;
- denylist hits are reported without echoing the text;
- a missing denylist refuses;
- tree mode finds nothing on a clean fixture.

### Task 15 (rewritten): publish and dry run, owner-assisted

The owner guide `docs/dev/merge-guard.md` is still written first (original Step 1). It names the new repository and covers every step and checklist item below (Ruling R38), including the denylist, the no-reply identity, the ruleset settings, the export checks and which terminal to use for the passphrase prompts.

1. **Owner preparation, in the browser.**
   - Check the GitHub profile for real name, location and bio.
   - Turn on "Keep my email addresses private" and "Block command line pushes that expose my email".
   - Choose the public repository's name. If it should reuse this repo's name, rename this private archive first.

   The controller asks before anything is created.
2. **Initial commit.**
   - Create `%USERPROFILE%\.worldwright\privacy-denylist.txt` before any push. It holds the owner's real e-mail addresses, name and username forms, one per line. The privacy scan (R36) relies on it, and the `pre-push` hook refuses every push until it exists.
   - Export the cleaned tree of `plan-1-foundation` (`git archive`) into a new folder and run `git init` there.
   - Set the export's repository-local `user.email` to the owner's no-reply address (`<id>+<login>@users.noreply.github.com`) before any commit. Do the same in every later clone (step 6) before its first commit.
   - `git add -A`, then `git update-index --chmod=+x tools/git-hooks/pre-push tools/git-hooks/pre-merge-commit` (name the two files: PowerShell doesn't expand `*`): a Windows `git add` doesn't keep the executable bit.
   - Check the export against the branch, before any edit: `git write-tree` in the export prints the same id as `git rev-parse plan-1-foundation^{tree}` in this repository.
   - Update `dev/merge-guard.toml`'s `repo` to the chosen name and `git add` it.
   - Compare file lists and modes: `git ls-tree -r` of the export's `git write-tree` and of `plan-1-foundation` list the same paths, modes (`100755` for the hooks) and object ids, except `dev/merge-guard.toml`.
   - Make one commit.
   - Build `ww` inside the export (`cargo run` there): a `ww` built in another checkout refuses to work on this one (R38), so the privacy gate must be this checkout's own build.
   - Run `ww dev privacy-scan --tree HEAD` and `--commits` on it; both must be clean.
3. **Create and push**, after the owner's yes. The owner creates the public repository, or confirms the controller creating it with the owner's login. Then push `main`. The post-merge run logs "repository creation push" (R27).
4. **Settings**, done or confirmed by the owner:
   - a ruleset on `main`:
     - "Require a pull request before merging", with 0 required approvals (the reviewer App's approval is checked by the guard, not by the ruleset);
     - merge commits allowed, since `ww dev merge` uses the merge method;
     - no "Require linear history" and no "Require branches to be up to date before merging";
     - required status checks `every-push`, `pr-ready`, `protected-test-check` and `privacy-check`, each from the GitHub Actions source;
     - force-push and deletion blocked;
     - no bypass list;
   - pull requests restricted to collaborators;
   - fork-workflow approval required for all outside contributors;
   - secret scanning push protection on;
   - the default workflow token read-only;
   - "Allow GitHub Actions to create pull requests" on (for revert PRs);
   - the `WW_PRIVACY_DENYLIST` secret;
   - issues set per the owner's choice.
5. **Reviewer App.** Create it on the new repository and run `ww dev reviewer-setup` (original Step 1). Run `ww dev reviewer-setup` and every later `ww dev approve` from PowerShell or Windows Terminal, not from mintty (Git Bash's default terminal), where the hidden passphrase input can hang.
6. **Agent token.**
   - The owner creates a fine-grained token: resource owner = the account, only the public repository; Contents, Pull requests and Issues read/write; Actions, Checks and Metadata read; no Administration, no Workflows; with an expiry.
   - The owner then runs `ww dev agent-token set` and `ww dev install-hooks` in a fresh clone of the public repository, which becomes the working folder.
   - After a confirmation, the owner's full-access login is removed from the machine agents run on: `gh auth logout`, plus the Git Credential Manager entry for github.com.
7. **Dry runs**, as in the original Steps 3–4, against the public repository:
   - a protected change refused, then approved, then merged;
   - a stale approval refused;
   - a privacy dry run: a branch adding a fake user path is refused by the hook and by `privacy-check`;
   - an agent-token check: pushing a change to `.github/workflows/` with the agent token is rejected by GitHub.
8. **Record.** Write the outcomes into the guide's "First run" section through a PR merged with `ww dev merge`.

## Rulings cited in public text

These plan-1 rulings were made while the plan ran and lived only in its private ledger. This plan, plan 2a, the rename plan and the spec cite them by id, so each is recorded here with its reason (QA-090), as the public text and the code it produced state it. The rest stay in the ledger; the code comments that cite them describe what each one changed.

- **R17** The git hook scripts filter cheaply in the shell before handing off to `lil`; from Amendment A, `pre-push` hands off every push that names a ref, because the privacy scan covers every branch, not only `main`. Why: a push that doesn't touch `main` must work offline and without a build.
- **R22** Every CI job is time-boxed (`timeout-minutes` between 1 and 120). Its minute-saving trims for a private repository were dropped by R25.
- **R25** Every push runs every CI job. Why: the repository is public, so GitHub-hosted minutes are free, and a skipped run could report green on untested code.
- **R26** A protected-paths line that starts `add-ok:` lets new files under its pattern in without approval; changing or deleting files there still needs it. Only the test homes carry it. Why: adding a test must not need the reviewer, while weakening an existing one must.
- **R27** The post-merge check judges and reverts nothing on a push that creates `main` (its `before` is all zeros), and logs "repository creation push". Why: there is no earlier `main` to compare it with.
- **R30** The privacy scan's allowlisted profile-path placeholders are `%USERPROFILE%`, `<user>`, `<name>`, `Fake`, `runneradmin`, `Public` and `Default`, compared without regard to case. Why: these name no real person, and docs and tests need them.
- **R36** The privacy scan also checks every file, path and commit against the owner's private denylist of real names and addresses, which is never printed or committed; `pre-push` refuses every push until the denylist exists. Why: the pattern rules can't know a person's real names.
- **R38** The Task 15 review fixes: the owner guide covers every publish step and checklist item; a `lil` built in one checkout refuses to judge, scan or merge another; CI judges build the CLI from `main` (or the push's `before`), never from the pushed commit; a changed path holding a control, bidi or invisible character is refused. Why: each closes a way the guard could judge the wrong tree or show something other than what it checked.
- **R40** The guard check starts only on `opened`, `synchronize` and `reopened`, its concurrency is per head without cancelling, and `lil dev approve` re-runs a red guard run after approving. Why: every workflow run is its own check suite, and the ruleset refuses a merge while any suite on the head holds a cancelled or failed guard check.
