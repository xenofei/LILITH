# LILITH Rename Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Rename the suite from Worldwright to **LILITH** (Latent Image & Likeness Instrumentality Toolchain Hub), with the short prefix **`lil`**: CLI `lil`, crates `lil-*`, env `LIL_*`, and every name in the Name map below. The rename lands in three PRs (PR-A, PR-B, PR-C) without breaking CI, the merge guard or the owner's access to the agent token, the reviewer key and the privacy denylist. The core keeps reading the legacy names for one release (R4), so the plugin-era workspace and the `worldwright` Claude plugin keep working (R1). The director agent gets its name, **MAGI Core**, once, at AI-041.

**Architecture:**
- **PR-A "rename-ready"** changes no product name. CI's judges pick the CLI crate from the tree they build. The workflows pass both denylist variable names. Git's credential helper becomes a committed script that runs either crate. The owner-tooling readers (agent token, reviewer key, denylist file and variable) try the new name first, then the legacy one.
- **PR-B "rename"**: `git mv` the crates, the fixture and the spec, then apply the map to every identifier, string, doc and fixture. Format readers accept both names and writers emit only the new ones. `protected-paths.txt`, the guard constants and the `.claude` deny rules for `.lilith` change in the same PR, and a repo-wide test fails on any old name outside an explicit allowlist. It is pushed once, complete (B3 of the inventory).
- **PR-C "merge-guard names"** changes `dev/merge-guard.toml` (`repo`, and `reviewer_app_slug` if a rename changes the slug) and the literals that mirror it. The owner renames the repo and the App on GitHub right after it merges.
- **PR-D "cleanup"** comes later (see the last section).

**Tech Stack:** Rust 1.99.0 (MSVC), edition 2024, `cargo-nextest`, `cargo-deny`, Git for Windows `sh` (hooks, credential helper, workflow steps). No new dependencies, no new crate.

**Spec:** `docs/superpowers/specs/2026-10-12-worldwright-suite-design.md`. Task B6 moves it to `docs/superpowers/specs/2026-10-12-lilith-design.md`. The rename edits rule text only: no rule ID changes, and the count stays 981. Sources: `.superpowers/rename-lilith/plan-brief.md` (rulings R1–R7) and `inventory.md` (categories, B1–B6, Appendix B). Neither file is tracked.

## Rulings and decisions

The brief's rulings bind this plan:
- **R1:** the Claude plugin keeps the name `worldwright`.
- **R2:** the repo becomes `<owner>/LILITH` and the App becomes `lilith-reviewer`; the owner does both on GitHub.
- **R3:** the Name map.
- **R4:** legacy compatibility for one release (the map's last column).
- **R5:** the PR sequence.
- **R6:** the owner checklist.
- **R7:** privacy, including the denylist probe.

Decisions this plan makes. Each stands unless the owner says otherwise at review:
- **D1. No personal data in text.** The repo owner is written `<owner>`; the real value lives only in `dev/merge-guard.toml`. Paths are written `%USERPROFILE%`, `%APPDATA%` and `%LOCALAPPDATA%`.
- **D2. When the agent token moves.** The token's reader fallback and its writer both land in PR-A: `set` writes only `LILITH-dev/github/agent-token`. The token lives in Credential Manager, not in a folder, so it needs no `.claude` rule, and the owner can re-store it before the big PR. The reviewer key's writer (`reviewer-setup`) moves only in PR-B, together with the `.claude` deny rules for `%USERPROFILE%\.lilith` (inventory order 7).
- **D3. Workspace-local data is copied forward.** Applies to `%LOCALAPPDATA%\Worldwright[-<ch>]\workspaces\<hash>` and `<vol>:\.worldwright-local\<hash>`.
  - On a read-write open, when the new folder is missing and the legacy one exists, the core copies `state.db` (with any `-wal` and `-shm`) into the new folder through `store::fsops`, then opens the copy.
  - A read-only open uses the new database if it exists, else the legacy one read-only, else the in-memory database it uses today.
  - The legacy files are never written, renamed or deleted (R4: never write old names; `state.db` is never deleted).
- **D4. Both locks count.** The core takes its own `core.lock`. If the other name's `core.lock` exists, it also locks that file, opening it without creating it. It serves read-write only when it holds every lock that exists, and otherwise read-only, which is how a held `core.lock` is handled today (ARCH-028). An old core and a new core never both write to one workspace (B6).
- **D5. The plugin keeps its id, the suite's MCP server becomes `lil`.**
  - The spec's future MCP server (plugin 1.3, built in this repo) is renamed `lil`. Tool names become `mcp__plugin_worldwright_lil__<tool>` and, in Engine A, `mcp__lil__<tool>`.
  - `WW_ENGINE` and `WW_PROFILE` become `LIL_*`.
  - Prose that names the plugin writes it as "the `worldwright` plugin".
- **D6. Whether a rename changes the App's slug is not yet known.** GitHub's docs only say the slug is "based on the name". The owner settles it with a throwaway App before PR-C (owner checklist step 9), and Task C1 handles both outcomes.
- **D7. CI uses neutral names.** The CI transition uses names without "ww": step names "Pick the CLI crate", "Build the CLI from main" and "Choose the tree the CLI is built from", the variable `CLI_BIN`, and the folders `cli-before` and `cli-target`. Only the transition branch itself still says `ww-cli`.
- **D8. No protocol major bump.** The pipe names differ, so builds from before and after the rename never connect to each other (R4, wire).
- **D9. The repo and App names wait for PR-C.** PR-B leaves `Worldwright-Suite` and `worldwright-reviewer` wherever they appear: the toml, the tests that read it, `docs/dev/merge-guard.md` and the spec. A temporary allowlist row covers them, and PR-C removes it.

## Name map

Every task applies this table. "Compat" is the R4 behaviour. A blank Compat cell means a hard switch in PR-B.

| # | Thing | Old | New | Compat |
|---|---|---|---|---|
| 1 | Product name in prose | `Worldwright`, `Worldwright Suite` | `LILITH`; first use in each doc: "LILITH (Latent Image & Likeness Instrumentality Toolchain Hub)" | |
| 2 | CLI | `ww`, `ww.exe`, `[[bin]] name = "ww"`, clap `name = "ww"`, `about = "Worldwright command line"` | `lil`, `lil.exe`, `about = "LILITH command line"` | |
| 3 | Crates | `ww-{cli,client,core,platform,protocol,schema,spec-tests,testkit}`, pattern `ww-dev-guard` | `lil-…`, `lil-dev-guard` | |
| 4 | Import paths | `ww_<crate>::` | `lil_<crate>::` | |
| 5 | Binaries named in code or spec | `ww-core.exe`, `ww-mcp(.exe)`, `ww-image(.exe)`, `ww-hub`, `ww-qt`, `ww-qt-shim`, `ww-ui-kit` | `lil-…` | |
| 6 | Workspace file | `worldwright.yaml` | `lilith.yaml` | found if no `lilith.yaml`; never renamed or written |
| 7 | Workspace data folder | `.worldwright\` (`state.json`, `staging\`, `fence\`, `core.lock`) | `.lilith\` | `state.json` read when the new one is missing; `core.lock` per D4; never written |
| 8 | Workspace-local data | `%LOCALAPPDATA%\Worldwright[-<ch>]\workspaces\<hash>`, `<vol>:\.worldwright-local\<hash>` | `…\LILITH[-<ch>]\workspaces\<hash>`, `<vol>:\.lilith-local\<hash>` | copied forward (D3) |
| 9 | Data folders | `%APPDATA%\Worldwright[-<ch>]`, `%LOCALAPPDATA%\Worldwright[-<ch>]` | `%APPDATA%\LILITH[-<ch>]`, `%LOCALAPPDATA%\LILITH[-<ch>]` | `suite.toml` read from the old folder when the new one is missing; never written there |
| 10 | Install folder, pack ids | `WorldwrightSuite\current`, `WorldwrightSuite`, `WorldwrightSuite.Dev` | `LILITHSuite\current`, `LILITHSuite`, `LILITHSuite.Dev` (no hyphen: the guard reads `<name>-x` as a channel folder) | |
| 11 | Home folder | `%USERPROFILE%\.worldwright\` (`reviewer\app-key.age`, `privacy-denylist.txt`) | `%USERPROFILE%\.lilith\` | read new, else old (PR-A); `reviewer-setup` writes new (PR-B) |
| 12 | Env vars | `WW_*`, incl. compile-time `WW_BUILD_CHANNEL`, `WW_BUILD_ID`, `WW_PROTOCOL_BUILD_SUFFIX`, spec-only `WW_ENGINE`, `WW_PROFILE`, lowercase `ww_channel` | `LIL_*` (`lil_channel`) | `LIL_PRIVACY_DENYLIST`, else `WW_PRIVACY_DENYLIST` (PR-A) |
| 13 | Named pipes | `\\.\pipe\worldwright-core-<SID>-<ch>`, `\\.\pipe\worldwright-app-…` | `\\.\pipe\lilith-core-…`, `\\.\pipe\lilith-app-…` | old pipe not served |
| 14 | Window class and title | `WorldwrightCoreSessionEnd`, `Worldwright core` | `LilithCoreSessionEnd`, `LILITH core` | |
| 15 | Toast AUMID | `Worldwright.Suite.<Channel>` | `LILITH.Suite.<Channel>` | |
| 16 | Registry root | `HKCU\Software\Worldwright[-<ch>]` | `HKCU\Software\LILITH[-<ch>]` | |
| 17 | Credential prefixes | `Worldwright[-<ch>]/`, `Worldwright-dev/`, `Worldwright-test-<id>/` | `LILITH[-<ch>]/`, `LILITH-dev/`, `LILITH-test-<id>/` | legacy `Worldwright-dev/`: get and delete only |
| 18 | Agent token target | `Worldwright-dev/github/agent-token` | `LILITH-dev/github/agent-token` | read new, then old; `set` writes new; `clear` removes both (PR-A) |
| 19 | JSON-RPC methods | `ww/subscribe`, `ww/unsubscribe`, `ww/event`, reserved prefix `ww/` | `lil/…` | none (R4, wire) |
| 20 | Reference URI | `ww://` | `lil://` | parsed; always written `lil://` |
| 21 | URL protocol (spec) | `ww:open/`, `ww:confirm/`, `ww:task/` | `lil:…` | |
| 22 | Dot-file markers | `.ww-tmp-`, `.ww-bak-`, `.ww-conflict-` | `.lil-tmp-`, `.lil-bak-`, `.lil-conflict-` | recovery and the index recognise both; existing conflict copies are left alone |
| 23 | Embedded keys (spec) | `application/x-worldwright-ref`, `extra.worldwright` | `application/x-lilith-ref`, `extra.lilith` | |
| 24 | MCP names (spec) | tool `ww.status`, field `ww.profile`, server `ww`, `mcp__plugin_worldwright_ww__`, `mcp__ww__` | `lil.status`, `lil.profile`, server `lil`, `mcp__plugin_worldwright_lil__`, `mcp__lil__` (D5) | |
| 25 | Other identifiers | threads `ww-session-end`, `ww-dir-watcher`; temp names `ww-paths-`, `ww-no-such-tool-on-path`, `ww-platform-no-such-dir`; helpers `ww_with_stdin`, `ww_spawn`, `ww_env`, `ww_in`; test names `*worldwright*`; key `ww_cli_s` | `lil-…`, `lil_…`, `*lilith*`, `lil_cli_s` | |
| 26 | CI names | `$WW`, `steps.src.outputs.ww`, `ww-before`, `ww-target`, step names with "ww" | D7 names (PR-A) | |
| 27 | Spec file | `docs/superpowers/specs/2026-10-12-worldwright-suite-design.md` | `docs/superpowers/specs/2026-10-12-lilith-design.md` | |
| 28 | Backup repo (spec OPS-004, OPS-011) | `Worldwright-Workspace(.git)` | `LILITH-Workspace(.git)` | |
| 29 | GitHub repo | `<owner>/Worldwright-Suite` | `<owner>/LILITH` | GitHub redirects; PR-C |
| 30 | Reviewer App | `worldwright-reviewer`, `worldwright-reviewer[bot]` | `lilith-reviewer` | PR-C (D6) |
| 31 | Actions secret | `WW_PRIVACY_DENYLIST` | `LIL_PRIVACY_DENYLIST` | both variables set from the new secret until PR-D |
| 32 | Director agent | unnamed | **MAGI Core**, at AI-041 only | |

**Unchanged:** the plugin id `worldwright` (R1), plan file names, rule IDs and the count 981, `core.hooksPath`, the required check names, the `.damaged-<utc>` set-aside and the spike folders' text.

**How to match:**
- Replace only at word boundaries: `\bww\b`, `\bww[-_:/.]`, `\bWW_`, `\.ww-`, `ww://`, case-insensitive `worldwright`. Never a bare case-insensitive `ww`.
- These must survive: `www` in URLs, `wwwauth[]` (`agent_access.rs`), `ShowWindow` and `NewWindow` (spikes), and "Magic wand" (IMG-055, IMG-150).
- Windows compares these names case-insensitively. A test that checks case handling keeps its shape with the new name: `"worldwright-DEV/x"` becomes `"lilith-DEV/x"`, and `.WorldWright-Local` becomes `.LiLiTh-Local`.

## Global Constraints

- **Branches:**
  - Cut `rename-ready` (PR-A), `rename` (PR-B) and `rename-merge-guard` (PR-C) from `main`, each after the previous one merges.
  - Commit after every task. Each message ends with `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`.
  - Merge with `ww dev merge <PR>` for PR-A and `lil dev merge <PR>` afterwards.
- **Protected files** (QA-019/020; every change needs `dev approve` on the exact head). Each task names its own:
  - **PR-A, 9 files:**
    - the four workflows `ci-guard.yml`, `ci-push.yml`, `ci-pr-ready.yml` and `post-merge.yml`;
    - `crates/ww-cli/src/dev/{workflows,agent_access,reviewer,privacy}.rs`;
    - the new `tools/git-hooks/credential-helper`.
  - **PR-B, 63 files:** the 64 files of inventory Appendix B except `dev/merge-guard.toml`, at their `crates/lil-*` paths. 20 of them only move.
  - **PR-C, up to 7 files:** `dev/merge-guard.toml` and `crates/lil-cli/src/dev/{merge,post_merge,reviewer,github,github_fake,mod}.rs`.
- **Rule IDs:** no `- **<ID>** [` line is added or removed. `lil dev spec-report` must report 981 rules and no dangling citations.
- **Behaviour:** nothing changes beyond the names and the R4 readers. The old pipe, old wire names and old writers don't survive PR-B.
- **Legacy names in code** appear only in `LEGACY_*` constants and on the lines that use them:
  - `ww_platform::names` holds every one except the URI scheme;
  - the scheme is `LEGACY_SCHEME` in `refs.rs`, because `ww-schema` depends on no other crate (QA-027).
- **Privacy** (R7, QA-086): no user names, local user paths or e-mail addresses. Never print the denylist; check it only with the probe in Task A1.

## Review Focus

1. **The agent token is stored only under the legacy name:** `merge` and `git push` still authenticate, `set` writes only the new name and `clear` removes both. Covered by Task A2, `resolve_token_falls_back_to_the_legacy_target` and `set_writes_only_the_new_target`.
2. **An old core still serves a workspace when the new core starts:** the new core serves it read-only and never writes beside the old core (D4). Covered by Task B4, `held_legacy_core_lock_keeps_the_new_core_read_only`.
3. **A crash before the upgrade left `.x.ww-tmp-…` and `.x.ww-bak-…` files:** recovery restores them, the index never lists them as user files, and new writes use only `lil` markers. Covered by Task B4, `recovery_restores_legacy_leftovers_and_the_index_skips_them`.
4. **CI runs on PR-B:** every judge builds the CLI crate that exists in the main or `before` tree, never the pushed commit (R38), and the scan sees the denylist under either variable name. Covered by Task A4, `judges_pick_the_cli_crate_from_the_tree_they_build`.
5. **The renamed core opens a plugin-era workspace** (`worldwright.yaml`, `.worldwright\state.json`): it is found and served, the active project survives, new state goes to `.lilith\`, and no file of the person's is renamed or rewritten. Covered by Task B4, `legacy_workspace_is_served_and_never_renamed`.

## Execution notes

- **`<cli>`** means `ww-cli` with `--bin ww` until Task B2 commits, and `lil-cli` with `--bin lil` from then on.
- **The full gate**, run by the last task of each PR, in order:
  1. `cargo fmt --check`
  2. `cargo clippy --workspace --all-targets -- -D warnings`
  3. `cargo deny check`
  4. `cargo nextest run --workspace`
  5. `cargo run -q -p <cli> -- dev spec-tests`. Expected: `spec tests: 66 passed, 0 expected-fail`.
  6. `cargo run -q -p <cli> -- dev spec-report`. Expected: 981 rules, no dangling citations.
  7. `cargo run -q -p <cli> -- dev privacy-scan --tree HEAD`. Expected: exit 0.
- **Inner loop:** each task's "Run" lines. Tasks B2–B6 can't pass `no_stale_names` until B6, so they run their own packages.
- **Adversarial review** (QA-023) for Tasks A2, B3 and B4, which change credentials, `state` and `store`. The reviewer must try to write a failing test.

---

## PR-A "rename-ready"

### Task A1: Denylist probe, `names`, and the test guard for both names

**Files:**
- Create: `crates/ww-platform/src/names.rs`. Modify `crates/ww-platform/src/{lib.rs, guard.rs, credentials.rs}`.
- Test: unit tests in `guard.rs` and `credentials.rs`.

**Interfaces:**

```rust
// ww-platform/src/names.rs: the new names, and every legacy name the code still reads (PR-D removes LEGACY_*).
pub const APP_NAME: &str = "LILITH";                pub const LEGACY_APP_NAME: &str = "Worldwright";
pub const HOME_DIR: &str = ".lilith";               pub const LEGACY_HOME_DIR: &str = ".worldwright";
pub const LOCAL_DIR: &str = ".lilith-local";        pub const LEGACY_LOCAL_DIR: &str = ".worldwright-local";
pub const DEV_CREDENTIAL_PREFIX: &str = "LILITH-dev/";
pub const LEGACY_DEV_CREDENTIAL_PREFIX: &str = "Worldwright-dev/";
pub const DENYLIST_ENV: &str = "LIL_PRIVACY_DENYLIST";
pub const LEGACY_DENYLIST_ENV: &str = "WW_PRIVACY_DENYLIST";
// credentials.rs
pub enum TargetUse { Read, Write, Delete }
pub fn check_target(target: &str, use_: TargetUse) -> Result<(), CredentialError>;
    // LILITH-dev/<name>: every use. Worldwright-dev/<name>: Read and Delete only.
    // Worldwright-test-<id>/<name>: every use (test channels keep the old prefix until Task B3).
```

- [ ] **Step 1: Probe the denylist (R7).** Never print the denylist.

```bash
blob=$(printf 'LILITH lilith lil lil-core lil_core LIL_CHANNEL lil:// .lilith LILITH-dev LILITHSuite\nMAGI Core magi\nLatent Image & Likeness Instrumentality Toolchain Hub\n' | git hash-object -w --stdin)
tree=$(printf '100644 blob %s\tprobe.txt\n' "$blob" | git mktree)
commit=$(git commit-tree "$tree" -m probe)
cargo run -q -p ww-cli --bin ww -- dev privacy-scan --tree "$commit"
```

Expected: exit 0. The dangling objects are never pushed. Any finding: stop and ask the owner to edit the denylist (owner checklist step 1).
- [ ] **Step 2: Write the failing tests:**
  - `guard_blocks_lilith_and_legacy_data_folders`: under fake roots, `LILITH`, `lilith-dev`, `Worldwright` and `worldwright-test-x` are refused, and so are path components `.lilith-local` and `.Worldwright-Local`;
  - `lilithsuite_is_not_a_channel_folder`: the `WorldwrightSuite` "NotAChannelFolder" test, repeated for `LILITHSuite`;
  - `guard_blocks_both_real_credential_prefixes`: `LILITH/x`, `LILITH-dev/x`, `Worldwright/x` and `Worldwright-dev/x` are refused, and the test-channel prefix is allowed;
  - `guard_blocks_both_registry_roots`;
  - `check_target_accepts_the_new_dev_prefix_for_every_use`;
  - `check_target_refuses_writing_the_legacy_dev_prefix` (`Write` on `Worldwright-dev/x` gives `TargetRefused`; `Read` and `Delete` pass).
- [ ] **Step 3: Run them.** Run: `cargo nextest run -p ww-platform`. Expected: FAIL.
- [ ] **Step 4: Implement them.**
  - The guard checks both names wherever it checks one today.
  - `check_target` callers pass their use: `get` gives `Read`, `set` gives `Write`, `delete` gives `Delete`.
  - `DEV_PREFIX` becomes `names::DEV_CREDENTIAL_PREFIX`. `Env` and the data folders stay on the legacy name until Task B3.
- [ ] **Step 5: Run them.** Run: `cargo nextest run -p ww-platform -p ww-cli`. Expected: PASS.
- [ ] **Step 6: Commit.**

```bash
git add crates/ww-platform
git commit -m "platform: names module; test guard and credential allowlist know LILITH and legacy names"
```

### Task A2: Owner-tooling readers: agent token, reviewer key, denylist (protected: `agent_access.rs`, `reviewer.rs`, `privacy.rs`)

**Files:**
- Modify (protected): `crates/ww-cli/src/dev/{agent_access.rs, reviewer.rs, privacy.rs}`, with their unit tests and module docs.

**Interfaces:**

```rust
// agent_access.rs
pub const AGENT_TOKEN_TARGET: &str = "LILITH-dev/github/agent-token";
pub const LEGACY_AGENT_TOKEN_TARGET: &str = "Worldwright-dev/github/agent-token";
fn stored_token(store: &dyn CredentialStore) -> Result<Option<(Zeroizing<String>, &'static str)>>;
    // new target, then legacy when the new one is missing or blank; returns the target that answered
// reviewer.rs
pub fn key_path_in(profile: &Path) -> PathBuf;          // <profile>\.lilith\reviewer\app-key.age (reviewer-setup writes it from Task B5)
pub fn legacy_key_path_in(profile: &Path) -> PathBuf;   // <profile>\.worldwright\reviewer\app-key.age
pub fn find_key_in(profile: &Path) -> Option<PathBuf>;  // new if it exists, else legacy if it exists
// privacy.rs
pub fn load_denylist(env_new: Option<OsString>, env_legacy: Option<OsString>, files: &[&Path]) -> Result<Denylist, String>;
    // LIL_PRIVACY_DENYLIST, else WW_PRIVACY_DENYLIST, else the first file that exists: .lilith\, then .worldwright\
```

**Rules:**
- `resolve_token` and the `git-credential get` path use `stored_token`.
- `agent-token set` writes only `AGENT_TOKEN_TARGET`. `clear` deletes both targets. Both stay terminal-only: agents never write the token (R4).
- `status` names the target that answered. For the legacy target it adds: "stored under the legacy name; run `ww dev agent-token set` at a terminal to store it under the new name".
- `approve` uses `find_key_in` and prints the path it used. The "no reviewer key" message names both paths. `reviewer-setup` still writes the legacy path until Task B5.

- [ ] **Step 1: Write the failing tests** (fake store and temp profiles):
  - token: `resolve_token_prefers_the_new_target`, **`resolve_token_falls_back_to_the_legacy_target`** (Review Focus 1), `blank_new_target_falls_back`, **`set_writes_only_the_new_target`** (Review Focus 1: the fake store's legacy entry is untouched, and no write names it), `clear_removes_both_targets`, `status_names_the_target_that_answered`, `git_credential_get_falls_back_to_the_legacy_target`;
  - key: `key_lookup_prefers_the_new_path`, `key_lookup_finds_the_legacy_path`, `missing_key_message_names_both_paths`;
  - denylist: `denylist_env_prefers_the_new_variable`, `denylist_env_falls_back_to_the_legacy_variable`, `denylist_file_prefers_the_new_folder`, `denylist_file_falls_back_to_the_legacy_folder`, `no_denylist_anywhere_is_still_a_refusal`.
- [ ] **Step 2: Run them.** Run: `cargo nextest run -p ww-cli -E 'test(token) | test(key_) | test(denylist)'`. Expected: FAIL.
- [ ] **Step 3: Implement it.**
- [ ] **Step 4: Run them.** Run: `cargo nextest run -p ww-cli -p ww-platform`. Expected: PASS.
- [ ] **Step 5: Commit.**

```bash
git add crates/ww-cli/src/dev
git commit -m "dev: agent token, reviewer key and denylist read the LILITH name first, then the legacy one"
```

### Task A3: A committed credential helper that runs either crate (protected: new `tools/git-hooks/credential-helper`, `agent_access.rs`)

**Files:**
- Create (protected): `tools/git-hooks/credential-helper`, a POSIX `sh` script. `.gitattributes` already gives `tools/git-hooks/*` LF line endings.
- Modify (protected): `crates/ww-cli/src/dev/agent_access.rs` (`GIT_CREDENTIAL_HELPER`).
- Test: `crates/ww-cli/tests/credential_helper.rs` (not protected).

**Interfaces:**

```rust
pub const GIT_CREDENTIAL_HELPER: &str =
    r#"!sh "$(git rev-parse --show-toplevel)/tools/git-hooks/credential-helper""#;
```

The script:
1. Finds cargo the way `pre-push` does: on `PATH`, else in `$HOME/.cargo/bin`.
2. Runs `cd "$(git rev-parse --show-toplevel)"`.
3. Runs `exec cargo run -q --locked -p lil-cli --bin lil -- dev git-credential "$@"` if `crates/lil-cli/Cargo.toml` exists, else the same with `-p ww-cli --bin ww` if `crates/ww-cli/Cargo.toml` exists.
4. Otherwise prints "credential-helper: this checkout has no CLI crate" and exits 1.

- [ ] **Step 1: Write the failing tests.** Run the script with `sh`, the way the `pre-push` tests in `dev/hooks.rs` run their hook. A fake `cargo` comes first on `PATH` and prints its arguments, and stdin is passed through.
  - `helper_runs_lil_cli_when_the_checkout_has_it`;
  - `helper_runs_ww_cli_when_only_it_exists`;
  - `helper_fails_plainly_without_a_cli_crate`;
  - `helper_passes_the_action_and_stdin_through`.
  - The existing `install_hooks_writes_the_helper_reset_and_the_helper` stays as it is; it pins the constant.
- [ ] **Step 2: Run them.** Run: `cargo nextest run -p ww-cli -E 'binary(credential_helper)'`. Expected: FAIL.
- [ ] **Step 3: Implement it.**
- [ ] **Step 4: Run them.** Run: `cargo nextest run -p ww-cli`. Expected: PASS.
- [ ] **Step 5: Commit.**

```bash
git add tools/git-hooks/credential-helper crates/ww-cli/src/dev/agent_access.rs crates/ww-cli/tests/credential_helper.rs
git commit -m "dev: credential helper is a committed script that runs lil-cli or ww-cli"
```

### Task A4: CI judges pick the CLI crate; both denylist names (protected: four workflows, `workflows.rs`)

**Files:**
- Modify (protected): `.github/workflows/{ci-guard.yml, ci-push.yml, ci-pr-ready.yml, post-merge.yml}` and `crates/ww-cli/src/dev/workflows.rs`.

**Rules (B1, B2):**
- Every step that builds the CLI from **main** or **`before`** is preceded by a step "Pick the CLI crate", which runs in that tree. The steps are `protected-test-check`, `privacy-check`, `privacy-check-pr` and `post-merge-check`.
  - The pick step tests `crates/lil-cli/Cargo.toml`, then `crates/ww-cli/Cargo.toml`. For `before` it uses `git cat-file -e "${BEFORE}:<path>"`.
  - It sets outputs `pkg` and `bin`, or refuses as the step does today.
  - The build and run lines use `-p "$pkg" --bin "$bin"` and `"$CLI_BIN"`.
  - The variable and folder names are the D7 names.
  - R38 still holds: nothing is ever built from `$GITHUB_WORKSPACE`'s pushed commit.
- Steps that build the PR's or the pushed tree itself (`every-push`, `pr-ready`, `post-merge-tests`) keep `-p ww-cli` until Task B2.
- Each scan step's `env:` sets `LIL_PRIVACY_DENYLIST` and `WW_PRIVACY_DENYLIST`, both from `${{ secrets.LIL_PRIVACY_DENYLIST }}`. No other step sees the secret.

- [ ] **Step 1: Write the failing tests** in `workflows.rs`, updating the existing exact pins to the D7 names:
  - **`judges_pick_the_cli_crate_from_the_tree_they_build`** (Review Focus 4): each of the four judges has the pick step with both branches, runs it in the main or `before` tree, and builds with its outputs;
  - `scan_steps_set_both_denylist_names_from_the_new_secret`;
  - `no_step_outside_a_scan_sees_the_denylist_secret`;
  - the existing R38 tests stay green: no `$GITHUB_WORKSPACE` in the source step, and refusal when no crate is found.
- [ ] **Step 2: Run them.** Run: `cargo nextest run -p ww-cli -E 'test(workflow) | test(judge) | test(scan_steps)'`. Expected: FAIL.
- [ ] **Step 3: Edit the four workflows.**
- [ ] **Step 4: Run them.** Run: `cargo nextest run -p ww-cli`. Expected: PASS.
- [ ] **Step 5: Commit.**

```bash
git add .github/workflows crates/ww-cli/src/dev/workflows.rs
git commit -m "ci: judges pick lil-cli or ww-cli from the tree they build; scans get both denylist names"
```

### Task A5: PR-A close-out

- [ ] **Step 1:** Run the full gate (`<cli>` = `ww-cli`). Every step passes.
- [ ] **Step 2:** Confirm with the owner that the secret `LIL_PRIVACY_DENYLIST` exists (owner checklist step 1), then push `rename-ready` and open the PR. It lists the 9 protected files for approval.
- [ ] **Step 3:** Once the owner has approved it (step 2), merge with `ww dev merge <PR>`. The owner then runs step 3 (`install-hooks` in each clone) before any PR-B push.

---

## PR-B "rename"

PR-B is built locally from Tasks B1–B7 and pushed once, after Task B7's gate (B3).

### Task B1: The repo-wide "no stale names" test

**Files:**
- Create: `crates/ww-testkit/tests/no_stale_names.rs`. It is not protected, and it moves to `crates/lil-testkit/` in Task B2.

**Interfaces:**

```rust
struct Allow { globs: &'static [&'static str], lines_with: &'static [&'static str], why: &'static str, until: &'static str }
const ALLOW: &[Allow];   // the table below, verbatim; an empty `lines_with` allows the whole file
fn is_stale(text: &str) -> bool;
    // case-insensitive "worldwright", or "ww" (any case) with no ASCII letter or digit on either side
```

| Globs | Lines containing | Why | Until |
|---|---|---|---|
| `docs/superpowers/plans/**` | (whole file) | executed plans are history (R3) | never |
| `docs/spikes/**`, `spikes/**` | (whole file) | M0 spike records are history | never |
| `**/*.rs` | `LEGACY_` | legacy readers (R4) and their tests | PR-D |
| `.github/workflows/*.yml`, `tools/git-hooks/credential-helper`, `crates/lil-cli/src/dev/workflows.rs` | `ww-cli`, `ww.exe`, `--bin ww`, `WW_PRIVACY_DENYLIST` | CI and helper transition (B1, B2, B5) | PR-D |
| `.claude/settings.json`, `crates/lil-testkit/tests/claude_settings_patterns.rs` | `.worldwright` | deny rules stay while that folder exists | owner says the folder is gone |
| `docs/superpowers/specs/2026-10-12-lilith-design.md` | `` `worldwright` plugin ``, `plugin_worldwright_` | R1, D5 | plugin rename |
| `**` | `Worldwright-Suite`, `worldwright-reviewer` | repo and App names (D9) | PR-C |

- [ ] **Step 1: Write the tests:**
  - `no_stale_names_outside_the_allowlist`: every path from `git ls-files -z` at the repo root, and the text of every file that is valid UTF-8 with no NUL in its first 8 KiB. It fails with one `path:line: text` per hit.
  - `every_allowlist_row_still_allows_something`, so rows can't rot.
  - `is_stale_finds_old_names_and_ignores_lookalikes`: `ww dev`, `ww_core`, `WW_X`, `.ww-tmp-`, `ww://`, `ww/event` and `Worldwright` are hits. `www.example`, `wwwauth[]`, `ShowWindow`, `lil` and `lilith` are not.
- [ ] **Step 2: Run them.** Run: `cargo nextest run -p ww-testkit -E 'binary(no_stale_names)'`. Expected: `is_stale_…` passes; the other two fail with thousands of hits. They stay red until Task B6.
- [ ] **Step 3: Commit.**

```bash
git add crates/ww-testkit/tests/no_stale_names.rs
git commit -m "testkit: repo-wide no-stale-names test with an explicit allowlist (red until the rename)"
```

### Task B2: Crates, CLI and build plumbing, in one commit (protected: see Files)

**Files:**
- `git mv crates/ww-<x> crates/lil-<x>` for all eight crates. This moves the 20 hit-free protected files.
- Modify each `Cargo.toml`: names, path deps, `[[bin]] name = "lil"`.
- Modify the root `Cargo.toml` comments (protected), `Cargo.lock`, `crates/lil-core/clippy.toml` (method paths; silently dead if missed), the comments in `deny.toml` and `dev/rule-classification.toml`, and every `ww_*::` import.
- Protected:
  - `.config/nextest.toml` (filters and the setup script);
  - `test-params.toml` (`lil_cli_s` and comments);
  - `crates/lil-cli/Cargo.toml` and `crates/lil-cli/src/main.rs` (clap name and about);
  - all 14 files of `crates/lil-cli/src/dev/`;
  - `dev/protected-paths.txt` (map rows 3 and 27);
  - `tools/git-hooks/{pre-push, pre-merge-commit}` (`-p lil-cli --bin lil`, message text);
  - the pushed-tree steps of the four workflows (`-p lil-cli`);
  - the 15 spec-test files, for the edits this task forces (imports, crate names, CLI name).
- Also modify: `crates/lil-testkit/src/bins.rs` (`lil-core.exe`, `-p lil-cli -p lil-core`), `crates/lil-testkit/tests/dependency_boundaries.rs`, and `crates/lil-core/src/identity.rs` (`APP_EXES`, `MCP_EXE`, map row 5).

**Rules:**
- In `crates/lil-cli/src/dev/guard.rs`, `EXPECTED_FAIL_PATH` and `SPEC_CRATE_DIR` move to `crates/lil-spec-tests/…`, and the pinned path tests (inventory section 7) move with them.
- The union of the base and head lists (`guard.rs`) keeps the PR judgeable, because main's judge still knows the `ww` patterns.
- `spec_tests.rs` passes `-p lil-spec-tests`. JUnit class names become `lil-spec-tests::…`.
- `hooks.rs` `HANDOFF` and `REFUSAL_HEAD`, and `root.rs`'s "this lil was built for …", follow the map, and so do their tests.

- [ ] **Step 1: Update the pinned tests first:** `guard.rs` path pins, `workflows.rs` pushed-tree pins, `hooks.rs` `HANDOFF`, `crates/lil-cli/tests/repo_root.rs` `REFUSAL`, and the `user/mod.rs` argv `["lil", …]`.
- [ ] **Step 2: Run them.** Run: `cargo nextest run -p ww-cli`. Expected: FAIL; the pins name `lil` and the code still says `ww`.
- [ ] **Step 3: Do the moves and edits.** Regenerate the lock with `cargo update --workspace --offline`.
- [ ] **Step 4: Run.** Run: `cargo build --locked --workspace --all-targets && cargo clippy --workspace --all-targets -- -D warnings && cargo nextest run -p lil-cli -p lil-testkit -E 'not binary(no_stale_names)' && cargo run -q -p lil-cli --bin lil -- dev spec-tests`. Expected: PASS; `spec tests: 66 passed, 0 expected-fail`.
- [ ] **Step 5: Commit.**

```bash
git add -A crates Cargo.toml Cargo.lock deny.toml .config/nextest.toml test-params.toml dev/protected-paths.txt dev/rule-classification.toml tools/git-hooks .github/workflows
git commit -m "rename: crates ww-* -> lil-*, CLI ww -> lil, protected paths and guard constants"
```

### Task B3: Platform names: data folders, Windows objects, env vars, `suite.toml` and local data (D3) — **protected: spec-test files it breaks, if any**

**Files:**
- Modify:
  - `crates/lil-platform/src/{names.rs, env.rs, process_env.rs, session_end.rs, watcher.rs, guard.rs, credentials.rs, paths.rs, fs.rs}`;
  - `crates/lil-protocol/build.rs`;
  - `crates/lil-core/src/{store/failpoints.rs, store/logs.rs, store/suite_toml.rs, state/mod.rs, identity.rs}`;
  - `crates/lil-testkit/src/{env.rs, failpoints.rs, raw_pipe.rs}`;
  - every `WW_*` test helper variable;
  - platform and testkit tests.
- Protected: only the spec-test files whose strings this task changes (for example pipe or data-folder names); the commit message names them.

**Interfaces:**

```rust
// env.rs: map rows 8, 9, 10, 13, 15, 16, 17
impl Env {
    pub fn suite_toml(&self) -> PathBuf;                    // %APPDATA%\LILITH[-<ch>]\suite.toml (writes go here only)
    pub fn suite_toml_to_read(&self) -> PathBuf;            // suite_toml() if it exists, else the legacy file if that exists, else suite_toml()
    pub fn legacy_workspace_local_dir_for_canonical(&self, canonical: &Path) -> Result<PathBuf, EnvError>;
}
// ww-core state: D3
fn open_workspace_db(env: &Env, ws: &Path, access: Access) -> Result<StateDb, StateError>;  // copy-forward per D3
```

- [ ] **Step 1: Write the failing tests.** First update the existing pins in `env.rs` and testkit `env.rs` to the map's new values: data folder, registry root, credential prefix, pipe and AUMID. Then add:
  - `suite_toml_prefers_the_new_file`;
  - `suite_toml_reads_the_legacy_file_when_the_new_one_is_missing`;
  - `suite_toml_registration_never_writes_the_legacy_file`: after `register_workspace` with only the legacy file present, the legacy bytes are unchanged and the new file holds the entry;
  - `local_db_is_copied_forward_once_and_the_legacy_is_untouched`, for the same-volume and other-volume layouts;
  - `existing_new_local_db_wins_over_the_legacy`;
  - `read_only_open_reads_the_legacy_db_without_writing`;
  - `test_channel_credential_prefix_is_lilith_test`;
  - `env_vars_are_lil_prefixed`, which pins `process_env` names and the build-time variables.
- [ ] **Step 2: Run them.** Run: `cargo nextest run -p lil-platform -p lil-core -p lil-testkit -E 'not binary(no_stale_names)'`. Expected: FAIL.
- [ ] **Step 3: Implement it.**
  - `Channel::app_name()` uses `names::APP_NAME`.
  - `check_target`'s test prefix becomes `LILITH-test-`.
  - The guard keeps both names (Task A1).
  - Window class and title, thread names and temp names follow map rows 14 and 25.
- [ ] **Step 4: Run them.** Run: the same command, plus `cargo run -q -p lil-cli --bin lil -- dev spec-tests`. Expected: PASS; 66 passed.
- [ ] **Step 5: Commit.**

```bash
git add -A crates
git commit -m "rename: LILITH data folders, Windows names and LIL_ env; suite.toml and local data read the legacy names"
```

### Task B4: Workspace formats and wire: file, folder, locks, markers, refs, methods — **protected: spec-test files and the fixture below**

**Files:**
- Modify:
  - `crates/lil-platform/src/names.rs` (`WORKSPACE_FILE`, `WORKSPACE_DIR`, `LEGACY_*`, the markers);
  - `crates/lil-client/src/workspace.rs` (drop `WORKSPACE_MANIFEST`);
  - `crates/lil-core/src/tools/{workspace_tools.rs, doctor_tools.rs}` (drop `WORKSPACE_YAML`);
  - `crates/lil-core/src/{projects/active.rs, store/staging.rs, store/write.rs, store/fsops.rs, store/order.rs, store/suite_toml.rs, index/reconcile.rs, watch/git.rs, workspace.rs}`;
  - `crates/lil-schema/src/{refs.rs, lib.rs}` and the ref producers in `lil-core`;
  - `crates/lil-protocol/src/{jsonrpc.rs, events.rs}`;
  - `crates/lil-testkit/src/handles.rs`;
  - core and client tests.
- Protected: the spec-test files whose strings this task changes (`asset_record.rs`, `projects_settings.rs`, `versions_trash.rs`, `ws_tools.rs`, `index_watch.rs`, `doctor.rs`, `store_order.rs`, `store_writes.rs`, `core_events.rs`) and `git mv crates/lil-spec-tests/fixtures/sample-workspace/worldwright.yaml …/lilith.yaml`.

**Interfaces:**

```rust
// names.rs (map rows 6, 7, 22)
pub const WORKSPACE_FILE: &str = "lilith.yaml";   pub const LEGACY_WORKSPACE_FILE: &str = "worldwright.yaml";
pub const WORKSPACE_DIR: &str = ".lilith";        pub const LEGACY_WORKSPACE_DIR: &str = ".worldwright";
pub const TMP_MARKER: &str = ".lil-tmp-";         pub const LEGACY_TMP_MARKER: &str = ".ww-tmp-";
pub const BAK_MARKER: &str = ".lil-bak-";         pub const LEGACY_BAK_MARKER: &str = ".ww-bak-";
pub const CONFLICT_MARKER: &str = ".lil-conflict-"; pub const LEGACY_CONFLICT_MARKER: &str = ".ww-conflict-";
pub fn workspace_file_in(dir: &Path) -> Option<PathBuf>;   // lilith.yaml if it is a file, else worldwright.yaml if it is, else None
// refs.rs (map row 20)
pub const SCHEME: &str = "lil://";  pub const LEGACY_SCHEME: &str = "ww://";   // FromStr accepts both; Display writes SCHEME
```

**Rules:**
- **Workspace detection.** Detection, `workspace.create`'s adopt check and `workspace.set_default`'s check all use `workspace_file_in`. `workspace.create` writes only `lilith.yaml`.
- **`state.json`.** Read `.lilith\state.json`, else `.worldwright\state.json`. Writes go only to `.lilith\`.
- **Fixed folders.** Staging and the test fence use `.lilith\` only.
- **Locks.** `core.lock` follows D4. A new `fsops::lock_existing_exclusive` opens an existing file only.
- **Reconcile** skips root folders `.lilith` and `.worldwright`, and files with any new or legacy marker.
- **Markers.** `parse_leftover`, crash recovery and the conflict recognisers in `order.rs` and `suite_toml.rs` accept both marker sets. Writers (`Names::pick`, the conflict copy) use only the new markers. `NAME_SUFFIX_UNITS` counts `.lil-tmp-` (9 units).
- **`.gitignore`.** `GITIGNORE_ENTRIES[0]` becomes `.lilith/`. `doctor` never removes a `.worldwright/` line, and its text says "LILITH entries".
- **Topics.** The topic of a legacy ref is `asset:lil://…`.
- **JSON-RPC.** Methods are `lil/subscribe`, `lil/unsubscribe` and `lil/event`. The reserved-prefix test lists `lil/` and `app/`.

- [ ] **Step 1: Write the failing tests.** Fixtures build legacy names from `LEGACY_*`.
  - **`legacy_workspace_is_served_and_never_renamed`** (Review Focus 5): a workspace with `worldwright.yaml` and `.worldwright\state.json` (active project `p2`) is found from a subfolder and served, and `projects.active` is `p2`. After `projects.set_active`, `.lilith\state.json` holds the change. `worldwright.yaml` and `.worldwright\` keep their SHA-256, and no `lilith.yaml` exists.
  - `workspace_file_prefers_lilith_yaml`, `create_writes_only_lilith_yaml_and_adopts_a_legacy_workspace`.
  - **`held_legacy_core_lock_keeps_the_new_core_read_only`** (Review Focus 2): another process holds `.worldwright\core.lock`, the hub opens read-only, and nothing is created under `.worldwright\`. Also `absent_legacy_lock_is_never_created`.
  - **`recovery_restores_legacy_leftovers_and_the_index_skips_them`** (Review Focus 3): leftovers `.a.txt.ww-bak-…` and `.a.txt.ww-tmp-…` from the ARCH-074 crash shapes are restored or removed as their new-marker twins are, and `assets.query` never lists them. Also `legacy_conflict_copies_are_left_alone_and_not_indexed`.
  - `leftover_names_parse_back` gains legacy rows, and `writer_names_use_only_lil_markers`.
  - `legacy_ww_ref_parses_to_the_same_ref`, `refs_display_with_lil_scheme`, `topic_of_a_legacy_ref_uses_lil`.
  - `reconcile_skips_both_workspace_folders`, `gitignore_entries_name_lilith`, `rpc_methods_are_lil_prefixed`.
- [ ] **Step 2: Run them.** Run: `cargo nextest run -p lil-schema -p lil-protocol -p lil-client -p lil-core -E 'not binary(no_stale_names)'`. Expected: FAIL.
- [ ] **Step 3: Implement it.**
- [ ] **Step 4: Run them.** Run: the same command, plus `cargo run -q -p lil-cli --bin lil -- dev spec-tests`. Expected: PASS; 66 passed.
- [ ] **Step 5: Commit.**

```bash
git add -A crates
git commit -m "rename: lilith.yaml, .lilith, lil markers, lil:// and lil/* with legacy readers"
```

### Task B5: Owner-tooling writers and the `.claude` deny rules (protected: `reviewer.rs`, `privacy.rs`, `hooks.rs`, `mod.rs`, `.claude/settings.json`)

**Files:**
- Modify (protected): `crates/lil-cli/src/dev/{reviewer.rs, privacy.rs, hooks.rs, mod.rs}` and `.claude/settings.json`.
- Modify: `crates/lil-testkit/tests/claude_settings_patterns.rs`.

**Rules:**
- `reviewer-setup` writes only `key_path_in` (`.lilith`).
- Messages and docs name the new paths and `lil`. `privacy.rs`'s `DENYLIST_ENV` comes from `names`.
- `.claude/settings.json` gains, for `.lilith`, the same rule set it has today for `.worldwright`: the `cat`, `type`, `Get-Content` and `gc` rules for Bash and PowerShell, plus `Read`/`Edit(~/.lilith/**)` and `Read`/`Edit(//**/.lilith/**)`. The `.worldwright` rules stay.

- [ ] **Step 1: Write the failing tests:**
  - `reviewer_setup_writes_only_the_new_key_path`: the legacy key file is untouched, and no file is created under `.worldwright\`;
  - in `claude_settings_patterns.rs`, every `.worldwright` case repeated for `.lilith` (deny, plus the allowed lookalikes), and `worldwright_rules_are_kept`.
- [ ] **Step 2: Run them.** Run: `cargo nextest run -p lil-cli -p lil-testkit -E 'not binary(no_stale_names)'`. Expected: FAIL.
- [ ] **Step 3: Implement it.**
- [ ] **Step 4: Run them.** Run: the same command. Expected: PASS.
- [ ] **Step 5: Commit.**

```bash
git add crates/lil-cli/src/dev crates/lil-testkit/tests/claude_settings_patterns.rs .claude/settings.json
git commit -m "dev: reviewer-setup writes %USERPROFILE%\\.lilith; .claude denies .lilith as it denies .worldwright"
```

### Task B6: Prose, messages, docs and the spec (protected: `spec_report.rs`, `spec-tests/tests/README.md`)

**Files:**
- `git mv` the spec (map row 27).
- Modify (protected): `crates/lil-cli/src/dev/spec_report.rs` (`SPEC_PATH`) and `crates/lil-spec-tests/tests/README.md`.
- Modify:
  - user-facing strings in `lil-client/src/lib.rs`, `lil-core/src/{rpc.rs, tools/dispatcher.rs, store/write.rs, tools/workspace_tools.rs, tools/doctor_tools.rs}`, `lil-protocol/src/session.rs` and `lil-cli/src/user/*`, with their tests;
  - `README.md`, `NOTICE` ("Copyright (C) 2026 the LILITH contributors."), `docs/reference/workspace-format.md`, `docs/dev/merge-guard.md` (apart from D9 lines) and `docs/research/*`;
  - the spec links only in `docs/spikes/README.md` and the five plans (inventory section 6).

- [ ] **Step 1: Apply the map to the spec.**
  - Title `# LILITH — Design Specification`. Expand the name once, in §0.1.
  - AI-041 gains, after "**Director** mode": "(the director agent, **MAGI Core**)". Nowhere else.
  - Prose naming the plugin becomes "the `worldwright` plugin" (D5).
  - Leave D9's repo and App lines as they are.
- [ ] **Step 2: Apply the map to the other files listed.**
- [ ] **Step 3: Run.** Run: `cargo run -q -p lil-cli --bin lil -- dev spec-report && git grep -c "MAGI Core" -- docs/superpowers/specs && cargo nextest run --workspace`. Expected:
  - 981 rules and no dangling citations;
  - `MAGI Core` once;
  - every test passes, `no_stale_names` included, and `every_allowlist_row_still_allows_something` passes.
- [ ] **Step 4: Commit.**

```bash
git add -A docs README.md NOTICE crates
git commit -m "rename: LILITH in prose, messages, docs and spec (MAGI Core at AI-041)"
```

### Task B7: PR-B close-out

- [ ] **Step 1:** Run the full gate (`<cli>` = `lil-cli`). Every step passes.
- [ ] **Step 2:** Repeat Task A1 Step 1's probe with `-p lil-cli --bin lil`. Expected: exit 0.
- [ ] **Step 3:** Push `rename` once and open the PR. The body lists the 63 protected files and the "renamed from" entries the guard reports.
- [ ] **Step 4:** Watch CI. The judges must have picked `ww-cli` from main (Review Focus 4).
- [ ] **Step 5:** After the owner approves (checklist step 4), merge with `lil dev merge <PR>`. The first `post-merge-check` builds `ww-cli` from `before`, and it must pass. Any fix-up is a new commit, which needs a fresh approval.

---

## PR-C "merge-guard names"

### Task C1: Repo and App names (protected: `dev/merge-guard.toml`, the `dev/` files below)

**Files:**
- Modify (protected): `dev/merge-guard.toml` and `crates/lil-cli/src/dev/{merge.rs, post_merge.rs, reviewer.rs, github.rs, github_fake.rs, mod.rs}`.
- Modify: `docs/dev/merge-guard.md`, the spec (ARCH-109, OPS-003, QA-020), `crates/lil-testkit/tests/claude_settings_patterns.rs` (the fake clone folder name) and `no_stale_names.rs` (delete the D9 row).

- [ ] **Step 1: Read the owner's slug result** (checklist step 9):
  - **The slug changes:** `repo = "LILITH"` and `reviewer_app_slug = "lilith-reviewer"`.
  - **The slug doesn't change:** `repo = "LILITH"` only. The D9 row then keeps only `worldwright-reviewer`.
- [ ] **Step 2: Delete the D9 allowlist row.** Run: `cargo nextest run -p lil-testkit -E 'binary(no_stale_names)'`. Expected: FAIL, listing the repo and App literals.
- [ ] **Step 3: Replace every literal it lists**, by map rows 29 and 30, in tests (including the `include_str!` replacements in `merge.rs` and `hooks.rs`), docs and the spec. The test URLs keep their shapes (`.git`, the trailing slash, case, `-fork`, `//`, `evil.example`).
- [ ] **Step 4: Run.** Run: `cargo nextest run -p lil-cli -p lil-testkit && cargo run -q -p lil-cli --bin lil -- dev spec-report`. Expected: PASS; 981 rules.
- [ ] **Step 5: Commit.**

```bash
git add dev/merge-guard.toml crates docs
git commit -m "merge guard: repo LILITH and reviewer App lilith-reviewer"
```

### Task C2: PR-C close-out

- [ ] **Step 1:** Run the full gate. Every step passes.
- [ ] **Step 2:** Push and open the PR. It is judged with main's old names, which still match GitHub.
- [ ] **Step 3:** After the owner approves (checklist step 10), merge with `lil dev merge <PR>`, then tell the owner to run checklist steps 11–13 **at once**, with no merge in between (B4).

---

## Owner checklist (R6)

Run the steps in this order. Commands are for PowerShell in a clone. `<owner>` is the GitHub account in `dev/merge-guard.toml`.

1. **Before PR-A is pushed:** if Task A1's probe reported a finding, edit `%USERPROFILE%\.worldwright\privacy-denylist.txt` so that no entry matches the new names. Then create the new Actions secret:
   `Get-Content -Raw "$env:USERPROFILE\.worldwright\privacy-denylist.txt" | gh secret set LIL_PRIVACY_DENYLIST --repo <owner>/Worldwright-Suite`
2. **Approve PR-A** on its head: `cargo run -q -p ww-cli --bin ww -- dev approve <PR-A>`.
3. **After PR-A merges**, in every clone: `git switch main; git pull; cargo run -q -p ww-cli --bin ww -- dev install-hooks`. This writes the script helper. Then re-store the agent token under its new name (D2), at a terminal:
   `cargo run -q -p ww-cli --bin ww -- dev agent-token clear`
   `cargo run -q -p ww-cli --bin ww -- dev agent-token set`
   `cargo run -q -p ww-cli --bin ww -- dev agent-token status` (it must name `LILITH-dev/github/agent-token`).
4. **Approve PR-B** on its single head, from a `main` checkout: `cargo run -q -p ww-cli --bin ww -- dev approve <PR-B>`.
5. **After PR-B merges**, in every clone: `git pull`. The helper and hooks need no reinstall.
6. **Move the owner files** now that the `.lilith` deny rules are on main:
   `New-Item -ItemType Directory -Force "$env:USERPROFILE\.lilith\reviewer"`
   `Move-Item "$env:USERPROFILE\.worldwright\reviewer\app-key.age" "$env:USERPROFILE\.lilith\reviewer\"`
   `Move-Item "$env:USERPROFILE\.worldwright\privacy-denylist.txt" "$env:USERPROFILE\.lilith\"`
   Leave everything else in `%USERPROFILE%\.worldwright\` (the plugin's tools) where it is.
7. **Check that the moves worked:** `cargo run -q -p lil-cli --bin lil -- dev privacy-scan --tree HEAD` exits 0, and the next `dev approve` prints the `.lilith` key path.
8. **Leave the plugin-era workspace as it is.** The core reads `worldwright.yaml` and `.worldwright\` (R4), and the `worldwright` plugin keeps working (R1). Nothing exists to migrate under `%APPDATA%`, `%LOCALAPPDATA%`, the registry, AUMIDs or URL protocols (inventory section 5).
9. **Before PR-C: settle D6 with a throwaway App.** Create a GitHub App named `lil-slug-probe-<random>` with no permissions, and don't install it. Rename it to `lil-slug-probe-<random>-b`, then open `https://api.github.com/apps/lil-slug-probe-<random>-b`.
   - 200 means a rename changes the slug.
   - 404 means it doesn't.

   Tell the agent which, then delete the throwaway App.
10. **Approve PR-C:** `cargo run -q -p lil-cli --bin lil -- dev approve <PR-C>`.
11. **Immediately after PR-C merges**, with no merge in between:
    - rename the repo: `gh repo rename LILITH --repo <owner>/Worldwright-Suite`, and don't create a repo under the old name, or the redirects stop;
    - rename the App `worldwright-reviewer` to `lilith-reviewer` (Developer settings → GitHub Apps → Edit). If step 9 said the slug changes, check that `https://github.com/apps/lilith-reviewer` resolves.
12. **In every clone:** `git remote set-url origin https://github.com/<owner>/LILITH.git; cargo run -q -p lil-cli --bin lil -- dev install-hooks; git fetch`. The fetch must authenticate. Fine-grained token repo selection is by ID, so `lil dev agent-token status` should still pass.
13. **Optional:**
    - update the repo description and topics, the App's description and homepage URL, and the token's note;
    - rename the private `Worldwright-Workspace` repo if it already exists;
    - plan the plugin rename (R1, not now).
14. **At PR-D:** `gh secret delete WW_PRIVACY_DENYLIST --repo <owner>/LILITH`.

## Later: PR-D cleanup

At the start of plan 3, or later if the owner prefers. It needs the first green `post-merge-check` on the renamed main, and owner checklist steps 3, 6 and 12 done.
- **Remove the legacy readers and their tests:**
  - every `LEGACY_*` in `lil_platform::names` and `refs.rs` and its readers: workspace file and folder, `state.json`, the second `core.lock`, copy-forward, `suite_toml_to_read`, the markers and `ww://`;
  - the legacy token target, key path, denylist file and `WW_PRIVACY_DENYLIST` reads;
  - legacy get and delete in `check_target`.
- **Keep** the test guard's legacy protection and the `.worldwright` deny rules while any legacy folder exists on the machine.
- **CI and helper:** remove the pick step's `ww-cli` branch and the second denylist variable from the workflows and `workflows.rs`. Remove the helper script's `ww-cli` branch.
- **Allowlist:** delete the rows marked "PR-D", and narrow the `LEGACY_` row to the guard. `every_allowlist_row_still_allows_something` shows what is left.
- **Owner:** checklist step 14.
- **Gate:** the full gate. Protected files: the four workflows, `workflows.rs`, `agent_access.rs`, `reviewer.rs`, `privacy.rs` and `tools/git-hooks/credential-helper`.
