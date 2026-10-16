# Plan 2b — Core Platform I-b: Workspace Data Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Give the core its workspace data layer:
- the core is the only suite writer of workspace files; every write is hash-checked, atomic and crash-safe, in a fixed write order;
- outside writers (you, Claude Code, git, LFS) are detected and reconciled, and never overwritten;
- assets have candidate runs, sealed read-only versions, content-hashed `ww://` references, a status machine and soft deletes to trash;
- projects, `progress.md` and the settings cascade work through core tools;
- a disposable `index.db` and a `ReadDirectoryChangesW` watcher keep the core's view current.

`ww workspace init|use`, `ww notify` and `ww doctor` drive it from the command line.

**Architecture:**
- **`ww-schema`** gains the workspace format as pure code:
  - manifests as ordered JSON maps with typed views (lossless round-trip);
  - `ww://` refs that carry content hashes, the status machine and version numbers;
  - targeted YAML and TOML edits, path rules, UTC date formatting and append-log lines.
- **`ww-platform`** gains `fs`: `ReplaceFileW`/`MoveFileExW`, file identity (FileId, ChangeTime, attributes), volume facts, `LockFileEx`, the read-only attribute, long paths and `ReadDirectoryChangesW`. It also gains `paths::folder_key`, the machine id and hidden child processes.
- **`ww-core` modules (CORE-014).**
  - `store`: writes, write order, logs, staging, versions, trash and `suite.toml`; it is still the only file-write home, in `store/fsops.rs`.
  - `index`, `watch`, `graph` (the parts 2b needs), `pipeline` (status, sign-off, Record), `projects` and `settings`.
  - The infrastructure module `workspace` gains `WorkspaceHub`, which opens a workspace: `core.lock`, `state.db`, store, index and watcher.
- **Dispatcher.** Resolve now picks the project; `Settings` becomes the cascade; Record becomes asynchronous and applies a tool's `RecordPlan` in write order.
- **Clients.** `ww-client` gains a `doctor` module for the suite `state.db`. `ww-cli` gains `notify`, `workspace` and `doctor` under `src/user/**`.

**Tech Stack:**
- Rust 1.99.0 (MSVC), edition 2024, `cargo-nextest`, `cargo-deny`, as in plan 2a.
- New workspace dependencies, all in Task 2: `proptest` 1.12.0 (shrinkable property tests, QA-046) and `toml_edit` 0.22.27 (comment-preserving `suite.toml` edits). `serde_json` gains `preserve_order` (ARCH-076). The same root edit optimises `libsqlite3-sys` in dev builds, as `sha2` already is, so the index benchmarks measure SQLite rather than debug code.
- `yaml-rust2` (already a workspace dependency) validates targeted YAML edits.
- New `windows` features in `ww-platform` only: `Win32_System_IO` and `Win32_Storage_CloudFilters`.

**Spec:** `docs/superpowers/specs/2026-10-12-worldwright-suite-design.md`. Task 1 adds its Appendix E, "Workspace data (plan 2b clarification)". The plan follows roadmap row 2b (`docs/superpowers/plans/2026-10-12-roadmap.md`), which cites §1.5–1.6, §2.1 and §2.3–2.4.

The rules this plan implements, fully or in the part named:
- §1.1: ARCH-002 (store), ARCH-016 (`ww notify`, `ww doctor` basics);
- §1.2: ARCH-028, ARCH-033 (staging on the workspace volume), ARCH-042, ARCH-044;
- §1.3: ARCH-058;
- §1.5: ARCH-073–083, ARCH-085, ARCH-088, ARCH-089, ARCH-090 (writing `suite.toml`);
- §1.6: ARCH-094 (no workspace in the install folder), ARCH-096;
- §1.7: ARCH-098 (the `sketchbook` default project; the app side is plan 4);
- §2.0: CORE-003 (staging handles; document save handles are plan 5), CORE-005 (`index.db` beside the workspace `state.db`), CORE-006, CORE-007 (writes), CORE-009, CORE-010 (the test);
- §2.1: CORE-014 (the new modules and their dependency direction);
- §2.2: CORE-019, CORE-020 (the candidate-run and sealed-version branches);
- §2.3: CORE-023, CORE-025 (ingest, record and events; repairs and QA are hooks), CORE-026–029, CORE-031–035, CORE-037–042, CORE-047;
- §2.4: CORE-048, CORE-049 (the flag and the setting), CORE-051, CORE-052 (the pick rule; the trigger after QA is later), CORE-053–055, CORE-057, CORE-058 (filtering by `modules`);
- §2.7, §2.12: CORE-110, CORE-148;
- §6: QA-011, QA-044 (manifest round-trip properties), QA-046, QA-048 (the manifest golden, synthetic per owner decision 6), QA-055 (the fast subset), QA-060 (`ww doctor` is idempotent), QA-066 (fixture workspaces, fail points, handle holder, git-ops simulator, watcher fence).

M0 results: `docs/spikes/README.md`. No spike covers this plan's area.

## Decisions recorded for this plan

The owner decisions of plan 2a (2026-10-15) bind this plan:

1. **Workspace creation (owner decision).** `ww workspace init <path>` and `ww workspace use <path>` are thin wrappers over the core tools `workspace.create` and `workspace.set_default`.
   - The core stays the only writer of `suite.toml`.
   - The Home panel (plan 4) later calls the same tools (ARCH-003).
2. **A second workspace is refused (owner decision).** The core serves only the default workspace (ARCH-090). A session naming another registered workspace gets `workspace.unsupported`; its message names the default workspace and says how to change it. This plan adds the agent hint (`workspace.set_default`).
3. **The protocol is written into the spec (controller ruling, QA-017).** Plan 2b follows it: Task 1 adds Appendix E, copied from this plan's Global Constraints. It adds no tagged rule lines, so the ID check stays `925 925 []`.
4. **Drain direction (controller ruling).** An automatic drain happens only when the client's major version is greater than the core's. This plan doesn't change it.
5. **A damaged or newer suite `state.db` stops the core from starting (owner decision).** The core exits with a distinct code, and `ww` explains it in plain words; nothing is changed on disk.
   - `ww doctor` explains it and, **only after you confirm**, moves the file aside to `state.db.damaged-<utc>` (with its `-wal`/`-shm`), next to it, never deleting it.
   - The core then starts with a fresh database.
6. **No real lantern manifest (owner decision).** The golden round-trip tests use a synthetic manifest with the same structure as the lantern manifest. Nothing comes from a real workspace.

Decisions made in this plan (each stands unless the owner says otherwise; owner questions 2 and 6–10 name the ones that touch them):
- **Targeted YAML edits are line edits**, checked by re-parsing with `yaml-rust2`. No YAML crate on crates.io keeps comments when it writes, and `yaml-rust2` is maintained, MIT/Apache and already a workspace dependency. An edit the line editor can't make safely (anchors, flow mappings at the edited key, several documents) fails with `yaml.unsupported_edit` and changes nothing.
- **`suite.toml` edits use `toml_edit`**, which keeps comments and unknown keys. It is maintained, MIT/Apache and already in `Cargo.lock` through `toml`.
- **Manifests are ordered maps.** A typed struct can't keep the original interleaving of known and unknown keys. `Manifest` is therefore the ordered `serde_json` map (with `preserve_order`), with typed views for reading and setters that edit in place: existing keys keep their position, and new keys are appended. ARCH-076 names "typed structs that keep unknown fields in a flattened extras map", so Task 1 records this as an ARCH-076 clarification: the typed views are the structs, and the map is what keeps unknown fields in place.
- **A live topic is per asset, not per version.** ARCH-044 says live links subscribe to `asset:<ref>`. The topic is `asset:` plus the ref without its `@<version>` and hash, so one subscription sees every new sealed version ("update available", APP-076). Task 1 records this as an ARCH-044 clarification.
- **`ReplaceFileW` has no write-through option.** Its documentation says `REPLACEFILE_WRITE_THROUGH` "is not supported", that the replaced, replacement and backup files must be on the same volume, and that the result has the replacement file's file ID. The store therefore flushes the temp file before the swap, keeps temp and backup files in the target's folder, uses write-through only on `MoveFileExW`, and takes each file's identity after the write. ARCH-074's "(`ReplaceFileW` / `MoveFileExW` with write-through)" is clarified in Task 1.
- **A new target is moved in without replace-existing, and so is everything else.** `MoveFileExW(MOVEFILE_WRITE_THROUGH)` fails if an outside writer created the file after the hash check, and the store reports `store.hash_conflict` instead of overwriting it (ARCH-074 step 1). The same holds on the fallback paths: the second rename of the two-rename fallback, the move after error 1177 and every put-back use `move_new`, because the name they move onto was just freed and an outside writer may have created a file there in the gap. No write path passes `MOVEFILE_REPLACE_EXISTING`, so `ww-platform` has no `move_replace`.
- **A replace is recorded before it happens.** Before the swap, the store writes a pending row (old and new SHA-256) to the workspace `state.db`. A crash or power cut can then leave either version on disk and both are recognised as the core's own, so neither is reported as a human edit, and recovery can tell an outside edit in a backup file from the core's old bytes (ARCH-074 clarification).
- **FileId and ChangeTime are trusted only where they are stable.** That means local NTFS or ReFS outside a cloud-files sync root. FAT file IDs can change over time, network IDs come from the server, and a OneDrive folder is NTFS whose sync client renumbers files and changes attributes (which moves ChangeTime). Elsewhere the index compares size and mtime, and it hashes any file whose mtime is within 2 s of the previous scan, because FAT write times have 2-second resolution. Task 1 records this as a CORE-041 clarification.
  - Even where they are stable, file IDs only detect changes in place; they never identify a file across folders. ReFS (Dev Drive) builds its 128-bit ID from the parent folder and the file within it (MS-FSCC), so a move to another folder changes it. The store reports each file's identity after every write and move, and the index takes that identity as is.
  - A volume whose facts can't be read (FAT or exFAT USB sticks, SMB shares, file systems without the cloud filter) is treated as having no stable IDs. It never fails `workspace.create` or an open; `workspace.create` returns a warning instead.
- **Hashes are taken only from stable reads.** A file's identity (size, last write, ChangeTime) is taken before and after it is hashed, and the digest counts only when both match. A file that is still being written, or is held without read sharing (errors 32 and 33, common while Photoshop, Krita or Blender save), is parked and re-checked later. It is never reported as removed or as a human edit (ARCH-079 clarification).
- **Long paths come from the `\\?\` prefix, not from `LongPathsEnabled`.** The opt-in needs a machine-wide registry value as well as a `longPathAware` manifest entry, and the suite changes no system setting. `ww-platform` prefixes absolute paths of 247 UTF-16 code units or more, as Rust's `std::fs` does: std's `LEGACY_MAX_PATH` is 248 including the terminating NUL (the `CreateDirectoryW` limit, which counts the NUL), so a path of 247 or more code units gets the prefix.
- **Workspace asset trees get no line-ending conversion.** With `core.autocrlf=true` (Git for Windows' default), a clone or checkout would give text assets CRLF endings, so their SHA-256 would stop matching the manifest on every other machine. `workspace.create` and `ww doctor` therefore write `projects/** -text` to the workspace `.gitattributes` (plan 7 later adds its LFS patterns there, ARCH-086), and `ww doctor` reports an inherited `core.autocrlf=true` as a `problem`. It sets `core.autocrlf false` in the repository's own config only after you answer `y` to a prompt: `--non-interactive` never answers yes, and the agent-callable `workspace.doctor` tool never changes it, because it overrides your own git setting and `projects/** -text` already protects the asset trees. Manifest, YAML and `progress.md` edits keep the line ending the file already has (CORE-040 clarification).
- **The settings request layer comes from tool arguments in 2b.** `ToolCallParams` has no `settings` field, and product tools reject unknown fields, so the per-request layer of CORE-057 is the `settings` argument of a tool that declares one. In 2b only `test.settings` does; product tools pass none. A per-call `settings` field in `tools/call` would be a protocol change that alters `schema_hash`, so it waits for plan 3 and its job tools (Appendix E records this; Not in this plan).
- **The asset folder name is authoritative.** The index keys an asset by its folder (`projects/<project>/assets/<asset_id>/`) and stores the manifest's `creation_uuid`. A manifest whose `asset_id` or `project` disagrees with its folder (for example a copy made in Explorer) is `needs_repair`, and is never rewritten automatically (CORE-047, ARCH-079 clarification).
- **Organising metadata lives in workspace files.** Tags, ratings, colour labels and collections, when a later plan adds them, are stored in manifests or tracked YAML, never only in `index.db`, which is disposable (decision recorded in Appendix E; no fields in 2b).
- **`creation_uuid` is not added to manifest `inputs` (research 5A-4, rejected).** A future rename can fix dependents by path at rename time through `index.dependents`, and refs that leave the workspace already carry the content hash (CORE-047). Adding it now would amend a contract for a feature that isn't on the roadmap.
- **Dates the core writes are UTC.** That covers trash folders, `progress.md` entries, log months, run ids and set-aside names. Per-machine monthly log names (ARCH-081) then don't depend on time zones. Plan 3 adds local-calendar time for caps (CORE-012).
- **Fail points exist only in non-stable builds and are armed only on test channels** (`WW_TEST_FAILPOINTS`), like the test tools. A stable build refuses test channels, so it can never arm one. Task 1 records this reading of QA-066's "compiled only into test builds".
- **`workspace.create` doesn't run `git init`.** Sync (plan 7+) owns repository setup. `ww doctor` skips its git checks, with a note, when the folder isn't a repository.

## Questions for the owner

Each question has the answer this plan uses unless you say otherwise.

1. **Should `ww workspace init` also run `git init` (and `git lfs install --local`)?** Default: **no**. Sync (plan 7+) owns repository setup, and `ww doctor` skips its git checks with a note when the folder isn't a repository.
2. **The index budgets (CORE-042, APP-138):** keep the cold rebuild of 2,000 assets at 30 s, and add a warm reconcile of 10,000 assets (nothing changed, stats only), measured in Task 10 and committed at about twice the local measurement, then checked against the CI runner's figure in the PR's CI log and adjusted by you, if needed, when you approve the PR? Default: **yes**. Both go in `test-params.toml` `[budgets]`; the benchmarks run in a debug test build with `libsqlite3-sys` optimised, and the report quotes the measured times and the machine class.
3. **Approve the six protected-file changes at PR time?** Default: **yes**. They are the root `Cargo.toml` (Task 2); the new `crates/ww-spec-tests/fixtures/.gitattributes` (Task 3; without it the fixtures are checked out with CRLF and the golden and hash tests fail); the two plan-2a spec-test tightenings in `core_session.rs` and `cli_basics.rs` (Task 5); `test-params.toml` (Task 10); and one match arm in `crates/ww-cli/src/main.rs` (Task 15). Optionally, drop the two plan-2a tightenings and the `main.rs` change (hardening, not 2b scope) to shrink the set to three files.
4. **Write `signed_off.by` only, with `cli` added to the format?** CORE-051 says a sign-off "is recorded with `approved_by`"; the workspace format records it as the run's `signed_off: {by, date, version}`. Default: **yes**: `signed_off.by` only, with the format's values `user`, `assistant` and `claude-code`, plus `cli` for `ww`, which Task 1 adds to `docs/reference/workspace-format.md`. Task 1 records `approved_by` as that field in an Appendix E clarification, so no manifest gains a second field.
5. **`toml_edit` 0.22.27 now, with `toml` 1.x and `toml_edit` 0.25 moved together in a separate change?** Default: **yes**. 0.22.27 is the version `toml` 0.8.23 already locks, so `suite.toml` is read and edited by one TOML parser and `Cargo.lock` gains no second copy.
6. **Reserve organising-metadata fields (tags, ratings, colour labels, collections)?** Default: **no fields in 2b**, but Appendix E records that such metadata will live in manifests or tracked YAML, never only in the index. Unknown fields already survive every edit (CORE-148), and `assets.query` takes new filters as new optional keys, so plan 4 can add them without breaking anything 2b writes.
7. **A `missing` file state (research 5A-2)?** Default: **yes, in 2b**, as an ARCH-079/CORE-038 clarification with no new rule ID: a file the suite still expects on disk (listed in `hashes`, a file of an open run, or a manifest) that disappears outside the suite is listed as `missing`, never forgotten, and recovers when the same content reappears. The suite's own moves (sign-off, trash, maintenance) never make a file `missing`.
8. **Line endings in workspaces:** add `projects/** -text` to the workspace `.gitattributes`, and have `ww doctor` flag `core.autocrlf=true` as a `problem`, setting it `false` in the repository's config only after you confirm at a y/N prompt (never with `--non-interactive`, never from an agent's `workspace.doctor` call)? Default: **yes**.
9. **Keep all of `candidates/` gitignored, so open runs don't sync between machines?** CORE-032 gitignores only rejected candidates. Default: **yes**, recorded as a CORE-032 clarification.
10. **Asset-folder copies and moves:** in 2b, only flag a folder/`asset_id` mismatch as `needs_repair`, and leave move pairing and duplicate-`creation_uuid` repair to the plan that adds `asset.rename`? Default: **yes**.

## Global Constraints

- **Toolchain:** Rust **1.99.0**, `x86_64-pc-windows-msvc`, edition **2024**.
- **Branch and merge:**
  - Work on `plan-2b-core-platform`, cut from `main` after this plan's docs PR merges.
  - Commit after every task; each message ends with `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`.
  - The branch lands as one PR through `ww dev merge`.
  - **Protected files** (QA-019/020, checked against `dev/protected-paths.txt`). Six are changed, so the owner must run `ww dev approve <PR>` on the final head:
    - `Cargo.toml` (the root `/Cargo.toml` pattern), Task 2;
    - `crates/ww-spec-tests/fixtures/.gitattributes`, a new file, Task 3. The list's `.gitattributes` pattern has no `/`, so the guard matches it at any depth, and it isn't `add-ok`; the stricter pattern wins over `add-ok:crates/ww-spec-tests/**`, so even this addition needs approval. It is still the narrowest fix: the alternative is editing the protected root `.gitattributes`;
    - `crates/ww-spec-tests/tests/core_session.rs` and `crates/ww-spec-tests/tests/cli_basics.rs` (existing files under `add-ok:crates/ww-spec-tests/**`, so a change needs approval), Task 5;
    - `test-params.toml`, Task 10;
    - `crates/ww-cli/src/main.rs`, Task 15.

    Other new files under `crates/ww-spec-tests/**` (tests and fixtures) and `**/proptest-regressions/**` are free additions (`add-ok`). Edits to `crates/ww-spec-tests/expected-fail.toml` follow the guard's own expected-fail rules. This plan changes no other protected file: not `crates/ww-cli/Cargo.toml` (`ww` reaches `ww-schema` only through `ww-client`), not `crates/ww-spec-tests/Cargo.toml`, not `.config/nextest.toml`, not `crates/ww-cli/src/dev/**` and no workflow file.
- **Crates** (ARCH-110): no new crate.
  - New `ww-core` modules: `index`, `watch`, `graph`, `pipeline`, `projects`, `settings`.
  - `store` grows from `fsops` into the module CORE-014 describes.
- **Dependency boundaries** (ARCH-111, QA-027):
  - `ww-schema` may add only `serde_json`, `yaml-rust2`, `toml_edit` and `sha2`; none of them pulls in a Windows crate. `crates/ww-testkit/tests/dependency_boundaries.rs` stays green.
  - Only `ww-platform` calls Windows APIs.
  - `ww-core` reaches every `ww_platform::fs` function that mutates files only from `store/fsops.rs`. Those functions are added to `crates/ww-core/clippy.toml`'s `disallowed-methods` (Task 6).
- **New dependencies** (OPS-108; `cargo deny check` must pass). All of them land in Task 2's one root `Cargo.toml` edit, pinned to these versions (checked on crates.io on 2026-10-16, as plan 2a pinned its own; `Cargo.lock` records them):

  | Crate | Version | Licence | Maintenance | Reason |
  |---|---|---|---|---|
  | `proptest` | `1.12.0`, `default-features = false, features = ["std"]` | MIT OR Apache-2.0 | proptest-rs project; the current release | the shrinkable simulated-filesystem reconciler variant (QA-046) and manifest round-trip properties (QA-044). Without the default `fork` and `timeout` features, `rusty-fork` and `wait-timeout` stay out of the tree. The `bit-set` feature is off too, so strategy authors don't have `prop::sample::subsequence` or `prop::bits`; strategies build operation sequences with `prop::collection::vec` |
  | `toml_edit` | `0.22.27` | MIT OR Apache-2.0 | toml-rs project; the version `toml` 0.8.23 already brings into `Cargo.lock` (owner question 5) | `suite.toml` edits that keep comments and unknown keys (CORE-007; the core is its only writer) |
  | `serde_json` feature `preserve_order` | `1` (lock: 1.0.154) | MIT OR Apache-2.0 | already a workspace dependency | manifests keep key order (ARCH-076). Set on the workspace dependency, so every crate builds the same `serde_json`. It brings in `indexmap` (MIT OR Apache-2.0), already in `Cargo.lock` at 2.14.2 |

  All three licences are on `deny.toml`'s allow list and compatible with GPL-3.0-or-later.

  The same root edit adds `[profile.dev.package.libsqlite3-sys] opt-level = 3`, next to the existing `sha2` entry, so the index benchmarks (Task 10) aren't dominated by unoptimised SQLite.

  **Existing workspace dependencies that crates newly use** (no root change): `ww-schema` adds `serde_json`, `yaml-rust2`, `toml_edit` and `sha2`, with `proptest` as a dev-dependency (Task 3); `ww-client` adds `rusqlite` (Task 14); `ww-core` adds `sha2` (Task 6: the store and the index hash file bytes) and `proptest` as a dev-dependency (Task 14).

  **New `windows` features in `ww-platform`:** `Win32_System_IO` (`OVERLAPPED`, `CancelIoEx`, `GetOverlappedResult`) and `Win32_Storage_CloudFilters` (`CfGetSyncRootInfoByPath`). `Win32_Storage_FileSystem` is already enabled.
- **File writes in `ww-core`** (ARCH-002, ARCH-112, QA-024): every file-mutating call stays in `crates/ww-core/src/store/fsops.rs`, one `#[allow(clippy::disallowed_methods)]` per function. `crates/ww-testkit/tests/single_write_allowance.rs` keeps enforcing it.
- **Tests** (QA-031): as in plan 2a, plus:
  - Retry backoff, the 60 s git wait, watcher batching and the 30- and 90-day purges are tested with `#[tokio::test(start_paused = true)]` or `FakeClock`, never with real sleeps.
  - E2E tests wait on the watcher with the **watcher fence** (`RawPipe::fence`), never with a sleep.
  - The only wall-clock measurements are the two CORE-042 index benchmarks (Task 10). They are performance gates with their budgets in `test-params.toml`, not correctness assertions.
  - Tests that need git use `TestEnv::git_init` (real `git`, isolated config, no remote, no network). The isolation is `TestEnv::git_env()` (`GIT_CONFIG_NOSYSTEM=1`, `HOME` and `XDG_CONFIG_HOME` set to `<root>\profile`, `GIT_CONFIG_GLOBAL` set to `<root>\profile\.gitconfig`). `child_env()` carries it too, so a spawned `ww` or core, and every `git` they run, never reads the system or user git config (QA-038): a developer machine or CI runner whose system git config sets `core.autocrlf=true` sees the same results as any other. In-process cores get it through `CoreConfig.git_env` (Task 14), which is empty in production.
  - Unit and integration tests in `ww-core` create fixture files with `ww_testkit::files::write_file` (Task 4), never with `std::fs` in `crates/ww-core/src`, so `single_write_allowance.rs` keeps passing (QA-024). Other outside-writer steps (a move, a delete, clearing the read-only bit) happen only in `crates/ww-core/tests/`, through `ww_testkit` or under a per-function `#[allow(clippy::disallowed_methods)]` there.
  - Spec tests get today's UTC date, month and set-aside suffix, and parse RFC 3339, through `ww_testkit::utc` (Task 4), which is written independently of `ww-schema::time` (QA-018).
- **Isolation** (QA-038, QA-040, QA-041): as in plan 2a. Every workspace a test touches is under its `TestEnv` root. `workspace.create` and `WorkspaceHub::open` pass each path through `check_workspace`, so a test core never opens a workspace registered in a real `suite.toml`. No test touches real OneDrive: cloud placeholders are faked through file attributes (Task 10).
- **Spec values used verbatim** (from the rule text):
  - path budget "about 180 characters per relative path" (CORE-040), fixed here at **180**;
  - trash "purged after 30 days" (ARCH-058, CORE-037);
  - rejected candidates "purged 90 days after sign-off unless pinned" (CORE-032);
  - git: "queues its writes for up to 60 s" (ARCH-083);
  - status values `planned → in_progress → candidate → in_review → approved → exported → released`, side states `stale`, `blocked` (a reason is required) and `deprecated`, and `status_before_stale` (CORE-048);
  - logs `queue/jobs.<machine>.<yyyy-mm>.jsonl` and `registry/usage.<machine>.<yyyy-mm>.jsonl`, plus `%LOCALAPPDATA%\Worldwright\usage-loose.jsonl` (ARCH-081);
  - `progress.md` entries "under `## Done` as `- YYYY-MM-DD: …`" (CORE-039);
  - "SHA-256 is the only content hash" (ARCH-078);
  - previews in events "≤ 1 MB" (ARCH-033).
- **Names that are contracts.** Task 1 copies the sub-bullets of this bullet into spec Appendix E, in the order Task 1 Step 3 lists them. The error-code table and exit code 7 go into Appendix D instead (Task 1 Step 2). The two bullets after this one (the test-channel tools table and Privacy) are not copied into the spec.
  - **Workspace-relative paths** use `/` and are checked before anything is written (CORE-040):
    - at most 180 characters;
    - no component may be a reserved Windows name (`CON`, `PRN`, `AUX`, `NUL`, `COM1`–`COM9`, `LPT1`–`LPT9`, and `COM¹`–`COM³`/`LPT¹`–`LPT³`), whatever its case and extension;
    - no component may end in a dot or a space, contain `<>:"\|?*` or a control character, or be `.` or `..`;
    - every component is at most 255 characters;
    - absolute paths of 247 UTF-16 code units or more go to Windows with the `\\?\` (or `\\?\UNC\`) prefix, after they are made absolute with `\` separators and no `.` or `..` components. The suite doesn't depend on the `LongPathsEnabled` opt-in.
  - **Store write algorithm** (ARCH-074; ARCH-074 clarification):
    1. Check the path.
    2. If writes are paused for git, wait (ARCH-083). Writes are paused while the watcher's git flag is set **or** `<ws>\.git\index.lock` exists. The store checks that file itself (one `GetFileAttributesW`), so a write never races git during the watcher's batch window, or when no watcher runs.
    3. Re-hash the target and compare it with the expected hash (the store reads file data only through `probe.open_read`):
       - for a full write, the file's last generated hash (ARCH-078), or either hash of its pending row (step 5) while one exists. `Expect::Generated` with no generated row is a conflict when the file exists, and is treated as `Expect::New` when it doesn't;
       - for a manifest or YAML edit, the hash read at the start of the edit;
       - LFS pointers compare by `oid`;
       - a cloud-only placeholder target is never read, since reading would download it: the write fails with `store.hash_conflict`, and the message says to make the file available offline first. The same check comes first in every store operation that reads a file's data (`Store::read`, `edit_manifest`, `edit_yaml`, `edit_text`): the attribute-only `probe.identity` runs before any byte is read, so the store never triggers a download. Edits of a placeholder fail with `store.hash_conflict`, and `Store::read` of one fails with `store.io`, with the same message.
    4. Write `<dir>\.<name>.ww-tmp-<pid>-<n>` in the same folder, and flush it with `FlushFileBuffers` before closing it. The first backup name of step 6 uses the same `<pid>-<n>`.
    5. Record the pending replace: a `generated_pending(path, old_sha256, new_sha256)` row in the workspace `state.db`. `old_sha256` is the hash checked in step 3, or null for a new target.
    6. Replace the target. The fail point `store.replace` comes just before this step, and `store.after_replace` just after it.
       - For an existing target, call `ReplaceFileW(REPLACEFILE_IGNORE_MERGE_ERRORS | REPLACEFILE_IGNORE_ACL_ERRORS)` with a backup name `<dir>\.<name>.ww-bak-<pid>-<n>`. `REPLACEFILE_WRITE_THROUGH` is never passed: it is documented as not supported.
       - For a new target, call `MoveFileExW(MOVEFILE_WRITE_THROUGH)` without `MOVEFILE_REPLACE_EXISTING`. Errors 80 and 183 mean an outside writer created the file after step 3: the temp file is removed, the pending row is cleared, and the call fails with `store.hash_conflict`.
    7. Hash the backup. If it equals the hash checked in step 3, the backup is removed. Otherwise an outside edit landed between steps 3 and 6, and the outside version is put back:
       - with `ReplaceFileW` (target = the core's new bytes, replacement = the backup, and a second backup name `<dir>\.<name>.ww-bak-<pid>-<n+1>`, which then holds the core's new bytes and is removed), or, where `ReplaceFileW` is unsupported, with the fallback's two `move_new` renames (target → the second backup name, then the first backup → target);
       - the fail point `store.put_back` comes after the first half of the put-back (the `ReplaceFileW`, or the fallback's first rename) and before the rest, so a crash in the middle of a put-back can be tested;
       - the put-back retries sharing violations under the same 10 s budget. If it still fails, the call ends with `store.hash_conflict` whose `data.backup` names the backup's workspace-relative path, and the pending row stays, so the next open's recovery puts the outside version back;
       - the core's own new content is discarded (the call failed, so nothing of yours is lost), the pending row is cleared, and the call fails with `store.hash_conflict`.
    8. In one `state.db` transaction, record the new SHA-256 as the file's last generated hash and clear the pending row.

    Error handling:
    - Sharing and lock violations (Windows errors 32, 33 and 1224) are retried, starting at 10 ms and doubling to a cap of 500 ms, for up to 10 s in total; after that the call fails with `store.busy`.
    - Errors 1175 and 1176 leave both files under their own names (a backup name was given), so they are retried the same way. Error 1177 leaves the old target under the backup name and the replacement under its temp name: the store moves the replacement onto the freed target name with `move_new`, then runs step 7.
    - Where `ReplaceFileW` is unsupported (errors 1 and 50, for example on some network volumes), the store makes two renames, both with `move_new` (`MoveFileExW(MOVEFILE_WRITE_THROUGH)`, no `MOVEFILE_REPLACE_EXISTING`): target → backup, then temp → target. The fail point `store.between_renames` comes between the two. Step 7 follows. If a crash leaves only the backup, the next workspace open restores it.
    - On the fallback's second rename and on the move after 1177, the target name was just freed, so errors 80 and 183 mean an outside writer created a file there in the gap. That file is kept; the backup is moved aside with `move_new` to `<dir>\<name>.ww-conflict-<YYYYMMDDTHHMMSSZ>`, the temp file is removed, the pending row is cleared, and the call fails with `store.hash_conflict`.
    - Disk full (errors 112 and 39) fails at once with `store.disk_full`; the temp file is removed and the pending row cleared.

    Durability: the temp file is flushed before the swap (step 4). Renames are atomic (journaled) but may be lost on power failure: `MOVEFILE_WRITE_THROUGH` is documented only to flush a move done as a copy and delete, so it adds no documented durability to a same-volume rename, and `ReplaceFileW` has no write-through option at all. A power cut can therefore leave either version. The temp flush makes both complete, and the pending row (step 5) makes either one acceptable as the core's own. `ReplaceFileW` needs the target, temp and backup files on one volume, which is why all three names are in the target's folder. The resulting file has the replacement's file ID, not the old target's, so the index records the identity the store reports after each write.
  - **Write order** (ARCH-075): output files → manifest → jobs log line → `progress.md` entry → index → events. Files already moved into place before the commit (a sealed version's files) reach the index in the index step too, never before the manifest edit. On workspace open, a read-write core runs `recover()`, which deletes no content:
    - **Orphans** are listed and never deleted (CORE-038):
      - `candidates/<run>/` folders missing from their manifest;
      - `v<semver>/` folders that hold a `manifest.snapshot.json` but whose version is neither the manifest's `approved_version` nor a run's `signed_off.version` (a crash between sealing and the manifest edit);
      - non-empty staging handles and `seal-*` folders left by a dead core (at open under `core.lock`, every existing handle belongs to a dead core);
      - `*.ww-conflict-*` files.
    - Stale `*.ww-tmp-*` files of processes that are no longer running, and empty staging handles, are removed.
    - **Pending replaces** are resolved, one row at a time, in this order:
      1. a backup whose hash equals the row's `new_sha256` holds the core's own new bytes (the second backup of a put-back cut short) and is removed;
      2. target missing and a backup left (a crash between the fallback's renames, or between the fallback put-back's renames): the remaining backup, which is the old content or the outside edit and never the core's new bytes, is moved back with `move_new`;
      3. backup and target both present, and the backup's hash equals the row's `old_sha256` (or the generated hash): the backup is removed;
      4. backup and target both present, and the backup's hash is anything else: the backup holds an outside edit. The target (the core's bytes) is moved aside with `move_new` to `<name>.ww-conflict-<YYYYMMDDTHHMMSSZ>` and the backup is moved back with `move_new`, so the outside edit is never left only in a dot-file;
      5. then, if the target's hash equals `new_sha256`, it becomes the generated hash. The row is cleared in every case.
    - A `*.ww-bak-*` file whose target is missing is restored the same way, with or without a pending row.
  - **Append logs** (ARCH-081, CORE-009):
    - one JSON object per line, ending in `\n`, with `"machine"` and `"seq"` (from 1 per file) first, then the record;
    - each line is one write under `LockFileEx` on the byte range starting at offset 2⁶⁴−2 with length 1 of the log file itself, which `ww` can take too. The lock is requested with `LOCKFILE_FAIL_IMMEDIATELY` and retried by the caller, under the store's retry budget (10 ms doubling to 500 ms, 10 s in all, then `store.busy`);
    - while the lock is held, the file is read, a last line without `\n` is closed with `\n`, and the line's `seq` is computed from that text, so two appenders never write the same `(machine, seq)`;
    - readers skip lines that don't parse and dedupe by `(machine, seq)`.

    `<machine>` is the computer name, lowercased, with every character outside `[a-z0-9-]` replaced by `-`. On test channels it is `testmachine`. `<yyyy-mm>` is the UTC month of the line.
  - **Workspace areas** (ARCH-080, CORE-003, ARCH-028, ARCH-058, CORE-037, ARCH-088):
    - staging handles: `<ws>\.worldwright\staging\<sh_16hex>\`; ingest is a rename onto the same volume;
    - `<ws>\.worldwright\core.lock`, held with `LockFileEx` while the core serves the workspace;
    - `<ws>\.worldwright\state.json` `{"active_project":"<slug>"}`, the untracked last-used hint;
    - trash entries: `<ws>\.trash\<YYYY-MM-DD>\<te_16hex>\payload\<original name>`, with `restore.json` beside it: `{"entry_id","original_path","moved_utc","purge_after","reason","sha256"?}`.
  - **`core.lock`** (ARCH-028): the lock gates both read-write access to the workspace's `state.db` and the release of stale `running` idempotency rows. A core that finds it held serves the workspace read-only:
    - read tools work;
    - write tools get `workspace.read_only`;
    - its `state.db` is opened read-only and its rows are never released. When that `state.db` doesn't exist yet (for example on another channel, whose local folder differs), an in-memory database with the migrations applied stands in, and nothing is created on disk;
    - it uses an in-memory index (the same code over SQLite `:memory:`), rebuilt at open, and never opens the shared `index.db`.
  - **Local data** (ARCH-097, CORE-005): `index.db` sits next to the workspace `state.db`, in `Env::workspace_local_dir(ws)`. It uses WAL, and `PRAGMA user_version` is `1`. A different version, or a failed `quick_check`, means the file is deleted and rebuilt. Only the core holding `core.lock` opens it: on a non-system volume both channels share `<volume>\.worldwright-local\<hash>\`, so a read-only core must never write it.
  - **`.gitignore` entries** added by `workspace.create` and `ww doctor` (CORE-040, ARCH-096): `.worldwright/`, `.trash/`, `inbox/*`, `projects/*/assets/*/candidates/`, `projects/*/assets/*/work/` (the root-relative form of CORE-040's `assets/*/work/`). All of `candidates/` is ignored, not only rejected runs (CORE-032 clarification), so open runs don't sync between machines.
  - **`.gitattributes` entry** added by `workspace.create` and `ww doctor` (CORE-040 clarification): `projects/** -text`, so git never converts line endings inside asset trees and recorded SHA-256 values hold on every clone. An existing file is appended to, never rewritten; plan 7 adds its LFS patterns to the same file (ARCH-086).
  - **Refs** (ARCH-042, CORE-031, CORE-047):
    - a sealed version: `ww://<workspace id>/<project>/<asset_id>@<major.minor.patch>`, optionally followed by `#sha256=<64 hex>`;
    - a candidate: `ww://<workspace id>/<project>/<asset_id>@candidate/<run_id>/<file>`;
    - manifest link forms (I6): `<asset_id>@<v>`, `proj:<slug>/<asset_id>@<v>`, `lib:<item>@<v>` and `style@<n>`;
    - every new reference to a sealed version carries the hash, and manifest `inputs` entries gain `"sha256"`.

    Name rules: a project slug matches `^[a-z0-9][a-z0-9-]{0,62}$`, an asset id `^[a-z][a-z0-9_]{0,62}$` and a run id `^r_\d{8}_\d{4}(_\d+)?$` (UTC). The `_<n>` suffix is the smallest one that is free among both the manifest's runs and the existing `candidates/*` folders, so a retry never collides with an orphan's folder. A version's **content hash** is the SHA-256 of a text with one line per file in `v<version>/`, except `source/**` and `manifest.snapshot.json`. The lines are sorted by `/`-separated relative path in byte order, and each reads `<sha256 hex>  <relative path>\n`.
  - **Manifest fields this plan writes** (`schema_version` 1 on new manifests):
    - `creation_uuid` (UUID v4; CORE-047);
    - `status_before_stale` and `status_before_blocked`;
    - `has_working_doc`;
    - `external: [{"path","sha256"}]` (link-external files);
    - `hashes` (I5): keys are paths relative to the asset folder (`v1.0.0/x.png`), values are written `sha256:<64 hex>` as in I5. Every reader (the index, the watcher, the test kit) accepts a value with or without the `sha256:` prefix and compares the bare hex;
    - `candidate_runs[]` in the I5 shape (`run_id`, `path`, `status`, `created`, `target_version`, `files`, …), plus `candidate_runs[].pinned`, `candidate_runs[].files[].sha256` (bare hex of the file as it was committed) and `candidate_runs[].trashed_utc` (RFC 3339, set when maintenance moves a rejected run's folder to trash; I5 clarification below);
    - `human_edits[]` entries `{"path","sha256","previous_sha256","date"}`: I5's `path`, `sha256` and `date` (UTC `YYYY-MM-DD` of the detection), plus `previous_sha256`. I5's `author` and `note` are left out, since the core doesn't know them;
    - `candidate_runs[].signed_off` `{"by","date","version"}`, the workspace format's field that CORE-051 calls `approved_by`; `by` is one of `user` (`ui:*`), `assistant`, `claude-code` (`mcp`) and `cli`;
    - `version` (I5): when new work starts on an asset that has an `approved_version`, the first candidate run after it sets `version` to `approved_version` bumped by `minor`, and the run's `target_version` to the same value ("set it by the bump rules"); a sign-off with a confirmed `bump` replaces it.

    Unknown fields are kept in place (CORE-148). Edits never reorder existing keys: a removed key (`external` after `asset.collect`, `status_before_stale` after a restore) is removed by shifting the keys after it, never by moving the last key into its slot. Edits never add `schema_version` to a manifest that lacks it (a legacy manifest stays a minimal diff), and a file keeps the line ending (`\n` or `\r\n`) it was read with.
  - **Product tools.** Every one declares the Worldwright module `workspace`, which is always available and never filtered by `modules` (CORE-058). Inputs reject unknown fields. A tool with a `project` argument uses the session's active project when it's omitted, else `sketchbook` (ARCH-088, ARCH-098). The one exception is `assets.query`, whose `project` is a filter: omitted means every project.

    | Tool | Effect | Arguments | `structured` result |
    |---|---|---|---|
    | `workspace.create` | write | `{path, name?, default?}` | `{id, path, created: bool, default: bool, warnings: [string]}` |
    | `workspace.set_default` | write | `{path}` | `{id, path}` |
    | `workspace.doctor` | write | `{fix}` | `{checks: [{id, state: "ok"\|"fixed"\|"problem", detail}]}` |
    | `watch.notify` | write | `{paths: [string]}` | `{reconciled: [string], ignored: [string]}` |
    | `assets.query` | read | `{project?, status?, type?, text?, sort?, limit?, cursor?}` | `{assets: [AssetSummary], next_cursor: string\|null, total}` |
    | `asset.get` | read | `{project?, asset_id}` | `AssetDetail` |
    | `asset.import` | write | `{project?, asset_id, type, files: [path], mode: "copy"\|"link_external"}` | `{ref, run_id?, handoff}` |
    | `asset.collect` | write | `{project?, asset_id}` | `{run_id, handoff}` |
    | `asset.signoff` | write | `{project?, asset_id, run_id, files: [name], bump?, keep_runs?}` | `{ref, version, handoff}` |
    | `asset.request_signoff` | write | `{project?, asset_id}` | `{status}` |
    | `asset.pin_input` | write | `{project?, asset_id, ref, reason}` | `{pinned_inputs}` |
    | `project.create` | write | `{project, name, type?}` | `{project, path}` |
    | `project.set_status` | write | `{project, status: "active"\|"paused"\|"archived"}` | `{project, status, stale: [ref]}` |
    | `project.status` | read | `{project?}` | `{project, status, assets_by_status, orphans: [path], needs_repair: [path], missing: [path]}` |
    | `project.context` | read | `{project?}` | `{project, settings, links, suggested_tools: [name]}` |
    | `trash.list` | read | `{}` | `{entries: [restore record]}` |
    | `trash.restore` | write | `{entry_id}` | `{path}` |

    - `AssetSummary` is `{ref, project, asset_id, type, status, version, approved_version, updated, needs_repair}`; `updated` is the RFC 3339 UTC last-write time of the manifest as last indexed. An asset is identified by its folder, `projects/<project>/assets/<asset_id>/`, whose name is authoritative: `needs_repair` is `true` for a manifest that doesn't parse, or whose `asset_id` or `project` disagrees with its folder (a folder copied or moved outside the suite). Such a manifest is never rewritten automatically.
    - **`assets.query` paging** (APP-025, APP-138):
      - `sort` is `{field, dir}`: `field` is one of `asset_id`, `updated`, `status` and `type`, and `dir` is `asc` or `desc`. The default is `{field:"asset_id", dir:"asc"}`. Ties always break on `project`, then `asset_id`, ascending, so the order is total.
      - A missing `status` or `type` (a legacy manifest may have no `type`) sorts as the empty string: it comes first ascending and last descending. The SQL sorts and compares on `COALESCE(<column>, '')`, and the cursor stores the sort value the same way, so no row drops out of later pages through a NULL comparison.
      - `limit` is 1–1000 (default 200); anything else is `params.invalid`.
      - `cursor` is the opaque `next_cursor` of the previous page. It is a keyset position (the last row's sort value, project and asset id) bound to the query's filters and sort: its `q` is the first 16 hex characters of the SHA-256 of `canonical_json({project, status, type, text, sort})`. `limit` is not part of it, so the page size may change between pages. It never expires: it stays valid across writes, restarts and index rebuilds. A cursor from a different query, or one that doesn't decode, is `params.invalid`. `next_cursor` is `null` on the last page.
      - Every asset that matches the filters on every page and whose sort value doesn't change is returned exactly once, in order. An asset added, removed or re-sorted between pages may be missing from the pages or appear on two of them.
      - `total` is exact: the number of assets matching the filters when that page was read, counted in the same read transaction as the page.
      - The arguments are optional filters, so later plans add filters (for example tags, ratings, colour labels or collections) and sort fields as new optional keys. Unknown keys are rejected, so an older core answers a newer filter with `params.invalid` rather than ignoring it.
    - `AssetDetail` is `AssetSummary` plus `{versions: [{version, ref, legacy}], next_versions: {major, minor, patch}, proposed_bump, candidate_runs, human_edits, external, files: [{path, state: "ok"|"not_fetched"|"cloud_only"|"human_edit"|"needs_repair"|"missing"}]}`.
      - `missing` (ARCH-079, CORE-038 clarification) is a file the suite still expects on disk that disappeared outside the suite: a file listed in the manifest's `hashes`, a file of an `open` run, or the manifest itself. It stays listed with its last SHA-256, is never cleaned up automatically, and returns to `ok` when a file with the same SHA-256 reappears at that path. Files of `selected` or `rejected` runs, and of runs with `trashed_utc`, are not expected there: sign-off moves chosen files into `v<version>/` and maintenance moves rejected runs to trash, so their removal is never `missing`.
      - `asset.get` of an asset whose manifest is missing fails with `manifest.needs_repair`, whose `data.missing` names the manifest path; the asset stays in `assets.query` and in `project.status.missing`.
      - Read tools (`assets.query`, `asset.get`, `project.status`) answer only after the workspace's first reconcile or cold rebuild has finished, so a fresh open never returns an empty or partial page.
    - `asset.pin_input` needs a ref that parses (`ref.invalid` otherwise). Its target doesn't have to exist on this machine, because a `proj:` or `lib:` source may be archived or not yet synced; the pin and the `links` entry are recorded either way (CORE-019).
    - Every write result that records an operation carries `handoff: {unlocks: [ref], made_stale: [ref], refs: [ref]}` (CORE-020).
    - `asset.import`'s `ref` is the candidate ref of its first file (for `link_external`, the asset's ref without a version); `handoff.refs` lists the ref of every file.
  - **Settings cascade** (CORE-057, I2, I4): workspace `worldwright.yaml` `defaults:` → `project.yaml` → the request layer, a plain override chain over dotted leaf keys. The project layer is `project.yaml`'s own top-level keys, as I4 says ("a project can override any of them"); there is no `overrides:` key. The cascade keys are `models`, `providers`, `qa_profile`, `modules`, `auto_candidate_on_save`, and `skip_stages` and `auto_approve`, which both layers write under `pipeline:` as in I4 (`pipeline.skip_stages`, `pipeline.auto_approve`). So `models: {image_generate: b}` at the top of `project.yaml` overrides `defaults: {models: {image_generate: a}}`, and both are the key `models.image_generate`.
  - **`progress.md` entries** (CORE-039): a new entry `- YYYY-MM-DD: <text>` (UTC) goes on the line directly after `## Done`, newest first; the blank line before the next heading and every other section stay byte for byte. A file without `## Done` gets `## Done` and the entry appended at the end. A CRLF file stays CRLF. A new project's `project.yaml` holds `status: active`, `links: []` and one comment line, so status changes and new links edit existing keys.
  - **Topics** (ARCH-044, ARCH-032):
    - `asset:ww://<workspace id>/<project>/<asset_id>` (the ref without `@<version>` and hash, so one topic covers every version) carries `{kind: "manifest"|"candidate_run"|"version_sealed"|"human_edit"|"needs_repair"|"missing"|"status", …}`; its snapshot is the `asset.get` result;
    - `project:<slug>` carries `{kind: "status"|"progress"|"asset"}`; its snapshot is the `project.status` result;
    - `workspace` carries `{kind: "orphans"|"git_paused"|"git_resumed"|"rescan"|"default_changed"|"watcher_unavailable"}`.

    History is kept per topic for 600 s and at most 1024 events (ARCH-032 clarification).
  - **Error codes.** The Appendix D table gains the 20 rows marked "plan 2b"; its two *provisional* rows `store.hash_conflict` and `workspace.required` become "confirmed by plan 2b". `ww-protocol/src/codes.rs` follows the same table (Task 6):

    | Code | Category | Retry | Source |
    |---|---|---|---|
    | `store.hash_conflict` | `data_conflict` | `after_resolve` | QA-005, confirmed by plan 2b |
    | `workspace.required` | `user_fixable` | `after_resolve` | QA-005, confirmed by plan 2b |
    | `store.busy` | `transient` | `same_idempotency_key` | plan 2b; a target held past the 10 s retry budget |
    | `store.path_invalid` | `user_fixable` | `after_resolve` | plan 2b; budget, reserved name, outside the workspace, a sealed folder, `registry/ledger.csv` |
    | `store.disk_full` | `user_fixable` | `after_resolve` | plan 2b |
    | `store.io` | `transient` | `same_idempotency_key` | plan 2b |
    | `store.git_busy` | `transient` | `same_idempotency_key` | plan 2b; git still active after 60 s |
    | `store.locked` | `data_conflict` | `after_resolve` | plan 2b; the path is in the manifest's `locks` (CORE-035) |
    | `workspace.read_only` | `user_fixable` | `after_resolve` | plan 2b; another core holds `core.lock` |
    | `workspace.path_invalid` | `user_fixable` | `after_resolve` | plan 2b |
    | `project.unknown` | `user_fixable` | `after_resolve` | plan 2b |
    | `project.exists` | `data_conflict` | `never` | plan 2b |
    | `asset.unknown` | `user_fixable` | `after_resolve` | plan 2b; also an unknown run or file |
    | `asset.link_external` | `user_fixable` | `after_resolve` | plan 2b; fix: "Collect into workspace", agent hint `asset.collect` |
    | `version.exists` | `data_conflict` | `after_resolve` | plan 2b |
    | `status.invalid_transition` | `user_fixable` | `after_resolve` | plan 2b |
    | `manifest.needs_repair` | `user_fixable` | `after_resolve` | plan 2b |
    | `yaml.unsupported_edit` | `user_fixable` | `after_resolve` | plan 2b |
    | `ref.invalid` | `user_fixable` | `after_resolve` | plan 2b |
    | `ref.hash_mismatch` | `data_conflict` | `after_resolve` | plan 2b |
    | `trash.unknown` | `user_fixable` | `after_resolve` | plan 2b |
    | `trash.restore_conflict` | `data_conflict` | `after_resolve` | plan 2b |

  - **`ww-core` exit code 7:** the suite `state.db` has a hot rollback journal (a crash mid-write); nothing was changed.
  - **The `ww` commands this plan adds.** Exit codes are as in Appendix D. `ww doctor` exits 0 when no problem remains, 1 when one does, and 2 when it couldn't run.

    | Command | Output |
    |---|---|
    | `ww workspace init <path> [--name <n>] [--default]` | `workspace created at <path>` or `workspace registered at <path>`; then `default workspace: <path>` when it is the default |
    | `ww workspace use <path>` | `default workspace: <path>`; it connects with workspace `"none"`, so a stale default never blocks it |
    | `ww notify <path>…` | nothing on success; with `--no-start` and no core running, a silent no-op exiting 0 (ARCH-018) |
    | `ww doctor` | one line per check, `ok: …`, `fixed: …` or `problem: … — <what to do>`; a damaged, newer or hot-journal suite `state.db` is explained, and is set aside (or recovered) only after you type `y`; an inherited `core.autocrlf=true` is a `problem`, set to `false` for the repository only after you type `y` (never with `--non-interactive`); with no `suite.toml` or no default workspace it prints `ok: no default workspace; workspace checks skipped` |

    The core-start messages become:
    - "The core can't start: its state database at <path> is damaged. Nothing was changed. `ww doctor` can set it aside."
    - "The core can't start: its state database at <path> was left mid-write by a crash. Nothing was changed. `ww doctor` can finish the recovery."

    The `workspace.unsupported` and unregistered-workspace hints quote the path (`ww workspace use "<path>"`). A workspace named by `--workspace` is never told to become the default; it is told which workspace the core serves.
  - **Fail points** (QA-066, QA-055): `WW_TEST_FAILPOINTS=<name>=<action>[,…]`. They are read only on test channels, and disabled at compile time in builds whose `WW_BUILD_CHANNEL` is `stable` (a compile-time constant, so a stable build's checks are constant-false).
    - Actions: `abort` (the process ends at once, as a crash would: `TerminateProcess` on the current process, so no destructor runs and Windows Error Reporting is not involved), `disk_full`, `busy`, `unsupported` and `io`.
    - Names: `store.write_temp`, `store.replace`, `store.after_replace`, `store.between_renames`, `store.put_back`, `store.suite_toml`, `store.after_output`, `store.after_manifest`, `store.after_jobs_log`, `store.after_progress`, `store.after_index`, `store.after_seal`, `watch.start`.
  - **Dates:** `YYYY-MM-DD` and RFC 3339 `…Z`, in UTC. The set-aside suffix is `YYYYMMDDTHHMMSSZ`.
  - **Clarifications recorded in Appendix E** (no new rule IDs):
    - ARCH-074: the write algorithm above. "`ReplaceFileW` / `MoveFileExW` with write-through" means `ReplaceFileW` without a write-through flag (`REPLACEFILE_WRITE_THROUGH` is documented as not supported) after the temp file is flushed, and `MoveFileExW` with `MOVEFILE_WRITE_THROUGH`, which is documented to flush only a move done as a copy and delete. Renames are atomic (journaled) but may be lost on power failure; the temp flush makes both states complete, and the pending row makes either acceptable. The window between the hash check and the replace is closed by checking the backup, not only by the watcher. No rename ever replaces an existing file, and recovery never leaves an outside edit only in a dot-file.
    - ARCH-075: the `progress.md` entry (CORE-020) comes between the jobs log line and the index, and events go out last.
    - ARCH-076: the typed structs are views over the ordered map, which is what keeps unknown fields in their original place.
    - ARCH-077, QA-048, QA-066: the golden round-trip uses a synthetic manifest with the lantern manifest's structure (owner decision 6). Fixture workspaces are synthetic; none is copied from a real workspace.
    - ARCH-044: the live topic of a ref is `asset:` plus the ref without `@<version>` and hash.
    - ARCH-079: a file's hash counts only when its identity (size, last write, ChangeTime) is the same before and after the read; an unstable or locked file is parked and re-checked, and is never a human edit or a removal. A directory event reconciles the folder's whole subtree. A human edit already recorded with the same SHA-256 is not recorded again.
    - ARCH-079, CORE-038: a file the suite still expects on disk (listed in `hashes`, a file of an `open` run, or a manifest) that disappears outside the suite is `missing`: it stays listed, is never cleaned up automatically, and recovers when the same content reappears. Moves the suite makes itself (sign-off, trash, maintenance) are never `missing`: files of `selected`, `rejected` or trashed runs aren't expected on disk, and the watcher defers removals under an asset whose sign-off is in progress until the sign-off ends, then reconciles that asset folder.
    - CORE-047, ARCH-079: the asset folder name is authoritative. The index stores each asset's `creation_uuid`, and a manifest whose `asset_id` or `project` disagrees with its folder is `needs_repair`.
    - CORE-019: new `proj:` and `lib:` links are written to `links` in the Execute stage of `asset.pin_input`, because Validate runs before Confirm and never writes.
    - CORE-032: "purged" means moved to trash (ARCH-058), where the entry is purged 30 days later, so a rejected run leaves the disk 120 days after sign-off. All of `candidates/` is gitignored, not only rejected runs, so open runs don't sync between machines.
    - CORE-039: a new entry goes on the line directly after `## Done`, newest first, and the blank line before the next heading is kept.
    - CORE-040: the budget is exactly 180 characters, and the `.gitignore` form is root-relative. The workspace `.gitattributes` holds `projects/** -text`, and edits keep the line ending a file was read with, so a recorded SHA-256 holds on every clone.
    - CORE-041, ARCH-089: FileId and ChangeTime are compared only on local NTFS or ReFS volumes outside a cloud-files sync root. Elsewhere size and mtime are compared, and a file whose mtime is within 2 s of the previous scan is hashed. A volume whose facts can't be read counts as having no stable IDs. File IDs only detect changes in place: ReFS IDs change on a move across folders, so the store reports each file's identity after every write and move.
    - CORE-042: the index budgets (a cold rebuild of 2,000 assets and a warm reconcile of 10,000) live in `test-params.toml`.
    - CORE-051: `approved_by` is the run's `signed_off.by`.
    - CORE-057: in plan 2b the per-request layer is the `settings` argument of a tool that declares one. A per-call `settings` field in `tools/call` is a later protocol change, and it changes `schema_hash`. The project layer is `project.yaml`'s top-level keys (I4), with `pipeline.skip_stages` and `pipeline.auto_approve` read as `skip_stages` and `auto_approve`.
    - QA-044: plain `proptest` strategies over operation sequences are an acceptable reading of "`proptest-state-machine`".
    - QA-066: fail points are disabled at compile time in stable builds and armed only on test channels.
    - ARCH-032: the per-topic history also has a count cap of 1024.
    - I5 (workspace format, `docs/reference/workspace-format.md`): "Runs are never edited after sign-off, apart from their status" also allows `trashed_utc`, which maintenance writes when it moves a rejected run's folder to trash (CORE-032).
  - **Decision recorded in Appendix E:** organising metadata (tags, ratings, labels, collections) lives in manifests or tracked YAML, never only in `index.db`. The fields and their UI are plan 4.
- **Test-channel tools added by this plan** (module `"test"`, registered only on `test-*` channels; a test contract, not copied into the spec):

  | Tool | Effect | Arguments | Result (`structured`) |
  |---|---|---|---|
  | `test.store_write` | write | `{path, text, expect: "new"\|"generated"\|"<sha256>"}` | `{sha256}`, through `Store::write` |
  | `test.ingest_run` | write | `{project?, asset_id, files: [{name, text}], job_line: bool}` | `{run_id, staging_dir, files: [path]}`; it writes the files into a staging handle as an engine would, then commits a candidate run. An omitted `project` is `sketchbook` until Task 12's resolver, then the resolved project |
  | `test.trash` | write | `{path}` | `{entry_id}` |
  | `test.watch_fence` | read | `{name}` | `{}` once the workspace's first reconcile has finished and the watcher has handled `<ws>\.worldwright\fence\<name>` |
  | `test.settings` | read | `{project?, settings?}` | the resolved `Settings` with sources; its `settings` argument is plan 2b's only source of the request layer (CORE-057 clarification) |
- **Privacy** (QA-086): no user-profile paths (write `%USERPROFILE%`), no personal names or e-mail addresses in code, fixtures, test names or reports.
  - Non-ASCII test labels use "Ünïcödé space".
  - Fixture manifests are made up. Authors are `"sample-author"`, and hashes are SHA-256 values of the fixture's own bytes (in `hashes`, written `sha256:<hex>` as in I5).

## Review Focus

1. **The core crashes between writing an output and updating its manifest**: a killed process, a power cut, a debugger stop.
   - **Expected:**
     - no file is half-written, and no temp file survives the next start;
     - an output with no manifest entry stays on disk and is listed as an orphan, never deleted;
     - a crash after the manifest step leaves a normal, readable run;
     - the dead core's `running` idempotency row is released at the restart (the restarted core holds `core.lock`), so a retry runs the operation again. The first attempt's files stay visible as an orphan or a run, and nothing is lost;
     - a crash inside a replace, or in the middle of putting an outside edit back, leaves the outside edit visible under the file's own name, and the core's own new bytes are not later reported as a human edit;
     - a crash between sealing a version and the manifest edit lists the sealed folder as an orphan, and no file is lost.
   - **Covered by:** Task 14, `crash_at_each_write_order_step_recovers_cleanly` and `crash_between_seal_and_manifest_lists_the_version_and_loses_nothing`; Task 6, `crash_after_replace_keeps_outside_edit_visible`, `crash_after_replace_does_not_flag_own_write_as_human_edit` and `crash_during_put_back_keeps_the_outside_edit`; Task 5, `crash_after_output_lists_orphan_and_keeps_files` (green in Task 12).
2. **A target is held open** by Defender, Explorer's preview pane or an image viewer when the core replaces it.
   - **Expected:** the write retries with backoff and succeeds once the handle is released. If the handle is still held after 10 s, the call fails with `store.busy` and leaves the target unchanged and no temp file behind.
   - **Covered by:** Task 6, `held_target_is_retried_until_released` and `target_held_past_budget_fails_with_store_busy_and_no_temp`.
3. **Long paths**: a relative path near 180 characters under a deep workspace, and an absolute path past 260.
   - **Expected:** a relative path over 180 characters is refused before any file is touched. A legal relative path under a deep workspace is written even when the absolute path passes 260 characters.
   - **Covered by:** Task 6, `relative_path_over_180_chars_is_refused_before_any_write` and `deep_workspace_past_260_chars_writes_through_long_paths`.
4. **A workspace in a OneDrive-synced, network or FAT/exFAT folder.** FileId or ChangeTime may be unstable there, `ReplaceFileW` or the watcher may be unsupported, and a read may download a cloud-only file. A OneDrive folder is itself NTFS (the cloud-files filter supports only NTFS), so "NTFS" alone doesn't make IDs trustworthy.
   - **Expected:**
     - the index trusts FileId and ChangeTime only on local NTFS or ReFS volumes outside a cloud-files sync root. Elsewhere it ignores their churn and compares size and mtime, hashing only what changed, so a sync client renumbering IDs doesn't trigger a rehash of everything; a file whose mtime is within 2 s of the previous scan is hashed anyway (FAT's write-time resolution);
     - cloud-only placeholders are listed as `cloud_only` and never read (identity uses an attribute-only open), so the reconciler never downloads them;
     - where `ReplaceFileW` is unsupported, the two-rename fallback still never loses the old or the new content, and never replaces a file an outside writer created between the renames;
     - where `ReadDirectoryChangesW` is unsupported, the workspace is still served without a watcher, and `ww doctor` reports it;
     - a FAT/exFAT USB stick or network share whose volume facts can't be read is created and served as a volume without stable IDs, with a warning, and never fails `workspace.create`;
     - on ReFS (Dev Drive), a move across folders changes a file's ID without being taken for a content change.
   - **Covered by:** Task 2, `stable_ids_only_on_local_ntfs_or_refs_outside_sync_roots` and `sync_root_failure_is_false_off_local_ntfs`; Task 8, `create_on_a_fat_or_usb_volume_warns_and_succeeds`; Task 10, `volume_without_stable_ids_ignores_id_churn`, `recent_mtime_is_hashed_on_volumes_without_stable_ids`, `cloud_placeholder_is_indexed_but_never_read` and `refs_cross_folder_move_changes_id_without_a_false_change`; Task 6, `replace_falls_back_to_two_renames_where_unsupported` and `fallback_second_rename_never_replaces_a_new_outside_file`; Task 11, `unsupported_watcher_leaves_the_workspace_served`.
5. **`git checkout` of thousands of files** overflows the watcher buffer.
   - **Expected:** the overflow triggers a full rescan, which re-parses every manifest before it decides any other file. Writes wait while `.git/index.lock` exists (the store checks the file itself), and the index converges. A file that matches its (new) manifest or run-record hash is never reported as a human edit, and a folder the checkout adds is indexed in full.
   - **Covered by:** Task 11, `checkout_of_3000_files_overflows_then_rescans_without_false_human_edits` and `folder_moved_into_the_workspace_is_indexed`.

## Changes from the outline

The outline at the end of plan 2a gives 10 tasks. This plan keeps their content and changes the order and the framing:
- **Spec tests are written early (Task 5), not in the asset-tools task.** QA-017 requires scenario tests for `store` to be merged as expected-to-fail before the implementation. The outline's Task 9 came after the store was built.
- **Store writes come before the workspace tools.** `workspace.create` writes `worldwright.yaml` and `suite.toml` through the ARCH-074 write path, so that path must exist first. The outline's order (workspace tools at 2, store writes at 3) would have meant writing them twice.
- **New tasks:**
  - spec changes first (Task 1, QA-029);
  - dependencies and the platform file layer (Task 2, the one root `Cargo.toml` edit);
  - the test kit (Task 4: fixtures, fail points, handle holder, git-ops simulator, fence);
  - a close-out (Task 15).
- **The root `Cargo.toml` changes after all** (the outline expected it might not). `proptest` and `toml_edit` aren't workspace dependencies yet, and `preserve_order` belongs on the shared `serde_json` entry.
- **`test-params.toml` changes (protected).** It records the CORE-042 index budgets (cold rebuild and warm reconcile), because QA-019 makes it the home of test budgets.
- **`asset.get` and `assets.query` land with the index (Task 10)**, so the watcher's spec tests can observe human edits before Task 13.
- **`assets.query` pages from the start.** It has a stable sort, an opaque keyset cursor and an exact `total`, because the asset browser pages its grid (APP-025) and must show the first page of 10,000 assets in under 200 ms (APP-138). Fixing the contract now means plan 4 adds filters without changing it.
- **Five tools beyond CORE-146** are added to the spec in Task 1, because the rules need a way to act:
  - `trash.list` and `trash.restore` (ARCH-058's restore records);
  - `asset.collect` ("Collect into workspace", CORE-029, APP-074);
  - `workspace.doctor` (`ww doctor` writes `.gitignore` only through the core, ARCH-002);
  - `watch.notify` (`ww notify`).

## Not in this plan

- **The proposal-run Record branch** (CORE-020 third branch, CORE-043, CORE-044): plan 6, with canvas generation.
- **Document save handles** (CORE-003 second name) and `has_working_doc` set by document saves (CORE-049, CORE-050): plan 5, with documents.
- **Deterministic repairs and QA in the completion chain** (CORE-025's middle steps, CORE-030), and the automatic sign-off trigger "once QA passes" (CORE-051, CORE-052). They belong to the plan that adds the `qa` module; this plan provides the hook points and the pick rule.
- **Job records and usage lines** (CORE-117, CORE-091 onward): plan 3 writes them, through this plan's append-log API and write-order slot.
- **`asset.transfer`** (CORE-146, the `library` module): not placed on the roadmap yet; the plan that adds `library` takes it.
- **The full crash suite nightly** (QA-055 "full suite nightly"). It needs a change to `.github/workflows/nightly.yml` (protected), and this plan adds only the fast subset. Plan 3 adds the nightly step together with the job-submission fail points.
- **`ww doctor` listing skills that still write directly** (ARCH-104): plan 3, with plugin 1.3.
- **Multiple workspaces served at once** (ARCH-091, Later); **`renamed_from` resolution** (ARCH-087, CORE-046, M1.5); **git commits** (ARCH-084, plan 7+).
- **Deferred from research 5A** (`01-asset-management.md`) and the plan-2b review:
  - **Asset moves and copies made outside the suite** (5A-3, the part not adopted): pairing an asset folder's `RenamedFrom`/`RenamedTo` into one move and reporting it as `moved`, and the "make independent copy" repair for two folders with one `creation_uuid`. Plan 3 takes them with the `asset.rename` tool (research 5B-1), and plan 4 adds the UI. 2b only marks a folder/manifest mismatch `needs_repair` and stores `creation_uuid` in the index.
  - **Re-applying the read-only attribute automatically after every post-git rescan** (5A-6, the automatic part): plan 7+, with Sync. In 2b, `ww doctor` re-applies it on request (`sealed.readonly`).
  - **`trash.restore {entry_id, to?}`**, a way out of a restore conflict (5A-7, the last part): plan 4, with the trash UI.
  - **The query side of APP-138** (the first page of 10,000 assets in under 200 ms, through the same cursor) and a cold-rebuild budget at 10,000 assets (5A-8, the part not adopted): plan 4, with the asset browser. 2b gates the cold rebuild at 2,000 and the warm reconcile at 10,000.
  - **An SQL index on `files.sha256` and a `thumbs` table keyed by content hash** (5A-9): plan 4, when duplicates and thumbnails exist. `index.db` is disposable and versioned, so adding them later costs nothing.
  - **Organising-metadata fields** (tags, ratings, colour labels, collections) and their UI (5A-10): plan 4. 2b records only where they will live.
  - **A per-call `settings` field in `tools/call`** (CORE-057's request layer for product tools; it changes `schema_hash`): plan 3, with the job tools.
  - **An optional `machine_label` in `suite.toml`**, so committed log names need not carry the computer name (ARCH-081; raised by the plan-2b review): plan 7+, with Sync.
  - **Moving to `toml` 1.x and `toml_edit` 0.25 together** (owner question 5): a separate dependency change after this plan merges.
- **Not adopted:** `creation_uuid` in manifest `inputs` entries (research 5A-4); see Decisions.

## Execution notes

- **Per-task gate**, run at the end of every task before its commit:
  1. `cargo nextest run --workspace --exclude ww-spec-tests`
  2. `cargo run -q -p ww-cli -- dev spec-tests` (exits 0; the expected-fail count only goes down after Task 5)
  3. `cargo clippy --workspace --all-targets -- -D warnings`
  4. `cargo fmt --check`
  5. `cargo deny check`
  6. `cargo run -q -p ww-cli -- dev spec-report`
  7. `cargo run -q -p ww-cli -- dev privacy-scan --tree HEAD` (after the commit)

  Each task's own "Run" lines are the quick inner loop; the gate is what the task's report quotes.
- **Tool registration.** Each task that adds a `register_*_tools` function also calls it from `core_registry` in `crates/ww-core/src/tools/mod.rs` (Task 8 gives it the hub, env and probe; Task 14 the git environment).
- **Plan 2a's pinning unit tests.** `crates/ww-core/src/tools/tests.rs` pins `Dispatcher::new`'s type, the test-tool names, the off-channel registry, every tool's module and the sync stages. Each task that changes one of those updates that file itself (Tasks 7, 8, 9, 11, 12, 13 and 14 list it): never delete a pin to get green.
- **Adversarial review** (QA-023): Tasks 6, 7, 9, 11, 13 and 14 change `store`, `state` or the dispatcher. Their review is done by a reviewer with fresh context, who must try to write a failing test, not just comment.
- **Spec tests** (QA-017, QA-018): Task 5's author reads only the spec (with Appendices D and E), this plan's Global Constraints and the Interfaces blocks. Later tasks only remove `expected-fail.toml` entries; none edits a spec test.

---

### Task 1: Spec changes first (QA-029)

**Files:**
- Modify: `docs/superpowers/specs/2026-10-12-worldwright-suite-design.md`. No new IDs, no renumbering and no new `- **ID** [` lines; the ID check stays `925 925 []`.
- Modify: `docs/reference/workspace-format.md` (not protected), two edits in I5:
  - in the sign-off step's `signed_off` object, `by` becomes `"user" | "assistant" | "claude-code" | "cli"` (owner question 4);
  - "Runs are never edited after sign-off, apart from their status." becomes "Runs are never edited after sign-off, apart from their status and `trashed_utc`, which the suite sets when it moves a rejected run's folder to trash (plan 2b clarification: spec Appendix E)."

**Interfaces:**
- Produces: spec Appendix E and the Appendix D additions, which Tasks 2–15 implement.

- [ ] **Step 1: Append clarifications to rule text.** Mark each "(plan 2b clarification: Appendix E)":
  - **ARCH-074.** "`ReplaceFileW` has no write-through flag (`REPLACEFILE_WRITE_THROUGH` is documented as not supported), and its target, replacement and backup must be on one volume. The temp file is flushed before the swap. Renames are atomic (journaled) but may be lost on power failure (`MOVEFILE_WRITE_THROUGH` is documented to flush only a move done as a copy and delete), so the store records the pending old and new hashes first and accepts either version as its own. No rename replaces an existing file. The outside-edit window is closed by checking the replaced file's backup, and recovery never leaves an outside edit only in a backup file (Appendix E)."
  - **ARCH-075.** "The `progress.md` entry (CORE-020) comes between the jobs log line and the index; events go out last."
  - **ARCH-076.** "The typed structs are views over the ordered map, which keeps unknown fields in their original place."
  - **ARCH-044.** "The topic of a ref is `asset:` plus the ref without its `@<version>` and hash, so one subscription sees every version."
  - **ARCH-077, QA-048 and QA-066.** "(owner decision 2026-10-15) The golden round-trip uses a synthetic manifest with the lantern manifest's structure. Fixture workspaces are synthetic; nothing is copied from a real workspace."
  - **ARCH-079.** "A hash counts only when the file's identity is the same before and after the read; an unstable or locked file is parked and re-checked, never recorded as a human edit or a removal. A directory event reconciles the folder's subtree. A human edit already recorded with the same SHA-256 isn't recorded again. A file the suite still expects on disk (listed in `hashes`, a file of an open run, or a manifest) that disappears is `missing` until the same content reappears (with CORE-038); the suite's own moves (sign-off, trash, maintenance) never make a file `missing`."
  - **CORE-038.** "A file the suite still expects on disk that disappears outside the suite is listed as `missing`, never forgotten (ARCH-079 clarification)."
  - **CORE-039.** "A new entry goes on the line directly after `## Done`, newest first; the blank line before the next heading is kept."
  - **CORE-047.** "The asset folder name is authoritative; the index stores `creation_uuid`, and a manifest whose `asset_id` or `project` disagrees with its folder is `needs_repair`."
  - **CORE-019.** "In plan 2b the links are written in the Execute stage of `asset.pin_input`, since Validate runs before Confirm and never writes."
  - **CORE-032.** "Purged here means moved to trash (ARCH-058), which purges the entry 30 days later: 120 days after sign-off in all. The workspace `.gitignore` ignores all of `candidates/`, so open runs don't sync between machines."
  - **CORE-040.** "The budget is exactly 180 characters. In the workspace's root `.gitignore`, `assets/*/work/` is written `projects/*/assets/*/work/`. The workspace `.gitattributes` holds `projects/** -text`, and edits keep the line ending a file was read with."
  - **CORE-041 and ARCH-089.** "FileId and ChangeTime are compared only on local NTFS or ReFS volumes outside a cloud-files sync root. Elsewhere size and mtime are compared, and a file whose mtime is within 2 s of the previous scan is hashed. A volume whose facts can't be read counts as having no stable IDs. File IDs detect changes in place only: ReFS IDs change on a move across folders."
  - **CORE-051.** "`approved_by` is the sign-off run's `signed_off.by` field of the workspace format."
  - **CORE-057.** "In plan 2b the per-request layer is the `settings` argument of a tool that declares one; a per-call `settings` field is a later protocol change (it changes `schema_hash`). The project layer is `project.yaml`'s top-level keys (I4), with `pipeline.skip_stages` and `pipeline.auto_approve` read as `skip_stages` and `auto_approve`."
  - **QA-044.** "Plain `proptest` strategies over operation sequences are an acceptable reading of `proptest-state-machine`."
  - **QA-066.** "Fail points are disabled at compile time in stable builds and armed only on test channels."
  - **CORE-042.** "The budgets (a cold rebuild of 2,000 assets, a warm reconcile of 10,000) are recorded in `test-params.toml`."
  - **ARCH-032.** "The history is also capped at 1024 events per topic; an older cursor gets a snapshot."
  - The I5 clarification of Global Constraints (`trashed_utc` on a signed-off run) has no rule text to attach to: it goes into Appendix E with the other clarifications (Step 3) and into `workspace-format.md` (Files).
  - **Appendix B.3**, the §1 Windows facts line: replace "`ReplaceFileW` / `MoveFileExW` write-through" with "`ReplaceFileW` (no write-through flag: `REPLACEFILE_WRITE_THROUGH` is not supported; one volume; the result keeps the replacement's file ID) and `MoveFileExW` `MOVEFILE_WRITE_THROUGH` (documented to flush only a move done as a copy and delete; re-checked 2026-10-16)". The line doesn't start with a rule ID, so the ID check is unaffected.
- [ ] **Step 2: Edit Appendix D.**
  - Add the 20 new error-code rows of Global Constraints (the ones whose source starts "plan 2b"). The existing rows `store.hash_conflict` and `workspace.required` lose *provisional* and gain "confirmed by plan 2b". `store.hash_conflict`'s row says that `data.backup` names the backup's workspace-relative path when a put-back couldn't finish.
  - Add exit code 7 to the `ww-core` exit-code table.
  - In the `ww` commands paragraph, replace "`ww doctor` (plan 2b) can set it aside." with "`ww doctor` can set it aside.", and add the hot-journal message.
  - In the Events bullet, after "History: 600 s per topic, by monotonic time", add "and at most 1024 events".
- [ ] **Step 3: Write `## Appendix E. Workspace data (plan 2b clarification)`** after Appendix D.
  - It contains, in order, the "Names that are contracts" bullets of this plan's Global Constraints: paths, write algorithm, write order, append logs, areas, `core.lock`, local data, `.gitignore`, `.gitattributes`, refs, manifest fields, product tools, settings cascade, `progress.md` entries, topics, the commands, fail points, dates, the clarifications and the organising-metadata decision. This list governs: the "Names that are contracts" lead-in is read the same way.
  - The error codes and exit code 7 go into Appendix D (Step 2), not here.
  - **Exceptions:** the test-channel tools table and the Privacy bullet (the two Global Constraints bullets after "Names that are contracts") stay out of the spec.
  - The text is copied as written. No line may start with `- **` followed by a rule ID.
- [ ] **Step 4: Update §0.5's document map** to end with "· Appendix E Workspace data (plan 2b clarification)".
- [ ] **Step 5: Verify the IDs.**

Run: `python -I -c "import re;t=open('docs/superpowers/specs/2026-10-12-worldwright-suite-design.md',encoding='utf-8').read();ids=re.findall(r'(?m)^- \*\*([A-Z]+-\d{3})\*\* \[',t);refs=set(re.findall(r'\b(?:ARCH|CORE|APP|AI|IMG|QA|OPS)-\d{3}\b',t));print(len(ids),len(set(ids)),sorted(refs-set(ids)))"`
Expected: `925 925 []`

- [ ] **Step 6: Commit.**

```bash
git add docs/superpowers/specs/2026-10-12-worldwright-suite-design.md docs/reference/workspace-format.md
git commit -m "spec: workspace data appendix and plan-2b clarifications (write path, codes, tools, golden)"
```

---

### Task 2: Workspace dependencies and the platform file layer (ARCH-074, ARCH-097, CORE-047, QA-038) — **protected: root `Cargo.toml`**

**Files:**
- Modify (protected): `Cargo.toml`. In one edit, add `proptest` and `toml_edit` to `[workspace.dependencies]`, give `serde_json` the `preserve_order` feature, and add `[profile.dev.package.libsqlite3-sys] opt-level = 3`.
- Modify: `crates/ww-platform/Cargo.toml` (`windows` features `Win32_System_IO` and `Win32_Storage_CloudFilters`).
- Create: `crates/ww-platform/src/{fs.rs, paths.rs}`. Modify `src/{lib.rs, env.rs, ids.rs, process.rs}`.
- Modify: `crates/ww-core/src/{identity.rs, workspace.rs}` and `crates/ww-client/src/connect.rs`. The private comparison normalisers (`identity.rs`'s `normalise`, `connect.rs`'s `folder_key`) become `ww_platform::paths::folder_key`. `workspace.rs`'s `id_of` is not a comparison: it produces `WorkspaceRef.path` (shown as `hello.workspace.path`) and keeps its case, so it uses `ww_platform::paths::strip_verbatim` (moved from `env.rs`'s private helper), never `folder_key`.
- Modify: `crates/ww-testkit/src/ids.rs` (`SeqIds::uuid`).
- Test: `crates/ww-platform/tests/fs.rs`; unit tests in `paths.rs`, `env.rs` and `ids.rs`; `crates/ww-platform/tests/pipes.rs` (hardened).

**Interfaces:**

Produces:

```rust
// ww_platform::fs — every function opens paths through long_path()
pub fn long_path(p: &Path) -> PathBuf;                            // made absolute ("\" separators, no "." or ".."), then
                                                                  // "\\?\" (or "\\?\UNC\") when 247 UTF-16 code units or more (std's rule)
pub fn flush(file: &std::fs::File) -> io::Result<()>;             // FlushFileBuffers
pub fn replace_file(target: &Path, replacement: &Path, backup: &Path) -> io::Result<()>;
    // ReplaceFileW(REPLACEFILE_IGNORE_MERGE_ERRORS | REPLACEFILE_IGNORE_ACL_ERRORS); never REPLACEFILE_WRITE_THROUGH
pub fn move_new(from: &Path, to: &Path) -> io::Result<()>;        // MoveFileExW(MOVEFILE_WRITE_THROUGH), no REPLACE_EXISTING; files and folders.
                                                                  // The only move: no function passes MOVEFILE_REPLACE_EXISTING
pub fn copy_new(from: &Path, to: &Path) -> io::Result<u64>;       // CopyFile2(COPY_FILE_FAIL_IF_EXISTS): an existing target is error 80 or 183,
                                                                  // never overwritten (std::fs::copy overwrites); block-clones on Dev Drive
pub fn set_readonly(path: &Path, on: bool) -> io::Result<()>;     // FILE_ATTRIBUTE_READONLY
pub fn attributes(path: &Path) -> io::Result<u32>;                // GetFileAttributesW; opens nothing (the store's index.lock check)
pub fn long_name(path: &Path) -> io::Result<PathBuf>;             // GetLongPathNameW: expands 8.3 short components; NotFound when gone
pub enum RetryClass { SharingViolation, ReplaceRetry, ReplacementNotMoved, TargetExists, Unsupported, DiskFull, Other }
pub fn classify(e: &io::Error) -> RetryClass;                     // 32/33/1224; 1175/1176; 1177; 80/183; 1/50; 112/39; else
pub struct FileIdentity { pub volume_serial: u64, pub file_id: Option<[u8; 16]>, pub size: u64,
                          pub last_write: i64, pub change_time: Option<i64>, pub attributes: u32 }
    // volume_serial and file_id from FILE_ID_INFO (64-bit serial, 128-bit id). file_id is None where FileIdInfo
    // fails, and when the id is all zero or all 0xFF (MS-FSCC: such ids "MUST be ignored"); change_time is None
    // when the file system reports 0. On ReFS the id is built from the parent folder, so a move across folders
    // changes it: ids detect changes in place, never identity across folders
impl FileIdentity { pub fn is_cloud_placeholder(&self) -> bool; }  // RECALL_ON_DATA_ACCESS | RECALL_ON_OPEN | OFFLINE
pub fn identity(path: &Path) -> io::Result<FileIdentity>;
    // CreateFileW(FILE_READ_ATTRIBUTES only, share read|write|delete, BACKUP_SEMANTICS|OPEN_REPARSE_POINT),
    // then GetFileInformationByHandleEx(FileIdInfo, FileBasicInfo, FileStandardInfo); an attribute-only open
    // never reads data, so a cloud placeholder is not hydrated
pub struct VolumeInfo { pub fs_name: String, pub remote: bool, pub cloud_sync_root: bool }
impl VolumeInfo { pub fn stable_ids(&self) -> bool; }             // !remote && !cloud_sync_root && fs_name is "NTFS" or "ReFS"
pub fn volume_info(path: &Path) -> io::Result<VolumeInfo>;
    // volume::volume_root + GetVolumeInformationByHandleW (fs name) + GetDriveTypeW (DRIVE_REMOTE)
    // + CfGetSyncRootInfoByPath(CF_SYNC_ROOT_INFO_BASIC), which succeeds only under a cloud-files sync root.
    // No volume serial here: GetVolumeInformationByHandleW's is 32-bit, FILE_ID_INFO's is 64-bit
pub fn sync_root_from(cf: Result<(), i32 /* HRESULT */>, fs_name: &str, remote: bool) -> io::Result<bool>;
    // pure: Ok(()) → true; HRESULT_FROM_WIN32(ERROR_CLOUD_FILE_NOT_UNDER_SYNC_ROOT = 390) → false; any other
    // failure → false on a non-NTFS or remote volume, and an error only on local NTFS
pub struct LockedFile;                                            // Drop unlocks and closes
pub fn try_lock_exclusive(path: &Path) -> io::Result<Option<LockedFile>>;  // creates; LockFileEx EXCLUSIVE|FAIL_IMMEDIATELY on [0,1)
pub struct AppendLock;                                            // Drop unlocks
pub fn try_lock_append(file: &std::fs::File) -> io::Result<Option<AppendLock>>;
    // LockFileEx(EXCLUSIVE | FAIL_IMMEDIATELY) on [2^64-2, +1): None at once when another handle holds it.
    // It never waits: the caller (the store) polls with tokio time under its own budget
pub enum Share { None, Read, ReadWrite }
pub fn open_shared(path: &Path, share: Share) -> io::Result<std::fs::File>;  // the handle-holding helper's primitive
pub trait Probe: Send + Sync + std::fmt::Debug {                  // injectable, so tests fake volumes and placeholders
    fn identity(&self, path: &Path) -> io::Result<FileIdentity>;
    fn volume_info(&self, path: &Path) -> io::Result<VolumeInfo>;
    fn open_read(&self, path: &Path) -> io::Result<std::fs::File>; // the only way store, index and watch read file data
}
pub struct SystemProbe;                                           // the functions above
// ww_platform::paths
pub fn folder_key(path: &Path) -> String;  // canonicalise (when it exists), strip \\?\ and \\?\UNC\, `\` separators,
                                           // trim trailing separators except on a bare drive root, which stays `x:\`
                                           // (as connect.rs does today), Unicode lowercase; for comparison only, never hashed
pub fn strip_verbatim(path: &Path) -> PathBuf;  // strips \\?\ (and \\?\UNC\ → \\); keeps case; for paths that are shown
// env.rs
impl Env { pub fn machine_id(&self) -> String; }                  // machine_id_from(COMPUTERNAME); test channels "testmachine"
pub fn machine_id_from(computer_name: &str) -> String;           // pure: lowercase, every char outside [a-z0-9-] → '-'
// ids.rs
pub trait IdSource { fn id(&self, prefix: &str) -> String; fn uuid(&self) -> String; }   // uuid: v4, lowercase, hyphenated
// process.rs
pub struct Captured { pub code: Option<i32>, pub stdout: String, pub stderr: String }
pub fn run_hidden(exe: &Path, args: &[&OsStr], cwd: &Path, env: &[(OsString, OsString)], within: Duration) -> io::Result<Captured>;
    // CREATE_NO_WINDOW, stdio captured, killed after `within`; the core never flashes a console (ARCH-005)
pub fn find_on_path(name: &str) -> Option<PathBuf>;
pub fn terminate_self(code: u32) -> !;   // TerminateProcess(GetCurrentProcess(), code): no destructors, no Windows Error
                                         // Reporting (std::process::abort is __fastfail, which goes through WER)
```

- [ ] **Step 1: Write the failing tests.**
  - **`tests/fs.rs`** (tempdirs only):
    - `replace_file_swaps_content_and_keeps_the_backup`;
    - `replaced_file_takes_the_replacement_file_id` (documented behaviour the index relies on);
    - `move_new_refuses_an_existing_target`;
    - `copy_new_refuses_an_existing_target` (the target's bytes are unchanged; the error classifies as `TargetExists`);
    - `held_target_is_a_sharing_violation`: `open_shared(target, Share::Read)`, then `replace_file` → `classify == SharingViolation`;
    - `long_path_round_trip_past_260`: a tempdir nested to 300 characters; `move_new`, `replace_file`, `identity` and `set_readonly` all work, and a folder whose path is 250 characters is created;
    - `file_id_survives_a_same_folder_rename` (NTFS, a rename within one folder; across folders only NTFS keeps the id, so the test asserts nothing about other file systems);
    - `long_name_expands_a_short_component`: a file whose 8.3 name comes from `GetShortPathNameW` maps back to its long path; when the temp volume has 8.3 names disabled (the short name equals the long one), the test returns early and says so;
    - `attributes_reads_a_held_file_without_opening_it` (while `open_shared(path, Share::None)` holds it);
    - `identity_needs_no_data_access`: while `open_shared(path, Share::None)` holds the file, `identity` still succeeds; a file with `FILE_ATTRIBUTE_OFFLINE` set reports `is_cloud_placeholder()`;
    - `volume_info_reports_ntfs_for_the_temp_volume` (local, not a sync root, `stable_ids()`);
    - `exclusive_lock_is_refused_while_held_and_free_after_drop`;
    - `append_lock_excludes_a_second_holder` (a second handle's `try_lock_append` returns `None` at once while the first holds it, and `Some` after the first is dropped);
    - `readonly_attribute_round_trip`.
  - **Unit tests:**
    - `folder_key_strips_prefixes_case_and_trailing_separators`: `\\?\C:\Ws\`, `c:\ws` and `\\?\UNC\srv\share\x` → `\\srv\share\x`; a bare drive root keeps its separator (`Z:\` → `z:\`);
    - `strip_verbatim_keeps_case` (`\\?\C:\Ünïcödé space\Ws` → `C:\Ünïcödé space\Ws`, `\\?\UNC\srv\share\X` → `\\srv\share\X`);
    - `folder_key_resolves_a_junction_to_its_target` (`mklink /J` in a tempdir through `run_hidden` of `%ComSpec%`);
    - `machine_id_is_lowercase_and_restricted` (through the pure `machine_id_from`: `"DESK_01.Ünï"` → `"desk-01--n-"`, and every output character is in `[a-z0-9-]`; `Env::machine_id` itself is only checked to equal `machine_id_from` of the real name), and `test_channel_machine_id_is_testmachine`;
    - `random_uuid_is_v4` (version nibble 4, variant `8`–`b`);
    - `stable_ids_only_on_local_ntfs_or_refs_outside_sync_roots` (Review Focus 4): the truth table over `fs_name` (`NTFS`, `ReFS`, `FAT32`, `exFAT`), `remote` and `cloud_sync_root`;
    - `long_path_prefixes_from_247_units_and_unc` (lengths in UTF-16 code units: 246 unchanged, 247 prefixed, including a path whose non-BMP characters make its code-unit count larger than its `char` count; a UNC path gets `\\?\UNC\`, `..` is resolved first);
    - `sync_root_failure_is_false_off_local_ntfs` (Review Focus 4): `sync_root_from` over success, 390, and another failure, on `FAT32`, `exFAT`, remote `NTFS` and local `NTFS` (only another failure on local NTFS is an error);
    - `file_id_all_zero_or_all_ff_is_none`;
    - `terminate_self_ends_the_process_with_its_code` (the test binary re-runs itself through `run_hidden` with a helper flag that calls `terminate_self(0xDEAD)`; the exit code is `0xDEAD`);
    - `classify_maps_documented_error_codes` (every code in the `classify` comment);
    - `replace_flags_exclude_write_through` (the flags constant is exactly `REPLACEFILE_IGNORE_MERGE_ERRORS | REPLACEFILE_IGNORE_ACL_ERRORS`);
    - `run_hidden_captures_output_and_exit_code` (`%ComSpec% /c echo hi & exit 3`).
  - **`tests/pipes.rs`:** `instance_dacl_is_user_and_system_only` also asserts each ACE's access mask is `GENERIC_ALL` (`GA`).
  - Every existing test that compares `serde_json` text keeps passing with `preserve_order` (Step 4 runs the whole workspace).
- [ ] **Step 2: Run them.**

Run: `cargo nextest run -p ww-platform`
Expected: FAIL. Nothing is implemented.

- [ ] **Step 3: Implement it.**
  - Make the four root `Cargo.toml` edits, with the exact versions of Global Constraints: `proptest = { version = "1.12.0", default-features = false, features = ["std"] }`, `toml_edit = "0.22.27"`, `serde_json = { version = "1", features = ["preserve_order"] }`, and `[profile.dev.package.libsqlite3-sys] opt-level = 3` beside the `sha2` entry. These are exact pins: a different version needs a plan change, not an implementer's choice.
  - `copy_new`, `attributes`, `long_name` and `terminate_self` need no new `windows` feature: `CopyFile2`, `GetFileAttributesW` and `GetLongPathNameW` are in `Win32_Storage_FileSystem`, and `TerminateProcess` and `GetCurrentProcess` in `Win32_System_Threading`, both already enabled.
  - `volume_info`'s sync-root check calls `CfGetSyncRootInfoByPath` and maps the result through `sync_root_from`. Outside a sync root the call fails with `HRESULT_FROM_WIN32(ERROR_CLOUD_FILE_NOT_UNDER_SYNC_ROOT)` (390 in `windows` 0.62.2), which means `false`. On a volume without the cloud filter (FAT and exFAT USB sticks, SMB shares) the call may fail with other codes, so any failure on a non-NTFS or remote volume also means `false`; an error is returned only for local NTFS. A caller that still gets an error from `volume_info` treats the volume as having no stable IDs (Tasks 8 and 10).
  - Replace the comparison normalisers with `folder_key`: `ww-core/src/identity.rs`'s `normalise` and `ww-client/src/connect.rs`'s `folder_key`; `connect.rs`'s existing `folder_keys_ignore_case_prefix_and_trailing_separator` (which pins `Z:\` → `z:\`) keeps passing unchanged. Move `env.rs`'s private `strip_verbatim` to `paths.rs` as the public `strip_verbatim`, and use it in `env.rs` and in `ww-core/src/workspace.rs`'s `id_of`, which keeps the path's case: `WorkspaceRef.path` and `hello.workspace.path` are never lowercased, so `default_workspace_id_resolves` keeps passing. `normalise_for_hash` stays: the workspace hash keeps its plan-1 ASCII-lowercase contract (QA-038).
  - Every `unsafe` block gets a `// SAFETY:` comment.
- [ ] **Step 4: Run the whole workspace**, the boundary test and the licence check.

Run: `cargo nextest run --workspace --exclude ww-spec-tests && cargo nextest run -p ww-testkit --test dependency_boundaries && cargo deny check && cargo run -q -p ww-cli -- dev spec-tests`
Expected: PASS.
- The report lists the pinned versions and the `cargo deny check` licence result, and states that `schema_hash` is unchanged (`canonical_json` sorts keys, so `preserve_order` doesn't affect it).
- Any test that compared `serde_json` output as text is fixed to compare values, and listed in the report.

- [ ] **Step 5: Commit.**

```bash
git add Cargo.toml Cargo.lock crates/ww-platform crates/ww-core/src/identity.rs crates/ww-core/src/workspace.rs crates/ww-client/src/connect.rs crates/ww-testkit/src/ids.rs
git add <each test file Step 4 fixed, as listed in the report>
git commit -m "platform: replace/move with write-through, file identity, volume facts, locks, long paths, folder_key; deps proptest, toml_edit"
```

---

### Task 3: The workspace format in `ww-schema` (ARCH-042, ARCH-076, ARCH-077, CORE-031, CORE-033, CORE-040, CORE-047, CORE-048, CORE-148, QA-044, QA-048) — **protected: `crates/ww-spec-tests/fixtures/.gitattributes` (new)**

**Files:**
- Modify: `crates/ww-schema/Cargo.toml` (`serde_json`, `yaml-rust2`, `toml_edit`, `sha2`; dev: `proptest`).
- Create: `crates/ww-schema/src/{manifest.rs, refs.rs, status.rs, version.rs, content_hash.rs, paths.rs, yaml_edit.rs, workspace_yaml.rs, suite_edit.rs, logs.rs, time.rs}`. Modify `src/lib.rs`.
- Create: `crates/ww-schema/clippy.toml`, with `disallowed-methods` entries for `serde_json::Map::remove` and `serde_json::Map::remove_entry`. With `preserve_order` they behave like `swap_remove` (serde_json's own docs), which moves the last key into the removed slot; `Manifest::remove` and `manifest::remove_key` use `shift_remove` instead.
- Create (free additions, `add-ok`): the synthetic fixture workspace `crates/ww-spec-tests/fixtures/sample-workspace/**` (below).
- Create (protected, new): `crates/ww-spec-tests/fixtures/.gitattributes` containing `* -text`.
  - The root `.gitattributes` is `* text=auto` (plus `*.md text eol=lf`), and Git for Windows sets `core.autocrlf=true` system-wide, as `actions/checkout` on Windows runners does. Without the nested file, the JSON, YAML, `progress.md` and `.txt` fixtures would be checked out with CRLF, and every golden and recorded-hash comparison would fail.
  - The nested file overrides the root one for everything under `fixtures/` (`eol` only takes effect where `text` is set), so the bytes on disk are the committed bytes. The root `.gitattributes` stays unchanged. The guard's `.gitattributes` pattern matches at any depth and isn't `add-ok`, so this addition needs the owner's approval with the others (Global Constraints).
- Test: unit tests in each file; `crates/ww-schema/tests/golden.rs`; `crates/ww-schema/tests/props.rs` (proptest, `cases` from `test-params.toml` `[proptest]`, read with the crate's existing `toml` dependency).

**Interfaces:**

Produces:

```rust
// manifest.rs: the ordered map is the truth; typed views read it, setters edit it in place
pub struct Manifest { /* serde_json::Map with preserve_order */ }
impl Manifest {
    pub fn parse(text: &str) -> Result<(Manifest, Vec<SchemaWarning>), serde_json::Error>;  // schema violations are warnings
    pub fn new(project: &str, asset_id: &str, asset_type: &str, creation_uuid: &str) -> Manifest; // schema_version 1, status "planned", version "1.0.0"
    pub fn to_text(&self) -> String;                       // serde_json pretty (2 spaces) + the line ending: the one parse() saw
                                                           // ("\r\n" when the text's first line break is CRLF), else "\n"
    pub fn view(&self) -> ManifestView;                    // typed read
    pub fn get(&self, key: &str) -> Option<&serde_json::Value>;
    pub fn set(&mut self, key: &str, value: serde_json::Value);           // existing key keeps its position, else appended
    pub fn runs_mut(&mut self) -> Vec<&mut serde_json::Map<String, serde_json::Value>>;
    pub fn push(&mut self, list_key: &str, value: serde_json::Value);    // creates the list at the end if missing
    pub fn remove(&mut self, key: &str) -> Option<serde_json::Value>;    // shift_remove: the keys after it keep their order
}
pub fn remove_key(map: &mut serde_json::Map<String, serde_json::Value>, key: &str) -> Option<serde_json::Value>; // shift_remove, for nested maps
// No setter adds `schema_version`: a manifest that lacks it never gains it (CORE-148, minimal diff).
pub struct ManifestView { pub schema_version: Option<u32>, pub asset_id: String, pub project: String, pub asset_type: Option<String>,
    pub status: Option<Status>, pub version: Option<Version>, pub approved_version: Option<Version>,
    pub creation_uuid: Option<String>, pub inputs: Vec<Input>, pub pinned_inputs: Vec<PinnedInput>,
    pub candidate_runs: Vec<CandidateRun>, pub hashes: BTreeMap<String, String>, pub locks: Vec<String>,
    pub human_edits: Vec<HumanEdit>, pub external: Vec<ExternalFile>, pub has_working_doc: bool }
    // hashes: key = path relative to the asset folder; value = bare 64-hex, read from "sha256:<hex>" or "<hex>"
pub struct Input { pub r#ref: String, pub role: Option<String>, pub sha256: Option<String> }
pub struct PinnedInput { pub r#ref: String, pub reason: Option<String>, pub by: Option<String>, pub date: Option<String> }
pub struct CandidateRun { pub run_id: String, pub path: Option<String>, pub status: Option<String> /* open | selected | rejected */,
                          pub created: Option<String>, pub target_version: Option<Version>, pub files: Vec<CandidateFile>,
                          pub selected: Vec<String>, pub signed_off: Option<SignedOff>, pub pinned: bool,
                          pub trashed_utc: Option<String> }
pub struct CandidateFile { pub path: String /* asset-relative, I5 */, pub seed: Option<i64>, pub role: String /* candidate | derived | repair */,
                           pub from: Option<String>, pub sha256: Option<String> /* bare hex; absent in pre-2b runs */ }
pub struct SignedOff { pub by: String, pub date: String, pub version: Version }
pub struct HumanEdit { pub path: String, pub sha256: String, pub previous_sha256: Option<String>, pub date: Option<String> }
    // I5's author and note are read when present and otherwise ignored; the core writes path, sha256, previous_sha256, date
pub struct ExternalFile { pub path: String, pub sha256: String }
pub fn bare_sha256(value: &str) -> Option<&str>;   // "sha256:<64 hex>" or "<64 hex>" → the hex; anything else → None
pub fn hash_value(hex: &str) -> String;            // "sha256:<hex>", the form the core writes into `hashes`
// Every view field is read leniently: a wrong type is a SchemaWarning and the field's default, never a parse error.
pub struct SchemaWarning { pub pointer: String, pub message: String }
// version.rs
pub struct Version { pub major: u64, pub minor: u64, pub patch: u64 }   // FromStr "^\d+\.\d+\.\d+$", Display, Ord
pub enum Bump { Major, Minor, Patch }
impl Version { pub fn bump(self, b: Bump) -> Version; }
// refs.rs
pub enum LinkRef { Local { asset: String, version: Version }, Project { slug: String, asset: String, version: Version },
                   Library { item: String, version: Version }, Style(u32) }
pub enum AssetRef { Sealed { ws: String, project: String, asset: String, version: Version, sha256: Option<String> },
                    Candidate { ws: String, project: String, asset: String, run: String, file: String } }
impl FromStr for LinkRef; impl FromStr for AssetRef; impl Display for both;
impl AssetRef { pub fn live_topic(&self) -> String; }  // "asset:ww://<ws>/<project>/<asset>"
pub fn is_slug(s: &str) -> bool; pub fn is_asset_id(s: &str) -> bool; pub fn is_run_id(s: &str) -> bool;
// content_hash.rs
pub fn version_content_hash(files: &[(String /* rel, '/' */, String /* sha256 hex */)]) -> String;  // excludes source/** and manifest.snapshot.json
// status.rs
pub enum Status { Planned, InProgress, Candidate, InReview, Approved, Exported, Released, Stale, Blocked, Deprecated }
pub enum StatusEvent { StartWork, CandidateRun, RequestSignoff, Signoff, Export, Release, MarkStale, Restore, Block(String), Unblock, Deprecate }
pub struct Transition { pub to: Status, pub set_before_stale: Option<Status>, pub set_before_blocked: Option<Status>, pub reason: Option<String> }
pub fn transition(from: Status, before_stale: Option<Status>, before_blocked: Option<Status>, ev: &StatusEvent) -> Result<Transition, String>;
// paths.rs
pub const PATH_BUDGET: usize = 180;
pub fn check_rel_path(rel: &str) -> Result<(), PathProblem>;   // Global Constraints rules
pub enum PathProblem { TooLong(usize), Reserved(String), BadComponent(String), NotRelative }
// yaml_edit.rs
pub enum YamlEdit { SetScalar { path: Vec<String>, value: String }, AppendToList { path: Vec<String>, item: String } }
pub fn apply(text: &str, edits: &[YamlEdit]) -> Result<String, YamlEditError>;   // line edits; re-parsed with yaml-rust2;
                                                                                 // new lines use the file's line ending.
    // A missing top-level key is appended at the end of the document (`key: value`, or `key: [item]` for AppendToList);
    // a missing nested key is Unsupported
pub enum YamlEditError { Unsupported(String), Invalid(String) }
// workspace_yaml.rs
pub struct WorkspaceYaml { pub name: Option<String>, pub defaults: serde_json::Map<String, serde_json::Value> }
pub struct ProjectYaml { pub name: Option<String>, pub slug: Option<String>, pub status: Option<String>, pub modules: Vec<String>,
                         pub links: Vec<String>, pub pipeline: serde_json::Map<String, serde_json::Value>,
                         pub overrides: serde_json::Map<String, serde_json::Value> }
    // overrides is not a YAML key: it is extracted from project.yaml's top-level cascade keys (I4; Global Constraints
    // "Settings cascade"): models, providers, qa_profile, modules, auto_candidate_on_save, and pipeline.skip_stages /
    // pipeline.auto_approve stored as skip_stages / auto_approve. WorkspaceYaml.defaults is extracted the same way
pub const CASCADE_KEYS: &[&str] = &["models", "providers", "qa_profile", "modules", "auto_candidate_on_save", "skip_stages", "auto_approve"];
pub fn read_workspace_yaml(text: &str) -> Result<WorkspaceYaml, String>; pub fn read_project_yaml(text: &str) -> Result<ProjectYaml, String>;
pub fn new_workspace_yaml(name: &str, created: &str) -> String;
pub fn new_project_yaml(slug: &str, name: &str, kind: &str, created: &str) -> String;
    // name, slug, type, `status: active`, `links: []`, created, and one comment line, so set_status and pin_input
    // edit keys that already exist
pub const PROGRESS_TEMPLATE: &str;                       // "# <name>\n\n## Done\n\n## Next\n\n## Open questions\n"
// suite_edit.rs (toml_edit)
pub fn register(text: &str, path: &str, make_default: bool) -> Result<String, String>;   // keeps comments, order and unknown keys
// logs.rs
pub fn encode_line(machine: &str, seq: u64, record: &serde_json::Map<String, serde_json::Value>) -> String;  // machine, seq first; "\n"
pub struct LogLine { pub machine: String, pub seq: u64, pub record: serde_json::Value }
pub fn read_lines(text: &str) -> Vec<LogLine>;          // skips unparsable lines; dedupes (machine, seq), first wins
pub fn last_seq(text: &str, machine: &str) -> u64;
// time.rs (pure, proleptic Gregorian, UTC)
pub fn date(unix_secs: i64) -> String; pub fn rfc3339(unix_secs: i64) -> String; pub fn year_month(unix_secs: i64) -> String;
pub fn compact(unix_secs: i64) -> String;                // YYYYMMDDTHHMMSSZ
pub fn run_id(unix_secs: i64) -> String;                 // r_<yyyymmdd>_<hhmm>
```

**Status transitions** (CORE-048). Every pair not listed is `Err` → `status.invalid_transition`.

| From | Event | To | Notes |
|---|---|---|---|
| `planned` | `StartWork` | `in_progress` | |
| any but `blocked`, `deprecated` | `CandidateRun` | `candidate` | new work on an approved or later asset starts a new version |
| `in_progress`, `candidate` | `RequestSignoff` | `in_review` | CORE-053 |
| `in_progress`, `candidate`, `in_review` | `Signoff` | `approved` | |
| `approved` | `Export` | `exported` | |
| `exported` | `Release` | `released` | |
| `approved`, `exported`, `released` | `MarkStale` | `stale` | `status_before_stale` = from |
| `stale` | `Restore` | `status_before_stale` | |
| any but `blocked`, `deprecated` | `Block(reason)` | `blocked` | the reason must be non-empty; `status_before_blocked` = from, so `Unblock` always leads out of `blocked` |
| `blocked` | `Unblock` | `status_before_blocked` | |
| any | `Deprecate` | `deprecated` | |

**The synthetic fixture** (owner decision 6). `crates/ww-spec-tests/fixtures/sample-workspace/` contains:
- `worldwright.yaml`, with comments;
- `projects/sketchbook/{project.yaml, progress.md}`; `project.yaml` has a comment line, `status: active` and `links: []`, as `new_project_yaml` writes;
- two asset folders under `projects/sketchbook/assets/`:
  - **`prop_signal_lamp`**, the lantern's structure (CORE-033). A pre-suite `v1.0.0/` folder holds the candidates, the chosen file and a derived file together, and has no `manifest.snapshot.json`. The manifest has `"status": "candidate"` (so `asset.request_signoff` is a valid transition for the spec test `legacy_manifest_edit_is_a_minimal_diff`), no `candidate_runs` and no `schema_version`, but it does have fields the suite no longer uses (`license_scope`, `commercial_use`, `content_rating`, `budget_usd`; CORE-147/148), an unknown nested object and a `null` field.
  - **`chr_paper_crane`**, in the modern I5 shape: a sealed `v1.0.0/` with `manifest.snapshot.json`, `hashes` (values written `sha256:<64 hex>`, as in I5), one `selected` and one open run (each run file with its `sha256`), `inputs` with a `proj:` ref, and `locks`.
- small made-up text and PNG files whose SHA-256 values are the ones recorded in the manifests.

Both manifests are stored in `Manifest::to_text` format, with `\n` line endings; no text fixture file (`.json`, `.yaml`, `.yml`, `.toml`, `.md`, `.txt`) contains `\r\n`. Binary fixtures are excluded from that rule: every PNG signature contains the bytes `0D 0A`.

- [ ] **Step 1: Write the failing tests.**
  - **`tests/golden.rs`** (ARCH-076, ARCH-077, CORE-148, QA-048):
    - `golden_manifests_round_trip_byte_for_byte`: parse then `to_text` equals the fixture bytes, for both manifests;
    - `one_field_change_is_a_one_line_diff`: setting `status` changes exactly one line;
    - `unknown_and_retired_fields_survive_edits`: `license_scope`, the nested unknown object and the `null` field are still present and in place after `set` and `push`;
    - `hand_formatted_manifest_round_trips_by_value_and_order`: a compact one-line copy parses and keeps its key order;
    - `legacy_manifest_reads_with_warnings_not_errors`: `prop_signal_lamp` gives a `SchemaWarning` for the missing `schema_version` and parses;
    - `edits_never_add_schema_version_to_a_legacy_manifest`: `set("status", …)` on `prop_signal_lamp`, then `to_text` → no `schema_version` key;
    - `fixture_files_have_no_crlf`: no file under `crates/ww-spec-tests/fixtures/` with one of the text extensions `.json`, `.yaml`, `.yml`, `.toml`, `.md` and `.txt` contains `\r\n` (the nested `.gitattributes` is in force). Other files are skipped, since a PNG signature always contains `0D 0A`; the test also fails if it finds no text file at all, so a wrong path can't make it pass;
    - `crlf_manifest_stays_crlf_and_a_one_field_edit_is_one_line`: a CRLF copy of `chr_paper_crane`'s manifest round-trips byte for byte, and setting `status` changes exactly one line;
    - `removing_a_key_keeps_the_order_of_the_rest` (`Manifest::remove` and `remove_key` of a middle key; the remaining keys keep their relative order).
  - **Unit tests:**
    - `version_parse_order_and_bump` (`1.4.0` minor → `1.5.0`; `1.4` and `v1.4.0` don't parse);
    - `link_refs_round_trip_all_four_forms`;
    - `asset_ref_with_hash_round_trips_and_names_its_live_topic`: `ww://0123456789abcdef/sketchbook/chr_paper_crane@1.0.0#sha256=<64 hex>`, topic `asset:ww://0123456789abcdef/sketchbook/chr_paper_crane`;
    - `candidate_ref_is_view_only_form`;
    - `bad_refs_are_rejected` (uppercase slug, `@1.0`, a 63-hex hash);
    - `content_hash_follows_the_contract`: a hand-built two-line text hashed independently equals `version_content_hash`, and `source/a.png` and `manifest.snapshot.json` are ignored;
    - `status_transitions_follow_the_table` (every row, plus four refusals: `planned` → `Signoff`, `deprecated` → anything but `Deprecate`, `Block("")`, and `blocked` → `Block("again")`, CORE-048);
    - `hashes_read_with_or_without_prefix_and_are_written_prefixed` (`bare_sha256` accepts `sha256:<hex>` and `<hex>`, refuses 63 hex and other prefixes; `hash_value` writes `sha256:<hex>`; `ManifestView.hashes` of the fixture holds bare hex);
    - `view_types_read_the_i5_run_shape` (the fixture's runs, files with `seed`, `role`, `from` and `sha256`, `signed_off`, `pinned_inputs`, `human_edits` with I5's `author`/`note` and with plan 2b's `previous_sha256`/`date`; a wrong type is a warning, not an error);
    - `project_layer_is_the_top_level_keys` (`read_project_yaml` of an I4 file gives `overrides` with `models`, `modules`, `skip_stages` and `auto_approve` from `pipeline:`, and not `name`, `links` or `created`; `read_workspace_yaml` reads `defaults:` the same way);
    - `new_project_yaml_has_status_links_and_a_comment` (it parses, has `status: active`, `links: []` and a `#` line, and a `SetScalar` of `status` and an `AppendToList` of `links` both apply);
    - `path_budget_is_180` (180 is ok, 181 is `TooLong`);
    - `reserved_names_are_refused_in_any_case_and_extension` (`con.png`, `Lpt1`, `com¹.txt`, `nul.tar.gz`);
    - `trailing_dot_space_and_bad_chars_are_refused`;
    - `yaml_set_scalar_keeps_comments_and_order`;
    - `yaml_edit_keeps_crlf_line_endings`;
    - `yaml_append_to_block_and_flow_lists` (including the empty flow list `links: []`, and a missing top-level `links`, which is appended as `links: [proj:harbor]`);
    - `yaml_edit_refuses_anchor_and_flow_mapping_targets` (the text is unchanged and the error is `Unsupported`);
    - `suite_register_keeps_comments_and_unknown_tables` (a `# note` line and `[caps] monthly_usd = 50` survive; a second register is a no-op);
    - `log_lines_put_machine_and_seq_first`;
    - `reader_skips_a_torn_last_line_and_dedupes`;
    - `utc_dates_cover_leap_years_and_month_ends` (`2028-02-29`, `1999-12-31T23:59:59Z`);
    - `run_ids_use_utc_minutes`.
  - **`tests/props.rs`** (QA-044): `any_json_object_round_trips_by_order_and_value`, and `edits_never_reorder_existing_keys`, whose operation sequences mix `set`, `push` and `remove` (a removal leaves the other keys in their relative order).
- [ ] **Step 2: Run them.**

Run: `cargo nextest run -p ww-schema`
Expected: FAIL. Nothing is implemented.

- [ ] **Step 3: Implement it**, and add the fixture files.
- [ ] **Step 4: Run them, and the boundary test.**

Run: `cargo nextest run -p ww-schema && cargo nextest run -p ww-testkit --test dependency_boundaries && cargo clippy -p ww-schema --all-targets -- -D warnings`
Expected: PASS. Then the deliberate-violation check: in a scratch copy, a `serde_json::Map::remove` call in `manifest.rs` must make clippy fail; record the output and remove the line.

- [ ] **Step 5: Commit.**

```bash
git add crates/ww-schema crates/ww-spec-tests/fixtures crates/ww-spec-tests/fixtures/.gitattributes Cargo.lock
git commit -m "schema: lossless manifests, ww:// refs with content hashes, status machine, targeted YAML/TOML edits, synthetic golden"
```

---

### Task 4: `ww-testkit`: fixture workspaces, fail points, handle holder, git-ops simulator, fence and hygiene (QA-031, QA-046, QA-055, QA-066)

**Files:**
- No dependency change: `registered()` parses `suite.toml` with the kit's own `toml`, like `register_workspace`, and nothing in the kit uses `ww-schema`, so spec tests never lean on the code they test (QA-018).
- Create: `crates/ww-testkit/src/{workspace.rs, handles.rs, gitops.rs, failpoints.rs, params.rs, utc.rs}`. Modify `src/{lib.rs, env.rs, raw_pipe.rs, bins.rs, files.rs}`.
  - `env.rs`: `child_env()` appends `git_env()`, so every `ww` and core a test spawns, and every `git` they run, is isolated from the system and user git config (QA-038). Without it, the core's `git config core.autocrlf` on a developer machine or CI runner whose system git config sets `core.autocrlf=true` would answer `true`, and the doctor spec test would get a prompt instead of "unset".
  - `utc.rs`: date helpers for spec tests, written independently of `ww-schema::time` (QA-018), so no spec test re-implements date math and `crates/ww-spec-tests/src/lib.rs` (an existing file under `add-ok`, so a change needs approval) stays unchanged.
  - `files.rs`: `write_file`, so `ww-core` tests create fixture files without `std::fs` in `crates/ww-core/src` (QA-024, `single_write_allowance.rs`).
- Modify: `crates/ww-cli/tests/user_cli.rs`, the one caller of `ww_spawn`: its `Spawned` helper wraps `WwChild`, so `events_follow_prints_ndjson` drains its child's stderr and shows it on failure, and the workspace still builds after this task.
- Test: unit tests in the new files.

**Interfaces:**

Produces:

```rust
impl TestEnv {
    pub fn copy_fixture_workspace(&self, name: &str) -> PathBuf;   // sample-workspace → <root>\ws-<name>
    pub fn workspace_id(&self, ws: &Path) -> String;               // Env::workspace_hash(canonicalize(ws))
    pub fn workspace_local_dir(&self, ws: &Path) -> PathBuf;
    pub fn registered(&self) -> (Option<PathBuf>, Vec<PathBuf>);  // parsed from this channel's suite.toml
    pub fn git_env(&self) -> Vec<(OsString, OsString)>;            // GIT_CONFIG_NOSYSTEM=1; HOME and XDG_CONFIG_HOME =
                                                                   // <root>\profile; GIT_CONFIG_GLOBAL = <root>\profile\.gitconfig
    pub fn child_env(&self) -> Vec<(OsString, OsString)>;          // plan 2a's three variables, then git_env()
    pub fn git_init(&self, ws: &Path);                             // git init + one commit, with git_env(); no remote
    pub fn git(&self, ws: &Path, args: &[&str]) -> String;         // same isolation; panics on failure
    pub fn ww_spawn(&self, args: &[&str]) -> WwChild;              // now a wrapper: Drop kills and waits
}
pub struct WwChild; impl WwChild { pub fn stdout_lines(&mut self) -> &mut dyn Iterator<Item = String>;
                                   pub fn stderr_so_far(&self) -> String; }   // stderr drained on a thread
pub fn sha256_bytes(b: &[u8]) -> String;
pub mod files { pub fn write_file(path: &Path, bytes: &[u8]); }   // creates parent folders; panics on failure
pub mod utc {        // UTC, proleptic Gregorian, from the system clock; independent of ww-schema (QA-018)
    pub fn today() -> String;                          // YYYY-MM-DD
    pub fn year_month() -> String;                     // YYYY-MM
    pub fn compact_now_pattern() -> impl Fn(&str) -> bool;
        // accepts YYYYMMDDTHHMMSSZ whose date is today's or yesterday's UTC date (a call that crosses midnight)
    pub fn parse_rfc3339(s: &str) -> i64;              // "…Z" → unix seconds; panics on anything else
}
// Tests that check "today" take today() before and after the call and accept either value.
pub fn version_content_hash_of(dir: &Path) -> String;             // the Appendix E rule, written independently of ww-schema's
pub struct HandleHolder; impl HandleHolder { pub fn hold(path: &Path, share: ww_platform::fs::Share) -> HandleHolder;
                                             pub fn release(self); }   // Defender- or viewer-style open handle
pub mod failpoints { pub const ENV: &str = "WW_TEST_FAILPOINTS"; pub fn spec(points: &[(&str, &str)]) -> String; }
pub mod params {     // reads the workspace-root test-params.toml with the kit's own toml (QA-019); panics on a missing key
    pub fn proptest_cases() -> u32;            // [proptest] cases
    pub fn budget(key: &str) -> f64;           // [budgets] <key>, in seconds
}
pub struct GitOps;    // simulates `git checkout`: holds .git\index.lock while it rewrites files, then moves HEAD
impl GitOps { pub fn begin(ws: &Path) -> GitOps; pub fn write(&self, rel: &str, bytes: &[u8]); pub fn remove(&self, rel: &str);
              pub fn finish(self); }
impl RawPipe {
    pub fn fence(&mut self, ws: &Path);                            // writes <ws>\.worldwright\fence\<n>, then test.watch_fence {name: n}
    pub fn tool_ok(&mut self, name: &str, args: Value, key: Option<&str>) -> Value;   // panics unless status ok; returns structured
}
```

**Hygiene:**
- `ensure_built` checks that `ww.exe` and `ww-core.exe` exist even when the setup-script flag says they were built, and builds if not.
- `TestEnv`'s `Drop` probes once with `open_client`: `NotFound` returns at once; only a found pipe gets hello, `core/drain` and the 5 s wait.
- `RawPipe::call` returns at once on a response whose `id` is `null` (a parse error), instead of waiting 30 s for its own id.
- `ww_spawn` returns `WwChild`, which kills its child on drop and drains its stderr on a thread; panics include that stderr.

- [ ] **Step 1: Write the failing unit tests:**
  - `fixture_workspace_copies_with_recorded_hashes` (every `hashes` value, with its `sha256:` prefix stripped, matches `sha256_file`, and so does every run file's `sha256`);
  - `workspace_id_matches_platform_hash`;
  - `git_init_is_isolated_from_user_config` (`git config --show-origin user.name` names only the test profile);
  - `child_env_carries_the_git_isolation` (`GIT_CONFIG_NOSYSTEM`, `HOME`, `XDG_CONFIG_HOME` and `GIT_CONFIG_GLOBAL` each appear once, under `<root>\profile`; a `git config --show-origin core.autocrlf` run with only `child_env()` prints nothing and exits 1, whatever the machine's system config says);
  - `utc_helpers_agree_with_each_other` (`today()` starts `year_month()`; `parse_rfc3339("2028-02-29T00:00:00Z") - parse_rfc3339("2028-01-30T00:00:00Z") == 30 * 86400`; `compact_now_pattern()` accepts a suffix built from today's date and refuses `20261016T1200Z`);
  - `write_file_creates_parents`;
  - `handle_holder_blocks_replace_until_released`;
  - `gitops_holds_index_lock_until_finish`;
  - `failpoint_spec_formats_pairs`;
  - `params_read_proptest_cases_and_panic_on_a_missing_budget` (`proptest_cases()` is 256 today; `budget("no_such_key")` panics naming the key and the file);
  - `version_content_hash_of_ignores_source_and_snapshot`;
  - `drop_without_a_core_returns_without_waiting` (a fresh `TestEnv`; dropping it makes no hello attempt, checked through a counter in the drop path, not a timer);
  - plan 2a's `child_env_names_test_channel_root_and_parent` keeps passing (it looks the three variables up by name, so the appended git variables don't disturb it);
  - `call_returns_on_a_null_id_error` (a fake pipe server answers `{nope` with -32700);
  - `ww_child_is_killed_on_drop`;
  - `ensure_built_rebuilds_when_flagged_but_missing` (pure helper `needs_build(flag, exists) -> bool`).
- [ ] **Step 2: Run them.**

Run: `cargo nextest run -p ww-testkit`
Expected: FAIL. Nothing is implemented.

- [ ] **Step 3: Implement it.**
- [ ] **Step 4: Run them again.**

Run: `cargo nextest run -p ww-testkit -p ww-cli && cargo clippy -p ww-testkit -p ww-cli --all-targets -- -D warnings`
Expected: PASS.

- [ ] **Step 5: Commit.**

```bash
git add crates/ww-testkit crates/ww-cli/tests/user_cli.rs Cargo.lock
git commit -m "testkit: fixture workspaces, handle holder, git-ops simulator, fail points, watcher fence; faster drop and null-id replies"
```

---

### Task 5: Protected spec tests for plan 2b, expected to fail (QA-017, QA-018, QA-021) — **protected: `crates/ww-spec-tests/tests/core_session.rs`, `crates/ww-spec-tests/tests/cli_basics.rs`**

A test author who doesn't read the implementation writes these from the spec (with Appendices D and E), Global Constraints and the Interfaces blocks above only. They drive only stable surfaces (QA-018): `RawPipe` tool and RPC calls (including the test-channel tools of Global Constraints), the `ww` CLI through `TestEnv::ww`, and the workspace files themselves, read as an outside program would. Each test cites its rules with `// spec:`.

**Files:**
- Create (free additions): `crates/ww-spec-tests/tests/{ws_tools.rs, store_writes.rs, store_order.rs, versions_trash.rs, index_watch.rs, projects_settings.rs, asset_record.rs, doctor.rs}`.
- Modify: `crates/ww-spec-tests/expected-fail.toml`. Add one entry per new test, in plan 2a's form: `{ name = "ww-spec-tests::<file>::<test>", rule = "<first rule>", file = "tests/<file>.rs" }`.
- Modify (protected): two plan-2a tightenings.
  - `core_session.rs`: `drain_is_the_only_call_after_incompatible_hello` asserts `wait_exit(DEADLINE) == Some(0)`, not `is_some()`.
  - `cli_basics.rs`: `status_no_start_reports_not_running` also asserts that `env.env.suite_state_db()` doesn't exist afterwards (a started core always creates it).
- No change to `crates/ww-spec-tests/Cargo.toml`: everything comes through `ww-testkit`, `ww-protocol` and `serde_json`.

**Interfaces:**
- Consumes: Task 4's testkit API (including `ww_testkit::utc` for every date, month and set-aside suffix, and the git isolation that `child_env()` carries into every `ww` and core), the Global Constraints contracts and test-channel tools, and the fixture of Task 3. A test never edits the fixture under `crates/ww-spec-tests/fixtures/`: it adds what it needs to its own copy from `copy_fixture_workspace`.
- Produces: the scenario tests that Tasks 7–14 turn green; each task removes its entries. The last column says which task.

- [ ] **Step 1: Write the tests.**

  **`ws_tools.rs`**

  | Test | Rules | Assertion | Green in |
  |---|---|---|---|
  | `workspace_init_writes_layout_and_registers_default` | CORE-007, ARCH-098 | `ww workspace init "<root>\Ünïcödé space ws"` → exit 0; `worldwright.yaml`, `.gitignore` (all five entries), `.gitattributes` (with the line `projects/** -text`), `projects/sketchbook/project.yaml` and `progress.md` exist; `registered().0` is that path; `ww status --json` `hello.workspace.path` equals it | 8 |
  | `workspace_use_switches_default_and_old_sessions_get_unsupported` | ARCH-090 | session A says hello with workspace 1; `ww workspace use <ws2>` → exit 0; A's next `test.store_write` → `workspace.unsupported` naming ws2; a new hello with ws2's id is ok | 8 |
  | `unsupported_workspace_error_carries_set_default_hint` | QA-003, ARCH-090 | a hello naming a registered non-default workspace → `error.fix.agent_hint.tool == "workspace.set_default"`, `arguments.path` = that path | 8 |
  | `suite_toml_edits_keep_comments_and_unknown_keys` | CORE-007, ARCH-076 | `suite.toml` holding `# my note` and `[caps]\nmonthly_usd = 50` before `ww workspace init`; afterwards both lines are unchanged, and the file parses with the new entry | 8 |

  **`store_writes.rs`**

  | Test | Rules | Assertion | Green in |
  |---|---|---|---|
  | `outside_edit_is_never_overwritten` | ARCH-074, ARCH-073 | `test.store_write {path:"notes/a.txt", text:"core", expect:"new"}` → ok; the test writes "mine" to the file; `test.store_write {…, text:"core 2", expect:"generated"}` → `store.hash_conflict`; the file reads "mine" | 7 |
  | `failed_write_leaves_no_half_written_file` | QA-011, ARCH-074 | core started with `store.write_temp=disk_full`; a write over an existing file → `store.disk_full`; the file's SHA-256 is unchanged and no `*.ww-tmp-*` file exists anywhere under the workspace | 7 |
  | `relative_path_over_budget_is_refused` | CORE-040 | a 181-character path → `store.path_invalid` and nothing exists under its first folder; `con.txt` → `store.path_invalid`; a 180-character path → ok | 7 |
  | `registry_ledger_csv_is_never_written` | ARCH-082 | the test creates `registry/ledger.csv` in its workspace copy (the Task 3 fixture has none); writing to it → `store.path_invalid`; its bytes are unchanged | 7 |

  **`store_order.rs`**

  | Test | Rules | Assertion | Green in |
  |---|---|---|---|
  | `crash_after_output_lists_orphan_and_keeps_files` | ARCH-075, CORE-038, QA-055 | core with `store.after_output=abort`; `test.ingest_run` → the core exits within 30 s; the run's files exist under `candidates/<run>/`; a restarted core's `project.status` lists that folder in `orphans`; the files still exist | 12 |
  | `append_log_lines_are_per_machine_per_month_with_seq` | ARCH-081, CORE-009 | two `test.ingest_run {job_line:true}` → exactly one `queue/jobs.testmachine.<yyyy-mm>.jsonl` (`<yyyy-mm>` from `utc::year_month()`); its lines parse with `machine == "testmachine"` and `seq` 1 and 2, `machine` and `seq` being the first two keys | 7 |
  | `torn_last_line_is_closed_before_the_next_append` | ARCH-081 | append `{"seq":` (no newline) to that file; one more `test.ingest_run` → the fragment is alone on its line, and the last line parses with `seq` 3 | 7 |
  | `staging_handle_is_in_workspace_and_ingest_moves_files` | ARCH-080, ARCH-033, CORE-003 | `test.ingest_run` → `staging_dir` starts with `<ws>\.worldwright\staging\` and no longer exists; the files are under `projects/sketchbook/assets/<id>/candidates/<run_id>/`; the manifest's `candidate_runs` has `run_id` | 7 |
  | `second_channel_core_gets_read_only_workspace` | ARCH-028 | two `TestEnv`s register the same workspace as default; the first core serves it; the second core's `test.store_write` → `workspace.read_only` | 7 |

  **`versions_trash.rs`**

  | Test | Rules | Assertion | Green in |
  |---|---|---|---|
  | `signoff_seals_read_only_version_with_snapshot` | CORE-026, CORE-051 | `asset.import` (copy) then `asset.signoff` without `bump` → `v1.0.0/` holds the file and `manifest.snapshot.json`, both read-only; manifest `approved_version == "1.0.0"`, `status == "approved"`, the run `selected`, `signed_off.by == "cli"` | 13 |
  | `signoff_uses_the_confirmed_bump` | CORE-027 | after `1.0.0`, `asset.get` `next_versions.minor == "1.1.0"`; a new run, then `signoff {bump:"minor"}` → `v1.1.0/` | 13 |
  | `sealed_ref_carries_the_content_hash` | ARCH-042, CORE-031, CORE-047 | the `signoff` `ref` is `ww://<id>/sketchbook/<asset>@1.0.0#sha256=<h>` with `h == version_content_hash_of(v1.0.0)` | 13 |
  | `link_external_asset_cannot_be_signed_off_until_collected` | CORE-029 | `asset.import {mode:"link_external"}` of a file outside the workspace, then `signoff` → `asset.link_external` with `fix.agent_hint.tool == "asset.collect"`; `asset.collect`, then `signoff` → ok and the version is inside the workspace | 13 |
  | `trash_keeps_restore_record_and_restores` | ARCH-058, CORE-037, CORE-110 | `test.trash {path}` → the file is gone; `trash.list` has an entry with `original_path` and a `purge_after` 30 days after `moved_utc` (`utc::parse_rfc3339` of both differ by `30 * 86400`); the folder is `.trash\<YYYY-MM-DD>\…` with `utc::today()`; `trash.restore` puts the bytes back | 9 |

  **`index_watch.rs`** (each watcher assertion follows a `RawPipe::fence`)

  | Test | Rules | Assertion | Green in |
  |---|---|---|---|
  | `damaged_index_is_rebuilt` | ARCH-089 | stop the core; write 4 KiB of `0xAB` over `workspace_local_dir(ws)\index.db`; restart → the first `assets.query` (read tools wait for the first reconcile, Appendix E) lists both fixture assets | 10 |
  | `outside_edit_to_sealed_file_is_a_human_edit` | CORE-034, ARCH-079 | clear the read-only bit of a `chr_paper_crane/v1.0.0/` file and change it; fence → `asset.get` `human_edits` has that path with the new SHA-256; the file keeps the edited bytes | 11 |
  | `invalid_manifest_is_marked_needs_repair_and_kept` | ARCH-079, ARCH-076 | write `{` into a manifest; fence → `asset.get` `needs_repair == true`; the file is still exactly `{` | 11 |
  | `writes_wait_while_git_holds_index_lock` | ARCH-083 | in this order: subscribe to `workspace`; create `.git\index.lock`; `RawPipe::fence`; wait for the `git_paused` event; only then `send_line` a `test.store_write` (no response yet); remove the lock → `git_resumed` arrives before the write's ok response | 11 |
  | `lfs_pointer_is_not_fetched_not_corrupt` | ARCH-085 | replace a sealed file with an LFS pointer whose `oid` is its recorded SHA-256; fence → `asset.get` `files[].state == "not_fetched"`, no human edit | 11 |
  | `ww_notify_reconciles_and_is_silent_without_core` | ARCH-016, ARCH-073 | with no core, `ww notify --no-start <file>` → exit 0, empty output; with a core running, an outside edit then `ww notify <file>` (no fence) → `asset.get` shows the human edit | 11 |

  **`projects_settings.rs`**

  | Test | Rules | Assertion | Green in |
  |---|---|---|---|
  | `project_create_status_and_context` | CORE-055 | `project.create {project:"harbor", name:"Harbor"}` → `projects/harbor/project.yaml` and `progress.md`; `project.status` ok; `project.context` lists settings and `suggested_tools`; a second create → `project.exists` | 12 |
  | `settings_cascade_overrides_workspace_then_project_then_request` | CORE-057 | `defaults: {models: {image_generate: a}}` in `worldwright.yaml`, and `models: {image_generate: b}` as a top-level key of `project.yaml` (Global Constraints "Settings cascade"); `test.settings` → `b` from `project`; with `settings {"models.image_generate":"c"}` → `c` from `request`; `{"caps.monthly_usd":1}` → `params.invalid` | 12 |
  | `progress_entries_go_under_done` | CORE-039 | after `project.create`, the line directly after `## Done` in `progress.md` is `- YYYY-MM-DD: …` (`utc::today()`); the blank line before `## Next` and the other sections are byte-equal to the template | 12 |
  | `archived_project_restore_runs_staleness_check` | CORE-055, CORE-048 | the test sets up both assets itself as an outside writer (the fixture promises no approved dependent, and `asset.signoff` is Task 13): it writes an input asset's manifest with `approved_version` `1.0.0` and a sealed `v1.0.0/`, and a dependent's manifest with `status: approved` whose `inputs` names the input `@1.0.0`, then fences; archive `sketchbook`; give the input a newer sealed version on disk (`approved_version` `1.1.0` and `v1.1.0/`) and fence; restore → `stale` lists the dependent's ref, and its manifest `status == "stale"` with `status_before_stale == "approved"` | 12 |
  | `active_project_is_per_session_with_state_json_hint` | ARCH-088 | connection A calls `project.status {project:"harbor"}`; connection B's `project.status {}` → the hint (`harbor`), not A's choice made later; after B names `sketchbook`, A's `project.status {}` is still `harbor`; `.worldwright\state.json` names the last one used | 12 |

  **`asset_record.rs`**

  | Test | Rules | Assertion | Green in |
  |---|---|---|---|
  | `import_creates_candidate_run_with_handoff_and_uuid` | CORE-020, CORE-023, CORE-047 | `asset.import` → `structured.handoff` has `unlocks`, `made_stale` and `refs`; the manifest has `creation_uuid` (UUID v4), one run and status `candidate`; `progress.md` has one new `## Done` line | 13 |
  | `request_signoff_sets_in_review` | CORE-053 | → `status == "in_review"` in both the result and the manifest | 13 |
  | `asset_topic_delivers_events_and_snapshots` | ARCH-044, ARCH-032 | subscribe to the asset's live topic; sign-off → an event with `kind == "version_sealed"`, `version == "1.0.0"`; a second connection subscribing with `since_seq` 999 gets one `snapshot` whose `data.asset_id` matches | 13 |
  | `proj_input_is_recorded_in_project_links` | CORE-019 | `asset.pin_input {ref:"proj:harbor/chr_x@1.0.0"}` (the target needn't exist, Appendix E) → `project.yaml` `links` gains `proj:harbor`, with the file's comments kept | 13 |
  | `legacy_manifest_edit_is_a_minimal_diff` | ARCH-076, ARCH-077, CORE-033, CORE-148 | `asset.get prop_signal_lamp` lists `v1.0.0` as `legacy: true`; `asset.request_signoff` on it (fixture status `candidate`) changes only the `status` line, apart from new keys appended at the end, and adds no `schema_version`; the retired fields are byte-identical | 13 |

  **`doctor.rs`**

  | Test | Rules | Assertion | Green in |
  |---|---|---|---|
  | `doctor_fixes_git_config_and_gitignore_then_changes_nothing` | CORE-040, ARCH-085, QA-060 | `git_init`, then `.gitignore` emptied and `.gitattributes` removed; `ww doctor` → exit 0 with `fixed:` lines; `git config core.longpaths` is `true` and `lfs.setlockablereadonly` is `false`; `core.autocrlf` is still unset (`child_env()` carries the kit's git isolation into `ww`, the core and their `git` calls, so no system or user value is inherited, and doctor neither asks nor sets it); `.gitattributes` holds `projects/** -text`; a second run prints only `ok:` lines and the SHA-256 values of `.gitignore`, `.gitattributes` and `.git\config` are unchanged | 14 |
  | `doctor_sets_damaged_state_db_aside_only_after_yes` | CORE-004 | a corrupt suite `state.db`; `ww status` stderr says "`ww doctor` can set it aside." and not "(plan 2b)"; `ww doctor` with stdin `n` → exit 1, file unchanged; with `y` → `state.db.damaged-<YYYYMMDDTHHMMSSZ>` beside it (the suffix accepted by `utc::compact_now_pattern()`) with the old SHA-256; `ww status` → exit 0. No workspace is registered, so the doctor prints `ok: no default workspace; workspace checks skipped` | 14 |
  | `non_interactive_doctor_never_moves_state_db` | CORE-004 | `ww --non-interactive doctor` → exit 1, the file is unchanged, and stdout says to run `ww doctor` interactively | 14 |

- [ ] **Step 2: Run them and list the failures.**

Run: `cargo run -q -p ww-cli -- dev spec-tests`
Expected: exit 0, `spec tests: 29 passed, 37 expected-fail`. Every new test fails and is listed; the two tightened tests pass.

- [ ] **Step 3: Commit.**

```bash
git add crates/ww-spec-tests
git commit -m "spec-tests: plan-2b workspace data scenarios, expected to fail (QA-017); tighten two plan-2a checks"
```

---

### Task 6: `store` writes: hash check, atomic replace, retries, paths and last generated hashes (ARCH-002, ARCH-074, ARCH-078, ARCH-082, CORE-040, QA-011, QA-024, QA-066)

**Files:**
- Create: `crates/ww-core/src/store/{paths.rs, write.rs, failpoints.rs}`. Modify `store/{mod.rs, fsops.rs}`.
- Modify: `crates/ww-core/clippy.toml`. Add `ww_platform::fs::{replace_file, move_new, copy_new, set_readonly, try_lock_exclusive, try_lock_append, open_shared, flush}` to `disallowed-methods`, with the ARCH-112 reason.
- Modify: `crates/ww-core/Cargo.toml` (not protected): add `sha2.workspace = true`, since the store (and Task 10's index) hash file bytes. The root `Cargo.toml` already has `sha2` and doesn't change.
- Modify: `crates/ww-core/src/state/{mod.rs, migrations.rs}`.
  - Migration 2, both scopes: `CREATE INDEX idempotency_prune ON idempotency(state, created_utc);`, so the prune in `idem_begin` no longer scans the table inside `BEGIN IMMEDIATE`.
  - Migration 3, workspace scope: `generated(path TEXT PRIMARY KEY, sha256 TEXT NOT NULL, written_utc INTEGER NOT NULL)` and `generated_pending(path TEXT PRIMARY KEY, old_sha256 TEXT, new_sha256 TEXT NOT NULL, started_utc INTEGER NOT NULL)` (the pending replace of write step 5).
- Modify: `crates/ww-core/src/config.rs` (`failpoints`).
- Modify: `crates/ww-protocol/src/codes.rs`. Add the 20 plan-2b codes of Global Constraints as `CodeSpec` constants in table order, add them to `ALL`, set `provisional: false` on `STORE_HASH_CONFLICT` and `WORKSPACE_REQUIRED`, and extend the module's table test row by row. Codes aren't wire types, so `schema_hash` doesn't change.
- Test: unit tests in `store/`; `crates/ww-core/tests/store_write.rs` (paused runtime). Tests create fixture files with `ww_testkit::files::write_file`, never with `std::fs` in `crates/ww-core/src` (`single_write_allowance.rs`, QA-024).

**Interfaces:**

Consumes:
- Task 2: `ww_platform::fs`.
- Task 3: `check_rel_path`, `Manifest`, `yaml_edit`.
- Plan 2a: `StateDb`.

Produces:

```rust
// store/failpoints.rs
pub enum FailAction { Abort, DiskFull, Busy, Unsupported, Io }
pub struct FailPoints;
impl FailPoints {
    pub fn none() -> Arc<FailPoints>;
    pub fn parse(spec: &str) -> Result<FailPoints, String>;                  // "name=action[,…]"
    pub fn from_env(env: &Env, var: &dyn Fn(&str) -> Option<String>) -> Arc<FailPoints>; // none unless armed(env.channel(), BUILD_CHANNEL)
    pub fn hit(&self, name: &str) -> io::Result<()>;                         // Abort → ww_platform::process::terminate_self(0xDEAD)
    pub fn on(&self, name: &str, hook: Box<dyn Fn() -> io::Result<()> + Send + Sync>);
        // in-process tests: run code at a point. An Err from the hook is returned by hit() and treated by the store as
        // the OS error of the call that follows the point (io::Error::from_raw_os_error(1177) simulates a 1177)
}
pub fn armed(channel: &Channel, build_channel: &str) -> bool;            // pure: a test-* channel and build_channel != "stable"
// CoreConfig gains `pub failpoints: Arc<FailPoints>` (from_env in CoreConfig::for_env)
// store/paths.rs
pub struct WsPath(String);                                     // validated, '/' separators
impl WsPath { pub fn parse(rel: &str) -> Result<WsPath, StoreError>; pub fn as_str(&self) -> &str;
              pub fn abs(&self, root: &Path) -> PathBuf; pub fn join(&self, segment: &str) -> Result<WsPath, StoreError>; }
pub fn writable(rel: &WsPath) -> Result<(), StoreError>;       // refuses registry/ledger.csv and anything under projects/*/assets/*/v<semver>/
// store/write.rs
pub enum Expect { New, Generated, Hash(String) }
pub struct Written { pub path: WsPath, pub sha256: String, pub identity: FileIdentity }
pub enum StoreError { HashConflict { path: WsPath, backup: Option<WsPath> /* data.backup: a put-back that couldn't finish */ },
                      Busy(WsPath), PathInvalid { path: String, why: String }, DiskFull(WsPath),
                      Io { path: String, why: String }, Locked(WsPath), GitBusy, ReadOnly, NeedsRepair(WsPath),
                      YamlUnsupported(String), VersionExists(String) }   // Task 9 adds TrashUnknown and RestoreConflict
impl StoreError { pub fn to_error(&self, trace_id: &str) -> ErrorBody; }   // the Appendix D codes; HashConflict with a
                                                                           // backup sets data.backup to its relative path
pub enum ChangeKind { Written, Created, Removed, MovedFrom(WsPath) }
pub struct StoreEvent { pub path: WsPath, pub kind: ChangeKind, pub sha256: Option<String>, pub identity: Option<FileIdentity> }
pub trait ChangeSink: Send + Sync {                            // the index step (Task 10); Store emits, the index consumes (CORE-010)
    fn applied<'a>(&'a self, events: &'a [StoreEvent]) -> Pin<Box<dyn Future<Output = ()> + Send + 'a>>;
}
pub fn lfs_pointer_oid(bytes: &[u8]) -> Option<String>;        // "version https://git-lfs.github.com/spec/v1" … "oid sha256:<hex>"
pub struct Store;
impl Store {
    pub fn new(root: PathBuf, state: Arc<StateDb>, failpoints: Arc<FailPoints>, clock: Arc<dyn Clock>, ids: Arc<dyn IdSource>,
               probe: Arc<dyn Probe>) -> Store;                 // probe: placeholder check in step 3, post-write identities
    pub fn root(&self) -> &Path;
    pub fn ids(&self) -> &Arc<dyn IdSource>; pub fn clock(&self) -> &Arc<dyn Clock>;   // for tools that build manifests
    pub fn set_sink(&self, sink: Arc<dyn ChangeSink>);
    pub async fn read(&self, rel: &WsPath) -> Result<Option<Vec<u8>>, StoreError>;
    pub async fn write(&self, rel: &WsPath, bytes: Vec<u8>, expect: Expect) -> Result<Written, StoreError>;
    pub async fn edit_manifest(&self, rel: &WsPath, f: &(dyn Fn(&mut Manifest) -> Result<(), StoreError> + Send + Sync))
        -> Result<(Manifest, Written), StoreError>;            // read → f → write with Expect::Hash(read); an outside change re-reads and re-applies, at most 3 times
    pub async fn edit_yaml(&self, rel: &WsPath, edits: &[YamlEdit]) -> Result<Written, StoreError>;
    pub async fn edit_text(&self, rel: &WsPath, f: &(dyn Fn(&str) -> Result<String, StoreError> + Send + Sync)) -> Result<Written, StoreError>;
    pub async fn recover(&self) -> Result<RecoveryReport, StoreError>;   // the Global Constraints recovery rules
    pub async fn accepts_as_own(&self, rel: &WsPath, sha256: &str) -> Result<bool, StoreError>; // the generated hash or a pending one
}
pub struct RecoveryReport { pub temps_removed: Vec<WsPath>, pub backups_restored: Vec<WsPath>, pub backups_removed: Vec<WsPath>,
                            pub conflicts_kept: Vec<WsPath> /* <name>.ww-conflict-<utc> */, pub pending_resolved: Vec<WsPath> }
// state additions
pub struct Pending { pub path: String, pub old_sha256: Option<String>, pub new_sha256: String, pub started_utc: i64 }
impl StateDb { pub async fn generated(&self, path: &str) -> Result<Option<String>, StateError>;
               pub async fn set_generated(&self, path: &str, sha256: &str, utc: i64) -> Result<(), StateError>;
               pub async fn set_pending(&self, p: &Pending) -> Result<(), StateError>;
               pub async fn pending(&self, path: &str) -> Result<Option<Pending>, StateError>;
               pub async fn pendings(&self) -> Result<Vec<Pending>, StateError>;
               pub async fn finish_pending(&self, path: &str, generated: Option<(&str, i64)>) -> Result<(), StateError>; }
                   // one transaction: clear the pending row and, when given, set the generated hash (write step 8)
// store/fsops.rs additions (one #[allow] each)
pub fn write_temp(dir: &Path, name: &str, bytes: &[u8], fp: &FailPoints) -> io::Result<PathBuf>;   // create, write, ww_platform::fs::flush
pub fn replace(target: &Path, temp: &Path, backup: &Path) -> io::Result<()>;
pub fn move_new(from: &Path, to: &Path) -> io::Result<()>;                      // the only move; no replace-existing variant exists
pub fn remove_file(path: &Path) -> io::Result<()>; pub fn remove_dir_all(path: &Path) -> io::Result<()>;
    // both clear FILE_ATTRIBUTE_READONLY first: DeleteFile fails with ERROR_ACCESS_DENIED on a read-only file
pub fn set_readonly(path: &Path, on: bool) -> io::Result<()>;
pub fn copy_new(from: &Path, to: &Path) -> io::Result<u64>;                     // ww_platform::fs::copy_new (CopyFile2, fail if exists)
pub fn lock_exclusive(path: &Path) -> io::Result<Option<ww_platform::fs::LockedFile>>;
pub fn append_locked(path: &Path, make_line: &dyn Fn(&str /* current text */) -> String) -> io::Result<Option<String>>;
    // try_lock_append; None at once when another holder has the lock (the caller polls with tokio::time::sleep under
    // the 10 s budget, then store.busy). With the lock held: read the file, close a torn last line with "\n", call
    // make_line on the text (so the seq comes from last_seq of what is really there), one write_all; returns the line
```

**The write path** is exactly the Global Constraints algorithm, including the pending row (step 5), the put-back rules (step 7), the `move_new`-only fallbacks and the fail point `store.between_renames` between the fallback's two renames. Blocking file calls run in `spawn_blocking`, and the retry sleeps are `tokio::time::sleep`, so paused-time tests drive them. The store reads file data (`read`, the `edit_*` reads, the step-3 re-hash and the step-7 backup hash) only through `probe.open_read`, after the attribute-only `probe.identity` placeholder check. Until Task 11 adds the git gate, step 2 is a no-op. `recover()` follows the Global Constraints recovery rules for temps, backups and pending rows; Task 7 adds the staging and orphan parts.

- [ ] **Step 1: Write the failing tests** (`tests/store_write.rs`, `#[tokio::test(start_paused = true)]`, tempdir workspaces):
  - `new_file_write_records_generated_hash` (ARCH-002, ARCH-078): the write goes through `Store::write`, the only file-write home, and the `generated` row holds the new SHA-256;
  - `full_write_over_outside_edit_is_hash_conflict` (ARCH-074);
  - `edit_between_check_and_replace_puts_the_outside_version_back` (ARCH-074): a hook on `store.replace` writes "outside" to the target; the result is `HashConflict`, the file reads "outside", and no backup, temp file or pending row remains;
  - `put_back_held_past_budget_names_the_backup_and_recovers` (ARCH-074): as above, plus `HandleHolder::hold(target, Share::Read)` taken in the hook; `advance(11 s)` → `HashConflict { backup: Some(_) }`, whose `to_error` has `data.backup` naming that backup's workspace-relative path, and the pending row kept; after release, `recover()` puts "outside" back under the target's name;
  - **`crash_after_replace_keeps_outside_edit_visible`** (ARCH-074, Review Focus 1): a hook on `store.replace` writes "outside" to the target, and `store.after_replace=io` stops the write before step 7. The store is dropped, and a new `Store` over the same root and `state.db` runs `recover()`: the target reads "outside", the core's bytes are in `<name>.ww-conflict-<utc>`, no `.ww-bak-` file remains and the pending row is gone;
  - **`crash_after_replace_does_not_flag_own_write_as_human_edit`** (ARCH-074, Review Focus 1): `store.after_replace=io` with no outside edit; after `recover()` in a new `Store`, the `generated` row holds the new SHA-256, `accepts_as_own(new)` is true, and the next `Expect::Generated` write succeeds. Before `recover()`, `accepts_as_own` is true for both the old and the new hash;
  - `expect_generated_without_a_row_is_conflict_or_new` (an existing file → `HashConflict`; a missing one is written);
  - `cloud_only_target_is_a_conflict_and_never_read` (a fake `Probe` reports the file as a placeholder; `write`, `edit_manifest`, `edit_text` and `read` all refuse it, and no byte of it is read: the fake probe's `open_read` counter, the store's only way to read file data, stays at 0);
  - **`crash_during_put_back_keeps_the_outside_edit`** (ARCH-074, Review Focus 1): a hook on `store.replace` writes "outside" to the target, and `store.put_back=io` stops the put-back halfway. Two variants: the `ReplaceFileW` put-back (the second backup, holding the core's new bytes, is left beside the target) and the fallback put-back with `store.replace=unsupported` (the target is missing, and both backups are left). In both, a new `Store` over the same root and `state.db` runs `recover()`: the target reads "outside", no `.ww-bak-` file and no pending row remains, and the core's new bytes are gone (they were its own, and the call had failed);
  - **`held_target_is_retried_until_released`** (Review Focus 2): `HandleHolder::hold(target, Share::Read)`; start the write; `advance(3 s)`; release; the write is ok;
  - **`target_held_past_budget_fails_with_store_busy_and_no_temp`** (Review Focus 2): `advance(11 s)` → `Busy`; the target's SHA-256 is unchanged; no `*.ww-tmp-*` file;
  - **`relative_path_over_180_chars_is_refused_before_any_write`** (Review Focus 3): no folder of the path was created;
  - **`deep_workspace_past_260_chars_writes_through_long_paths`** (Review Focus 3): the workspace root is nested so that the target's absolute path is 320 characters; the write and the read are ok;
  - **`replace_falls_back_to_two_renames_where_unsupported`** (Review Focus 4): `store.replace=unsupported` → ok with the new content;
  - `fallback_crash_between_renames_is_restored_by_recover`: `store.replace=unsupported`, and a hook on `store.between_renames` returns an `io` error, so the write stops after target → backup; the store is dropped, and `recover()` in a new `Store` restores the old content;
  - **`fallback_second_rename_never_replaces_a_new_outside_file`** (ARCH-074, Review Focus 4): `store.replace=unsupported`, and a hook on `store.between_renames` creates the target with "outside" and returns `Ok`; the result is `HashConflict`, the target reads "outside", the old content is in `<name>.ww-conflict-<utc>`, and no temp file or pending row remains. The 1177 variant gives the same result: a hook on `store.replace` does what a real 1177 leaves behind (it renames the target to the first backup name, which is the one `.<name>.ww-tmp-<pid>-<n>` file in the folder with `ww-tmp` read as `ww-bak`), writes "outside" at the target, and returns `io::Error::from_raw_os_error(1177)`;
  - `replace_errors_map_to_retry_recover_or_fail` (pure: `RetryClass` → next step, for 32, 1175, 1176, 1177, 183, 50 and 112; 1177 and the fallback's second rename go to `move_new`);
  - `new_target_created_by_an_outside_writer_is_hash_conflict` (ARCH-074): a hook on `store.replace` creates the target before the move; the result is `HashConflict`, the outside file is unchanged, and no temp file remains;
  - `disk_full_leaves_no_temp_and_the_target_unchanged` (QA-011);
  - `ledger_csv_and_sealed_folders_are_refused` (ARCH-082, CORE-026);
  - `lfs_pointer_compares_by_oid` (ARCH-078): a pointer whose `oid` equals the generated hash passes `Expect::Generated`;
  - `manifest_edit_rereads_and_reapplies_after_an_outside_change`;
  - `invalid_manifest_is_needs_repair_and_untouched`;
  - `yaml_edit_keeps_comments_and_is_hash_checked`;
  - `stale_temps_of_dead_processes_are_removed_by_recover` (a temp named for a live PID is kept);
  - `failpoints_arm_only_on_test_channels_of_non_stable_builds` (QA-066): the pure `armed` over every channel kind (stable, dev, `test-*`) and the build channels `stable` and `dev`, since a stable or dev `Env` can't be built under the testkit's armed guard; plus `from_env` on a test `Env` arms a parsed spec;
  - `store_errors_map_to_appendix_d_codes`;
  - `idempotency_prune_uses_its_index` (`EXPLAIN QUERY PLAN` of the prune names `idempotency_prune`).
- [ ] **Step 2: Run them.**

Run: `cargo nextest run -p ww-core --test store_write`
Expected: FAIL. Nothing is implemented.

- [ ] **Step 3: Implement it.**
- [ ] **Step 4: Run them, lint, and do a deliberate-violation check.**

Run: `cargo nextest run -p ww-core -p ww-testkit && cargo clippy -p ww-core --all-targets -- -D warnings`
Expected: PASS.

Then the deliberate-violation check (ARCH-002, ARCH-112; recorded in the report): in a scratch copy, a `ww_platform::fs::replace_file` call in `store/write.rs` must make clippy fail. Record the output, then remove the line.

- [ ] **Step 5: Commit.**

```bash
git add crates/ww-core crates/ww-protocol Cargo.lock
git commit -m "store: hash-checked atomic writes with ReplaceFileW backup check, sharing-violation retries, path budget, fail points"
```

---

### Task 7: `store` order and areas, `WorkspaceHub` and `core.lock` (ARCH-028, ARCH-033, ARCH-075, ARCH-080, ARCH-081, CORE-003, CORE-009, CORE-038, QA-012)

**Files:**
- Create: `crates/ww-core/src/store/{order.rs, logs.rs, staging.rs}`.
- Modify: `crates/ww-core/src/workspace.rs` (`WorkspaceHub`, `OpenWorkspace`, `Access`).
- Modify: `crates/ww-core/src/state/mod.rs`. `for_workspace` takes an `Access`; read-only opens never migrate the file or release rows. A `ReadOnly` open of a workspace whose `state.db` doesn't exist (SQLite can't open a missing file read-only; the second channel's local folder may differ from the first's) gets an in-memory `StateDb` (`rusqlite::Connection::open_in_memory`) with the workspace migrations applied, and creates nothing on disk (ARCH-028).
- Modify: `crates/ww-core/src/tools/{registry.rs, dispatcher.rs, test_tools.rs, mod.rs}` and `crates/ww-core/src/server/mod.rs`. The hub opens the default workspace at start and `close_all` runs before the state databases close.
- Modify: `crates/ww-core/src/config.rs`: `CoreConfig` gains `pub probe: Arc<dyn Probe>` (`SystemProbe` in `CoreConfig::for_env`), which `server/mod.rs` passes to `WorkspaceHub::new`, so in-process tests can fake volumes and placeholders.
- Modify: `crates/ww-core/src/tools/tests.rs` (plan 2a's unit tests, which pin what this task changes): `registry_and_dispatcher_live_in_the_tools_module`'s `NewDispatcher` type gains `Arc<WorkspaceHub>` in `Dispatcher::new`'s new position, every `Dispatcher::new` call in the file passes a hub, and `test_tools_exist_only_on_test_channels`'s name list gains `test.ingest_run` and `test.store_write` in sorted order.
- Test: unit tests in the new files; `crates/ww-core/tests/{store_order.rs, workspace_hub.rs}`; additions to `tests/state.rs` and `tests/dispatch.rs`. Tests create fixture files with `ww_testkit::files::write_file`, never with `std::fs` in `crates/ww-core/src` (QA-024).

**Interfaces:**

Consumes: Task 6's `Store`.

Produces:

```rust
// store/order.rs
pub struct OutputMove { pub from: PathBuf /* inside a staging handle */, pub to: WsPath }
pub type ManifestEdit = Arc<dyn Fn(&mut Manifest) -> Result<(), StoreError> + Send + Sync>;
pub type TextEdit = Arc<dyn Fn(&str) -> Result<String, StoreError> + Send + Sync>;
pub struct CommitOp { pub outputs: Vec<OutputMove>, pub moved: Vec<Written> /* files already in place, e.g. a sealed version */,
                      pub manifest: Option<(WsPath, ManifestEdit)>,
                      pub job_line: Option<serde_json::Map<String, Value>>, pub progress: Option<(WsPath, TextEdit)> }
pub struct CommitReport { pub outputs: Vec<Written>, pub manifest: Option<Manifest>, pub job_seq: Option<u64> }
impl Store {
    pub async fn commit(&self, op: CommitOp) -> Result<CommitReport, StoreError>;  // ARCH-075 order; fail points between steps
    pub async fn orphans(&self) -> Result<Vec<WsPath>, StoreError>;
        // every orphan kind of Global Constraints: candidates/<run>/ folders missing from their manifest, v<semver>/ folders
        // with a snapshot that no manifest records, non-empty staging handles and seal-* folders of a dead core,
        // *.ww-conflict-* files
    pub async fn next_run_id(&self, asset_dir: &WsPath, m: Option<&Manifest>, now_utc: i64) -> Result<String, StoreError>;
        // r_<date>_<hhmm>, with the smallest _<n> free among the manifest's runs and the existing candidates/* folders
}
// store/logs.rs
pub enum LogKind { Jobs, Usage }
impl Store { pub async fn append_log(&self, kind: LogKind, record: serde_json::Map<String, Value>) -> Result<u64, StoreError>; }
    // fsops::append_locked in spawn_blocking; on None (another holder), tokio::time::sleep from 10 ms doubling to 500 ms,
    // 10 s in all, then store.busy. The seq is last_seq(text under the lock, machine) + 1
pub async fn append_loose_usage(env: &Env, record: serde_json::Map<String, Value>) -> Result<u64, StoreError>; // local_dir\usage-loose.jsonl
// store/staging.rs
pub struct StagingHandle { pub id: String, pub dir: PathBuf }
impl Store { pub async fn staging_handle(&self) -> Result<StagingHandle, StoreError>;
             pub async fn release_handle(&self, h: StagingHandle) -> Result<(), StoreError>;     // removes an emptied handle folder
             pub async fn sweep_staging(&self) -> Result<Vec<WsPath>, StoreError>; }
    // called by recover() at a read-write open: under core.lock every existing handle is a dead core's, so empty ones are
    // removed and non-empty ones are returned for the orphan list. It removes empty folders only (the CORE-110 scan allows it)
// workspace.rs
pub enum Access { ReadWrite, ReadOnly }
pub struct OpenWorkspace { pub ws: WorkspaceRef, pub access: Access, pub state: Arc<StateDb>, pub store: Arc<Store>,
                           pub orphans: Vec<WsPath> /* listed at open */ }   // Task 10 adds `index`, Task 11 `watch`
pub struct WorkspaceHub;
impl WorkspaceHub {
    pub fn new(env: Env, states: Arc<StateRegistry>, events: Arc<EventBus>, clock: Arc<dyn Clock>,
               ids: Arc<dyn IdSource>, failpoints: Arc<FailPoints>, probe: Arc<dyn Probe>) -> Arc<WorkspaceHub>;
    pub async fn open(&self, ws: &WorkspaceRef) -> Result<Arc<OpenWorkspace>, ErrorBody>;  // one open per id, behind a once-cell
    pub fn default_id(&self) -> Option<String>;
    pub fn refresh_default(&self);                                    // re-reads suite.toml
    pub async fn close(&self, id: &str); pub async fn close_all(&self);
}
// state
impl StateRegistry { pub async fn for_workspace(&self, ws: &WorkspaceRef, access: Access) -> Result<Arc<StateDb>, StateError>; }
// tools: CallCtx gains `pub workspace: Option<Arc<OpenWorkspace>>`
impl CallCtx { pub fn workspace(&self) -> Result<&Arc<OpenWorkspace>, ErrorBody>; }   // workspace.required when None
// Dispatcher::new(reg, hooks, states, events, hub, ids, clock, lifecycle)
// config: CoreConfig gains `pub probe: Arc<dyn Probe>` (SystemProbe by default), passed to WorkspaceHub::new
```

**Rules:**
- **`WorkspaceHub::open`**, in order:
  1. `check_workspace(path)`;
  2. `fsops::ensure_dir(<ws>\.worldwright)` and `fsops::lock_exclusive(core.lock)`: `Some` → `ReadWrite`, `None` → `ReadOnly`;
  3. `states.for_workspace(ws, access)`; for `ReadOnly` with no `state.db` on disk, an in-memory `StateDb` with the migrations applied, and nothing is created;
  4. `Store::new`, then `store.recover()` (read-write only);
  5. `orphans()`, published once on `workspace` as `{kind:"orphans", paths}` when not empty.

  The lock lives as long as the `OpenWorkspace`.
- **Dispatcher, Resolve.** With a session workspace:
  - its id differs from `hub.default_id()` → `workspace.unsupported` (owner decision 2, after a switch);
  - otherwise `hub.open`, and `CallCtx.workspace` is set;
  - an effect other than `read` on a `ReadOnly` workspace → `workspace.read_only`, before `idem_begin`.
- **`commit`** steps:
  1. outputs: `move_new` each `OutputMove`, then take its identity after the move (CORE-041 clarification: on ReFS a move across folders changes the file ID, so the index gets the post-move identity in `Written.identity`);
  2. `edit_manifest`;
  3. `append_log(Jobs)`;
  4. `edit_text(progress)`;
  5. `sink.applied(events of 1–4)`, plus one `ChangeKind::Written` event per entry of `op.moved`, with its `sha256` and `identity` as given. Files that a caller moved into place before the commit (Task 13's sealed version) therefore reach the index only after the manifest edit (ARCH-075).

  The fail points are `store.after_output`, `store.after_manifest`, `store.after_jobs_log`, `store.after_progress` and `store.after_index`. An error after step 1 leaves the outputs where they are (they become orphans); nothing is ever deleted.
- **`recover`** (Task 6) gains the staging and orphan parts here: it calls `sweep_staging`, and `orphans()` lists everything the Global Constraints name, including `v<semver>/` folders that hold a `manifest.snapshot.json` whose version is neither the manifest's `approved_version` nor a run's `signed_off.version`.
- **Test tools:** `test.store_write` and `test.ingest_run`, per the Global Constraints table. `test.ingest_run` uses `staging_handle`, writes the files through `fsops::write_temp` as an engine would, takes its run id from `next_run_id`, and commits one candidate run (`Manifest::new` with `store.ids().uuid()` if missing, status via `transition(…, CandidateRun)`, each run file with its `sha256`). An omitted `project` is `sketchbook` here; Task 12 replaces that default with its resolver.

- [ ] **Step 1: Write the failing tests:**
  - **`tests/store_order.rs`:**
    - `commit_writes_output_manifest_log_progress_then_index` (ARCH-075): a recording `ChangeSink` and hooks see the steps in that order, and an `op.moved` entry reaches the sink only in step 5, with the identity it was given;
    - `error_after_output_leaves_a_listed_orphan_never_deleted` (ARCH-075, CORE-038), for each fail point after step 1;
    - `append_takes_the_shared_lock_and_numbers_lines` (ARCH-081; paused time): a second handle holding the lock through `try_lock_append` delays the append until released; two concurrent `append_log` calls get seqs 1 and 2, never the same one;
    - `month_rollover_starts_a_new_file` (`FakeClock::shift_utc`);
    - `torn_last_line_is_closed_and_skipped_by_readers`;
    - `loose_usage_goes_to_the_local_folder` (ARCH-081);
    - `staging_handles_are_in_the_workspace_and_ingest_keeps_file_ids` (ARCH-080, CORE-003, ARCH-033): the file's `volume_serial` equals the one `identity` reports for the workspace root (both from `FILE_ID_INFO`) and its content is unchanged. Only when `volume_info(root).fs_name == "NTFS"` does it also assert that `file_id` is equal before and after the move: on ReFS (a Dev Drive `%TEMP%`) a move across folders changes the ID. On every file system, `Written.identity` equals a fresh `identity()` of the moved file;
    - `run_id_skips_orphan_candidate_folders`: with `candidates/r_20261016_1200/` on disk but not in the manifest, `next_run_id` at 12:00 UTC gives `r_20261016_1200_2`;
    - `recover_sweeps_empty_handles_and_lists_the_rest` (a dead core's empty handle is removed; a non-empty handle and a `seal-*` folder are listed as orphans and kept);
    - `orphans_include_unrecorded_sealed_versions_and_conflict_files` (a `v1.1.0/` with a snapshot that no manifest field records, and a `notes.txt.ww-conflict-20261016T120000Z` file, are both listed).
  - **`tests/workspace_hub.rs`:**
    - `core_lock_gives_read_write_and_releases_running_rows` (ARCH-028);
    - `second_hub_without_the_lock_is_read_only_and_keeps_rows` (ARCH-028): two `StateRegistry`s whose envs map the workspace to the same local folder; the second gets `ReadOnly`; the first's `running` row survives; the second's `state.db` connection is read-only;
    - `read_only_open_without_a_state_db_uses_memory_and_creates_nothing` (ARCH-028): a second hub whose env maps the workspace to a different, empty local folder (another channel) opens it `ReadOnly`; reads work, a write tool gets `workspace.read_only`, and no file appears in that local folder;
    - `concurrent_first_opens_share_one_open_workspace`: 16 tasks → one `Arc` (`ptr_eq`) and one state actor;
    - `open_lists_orphans_once_on_the_workspace_topic`.
  - **`tests/dispatch.rs` additions:**
    - `read_only_workspace_refuses_writes_before_idempotency`;
    - `default_switch_makes_old_sessions_unsupported`;
    - `no_workspace_tool_gets_workspace_required` (a test tool that calls `ctx.workspace()`);
    - `write_tool_panic_is_core_bug_and_releases_the_key`;
    - `state_errors_map_to_state_unavailable`.
  - **`tests/state.rs` hardening:**
    - `newer_schema_is_refused_and_untouched_with_a_live_wal` (a second connection holds uncheckpointed WAL frames during the check);
    - `losing_core_never_touches_state_db` also compares the `-wal` file's mtime;
    - `replay_of_a_null_outcome_is_core_bug` (`Replay("")`);
    - `idem_finish_on_a_missing_row_is_an_error`: it returns `StateError::Io("no idempotency row …")` and is logged, instead of `Ok`.
  - Remove from `expected-fail.toml`: the four `store_writes.rs` entries, plus `append_log_lines_are_per_machine_per_month_with_seq`, `torn_last_line_is_closed_before_the_next_append`, `staging_handle_is_in_workspace_and_ingest_moves_files` and `second_channel_core_gets_read_only_workspace`.
- [ ] **Step 2: Run them.**

Run: `cargo nextest run -p ww-core`
Expected: FAIL for the tests of the new code. The hardening tests (`write_tool_panic_is_core_bug_and_releases_the_key`, `state_errors_map_to_state_unavailable`, `replay_of_a_null_outcome_is_core_bug`, `newer_schema_is_refused_and_untouched_with_a_live_wal` and the extended `losing_core_never_touches_state_db`) may already pass on `main`: the report lists each one as RED or as a regression pin that passed before the change, never claims a RED it didn't see.

- [ ] **Step 3: Implement it.**
- [ ] **Step 4: Run them, then the spec tests.**

Run: `cargo nextest run -p ww-core && cargo clippy -p ww-core --all-targets -- -D warnings && cargo run -q -p ww-cli -- dev spec-tests`
Expected: PASS; `spec tests: 37 passed, 29 expected-fail`.

- [ ] **Step 5: Commit.**

```bash
git add crates/ww-core crates/ww-spec-tests/expected-fail.toml Cargo.lock
git commit -m "store: write order with orphans, locked per-machine monthly logs, staging handles; workspace hub with core.lock (ARCH-028/075/081)"
```

---

### Task 8: Workspace tools, `suite.toml` writes and `ww workspace init|use` (ARCH-003, ARCH-090, ARCH-094, ARCH-098, CORE-007, QA-003, QA-040; owner decisions 1 and 2)

**Files:**
- Create: `crates/ww-core/src/store/suite_toml.rs` and `crates/ww-core/src/tools/workspace_tools.rs`.
- Modify: `crates/ww-core/src/tools/{registry.rs, dispatcher.rs, mod.rs}` (`core_registry` gains `hub: &Arc<WorkspaceHub>`, `env: &Env` and `probe: &Arc<dyn Probe>`, and always registers the product tools), `crates/ww-core/src/server/mod.rs` (passes the hub, env and `CoreConfig.probe` to `core_registry`; wiring) and `crates/ww-core/src/workspace.rs` (the agent hint, quoted paths).
- Modify: `crates/ww-core/src/tools/tests.rs` (plan 2a's unit tests, which pin what this task changes):
  - every `core_registry` call passes the new arguments;
  - `test_tools_exist_only_on_test_channels` compares only the names that start with `test.`, and its off-channel assertion becomes "no `test.*` tool is listed" (the product tools are registered on every channel);
  - `every_tool_declares_a_module` accepts `test` for `test.*` tools and `workspace` for every other one;
  - `RecArgs` gains `#[serde(deny_unknown_fields)]`, so its schema has `"additionalProperties": false` and `RecTool` still registers under the new precondition.
- Modify: `crates/ww-client/src/lib.rs` (hint quoting, the `--workspace` wording, and the unregistered-workspace hint loses its "(plan 2b)" text; Task 14 removes the one in the start-failure message) and its tests (`tests/client.rs`).
- Create: `crates/ww-cli/src/user/workspace.rs`. Modify `user/mod.rs` (`Workspace` subcommand; the `--no-start` probe).
- Test: unit tests; `crates/ww-core/tests/{workspace_tools.rs, workspace_create_guard.rs}`; `crates/ww-cli/tests/workspace_cli.rs`; `crates/ww-cli/tests/user_cli.rs` (hygiene).

**Interfaces:**

Consumes: Task 7's `WorkspaceHub`, Task 3's `suite_edit::register` and YAML templates.

Produces:

```rust
// store/suite_toml.rs: the core is the only writer of suite.toml (owner decision 1)
pub async fn register_workspace(env: &Env, path: &Path, make_default: bool, fp: &FailPoints) -> Result<bool, StoreError>;
    // read → suite_edit::register → fail point "store.suite_toml" → write_temp + replace with the read hash;
    // an outside change re-reads once. When suite.toml doesn't exist yet, register starts from an empty text and the
    // first write is a new target: move_new (ReplaceFileW needs an existing target), whose 80/183 means another
    // writer created the file meanwhile, so it re-reads once as for any outside change
// tools/workspace_tools.rs
pub fn register_workspace_tools(reg: &mut Registry, hub: Arc<WorkspaceHub>, env: Env, probe: Arc<dyn Probe>);
pub fn refuse_inside(path: &Path, install_dir: Option<&Path>) -> Result<(), String>;
    // pure (ARCH-094): Err when `path` is the install folder or inside it, compared with folder_key; Ok for None
#[serde(deny_unknown_fields)] pub struct WorkspaceCreateArgs { pub path: String, pub name: Option<String>, pub default: Option<bool> }
pub struct WorkspaceCreateResult { pub id: String, pub path: String, pub created: bool, pub default: bool, pub warnings: Vec<String> }
#[serde(deny_unknown_fields)] pub struct SetDefaultArgs { pub path: String }
pub struct SetDefaultResult { pub id: String, pub path: String }
// ww-cli user/mod.rs
pub enum UserCommand { /* plan 2a's */ #[command(subcommand)] Workspace(WorkspaceCommand) }
pub enum WorkspaceCommand {
    /// Create a workspace (or register an existing one) and make it the default when none is set.
    Init { path: PathBuf, #[arg(long)] name: Option<String>, #[arg(long)] default: bool },
    /// Make a workspace the default one the core serves.
    Use { path: PathBuf },
}
```

**Rules:**
- **`workspace.create`:**
  1. Make the path absolute and run `check_workspace` on it **before** anything is created (QA-040: a path inside a real workspace trips the guard before a folder exists); then `fsops::ensure_dir`, canonicalise, and run `check_workspace` again on the canonical path (a junction or short name can lead elsewhere).
  2. Refuse with `workspace.path_invalid` a path that `refuse_inside(path, env.install_dir())` rejects (ARCH-094), one inside another registered workspace, or one that is a file. `Env::install_dir()` is `None` on test channels, so the install-folder rule is tested through the pure `refuse_inside` with a made-up install folder.
  3. If `worldwright.yaml` exists, register only (`created: false`); nothing is written in the folder.
  4. Otherwise `hub.open(ws)`. Through its store, write `projects/sketchbook/project.yaml`, `projects/sketchbook/progress.md` (`PROGRESS_TEMPLATE`, ARCH-098), `.gitignore` (the five entries) and `.gitattributes` (`projects/** -text`, CORE-040 clarification), each appended to an existing file through `edit_text`, and **last** `worldwright.yaml`, whose presence marks the folder as a workspace.
  5. `register_workspace` (`default` = the argument, else true when no default is set); `hub.refresh_default()`; `hub.close(id)` unless it is the default.

  Warnings (Review Focus 4), never errors:
  - `probe.volume_info` is remote or a cloud-files sync root, or the folder carries cloud-file attributes → "This folder is on a network or cloud-synced volume…";
  - the volume has no stable IDs (FAT, exFAT), or `probe.volume_info` fails (a USB stick or a share whose facts can't be read) → "This volume can't track files by ID, so changes are found by size and time…". The workspace is created and served as one without stable IDs.
- **`workspace.set_default`:**
  - the path must hold `worldwright.yaml`, else `workspace.path_invalid`;
  - register with `make_default`;
  - `refresh_default`;
  - publish `{kind:"default_changed", id, path}` on `workspace`;
  - close the old default's `OpenWorkspace` (its watcher, index and `core.lock`) once no call holds it: the hub drops its own `Arc`, and the last in-flight call's drop closes it.
- **`workspace.unsupported`** gains `fix.agent_hint = {tool:"workspace.set_default", arguments:{path}}`. Its message quotes the path: `` `ww workspace use "<path>"` ``.
- **`ww-client` messages.**
  - The unregistered-workspace hint quotes the path.
  - For a `--workspace` source it doesn't suggest `ww workspace use`. It says the core serves only the default workspace (named), and to run the command without `--workspace` or from inside it.
- **`ww workspace use`** and **`ww workspace init`** connect with workspace `"none"` and never call `resolve_workspace_id`, so a stale `default_workspace` or a broken `suite.toml` never blocks the command that repairs it.
- **Registry precondition** (before the first product tool): `Registry::register` panics when an input schema with `"type":"object"` lacks `"additionalProperties": false`.
- **A failed `idem_finish` after a run that succeeded** keeps the row `running`, because the tool ran and must not run again. The outcome becomes `state.unavailable` with:
  - the message "The call ran, but the core could not save its result. Until the core restarts, a retry with the same idempotency key reports it as still running; check what it did before you retry with a new key.";
  - `data.ran == true`.

- [ ] **Step 1: Write the failing tests:**
  - **`tests/workspace_tools.rs`:**
    - `create_writes_layout_and_registers_last`: with `store.suite_toml=io` armed, the folder has its files and isn't registered; a retry adopts and registers it;
    - `create_adopts_an_existing_workspace_without_writing` (the SHA-256 of `worldwright.yaml` is unchanged);
    - `create_refuses_nested_workspace_and_files` (a folder inside another registered workspace, and a path that is a file, → `workspace.path_invalid`; nothing is created);
    - `create_warns_on_network_or_cloud_volumes` (fake `Probe`);
    - **`create_on_a_fat_or_usb_volume_warns_and_succeeds`** (Review Focus 4; fake `Probe` in an in-process core's `CoreConfig.probe`, Task 7): with `fs_name` `FAT32`, and again with a `volume_info` that returns an error, `workspace.create` succeeds with the "can't track files by ID" warning, and the workspace opens and serves a `test.store_write`;
    - `create_writes_gitattributes_and_appends_to_an_existing_one` (CORE-040: an existing `.gitattributes` keeps its lines, and `projects/** -text` is appended once);
    - `set_default_registers_and_switches` (ARCH-090);
    - `set_default_closes_the_old_workspace` (after the switch and the end of a call that held it, a second hub can take the old workspace's `core.lock`);
    - `suite_toml_keeps_comments_and_unknown_keys_and_is_hash_checked` (CORE-007): a `FailPoints::on("store.suite_toml", …)` hook changes the file between read and replace → re-read once → both edits are present;
    - `unsupported_fix_has_the_agent_hint_and_a_quoted_path` (QA-003).
  - **`tests/workspace_create_guard.rs`:** `create_on_a_real_registered_workspace_trips_the_guard` (QA-040, its own binary, ordered as plan 2a's guard test): the guard trips for a new subfolder of that workspace, and the subfolder was never created.
  - **Unit tests:**
    - `registry_refuses_schemas_that_allow_unknown_properties`;
    - `refuse_inside_rejects_the_install_folder_and_below` (ARCH-094: with a made-up install folder `C:\Programs\Worldwright`, that folder, a subfolder and a differently cased spelling are refused; a sibling folder and `None` pass);
    - `idem_finish_failure_says_what_happens`;
    - `ww-client`: `unregistered_hint_quotes_the_path_and_flag_source_is_not_told_to_switch` (the hint no longer says "(plan 2b)"; the existing unit test that expects that text is updated).
  - **`ww-cli` parse:** `workspace_init_and_use_parse`.
  - **`tests/workspace_cli.rs`:**
    - `init_creates_and_prints_the_default`;
    - `use_works_with_a_stale_default` (the default folder is deleted first);
    - `no_start_with_an_unresolvable_workspace_while_a_core_runs`: `ww --no-start --workspace <not a workspace> call test.echo '{"text":"x"}'` with a core running → exit 2 with the plain message; the `--no-start` probe passes on errors other than `NotRunning` instead of swallowing them.
  - **`ww-client` test gaps:**
    - `older_client_drain_and_wait_is_incompatible`;
    - `racing_starts_yield_one_core` also asserts exactly one `core.*.log` under the logs folder;
    - the fake-server test's raw-newline literals become `\n` escapes.
  - Remove the four `ws_tools.rs` entries from `expected-fail.toml`.
- [ ] **Step 2: Run them.**

Run: `cargo nextest run -p ww-core -p ww-client -p ww-cli`
Expected: FAIL. Nothing is implemented.

- [ ] **Step 3: Implement it.**
- [ ] **Step 4: Run them, then the spec tests.**

Run: `cargo nextest run -p ww-core -p ww-client -p ww-cli && cargo clippy --workspace --all-targets -- -D warnings && cargo run -q -p ww-cli -- dev spec-tests`
Expected: PASS; `spec tests: 41 passed, 25 expected-fail`.

- [ ] **Step 5: Commit.**

```bash
git add crates/ww-core crates/ww-client crates/ww-cli crates/ww-spec-tests/expected-fail.toml Cargo.lock
git commit -m "core: workspace.create/set_default writing suite.toml through store; ww workspace init|use; registry schema check"
```

---

### Task 9: Sealed versions, trash, retention and legacy folders (ARCH-058, CORE-026, CORE-028, CORE-032, CORE-033, CORE-035, CORE-037, CORE-110)

**Files:**
- Create: `crates/ww-core/src/store/{versions.rs, trash.rs}` and `crates/ww-core/src/tools/trash_tools.rs`.
- Modify: `crates/ww-core/src/store/write.rs` (the `locks` check; `StoreError` gains `TrashUnknown(String)` → `trash.unknown` and `RestoreConflict(WsPath)` → `trash.restore_conflict`), `tools/{mod.rs, test_tools.rs}` (`test.trash`), `tools/tests.rs` (`test_tools_exist_only_on_test_channels`'s `test.*` name list gains `test.trash`), and `workspace.rs` (maintenance at open and every 24 h).
- Test: unit tests; `crates/ww-core/tests/{versions.rs, trash.rs}`. Tests create fixture files with `ww_testkit::files::write_file`, never with `std::fs` in `crates/ww-core/src` (QA-024).

**Interfaces:**

Consumes: Task 6–7 `Store`; Task 3's `Version`, `version_content_hash`.

Produces:

```rust
// store/versions.rs
pub struct SealRequest { pub project: String, pub asset_id: String, pub version: Version, pub run_id: String,
                         pub files: Vec<String>, pub snapshot: Manifest /* the manifest as it stands after sign-off */ }
pub struct Sealed { pub dir: WsPath, pub files: Vec<(String, String)> /* rel, sha256 */, pub content_hash: String,
                    pub identities: Vec<(WsPath, FileIdentity)> /* taken after the final move */ }
pub struct VersionDir { pub version: Version, pub dir: WsPath, pub legacy: bool, pub content_hash: String }
impl Store {
    pub fn version_lock(&self, project: &str, asset_id: &str) -> Arc<tokio::sync::Mutex<()>>;   // per asset; callers take
                                                                    // lock_owned() and keep the guard until Record ends (Task 13)
    pub async fn seal(&self, req: SealRequest) -> Result<Sealed, StoreError>;   // the caller holds the version_lock guard
    pub async fn versions(&self, project: &str, asset_id: &str) -> Result<Vec<VersionDir>, StoreError>;
}
// store/trash.rs
pub const TRASH_KEEP_DAYS: i64 = 30; pub const REJECTED_KEEP_DAYS: i64 = 90;
pub struct TrashRecord { pub entry_id: String, pub original_path: String, pub moved_utc: String, pub purge_after: String,
                         pub reason: String, pub sha256: Option<String> }
pub struct MaintenanceReport { pub purged: Vec<String>, pub runs_trashed: Vec<WsPath> }
impl Store {
    pub async fn trash(&self, rel: &WsPath, reason: &str) -> Result<TrashRecord, StoreError>;
    pub async fn trash_list(&self) -> Result<Vec<TrashRecord>, StoreError>;
    pub async fn trash_restore(&self, entry_id: &str) -> Result<WsPath, StoreError>;
        // TrashUnknown(entry_id) for no such entry; RestoreConflict(original path) when that path is occupied
    pub async fn maintenance(&self, now_utc: i64) -> Result<MaintenanceReport, StoreError>;
}
// StoreError (Task 6) gains TrashUnknown(String) and RestoreConflict(WsPath); to_error maps them to the Appendix D codes
pub fn register_trash_tools(reg: &mut Registry);       // trash.list, trash.restore
```

**`seal`** (CORE-026). Its caller holds the `version_lock` guard for the whole call, and `asset.signoff` keeps holding it through the Record stage (Task 13).
1. Refuse when `v<version>/` exists (`version.exists`).
2. Create `<ws>\.worldwright\staging\seal-<sh_…>\`.
3. Move the chosen files and their `derived` files from `candidates/<run>/`. A chosen file that another run file was derived from is copied instead (I5). Move `source/` if the run has one.
4. Write `manifest.snapshot.json` (`snapshot.to_text()`).
5. `set_readonly` on every file.
6. `move_new` the folder to `projects/<p>/assets/<a>/v<version>/`.
7. Record the generated hashes, and compute `content_hash`. `Sealed.files` holds bare hex, keyed by the path relative to the asset folder (`v<version>/<file>`); the manifest's `hashes` entries that Task 13's Record stage writes from it use `ww_schema::hash_value` (`sha256:<hex>`, I5). Identities for the index are taken after step 6 and returned in `Sealed.identities`: setting the read-only attribute moves ChangeTime, and on ReFS a move across folders changes the file ID (CORE-041 clarification).
8. Hit the fail point `store.after_seal`. Between here and the Record stage's manifest edit, a crash leaves a `v<version>/` folder that no manifest records yet; `orphans()` lists it (Global Constraints), and nothing in it is lost.

Each move hits the fail point `store.replace`, which covers every swap or move of a file into place, and retries sharing violations as in Task 6. On an error, already-moved files are moved back to the run folder, and the staging folder (if not empty) is left for the orphan listing. A half-filled `v<version>/` never exists.

**Other rules:**
- **`versions`:** a `v*/` folder without `manifest.snapshot.json`, in an asset whose manifest has no matching `candidate_runs` entry, is `legacy` (CORE-033). It is read as it is and never moved.
- **Locks** (CORE-035): `Store::write` and `edit_*` refuse a path under an asset folder that its manifest lists in `locks` → `store.locked`.
- **Trash** (ARCH-058, CORE-037, CORE-110). `trash` moves the file or folder to `.trash\<date>\<te_…>\payload\<name>` and writes `restore.json` next to it. `trash_restore` of a path under `projects/*/assets/*/v<semver>/` sets the read-only attribute again on every restored file (CORE-026).
- **`maintenance`:**
  - It removes entries whose `purge_after` has passed. This is the core's own maintenance, not an agent call. `fsops::remove_file` and `remove_dir_all` clear the read-only attribute first, so a sealed payload can be purged (`DeleteFile` fails with `ERROR_ACCESS_DENIED` on a read-only file).
  - It moves the folders of `rejected` runs to trash 90 days after their version's sign-off date, unless `pinned` (CORE-032), and records `"trashed_utc"` (RFC 3339) on the run with `edit_manifest`; the run is otherwise unchanged (the I5 clarification of Global Constraints allows this one field on a signed-off run). Files of rejected or trashed runs aren't expected on disk, so the index never lists them as `missing` (Task 10). The trash entry is purged 30 days later, so a rejected run leaves the disk 120 days after sign-off (CORE-032 clarification: "purged" means moved to trash).
  - The only hard deletes in `ww-core` are this purge, stale temps and settled backups (Task 6), empty staging handles (Task 7) and the index rebuild (Task 10).

- [ ] **Step 1: Write the failing tests:**
  - **`tests/versions.rs`:**
    - `seal_moves_files_and_writes_a_read_only_snapshot` (CORE-026);
    - `derived_from_files_are_copied_not_moved`;
    - `source_moves_with_the_version_and_is_outside_the_content_hash` (CORE-028);
    - `seals_of_one_asset_are_serialised`: two concurrent tasks each take the `version_lock` guard and seal `1.1.0` and `1.2.0`; both succeed, one after the other, and a second `1.1.0` → `VersionExists`;
    - `held_file_during_seal_is_retried` (`HandleHolder`, paused time);
    - `failed_seal_leaves_no_half_version`: `store.replace=io` midway → no `v<version>/`, and every file exists exactly once, in the run or the staging folder;
    - `seal_reports_post_move_identities` (CORE-041): every `Sealed.identities` entry equals a fresh `identity()` of the sealed file, read-only bit included;
    - `writes_into_sealed_folders_are_refused`;
    - `locked_paths_are_refused_with_store_locked` (CORE-035);
    - `legacy_folder_is_listed_as_legacy` (fixture `prop_signal_lamp`, CORE-033).
  - **`tests/trash.rs`:**
    - `trash_moves_with_a_restore_record` (ARCH-058, CORE-037);
    - `restore_refuses_an_occupied_path` (`StoreError::RestoreConflict` → `trash.restore_conflict`; an unknown `entry_id` → `TrashUnknown` → `trash.unknown`);
    - `trash_purges_after_30_days_only` (`FakeClock`: 29 days kept, 31 days purged);
    - `purge_of_a_read_only_payload_succeeds` (a trashed sealed file, still read-only, is purged after 31 days);
    - `restore_under_a_version_folder_reapplies_read_only` (CORE-026);
    - `rejected_runs_go_to_trash_90_days_after_signoff_unless_pinned` (CORE-032): the run gains `trashed_utc` and nothing else in it changes;
    - `hard_deletes_happen_only_in_purge_temp_cleanup_and_index_rebuild` (CORE-110): a source scan of `crates/ww-core/src` finds every `remove_file(` and `remove_dir_all(` token, with or without a path prefix (so a `use … fsops::remove_file;` followed by a bare call is found too), only in `store/fsops.rs` (the definitions), `store/trash.rs`, `store/write.rs` (temps, settled backups and `recover`), `store/staging.rs` (empty handle folders only) and `index/`; the test also fails if it finds none at all;
    - `trash_tools_list_and_restore`.
  - Remove `versions_trash::trash_keeps_restore_record_and_restores` from `expected-fail.toml`.
- [ ] **Step 2: Run them.**

Run: `cargo nextest run -p ww-core --test versions --test trash`
Expected: FAIL. Nothing is implemented.

- [ ] **Step 3: Implement it.**
- [ ] **Step 4: Run them, then the spec tests.**

Run: `cargo nextest run -p ww-core && cargo clippy -p ww-core --all-targets -- -D warnings && cargo run -q -p ww-cli -- dev spec-tests`
Expected: PASS; `spec tests: 42 passed, 24 expected-fail`.

- [ ] **Step 5: Commit.**

```bash
git add crates/ww-core crates/ww-spec-tests/expected-fail.toml
git commit -m "store: sealed read-only versions under a per-asset lock, legacy folders, locks, trash with restore and purges"
```

---

### Task 10: `index` and the asset read tools (ARCH-079, ARCH-089, ARCH-096, CORE-005, CORE-006, CORE-010, CORE-014, CORE-041, CORE-042, CORE-047) — **protected: `test-params.toml`**

**Files:**
- Create: `crates/ww-core/src/index/{mod.rs, schema.rs, reconcile.rs, query.rs}` and `crates/ww-core/src/tools/asset_read_tools.rs` (`assets.query`, `asset.get`).
- Create: `crates/ww-core/src/pipeline/{mod.rs, detail.rs}`: `asset.get`'s detail builder is the plain function `pipeline::asset_detail`, over `Store` and `Index`, taking no `tools` types, so Task 13's `asset:` snapshot provider (in `tools/`) can call it without a module cycle (CORE-010). `tools/asset_read_tools.rs` only parses arguments and calls it.
- Modify: `crates/ww-core/src/workspace.rs`. `OpenWorkspace` gains `pub index: Arc<Index>`; `open` opens it after the store and sets it as the store's `ChangeSink`. A `ReadWrite` workspace opens `index.db` in `workspace_local_dir`; a `ReadOnly` one gets `Index::open_in_memory` and never touches `index.db`, which on a non-system volume is shared with the core that holds `core.lock` (ARCH-097, CORE-005). The first reconcile (or cold rebuild) runs in the background, so `open` doesn't wait for it, but read tools do: they await `Index::ready()` (below). `workspace.rs` also runs the `verify_pending` tick (1 s tokio interval) and, from Task 11 on, hands its mismatches to the watcher.
- Modify (protected): `test-params.toml`.
  - Add `[budgets]` with `index_rebuild_2000_assets_s = 30` and `index_warm_reconcile_10000_assets_s = <about twice the local measurement, rounded up to whole seconds>`, with a comment naming CORE-042 and `crates/ww-core/tests/index_bench.rs`, saying that the value is twice the local measurement of this task, and that the owner checks it against the CI Windows runner's figure (printed in the PR's CI log) and adjusts it, if needed, when approving the PR (owner question 2). The CI run happens only after this commit, so this task doesn't wait for it.
  - Remove the stale header lines: the "# Trial: protected-file change used to test the approval flow …" line, and "Plan-1 code does not consume this file yet" (with its continuation). In their place, one comment says that `ww_testkit::params` reads `[proptest]` and `[budgets]`, and that `.config/nextest.toml` still carries the timeout values directly.
- Test: unit tests; `crates/ww-core/tests/{index.rs, index_bench.rs, module_deps.rs}`. Tests create fixture files with `ww_testkit::files::write_file`, never with `std::fs` in `crates/ww-core/src` (QA-024).

**Interfaces:**

Consumes: Task 6's `ChangeSink`, `StoreEvent`; Task 9's `Store::versions`; Task 7's `Access`; Task 3's `ManifestView` (whose `hashes` and run-file `sha256` are bare hex, whatever prefix the file has); Task 2's `Probe`.

Produces:

```rust
pub const INDEX_SCHEMA_VERSION: i64 = 1;
pub enum IndexError { Sql(String), Io(String), ActorGone }
pub enum Scope { All, Paths(Vec<WsPath>) }
pub enum FileState { Ok, NotFetched, CloudOnly, HumanEdit, NeedsRepair, Missing }
pub enum FileKind { Candidate, Sealed, Work, Manifest, Other }  // from the path alone; watch::reconcile uses this one (CORE-010)
pub fn kind_of(path: &WsPath) -> FileKind;
pub struct FileRow { pub path: WsPath, pub size: u64, pub sha256: Option<String>, pub state: FileState, pub identity: Option<FileIdentity> }
pub struct AssetRow { pub project: String, pub asset_id: String, pub asset_type: Option<String>, pub status: Option<String>,
                      pub version: Option<String>, pub approved_version: Option<String>, pub manifest: WsPath,
                      pub creation_uuid: Option<String>,
                      pub updated: i64 /* manifest last write, unix secs */, pub needs_repair: bool }
    // keyed by the folder (project, asset_id from the path); needs_repair also when the manifest's own asset_id or
    // project disagrees with the folder
pub enum SortField { AssetId, Updated, Status, Type }
pub enum SortDir { Asc, Desc }
pub struct AssetQuery { pub project: Option<String> /* a filter: None means every project */, pub status: Option<String>, pub asset_type: Option<String>,
                        pub text: Option<String>, pub sort: (SortField, SortDir), pub limit: usize, pub after: Option<Keyset> }
pub struct Keyset { pub sort_value: serde_json::Value, pub project: String, pub asset_id: String }
pub struct AssetPage { pub rows: Vec<AssetRow>, pub next: Option<Keyset>, pub total: u64 }
pub fn encode_cursor(q: &AssetQuery, k: &Keyset) -> String;          // lowercase hex of the JSON {"v":1,"q":<query hash>,"k":…}; no new dependency
pub fn decode_cursor(q: &AssetQuery, cursor: &str) -> Result<Keyset, String>;  // Err = params.invalid message
pub struct ReconcileReport { pub unchanged: usize, pub hashed: usize, pub changed: Vec<WsPath>, pub removed: Vec<WsPath>,
                             pub missing: Vec<WsPath>, pub cloud_only: Vec<WsPath>, pub needs_repair: Vec<WsPath>,
                             pub parked: Vec<WsPath> /* unstable or locked reads, re-checked later */ }
pub struct Mismatch { pub path: WsPath, pub sha256: String, pub recorded: String }
pub enum ReconcileOrder { AsWalked, ManifestsFirst }   // ManifestsFirst: every manifest is re-parsed before any other file
pub struct Index;                                    // one connection on its own thread ("ww-index-<id>")
impl Index {
    pub fn open(path: &Path, root: &Path, probe: Arc<dyn Probe>) -> Result<Index, IndexError>;
    pub fn open_in_memory(root: &Path, probe: Arc<dyn Probe>) -> Result<Index, IndexError>;   // ReadOnly workspaces: same
                                                     // schema over SQLite :memory:, a cold rebuild at open, no file
    pub async fn ready(&self);                       // resolves once the first reconcile or cold rebuild has finished
    pub async fn reconcile(&self, scope: Scope) -> Result<ReconcileReport, IndexError>;   // ManifestsFirst
    pub async fn reconcile_ordered(&self, scope: Scope, order: ReconcileOrder) -> Result<ReconcileReport, IndexError>;
    pub async fn rebuild(&self) -> Result<ReconcileReport, IndexError>;
    pub async fn retry_parked(&self) -> Result<ReconcileReport, IndexError>;
    pub async fn verify_pending(&self, max_files: usize) -> Result<Vec<Mismatch>, IndexError>;
        // hashes unverified rows; returns the files whose bytes differ from the trusted manifest hash and leaves them
        // unverified. The index never decides that a mismatch is a human edit (CORE-010): the watcher does (Task 11)
    pub async fn assets(&self, q: AssetQuery) -> Result<AssetPage, IndexError>;   // page and total in one read transaction
    pub async fn asset(&self, project: &str, asset_id: &str) -> Result<Option<AssetRow>, IndexError>;
    pub async fn files_of(&self, project: &str, asset_id: &str) -> Result<Vec<FileRow>, IndexError>;
    pub async fn dependents(&self, project: &str, asset_id: &str) -> Result<Vec<(String, String)>, IndexError>; // (project, asset_id)
    pub async fn set_state(&self, path: &WsPath, state: FileState) -> Result<(), IndexError>;
}
impl ChangeSink for Index {}
pub fn register_asset_read_tools(reg: &mut Registry);  // assets.query, asset.get
// pipeline/detail.rs (no tools types: CORE-010)
pub struct AssetSummary; pub struct AssetDetail;       // the Global Constraints shapes, Serialize
pub async fn asset_detail(store: &Store, index: &Index, ws_id: &str, project: &str, asset_id: &str) -> Result<AssetDetail, ErrorBody>;
pub fn file_state_wire(s: FileState) -> &'static str;   // ok, not_fetched, cloud_only, human_edit, needs_repair, missing
```

**Rules:**
- **Open:** `index.db` in `workspace_local_dir(ws)`, WAL, `synchronous=NORMAL` (it is disposable).
  - A `user_version` other than `INDEX_SCHEMA_VERSION`, a failed `quick_check` or a file that isn't a database means it is closed, removed with its `-wal`/`-shm` through `fsops::remove_file`, and created afresh with a cold rebuild (ARCH-089).
  - Tables: `files`, `assets`, `edges(from_project, from_asset, to_project, to_asset, to_version, kind)` and `assets_fts` (FTS5).
- **Reconcile per file** (CORE-041), walking the workspace without `.git/`, `.worldwright/`, `.trash/` and the temp, backup and conflict names. The walk never follows a reparse point (a junction inside the workspace could loop or lead outside it); `identity` already opens with `OPEN_REPARSE_POINT`. With `ManifestsFirst` (the default, and always after git, Task 11), every changed manifest is re-parsed before any other file is looked at, so candidate and sealed files are judged against the new manifests.
  - `probe.identity` first.
  - A cloud placeholder is `CloudOnly` and is never opened for reading (Review Focus 4).
  - With `volume_info(root).stable_ids()`, the file is unchanged when size, last write, FileId and ChangeTime all match the row. Otherwise (a network, FAT/exFAT or cloud-sync-root folder, a `volume_info` error, or a `None` FileId or ChangeTime) only size and last write are compared, FileId/ChangeTime churn is ignored, and a file whose last write is within 2 s of the previous scan's start is hashed anyway (FAT write times have 2-second resolution).
  - File IDs detect changes in place only and are never used to pair a file across folders: on ReFS a move across folders changes the ID, and the store's post-move identity (Tasks 7 and 9) is taken as is.
  - Anything else is hashed. File data is read only through `probe.open_read`.
  - **Stable reads** (ARCH-079 clarification): `identity` is taken again after the hash, and the digest counts only when size, last write and ChangeTime are unchanged. Otherwise, or when `open_read` fails with error 32 or 33, the path is parked: it keeps its old row (never `Removed`), goes on a bounded list (at most 4,096 paths; beyond that, the next rescan picks it up) and is re-checked by `retry_parked` after the next watcher batch or reconcile tick. A path still unstable after 5 re-checks is logged and left for the next rescan.
  - A manifest that changes is re-parsed into `assets` (with `creation_uuid`) and `edges`; one that doesn't parse, or whose `asset_id` or `project` disagrees with its folder (the folder name is authoritative), is `NeedsRepair` and its asset row is flagged. A folder copied in Explorer therefore gives two rows, one per folder, and only the copy is flagged; the manifest is never rewritten.
  - An LFS pointer's row keeps the pointer's `oid` as `sha256`, with state `NotFetched`.
  - **Removed files** (ARCH-079, CORE-038 clarification): a file the suite still expects on disk keeps its row and last SHA-256 with state `Missing`. Expected means: a manifest, a file listed in its manifest's `hashes`, or a file of a run whose `status` is `open` and that has no `trashed_utc`. Every other removed row is deleted, including files of `selected` and `rejected` runs (sign-off moved the chosen files into `v<version>/`, maintenance moved rejected runs to trash) and `work/` files. A file that appears at a `Missing` path with the same SHA-256 returns to `Ok`. When the manifest is the missing file, the asset row stays in `assets` with its last values.
- **Cold rebuild:** manifests are parsed, their `hashes` (read with or without the `sha256:` prefix, compared as bare hex) are trusted as the files' SHA-256 and marked unverified, and `verify_pending` hashes them in the background (at most 200 files per tick, on a 1 s tokio interval, driven from `workspace.rs`). Its mismatches are logged until Task 11, which runs `decide` on them.
- **`Index::ready()`**: resolves when the first reconcile, or the cold rebuild, finishes. `assets.query`, `asset.get` and `project.status` await it with no timeout of their own (the dispatcher's call deadline applies), and so does `test.watch_fence` (Task 11). A fresh open therefore never answers with an empty or partial page, and `damaged_index_is_rebuilt` is deterministic.
- **`ChangeSink::applied`:** takes the store's `sha256` and `identity` as is, without rehashing the file the store just wrote.
- **`assets.query`** implements the paging contract of Global Constraints: one keyset `SELECT … ORDER BY COALESCE(<sort>, ''), project, asset_id LIMIT n+1`, whose keyset comparison uses the same `COALESCE`, and one `count(*)` with the same filters, in one read transaction; `next_cursor` comes from the last row returned, and its `q` is the hash the Global Constraints define. Tool schemas aren't in `schema_hash`, so the golden doesn't change.
- **`assets.query`'s `project`** is a filter: omitted means every project (Global Constraints), so the session's active project never narrows it.
- **`asset.get`** is `pipeline::asset_detail`: it reads the manifest through the store, the versions through `Store::versions` (with `legacy`), the file states from the index (`FileState::NeedsRepair` is `needs_repair` on the wire), and `next_versions` from `approved_version` (or `1.0.0` when there is none). `proposed_bump` is `patch` when the open run's `target_version` says so, else `minor`. An asset whose manifest is `Missing` gives `manifest.needs_repair` with `data.missing`.

- [ ] **Step 1: Write the failing tests:**
  - **`tests/index.rs`:**
    - `fresh_index_is_wal_and_versioned` (ARCH-089);
    - `version_mismatch_or_failed_quick_check_rebuilds` (ARCH-089);
    - `reconcile_hashes_only_changed_files` (CORE-041; `hashed` counts);
    - `store_writes_feed_the_index_without_rehashing`;
    - **`volume_without_stable_ids_ignores_id_churn`** (Review Focus 4): the fake `Probe` returns a new FileId and ChangeTime on every call and `stable_ids() == false`; two reconciles, the second more than 2 s after the files' last writes → the second has `hashed == 0`. On a stable volume, a FileId change alone → hashed;
    - **`recent_mtime_is_hashed_on_volumes_without_stable_ids`** (Review Focus 4): on a fake FAT volume, a file rewritten with the same size and a last write 1 s before the scan is hashed, and its new content is indexed;
    - **`cloud_placeholder_is_indexed_but_never_read`** (Review Focus 4): the fake probe sets `FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS`; its `open_read` counter stays at 0; the state is `CloudOnly`;
    - **`refs_cross_folder_move_changes_id_without_a_false_change`** (Review Focus 4, CORE-041): a fake ReFS probe whose file IDs depend on the parent folder; a store move from `candidates/<run>/` to `v1.0.0/` reported through `ChangeSink::applied`, then a reconcile → `hashed == 0` and no row is `changed`;
    - `volume_info_error_counts_as_no_stable_ids` (the fake probe's `volume_info` fails; reconcile still works and compares size and last write only);
    - `unstable_read_is_parked_not_hashed` (ARCH-079): the fake probe reports a different last write after the read; the row keeps its old SHA-256, the path is in `parked`, and `retry_parked` with a stable probe indexes it;
    - `locked_read_is_parked_not_removed` (ARCH-079): `open_read` fails with error 32; the row is kept and parked, never removed;
    - `removed_recorded_files_are_missing_and_recover` (ARCH-079, CORE-038): a sealed file listed in `hashes`, a candidate file of an `open` run and a manifest are removed → `Missing` with their last SHA-256; an untracked `work/` file's row, and the rows of a `selected` run's and a `rejected` run's files, are deleted, never `Missing`; the sealed file put back with the same bytes is `Ok` again;
    - `read_only_workspace_uses_an_in_memory_index_and_leaves_index_db_alone` (ARCH-097, CORE-005): with a read-write hub serving the workspace, a second hub over the same local folder opens it `ReadOnly`; its `assets.query` lists the assets, and `index.db`'s bytes, its `-wal` and its mtime are unchanged, even when its `user_version` is made different first (no delete and rebuild);
    - `cold_rebuild_reads_hashes_with_or_without_prefix` (a manifest whose `hashes` mixes `sha256:<hex>` and bare `<hex>` values → no mismatch from `verify_pending`);
    - `manifest_disagreeing_with_its_folder_is_needs_repair` (CORE-047): a copy of `chr_paper_crane/` as `chr_paper_crane_copy/` → two asset rows; only the copy has `needs_repair`; both rows store the `creation_uuid`; neither manifest is rewritten;
    - `reconcile_never_follows_a_junction` (a junction inside the workspace pointing at a folder outside it adds no rows);
    - `manifests_first_reconcile_judges_files_against_the_new_manifest` (a candidate file and its manifest change together; with `ManifestsFirst`, the candidate's recorded hash in the index is the new one when the file is looked at);
    - `read_tools_wait_for_the_first_reconcile` (an `open` with a probe gated on a barrier; `assets.query` resolves only after the barrier is released, and then lists every asset);
    - `cold_rebuild_trusts_manifest_hashes_then_verifies` (CORE-041);
    - `manifest_inputs_become_edges_and_dependents`;
    - `invalid_manifest_marks_needs_repair`;
    - `machine_local_files_never_land_in_the_workspace` (ARCH-096, CORE-005, CORE-006): after open, writes and reconciles, no `*.db*` file exists under the workspace, and `index.db` is in `workspace_local_dir`;
    - `asset_get_reports_legacy_versions_and_next_versions` (and a `needs_repair` file state is `"needs_repair"` on the wire);
    - `assets_query_filters_by_project_status_type_and_text`;
    - `assets_query_pages_every_asset_exactly_once_in_order`: 2,500 assets in three projects with repeated `status`, `type` and `updated` values, 40 of them with no `type` and 40 with no `status`; for every sort field and direction, no `project` argument (a filter, so omitted means every project), `limit` 97, the pages joined equal the full list sorted by (sort value with a missing value as `""`, project, asset id), with no repeats and none missing; `total == 2500` on every page; the last page's `next_cursor` is `null`;
    - `assets_query_cursor_allows_a_new_limit` (page 2 with `limit` 50 from a cursor made with `limit` 97 continues exactly after page 1);
    - `assets_query_cursor_survives_an_unrelated_write`: with the default sort, after page 1, a store write to an asset outside the remaining pages, a manifest edit that doesn't change the sort value, and a `rebuild()`; page 2 from the old cursor is exactly what it would have been;
    - `assets_query_cursor_of_another_query_is_params_invalid` (a different filter, a different sort, and a truncated cursor).
  - **`tests/module_deps.rs`:** `module_dependencies_are_acyclic_and_store_depends_on_no_consumer` (CORE-010, CORE-014). It scans `use crate::<module>` in each CORE-014 module's files and asserts:
    - the graph is acyclic;
    - `store` uses none of `index`, `watch`, `graph`, `pipeline`, `projects`, `settings`, `tools`;
    - `index` uses none of `watch`, `pipeline`, `projects`, `settings`, `tools`;
    - `pipeline` uses no `tools` (the `tools` → `pipeline` direction only; Task 13 keeps it so).
  - **`tests/index_bench.rs`** (CORE-042; `std::time::Instant`, the stated exception to QA-031; budgets read through `ww_testkit::params::budget`; each prints its measured time):
    - `cold_rebuild_of_2000_assets_fits_the_budget`: 2,000 assets × 3 files in a tempdir; times `rebuild()` against `index_rebuild_2000_assets_s`;
    - `warm_reconcile_of_10000_assets_fits_the_budget` (research 5A-8): 10,000 assets × 3 files, indexed once; times a second `reconcile(All)` with nothing changed (stats only, `hashed == 0`) against `index_warm_reconcile_10000_assets_s`. The committed budget is about twice this task's local measurement; the owner checks the CI runner's figure at PR time (above).
    - Both run under nextest's per-test cap for `ww-core` in `.config/nextest.toml` (protected, not changed by this plan). If either benchmark, with its setup, takes more than half of that cap locally, stop and report it instead of changing the file.
  - Remove `index_watch::damaged_index_is_rebuilt` from `expected-fail.toml`.
- [ ] **Step 2: Run them.**

Run: `cargo nextest run -p ww-core --test index --test module_deps --test index_bench`
Expected: FAIL. Nothing is implemented.

- [ ] **Step 3: Implement it**, with the protected `test-params.toml` edit.
- [ ] **Step 4: Run them, then the spec tests.**

Run: `cargo nextest run -p ww-core && cargo clippy -p ww-core --all-targets -- -D warnings && cargo run -q -p ww-cli -- dev spec-tests`
Expected: PASS; `spec tests: 43 passed, 23 expected-fail`. The report quotes both benchmarks' measured times and the machine class, the warm budget written to `test-params.toml` (about twice the local time), and a reminder that the owner checks it against the CI runner's figure at PR time.

- [ ] **Step 5: Commit.**

```bash
git add crates/ww-core test-params.toml crates/ww-spec-tests/expected-fail.toml
git commit -m "index: disposable index.db reconciled by path/size/mtime/FileId/ChangeTime, cloud-safe; asset read tools; rebuild benchmark"
```

---

### Task 11: `watch`, the git gate and `ww notify` (ARCH-016, ARCH-073, ARCH-079, ARCH-083, ARCH-085, CORE-034)

**Files:**
- Create: `crates/ww-platform/src/watcher.rs` (`ReadDirectoryChangesW` on its own thread, overlapped and completed with `GetOverlappedResult`, stopped with `CancelIoEx`).
- Create: `crates/ww-core/src/watch/{mod.rs, reconcile.rs, git.rs}`, `crates/ww-core/src/store/git_gate.rs` and `crates/ww-core/src/tools/watch_tools.rs`.
- Modify: `store/write.rs` (step 2 waits on the gate, which also checks `.git\index.lock` itself), `workspace.rs` (`OpenWorkspace.watch`, started for `ReadWrite` workspaces and stopped by `close`; the `verify_pending` tick hands its mismatches to the watcher), `tools/test_tools.rs` (`test.watch_fence`, which awaits `Index::ready()` first), and `tools/tests.rs` (`test_tools_exist_only_on_test_channels`'s `test.*` name list gains `test.watch_fence`).
- Modify: `crates/ww-core/src/config.rs` (`CoreConfig` gains `pub watch: WatchConfig`, the defaults below in `CoreConfig::for_env`) and `server/mod.rs`, which passes it to `WorkspaceHub::new`. `WorkspaceHub::new` gains a last parameter `watch: WatchConfig`; its callers (the server and the hub tests of Tasks 7–10) pass `WatchConfig::default()` unless a test needs another one.
- Create: `crates/ww-cli/src/user/notify.rs`. Modify `user/mod.rs`.
- Test: `crates/ww-platform/tests/watcher.rs`; unit tests; `crates/ww-core/tests/watch.rs`; `crates/ww-cli/tests/notify_cli.rs`. `ww-core` tests create fixture files with `ww_testkit::files::write_file`, never with `std::fs` in `crates/ww-core/src` (QA-024).

**Interfaces:**

Consumes: Task 10's `Index`, `FileKind`, `Mismatch`; Task 6's `Store::edit_manifest` and `Store::accepts_as_own`; Task 2's `fs::attributes` and `fs::long_name`.

Produces:

```rust
// ww-platform watcher.rs
pub enum DirChange { Added(PathBuf), Removed(PathBuf), Modified(PathBuf), RenamedFrom(PathBuf), RenamedTo(PathBuf), Overflow,
                     Failed(i32) }  // a completion error other than ERROR_OPERATION_ABORTED (our own cancel): the root was
                                    // renamed or removed, or its volume went away; the thread ends after sending it
pub struct DirWatcher;          // FILE_NOTIFY_CHANGE_{FILE_NAME,DIR_NAME,SIZE,LAST_WRITE,ATTRIBUTES}, subtree; relative paths
impl DirWatcher { pub fn start(root: &Path, buffer_bytes: u32) -> io::Result<(DirWatcher, tokio::sync::mpsc::UnboundedReceiver<DirChange>)>;
                  pub fn stop(&self); }
    // The root is opened with FILE_LIST_DIRECTORY, share read|write|delete, BACKUP_SEMANTICS|OVERLAPPED, so the
    // watch never blocks renames. The buffer is DWORD-aligned. Success with 0 bytes, or ERROR_NOTIFY_ENUM_DIR → Overflow.
    // start returns the error when the redirector or file system can't watch (ERROR_INVALID_FUNCTION), or when
    // buffer_bytes is over 64 KiB on a network folder (ERROR_INVALID_PARAMETER); buffer_bytes > 65536 is refused up front
// store/git_gate.rs (store owns it; watch sets it: CORE-010)
pub const GIT_WAIT: Duration = Duration::from_secs(60);
pub struct GitGate;
impl GitGate { pub fn new(root: &Path) -> GitGate;
               pub fn set_busy(&self, busy: bool);
               pub fn is_busy(&self) -> bool;     // the watcher's flag, or <root>\.git\index.lock exists (ww_platform::fs::attributes)
               pub async fn wait_clear(&self, budget: Duration) -> Result<(), StoreError>; }
    // polls is_busy every 250 ms (tokio time); GitBusy after the budget. Works with or without a watcher
impl Store { pub fn git_gate(&self) -> &Arc<GitGate>; }
// watch
pub struct WatchConfig { pub buffer_bytes: u32 /* 65536 */, pub batch: Duration /* 200 ms */,
                         pub git_quiet: Duration /* 2 s: rebase and pull release and retake index.lock between steps */ }
impl Default for WatchConfig {}                        // the values in the comments
// CoreConfig gains `pub watch: WatchConfig`; WorkspaceHub::new(env, states, events, clock, ids, failpoints, probe, watch)
pub struct Watcher;
impl Watcher {
    pub fn start(store: Arc<Store>, index: Arc<Index>, events: Arc<EventBus>, topic_ws: String, cfg: WatchConfig) -> io::Result<Arc<Watcher>>;
    pub async fn notify(&self, paths: Vec<WsPath>) -> NotifyReport;
    pub async fn fence(&self, name: &str);              // returns once `.worldwright/fence/<name>` and everything before it is handled
    pub fn stop(&self);
}
pub struct NotifyReport { pub reconciled: Vec<String>, pub ignored: Vec<String> }
// watch/reconcile.rs: pure, shared with the simulated QA-046 variant (Task 14). FileKind is index::FileKind
pub struct Recorded { pub sha256: String /* bare hex */, pub kind: FileKind, pub manifest: Option<WsPath>,
                      pub expected: bool /* still expected on disk: a manifest, a file in `hashes`, or a file of an
                                            open run without trashed_utc (Task 10's Missing rule) */,
                      pub pending: Vec<String> /* old and new hashes of a pending store replace (Task 6) */,
                      pub last_human_edit: Option<String> /* the latest human_edits[path].sha256 */ }
pub enum Observed { Changed { path: WsPath, sha256: String, lfs_oid: Option<String> }, CloudOnly(WsPath), Removed(WsPath), BadManifest(WsPath) }
    // only stable reads become Observed: unstable or locked reads are parked by the index (Task 10) and never reach decide
pub enum Action { Reindex(WsPath), HumanEdit { manifest: WsPath, path: WsPath, sha256: String, previous: String },
                  NeedsRepair(WsPath), NotFetched(WsPath), Missing(WsPath), DeferUntilGitDone(WsPath) }
pub fn decide(obs: &Observed, recorded: Option<&Recorded>, git_busy: bool) -> Action;
pub fn map_notify_path(rel: &str, resolve: &dyn Fn(&Path) -> io::Result<PathBuf>) -> Result<WsPath, WsPath /* parent to reconcile */>;
    // a component containing '~' may be an 8.3 short name: it is resolved with `resolve` (fs::long_name); when it can't
    // be resolved (the file is gone), the nearest parent that maps to a WsPath is returned for a reconcile instead
pub fn register_watch_tools(reg: &mut Registry);       // watch.notify
// ww-cli
pub enum UserCommand { /* … */ Notify { #[arg(required = true)] paths: Vec<PathBuf> } }
```

**`decide`** (ARCH-079, CORE-034, ARCH-085). The rows are checked in order and the first match wins, so the table is exhaustive:

| Observed | Recorded | git busy | Action |
|---|---|---|---|
| any | any | yes | `DeferUntilGitDone` (decided by the rescan after git) |
| `CloudOnly` | any | no | `Reindex` (state `CloudOnly`, not read) |
| `BadManifest` | any | no | `NeedsRepair` (never rewritten) |
| `Removed` | Candidate, Sealed or Manifest, and `expected` | no | `Missing` (row and last SHA-256 kept) |
| `Removed` | anything else, or none | no | `Reindex` (row removed: a file of a `selected`, `rejected` or trashed run, a `work/` file, or an untracked one) |
| `Changed` | Manifest | no | `Reindex` (a valid manifest is re-parsed; outside manifest edits are allowed) |
| `Changed` | Work, Other or none | no | `Reindex` |
| `Changed`, `lfs_oid == Some(recorded)` | Candidate/Sealed | no | `NotFetched` |
| `Changed`, `lfs_oid` is `Some` and differs from recorded | Candidate/Sealed | no | `HumanEdit` (with the pointer's `oid` as `sha256`) |
| `Changed`, `sha256 == recorded`, or `sha256` in `pending` | Candidate/Sealed | no | `Reindex` (the core's own bytes, write step 5) |
| `Changed`, `sha256 == last_human_edit` | Candidate/Sealed | no | `Reindex` (already recorded, never pushed twice) |
| `Changed`, any other `sha256` | Candidate/Sealed | no | `HumanEdit` (the file is never touched) |

The recorded hash of a candidate or sealed file is its manifest's `hashes` entry (bare hex: the `sha256:` prefix of I5 is stripped, and an unprefixed value is accepted too), else its run's file record (`candidate_runs[].files[].sha256`), else its last generated hash. A `Missing` path that reappears with its recorded SHA-256 is `Changed` with `sha256 == recorded`, so it goes back to `Ok` through `Reindex`.

**Watcher rules:**
- The watcher uses `ReadDirectoryChangesW`, not `ReadDirectoryChangesExW`: the extended form is documented as NTFS-only, and every changed path is re-read through `probe.identity` anyway. The default buffer is 64 KiB, the largest a network folder accepts.
- **No watcher where watching is unsupported.** If `DirWatcher::start` fails (the fail point `watch.start=unsupported`, hit by `WorkspaceHub::open` just before it starts the watcher, simulates it), the workspace is still served: the store's hash check still refuses to overwrite outside edits, outside edits are found by `ww notify` and by the reconcile at open, `{kind:"watcher_unavailable", reason}` is published once on `workspace`, and `workspace.doctor` (Task 14) reports `workspace.watcher` as a `problem`. Writes still wait for git in that state, because the gate checks `index.lock` itself.
- **Losing the watched root.** `ReadDirectoryChangesW` doesn't report changes to the watched folder itself. When the pending read completes with any error other than our own cancel (`DirChange::Failed`: the root was renamed or removed, or a USB volume went away), the watcher publishes `watcher_unavailable` with the code and drops to the no-watcher mode above.
- Changes are batched for `batch` (tokio time). After each batch, `index.retry_parked()` re-checks the paths parked by unstable or locked reads (Task 10).
- **Directory events.** A folder rename, move or add produces one `RenamedTo` or `Added` for the folder, not one per file (a `seal`'s `move_new` of the version folder, a `git checkout` that adds a folder, an Explorer move). Any event whose path is a folder schedules `index.reconcile(Paths([dir]))`, which walks the folder's whole subtree; a `Removed` or `RenamedFrom` folder marks the rows under it as removed (recorded files become `Missing`).
- **Short names.** A `FILE_NOTIFY_INFORMATION` name may be the short or the long name ("it is unspecified which one", Microsoft Learn). Each event path goes through `map_notify_path` with `fs::long_name`; an unresolvable short component reconciles its parent folder instead.
- `.worldwright\staging\` and `.trash\` are ignored. Of `.worldwright\` only `fence\` is watched; of `.git\` only `index.lock` and `HEAD`.
- **The suite's own moves are never `missing`** (ARCH-079, CORE-038 clarification). A `Removed` or `RenamedFrom` event under an asset folder whose `version_lock` is held (a sign-off is between `seal` and its Record stage) is deferred: the watcher waits for the lock on its own task (it takes the lock and drops it at once), so other events keep flowing, and then reconciles that asset folder against the manifest the sign-off wrote. A fence covers the deferred work too. Maintenance moves only files of `rejected` runs, which aren't `expected`, so their removal is a plain `Reindex`.
- **Git** (ARCH-083):
  - `index.lock` created, or `HEAD` changed → `git_gate.set_busy(true)` and `{kind:"git_paused"}`;
  - `index.lock` gone and `HEAD` unchanged for `git_quiet` (2 s) → the post-git sequence, in this order:
    1. a full rescan, `index.reconcile_ordered(All, ManifestsFirst)`, which re-parses every manifest first and only then looks at every other file (index bookkeeping, not a store write);
    2. `decide` for every deferred and every changed path against the new manifests (with `git_busy = false`); `Reindex` and `NotFetched` are applied to the index here, and the actions that write or publish (`HumanEdit`, `NeedsRepair`, `Missing`) are queued;
    3. publish `{kind:"git_resumed"}`;
    4. `set_busy(false)`;
    5. apply the queued actions. `HumanEdit` writes through `edit_manifest`, whose step 2 would wait while the flag is set, so it must come after step 4: a human edit found by the post-git rescan is recorded at once, never after a 60 s wait ending in `store.git_busy`. `git_resumed` is published before any write can resume, so a client sees it before the response of a write that was waiting.

  Store writes wait in `wait_clear(GIT_WAIT)` and fail with `store.git_busy` after 60 s.
- **Overflow** → the same two-phase full rescan and `decide` (git isn't busy, so the actions are applied at once), and `{kind:"rescan"}`.
- **`verify_pending` mismatches** (from the cold-rebuild tick in `workspace.rs`) become `Observed::Changed` and go through `decide`; the index never records a human edit itself (CORE-010).
- `HumanEdit` → `edit_manifest` pushes `{path, sha256, previous_sha256, date}` to `human_edits` (I5's field names; `date` is the UTC `YYYY-MM-DD` of the detection) (a human edit is offered as a new version; the UI is plan 4), then `asset:` `{kind:"human_edit"}`. Because `decide` treats a hash equal to the latest `human_edits[path].sha256` as recorded, the same edit is pushed once, however often it is re-observed (`ww notify`, the reconcile at open, an overflow rescan, `verify_pending` after a cold rebuild). `NeedsRepair` → `index.set_state`, then `asset:` `{kind:"needs_repair"}`. `Missing` → `index.set_state`, then `asset:` `{kind:"missing", path}`.
- **`ww notify <path>…`** maps each path to the session workspace's relative path (with `folder_key`); a path outside it goes to `ignored`. It calls `watch.notify` and prints nothing on success. With `--no-start` and no core, it is a silent no-op (ARCH-018, ARCH-103).

- [ ] **Step 1: Write the failing tests:**
  - **`ww-platform/tests/watcher.rs`:**
    - `dir_watcher_reports_create_modify_rename_delete`;
    - `tiny_buffer_overflows_into_an_overflow_event` (4096 bytes, 2000 renames);
    - `oversized_buffer_is_refused_up_front` (65537 bytes);
    - `watched_root_can_still_be_renamed_into` (a file is renamed inside the watched tree, and a sibling folder is renamed, while the watch runs);
    - `removing_the_watched_root_ends_with_failed` (the watched folder is renamed away, or removed once empty → `DirChange::Failed`, then the channel closes);
    - `stop_ends_the_thread` (a stop is a cancel, never a `Failed`).
  - **Unit tests:**
    - `decide_follows_the_table` (every row, including a pending hash, a repeated human-edit hash, a mismatched LFS `oid`, and a `Removed` candidate file with `expected` true and false);
    - `git_gate_waits_up_to_60_s_then_git_busy` (paused time: 59 s pending, 61 s → `GitBusy`);
    - `git_gate_sees_index_lock_without_the_watcher` (ARCH-083: the flag is never set; with `.git\index.lock` present the gate is busy, and a write waits until the file is removed);
    - `notify_maps_paths_and_ignores_outside_ones`;
    - `short_name_component_is_resolved_or_parent_reconciled` (`map_notify_path` with a fake resolver: `ASSETS~1` resolves; an unresolvable `CHR_PA~1` gives the parent);
    - `watcher_failure_drops_to_no_watcher_mode` (a fake `DirChange::Failed` → one `watcher_unavailable` event; writes keep working).
  - **`ww-core/tests/watch.rs`** (real NTFS tempdir, fence):
    - `outside_edit_to_candidate_or_sealed_file_is_recorded_never_overwritten` (CORE-034);
    - `work_folder_edits_are_indexed_not_human_edits` (ARCH-079);
    - `staging_trash_and_git_internals_are_not_watched` (ARCH-079);
    - `invalid_manifest_is_needs_repair_and_edits_refuse` (`manifest.needs_repair`; the bytes are unchanged);
    - `lfs_pointer_with_matching_oid_is_not_fetched` (ARCH-085);
    - `index_lock_pauses_store_writes_until_the_rescan` (ARCH-083);
    - `folder_moved_into_the_workspace_is_indexed` (Review Focus 5): a folder with 20 files, two levels deep, is moved in from outside the workspace in one rename; after the fence, `index.files` has all 20;
    - `hash_of_a_file_being_written_is_not_a_human_edit` (ARCH-079): a writer appends to a sealed file in chunks while the fence runs; no `human_edits` entry appears until the writer has finished and the content differs, and then exactly one;
    - `locked_file_is_parked_not_removed` (ARCH-079): `HandleHolder::hold(sealed file, Share::None)` while it changes; after the fence the file is neither `Missing` nor a human edit; after release and another fence, the edit is recorded;
    - `human_edit_is_recorded_once_across_notify_rescan_and_rebuild` (CORE-034): one outside edit, then `watch.notify`, a forced overflow rescan, a restart with a damaged `index.db` (cold rebuild and `verify_pending`) → `human_edits` has one entry for the path;
    - `removed_sealed_file_is_missing_then_recovers` (ARCH-079, CORE-038): clear a sealed file's read-only bit and delete it → `asset.get` `files[].state == "missing"` and an `asset:` `{kind:"missing"}` event; put the same bytes back → `ok`, and no human edit;
    - `removals_under_a_held_version_lock_wait_for_its_release` (ARCH-079, CORE-038): the test takes an asset's `version_lock`, moves an open run's candidate file into a new `v1.1.0/` and edits the manifest so the run is `selected` and `hashes` lists the moved file, then releases the lock; after the fence the old path is gone from the index (never `missing`), the new one is `ok`, and no `{kind:"missing"}` event was published. A removal under an asset whose lock is free is decided at once;
    - `post_git_human_edit_is_recorded_without_waiting_for_the_gate` (ARCH-083, CORE-034; paused time): while `index.lock` exists, a sealed file is changed; after the lock is removed and `git_quiet` passes, the `human_edits` entry appears without `store.git_busy` and well inside the 60 s wait, and `git_resumed` was published before it;
    - **`unsupported_watcher_leaves_the_workspace_served`** (Review Focus 4): a core with `watch.start=unsupported` opens the workspace, `test.store_write` works, a `watcher_unavailable` event arrived on `workspace`, and an outside edit followed by `ww notify` records the human edit;
    - **`checkout_of_3000_files_overflows_then_rescans_without_false_human_edits`** (Review Focus 5). In an in-process core whose `CoreConfig.watch` has `buffer_bytes = 4096`, `GitOps` rewrites 2,985 plain files, 5 sealed files and their manifests with matching new `hashes`, and 5 candidate files whose manifests' run records gain their new hashes, then finishes. The candidate files matter: `candidates/` sorts before `manifest.json`, so a rescan that decided files in walk order would judge them against the old manifest. After the fence:
      - a `rescan` and a `git_resumed` event arrived;
      - `index.files` has every file at its new SHA-256;
      - no manifest gained a `human_edits` entry;
      - a `test.store_write` after it succeeds.
  - **`ww-cli`:** `notify_parses`; `tests/notify_cli.rs` `notify_no_start_is_silent_without_core`.
  - Remove from `expected-fail.toml`: `outside_edit_to_sealed_file_is_a_human_edit`, `invalid_manifest_is_marked_needs_repair_and_kept`, `writes_wait_while_git_holds_index_lock`, `lfs_pointer_is_not_fetched_not_corrupt`, `ww_notify_reconciles_and_is_silent_without_core`.
- [ ] **Step 2: Run them.**

Run: `cargo nextest run -p ww-platform --test watcher && cargo nextest run -p ww-core --test watch`
Expected: FAIL. Nothing is implemented.

- [ ] **Step 3: Implement it.**
- [ ] **Step 4: Run them, then the spec tests.**

Run: `cargo nextest run -p ww-platform -p ww-core -p ww-cli && cargo clippy --workspace --all-targets -- -D warnings && cargo run -q -p ww-cli -- dev spec-tests`
Expected: PASS; `spec tests: 48 passed, 18 expected-fail`.

- [ ] **Step 5: Commit.**

```bash
git add crates/ww-platform crates/ww-core crates/ww-cli crates/ww-spec-tests/expected-fail.toml Cargo.lock
git commit -m "watch: ReadDirectoryChangesW with overflow rescan, human edits recorded never overwritten, git pause, ww notify"
```

---

### Task 12: Settings cascade, projects, `progress.md` and the active project (ARCH-088, ARCH-098, CORE-039, CORE-048, CORE-049, CORE-054, CORE-055, CORE-057, CORE-058)

**Files:**
- Create: `crates/ww-core/src/settings/mod.rs`, `crates/ww-core/src/projects/{mod.rs, progress.rs, active.rs, staleness.rs}`, `crates/ww-core/src/graph/mod.rs` and `crates/ww-core/src/tools/project_tools.rs`.
- Modify: `tools/stages.rs` (`Settings`, `SettingsStage`, `PipelineHooks::plan2` uses `CascadeSettings`), `tools/{registry.rs, dispatcher.rs, mod.rs, test_tools.rs}` (`CallCtx.project`; `test.settings`; `test.ingest_run` takes its omitted `project` from `CallCtx.project` instead of Task 7's `sketchbook` default), and `rpc.rs` (`ActiveProjects::forget` in `Router::closed`).
- Modify: `crates/ww-core/src/tools/tests.rs` (plan 2a's unit tests): the `Recording` hooks' `SettingsStage` impl is ported to the async `resolve(SettingsInput)`, and `test_tools_exist_only_on_test_channels`'s `test.*` name list gains `test.settings`.
- Test: unit tests; `crates/ww-core/tests/{projects.rs, settings.rs}`.

**Interfaces:**

Consumes: Task 3's `WorkspaceYaml`, `ProjectYaml` (with `overrides` extracted from the top-level cascade keys), `CASCADE_KEYS`, `PROGRESS_TEMPLATE`, `new_project_yaml`, `transition`; Task 6's `Store::{edit_yaml, edit_text, write}`; Task 7's `Store::orphans`; Task 10's `Index::{ready, dependents, files_of}`.

Produces:

```rust
// settings
pub enum SettingSource { Workspace, Project, Request }
pub struct Settings { pub values: BTreeMap<String, serde_json::Value>, pub sources: BTreeMap<String, SettingSource> }
impl Settings { pub fn get(&self, key: &str) -> Option<&serde_json::Value>; pub fn model_for(&self, task: &str) -> Option<&str>;
                pub fn auto_approve(&self) -> Vec<String>; pub fn skip_stages(&self) -> Vec<String>;
                pub fn auto_candidate_on_save(&self) -> bool; pub fn modules(&self) -> Vec<String>; pub fn qa_profile(&self) -> Option<&str>; }
pub const SUITE_LEVEL_KEYS: &[&str] = &["caps", "currency", "fixed_costs", "psu_efficiency", "power_baseline", "electricity_rate", "assistant_engine"];
pub const UNSKIPPABLE_STAGES: &[&str] = &["check", "record"];
pub fn cascade(ws: &WorkspaceYaml, project: Option<&ProjectYaml>, request: Option<&serde_json::Map<String, Value>>)
    -> Result<(Settings, Vec<SchemaWarning>), String>;       // Err = params.invalid message
// tools/stages.rs (changed)
pub struct SettingsInput<'a> { pub session: &'a Session, pub descriptor: &'a ToolDescriptor, pub workspace: Option<&'a OpenWorkspace>,
                               pub project: Option<&'a str>, pub request: Option<&'a serde_json::Map<String, Value>> }
pub trait SettingsStage: Send + Sync {
    fn resolve<'a>(&'a self, input: SettingsInput<'a>) -> Pin<Box<dyn Future<Output = Result<Settings, ErrorBody>> + Send + 'a>>;
}
pub struct CascadeSettings;
// CallCtx gains `pub project: Option<String>` (resolved; None without a workspace)
// projects
pub struct ActiveProjects;
impl ActiveProjects { pub fn get(&self, session_id: &str) -> Option<String>; pub fn set(&self, session_id: &str, slug: &str);
                      pub fn forget(&self, session_id: &str); }
pub fn resolve_project(explicit: Option<&str>, active: Option<String>, hint: Option<String>) -> String;   // else "sketchbook"
pub async fn write_hint(store: &Store, slug: &str) -> Result<(), StoreError>;      // .worldwright/state.json
pub fn add_done_entry(text: &str, date: &str, entry: &str) -> Result<String, String>;  // the line directly after "## Done";
                                                                                    // the new line uses the file's line ending
pub async fn restore_staleness(ws: &OpenWorkspace, project: &str) -> Result<Vec<String /* ref */>, StoreError>;
pub struct ProjectStatus;                               // the project.status result, Serialize
pub async fn project_status(ws: &OpenWorkspace, project: &str) -> Result<ProjectStatus, ErrorBody>;
    // a plain function (no tools types), so tools/project_tools.rs and Task 13's project: snapshot provider share it
// graph (pure)
pub fn is_stale(input_version: &Version, approved: Option<&Version>, pinned: bool) -> bool;
pub fn cycles(edges: &[((String, String), (String, String))]) -> Vec<Vec<(String, String)>>;
pub fn register_project_tools(reg: &mut Registry, tools: Arc<OnceLock<Vec<ToolDescriptor>>>);   // filled by core_registry at the end
```

**Rules:**
- **Settings cascade** (CORE-057):
  - workspace `defaults` → `project.yaml` → the request's `settings` object: a plain override chain over dotted leaf keys (`models.<task>`, `providers.<…>`, `qa_profile`, `auto_approve`, `skip_stages`, `auto_candidate_on_save`, `modules`);
  - **where the project layer comes from** (I4, CORE-057 clarification): `project.yaml`'s own top-level keys, as Task 3's `ProjectYaml.overrides` extracts them. There is no `overrides:` key. `pipeline.skip_stages` and `pipeline.auto_approve` are read as `skip_stages` and `auto_approve`, and the workspace's `defaults:` uses the same shape. Keys that aren't cascade keys (`name`, `slug`, `type`, `status`, `links`, `created`, …) are never settings;
  - **where the request layer comes from** (CORE-057 clarification): `ToolCallParams` has only `name`, `arguments`, `idempotency_key` and `trace_id`, and product tools reject unknown fields. So `SettingsInput.request` is the `settings` argument of a tool whose input schema declares one, which in 2b is only `test.settings`; every product tool passes `None`. Nothing on the wire changes, so `schema_hash` is unchanged. A per-call `settings` field in `tools/call` would change `schema_hash`; it is left to plan 3 (Not in this plan);
  - lists replace whole lists;
  - a request key from `SUITE_LEVEL_KEYS` or an unknown key → `params.invalid` ("… is a suite-level setting");
  - `check` or `record` in `skip_stages` is dropped with a `SchemaWarning` (CORE-054);
  - `auto_candidate_on_save` defaults to `false`, and nothing in this plan acts on it (CORE-049).
- **Resolve** (ARCH-088, ARCH-098). The project is chosen in this order:
  1. the `project` argument;
  2. `ActiveProjects` for the session;
  3. `state.json`'s hint;
  4. `sketchbook`.

  A call that names a project and ends `ok` sets the session's active project and, on a `ReadWrite` workspace, writes the hint. `assets.query` is the exception: its `project` is a filter (omitted means every project), so it is never filled in from the session and never sets the active project.
- **Project tools** (CORE-055):
  - `project.create` writes `project.yaml` (from the template), then `progress.md` with the entry `- <date>: project created` → `project.exists` / `params.invalid` (bad slug);
  - `project.set_status` edits `status` with `edit_yaml`. Moving from `archived` or `paused` to `active` runs `restore_staleness`: each asset of the project whose input's `approved_version` is newer than its ref and isn't pinned goes `MarkStale` (manifest `status_before_stale`), and is listed in `stale`. Archived projects are skipped when dependents are scanned (I6);
  - `project.status` is `projects::project_status`: it awaits `Index::ready()`, then gives counts by status, `orphans` (from `Store::orphans`), `needs_repair` and `missing` (files in state `Missing`);
  - `project.context` gives the settings with sources, `links`, and `suggested_tools`: the tools whose module is `workspace` or in `modules()` (CORE-058).
- **`progress.md`** (CORE-039 clarification): the new entry goes on the line directly after `## Done`, newest first, so the template's blank line stays between the last entry and `## Next`; every other section is left byte for byte. A file without `## Done` gets `## Done` and the entry appended at the end. A CRLF file stays CRLF.
- **`project.yaml` edits** go to keys that `new_project_yaml` always writes (`status: active`, `links: []`): `project.set_status` is a `SetScalar` of `status`, and Task 13's `asset.pin_input` an `AppendToList` of `links`. A hand-written `project.yaml` without the key gets it appended by `edit_yaml`; one where the edit can't be made safely fails with `yaml.unsupported_edit` and changes nothing.

- [ ] **Step 1: Write the failing tests:**
  - **`tests/settings.rs`:**
    - `cascade_is_a_plain_override_chain` (CORE-057: `defaults: {models: {image_generate: a}}`, a top-level `models: {image_generate: b}` in `project.yaml`, and `pipeline: {skip_stages: [export]}` there giving `skip_stages`);
    - `request_cannot_set_suite_level_or_unknown_keys`;
    - `product_tools_resolve_settings_without_a_request_layer` (CORE-057: a product tool's `Settings` has no `Request` source, and its schema has no `settings` argument);
    - `check_and_record_cannot_be_skipped` (CORE-054);
    - `auto_candidate_on_save_defaults_off_and_has_working_doc_survives_edits` (CORE-049).
  - **`tests/projects.rs`:**
    - `resolve_explicit_then_session_then_hint_then_sketchbook` (ARCH-088, ARCH-098);
    - `active_project_is_forgotten_when_the_connection_closes`;
    - `hint_is_written_only_on_read_write_workspaces`;
    - `progress_entry_goes_first_under_done_and_other_sections_are_untouched` (CORE-039), for an LF and a CRLF file: the entry is the line directly after `## Done`, a second entry goes above the first, and the blank line before `## Next` is kept;
    - `project_create_refuses_existing_and_bad_slugs`;
    - `restore_marks_unpinned_dependents_stale_with_status_before_stale` (CORE-055, CORE-048);
    - `archived_projects_are_skipped_in_dependents`;
    - `project_context_suggests_tools_of_enabled_modules` (CORE-058);
    - `project_status_lists_orphans_needs_repair_and_missing` (CORE-038; a removed sealed file is in `missing`);
    - `graph_reports_cycles`.
  - Remove the five `projects_settings.rs` entries and `store_order::crash_after_output_lists_orphan_and_keeps_files` from `expected-fail.toml`.
- [ ] **Step 2: Run them.**

Run: `cargo nextest run -p ww-core --test settings --test projects`
Expected: FAIL. Nothing is implemented.

- [ ] **Step 3: Implement it.**
- [ ] **Step 4: Run them, then the spec tests.**

Run: `cargo nextest run -p ww-core && cargo clippy -p ww-core --all-targets -- -D warnings && cargo run -q -p ww-cli -- dev spec-tests`
Expected: PASS; `spec tests: 54 passed, 12 expected-fail`.

- [ ] **Step 5: Commit.**

```bash
git add crates/ww-core crates/ww-spec-tests/expected-fail.toml
git commit -m "core: settings cascade, project tools, progress.md entries, per-session active project, staleness on restore"
```

---

### Task 13: Asset tools, the Record stage, and live topics with snapshots (ARCH-032, ARCH-042, ARCH-044, CORE-019, CORE-020, CORE-023, CORE-025, CORE-027, CORE-029, CORE-031, CORE-051–053, QA-005)

**Files:**
- Create: `crates/ww-core/src/pipeline/{record.rs, signoff.rs}` (Task 10 created `pipeline/mod.rs` and `detail.rs`; modify `mod.rs`) and `crates/ww-core/src/tools/{asset_tools.rs, record_stage.rs, topics.rs}`.
  - **Module direction (CORE-010, CORE-014).** `tools` uses `pipeline`; `pipeline` never uses `tools`. So `pipeline` holds plain functions and types over `Store`, `Index` and `EventBus` (`RecordPlan`, `apply_record`, `plan_signoff`, `asset_detail`), and everything that touches a `tools` type lives in `tools/`: `impl RecordStage for WorkspaceRecord` (`tools/record_stage.rs`, whose `record` takes `&CallCtx`) and the `asset:`/`project:` snapshot providers (`tools/topics.rs`, which call `pipeline::asset_detail` and `projects::project_status`). Task 10's `module_dependencies_are_acyclic_and_store_depends_on_no_consumer` stays green unchanged.
- Modify: `tools/stages.rs` (`RecordStage` becomes asynchronous; `PipelineHooks::plan2` uses `WorkspaceRecord`), `tools/{registry.rs, dispatcher.rs}` (`CallCtx.record`), `events.rs` (snapshots outside the locks; history cap), `rpc.rs` (`ww/subscribe` awaits the bus; no connection lock is held across it), and `crates/ww-protocol/src/events.rs` (`HISTORY_MAX_EVENTS`).
- Modify: `crates/ww-core/src/tools/tests.rs` (plan 2a's unit tests): the `Recording` hooks' `RecordStage` impl is ported to the async `record(ctx, d, plan, out)`.
- Test: unit tests; `crates/ww-core/tests/{assets.rs, record.rs}`; additions to `tests/events.rs`. Tests create fixture files with `ww_testkit::files::write_file`, never with `std::fs` in `crates/ww-core/src` (QA-024).

**Interfaces:**

Consumes: Task 7's `Store::commit` and `CommitOp` (with `moved`); Task 9's `Store::{seal, version_lock}` and `Sealed`; Task 10's `Index::dependents` and `pipeline::asset_detail`; Task 12's `add_done_entry`, `project_status` and `CallCtx.project`; Task 3's `CandidateRun`, `CandidateFile`, `hash_value` and `Version::bump`.

Produces:

```rust
// ww-protocol
pub const HISTORY_MAX_EVENTS: usize = 1024;
// pipeline/record.rs (no tools types)
pub enum RecordBranch { CandidateRun { run_id: String }, SealedVersion { version: Version } }
pub struct HandOff { pub unlocks: Vec<String>, pub made_stale: Vec<String>, pub refs: Vec<String> }
pub struct RecordPlan { pub branch: RecordBranch, pub project: String, pub asset_id: String, pub commit: CommitOp,
                        pub progress_entry: String, pub refs: Vec<String>,
                        pub version_guard: Option<tokio::sync::OwnedMutexGuard<()>> }
    // SealedVersion plans carry the asset's version_lock guard (lock_owned) from Execute; the Record stage drops it
    // after commit returns, so no second sign-off of the asset can interleave between the seal and the manifest edit.
    // progress_entry is the only source of the progress.md line: tools leave commit.progress as None, and
    // apply_record fills it from progress_entry
pub struct RecordSlot; impl RecordSlot { pub fn set(&self, plan: RecordPlan); pub fn take(&self) -> Option<RecordPlan>; }
pub async fn apply_record(store: &Store, index: &Index, events: &EventBus, ws_id: &str, today: &str, plan: RecordPlan)
    -> Result<HandOff, StoreError>;                   // the Record stage's steps 1–3 below
pub fn start_candidate_run(m: &mut Manifest, run: serde_json::Map<String, Value>, now_utc: i64) -> Result<(), StoreError>;
    // appends the run; when the manifest has an approved_version and no open run targets a newer one, sets
    // `version` = approved.bump(Minor) and the run's `target_version` to it (I5 "set it by the bump rules"), else the
    // run's target_version = `version`; then transition(…, CandidateRun)
// tools/record_stage.rs
pub struct WorkspaceRecord;                           // impl RecordStage: takes ctx.record's plan, calls apply_record,
                                                      // maps a StoreError into the outcome, sets structured.handoff
// tools/stages.rs (changed)
pub trait RecordStage: Send + Sync {
    fn record<'a>(&'a self, ctx: &'a CallCtx, d: &'a ToolDescriptor, plan: Option<RecordPlan>,
                  out: &'a mut Outcome<ToolOutput>) -> Pin<Box<dyn Future<Output = ()> + Send + 'a>>;
}
// CallCtx gains `pub record: Arc<RecordSlot>`
// pipeline/signoff.rs
pub fn signed_off_by(caller: &CallerKind) -> &'static str;        // ui:* user, assistant, mcp claude-code, cli cli
pub fn plan_signoff(m: &Manifest, run_id: &str, files: &[String], bump: Option<Bump>, keep_runs: &[String],
                    by: &str, now_utc: i64) -> Result<(Manifest, Version), ErrorBody>;   // the manifest after sign-off (I5 step 3)
pub fn auto_pick(files: &[CandidateFile], vision: Option<&BTreeMap<String, f64>>, passes: &dyn Fn(&str) -> bool) -> Option<String>;
// tools/topics.rs (in tools, not pipeline: CORE-010)
pub struct AssetSnapshots; pub struct ProjectSnapshots;          // impl SnapshotProvider over pipeline::asset_detail
                                                                 // and projects::project_status
// events.rs (changed)
pub trait SnapshotProvider: Send + Sync { fn snapshot<'a>(&'a self, topic: &'a str) -> Pin<Box<dyn Future<Output = Value> + Send + 'a>>; }
impl EventBus { pub async fn subscribe(&self, client: ClientId, topics: &[String], since: &BTreeMap<String, u64>) -> Vec<TopicState>; }
pub fn register_asset_tools(reg: &mut Registry);  // asset.import, asset.collect, asset.signoff, asset.request_signoff, asset.pin_input
```

**The Record stage** (CORE-015 stage 9, CORE-020) runs only when the outcome is `ok` and the tool set a plan. `WorkspaceRecord` calls `pipeline::apply_record`, which does steps 1–3:
1. `store.commit(plan.commit)`, with `progress` = `projects/<project>/progress.md` and a `TextEdit` that calls `add_done_entry(text, today, &plan.progress_entry)`, mapping its `Err(message)` to `StoreError::Io { path, why: message }`. That covers output → manifest → jobs log → progress → index (including `commit.moved`). The plan's `version_guard` is dropped when `commit` returns, whether it succeeded or not.
2. For `SealedVersion`, the dependents from `index.dependents` that aren't pinned and are `approved`, `exported` or `released` go `MarkStale` (`edit_manifest`), into `made_stale`. `unlocks` lists dependents in `planned` or `blocked` whose every input is now sealed.
3. Events go out: `asset:` `{kind:"candidate_run"|"version_sealed", version?, run_id?}` and `project:` `{kind:"progress"}`.
4. `WorkspaceRecord` sets `structured.handoff` from the returned `HandOff`.

A commit error replaces the outcome with the store error. Outputs already moved stay as listed orphans (ARCH-075); a sealed `v<version>/` whose manifest edit failed, or was cut short by a crash (`store.after_seal`), is listed as an orphan too (Task 7). Repairs and QA (CORE-025's middle steps) are the empty stages between ingest and record.

**Tools** (Global Constraints table):
- **`asset.import`.** A missing asset gets `Manifest::new` with `ids.uuid()` as `creation_uuid` (CORE-047).
  - `copy` copies each file into a staging handle with `fsops::copy_new` (`CopyFile2` with `COPY_FILE_FAIL_IF_EXISTS`, so nothing is ever overwritten), hashes the staged copy (not the source, which may change meanwhile), and plans a candidate run (CORE-023) whose id comes from `Store::next_run_id`. Its manifest edit is `start_candidate_run`, and each run file records that staged hash as `files[].sha256`.
  - The result `ref` is the candidate ref of the first file (`ww://<ws>/<project>/<asset>@candidate/<run_id>/<file>`), and `handoff.refs` lists every file's candidate ref. For `link_external`, `ref` is the asset's ref without a version and `handoff.refs` is empty.
  - `link_external` records `external: [{path, sha256}]` and no run; the status goes to `in_progress`. Untrusted file names are marked untrusted in the result (ARCH-060).
- **`asset.collect`** re-hashes each external file. A mismatch is `ref.hash_mismatch`; otherwise the file is copied in (`copy_new`, hashing the staged copy) as a candidate run (`start_candidate_run`) and `external` is removed with `Manifest::remove`, so the other keys keep their order.
- **`asset.signoff`:**
  - with no session workspace → `workspace.required`;
  - with a non-empty `external` → `asset.link_external`, with `fix.ui_action {id:"asset.collect", label:"Collect into workspace"}` and `agent_hint {tool:"asset.collect", arguments:{project, asset_id}}`;
  - otherwise, holding the `version_lock` guard (`lock_owned`) from Execute until the end of Record: `plan_signoff` (the version is `bump(approved)` or the manifest's `version`, which `start_candidate_run` set by the bump rules when new work started, CORE-027), `Store::seal` with the planned manifest as the snapshot, then a `SealedVersion` plan that carries the guard and whose manifest edit re-applies `plan_signoff` to the re-read manifest. `plan_signoff` writes each chosen file into `hashes` as `hash_value` of its run file record's `sha256` (`sha256:<hex>`, I5), keyed by `v<version>/<file>`, so the snapshot already holds them (I5 step 2 comes before step 4); a run file without a recorded `sha256` (a pre-2b run) is hashed first. The Record stage's re-applied edit checks that these equal `Sealed.files`, and a difference fails the call with `store.hash_conflict`. Open runs for the version become `rejected` unless in `keep_runs`, which become `pinned: true`. The index gets the sealed files' post-move identities through `commit.moved`: one `Written { path, sha256, identity }` per sealed file, built from `Sealed.files` and `Sealed.identities`, so they reach the index in the index step, after the manifest edit (ARCH-075), never from `seal` itself.

  The result `ref` is `ww://…@<v>#sha256=<content hash>` (ARCH-042).
- **`asset.request_signoff`:** `RequestSignoff`, through `edit_manifest` (CORE-053). It writes no `progress.md` entry: it isn't a Record branch.
- **`asset.pin_input`** pushes `{ref, reason, by, date}` to `pinned_inputs`. A `proj:` or `lib:` ref whose source isn't in `project.yaml` `links` is appended there with `edit_yaml` (CORE-019). This happens in Execute, not Validate: Validate runs before Confirm and never writes (CORE-019 clarification). The ref must parse (`ref.invalid`), but its target need not exist on this machine, so a pin of an archived or unsynced source is recorded rather than refused with `asset.unknown`.

**Event bus changes** (before the first real snapshot provider):
- **`subscribe`:**
  1. Under the lock: record the topics; queue the replays the history covers; for each topic that needs a snapshot, remember `s0 = current seq` and mark the topic *pending* for that client (live events for a pending topic are kept in the history, not queued). Release the lock.
  2. Await each provider, holding no lock.
  3. Re-lock. If the client still subscribes to the topic, queue the snapshot (seq `s0`), then the history events with seq > `s0`, and clear *pending*.
- **History** is pruned by age (600 s) and count (`HISTORY_MAX_EVENTS`). A cursor older than the retained first event gets a snapshot.

- [ ] **Step 1: Write the failing tests:**
  - **`tests/record.rs`:**
    - `record_runs_after_meter_and_applies_the_plan_in_write_order` (CORE-015, CORE-020, CORE-025: ingest, then the empty repair and QA stages, then record and events);
    - `signoff_holds_the_version_lock_through_record` (CORE-026): with a hook that blocks the Record stage's manifest edit, a second `asset.signoff` of the same asset waits until the first returns, then sees the new `approved_version`;
    - `record_failure_turns_ok_into_the_store_error_and_lists_orphans`;
    - `signoff_marks_unpinned_dependents_stale_and_reports_them` (CORE-048, CORE-020);
    - `handoff_is_in_every_recorded_result`.
  - **`tests/assets.rs`:**
    - `import_copy_creates_asset_with_uuid_and_candidate_run` (CORE-023, CORE-047);
    - `import_link_external_records_path_and_hash_without_copying` (CORE-029);
    - `signoff_refuses_link_external_until_collected` (CORE-029);
    - `collect_refuses_a_changed_external_file` (`ref.hash_mismatch`);
    - `signoff_without_a_workspace_is_workspace_required` (QA-005);
    - `signoff_rejects_open_runs_unless_kept` (CORE-032);
    - `signed_off_by_follows_the_caller` (CORE-051);
    - `auto_pick_prefers_vision_score_then_lowest_passing_seed` (CORE-052);
    - `signoff_bump_is_proposed_then_confirmed` (CORE-027);
    - `new_work_after_approval_sets_version_by_the_bump_rules` (I5, CORE-027): after a sign-off of `1.0.0`, an `asset.import` sets the manifest's `version` and the run's `target_version` to `1.1.0`; a sign-off without `bump` then seals `v1.1.0/`, not `version.exists`;
    - `import_ref_is_the_first_file_and_handoff_lists_every_file` (a two-file import: `ref` is the first file's candidate ref, `handoff.refs` has both);
    - `progress_entry_is_written_once_from_the_plan` (a recorded call adds exactly one `## Done` line, from `progress_entry`; `apply_record` has a `debug_assert!` that `commit.progress` arrives as `None`);
    - `signoff_writes_prefixed_hashes_and_sealed_files_reach_the_index_after_the_manifest` (I5, ARCH-075: `hashes` values are `sha256:<hex>`; a recording sink sees the sealed files only after the manifest edit, with `Sealed.identities`);
    - `refs_exclude_source_and_carry_the_content_hash` (CORE-028, CORE-031);
    - `pin_input_records_proj_and_lib_links_keeping_comments` (CORE-019);
    - `pin_input_accepts_a_target_that_does_not_exist_yet` (CORE-019; `ref.invalid` only for a ref that doesn't parse);
    - `import_copy_never_overwrites_and_hashes_the_staged_copy` (CORE-023: a staged name that already exists is an error, never an overwrite; the recorded SHA-256 is the staged file's);
    - `collect_removes_external_without_reordering_keys` (CORE-029);
    - `request_signoff_moves_to_in_review` (CORE-053);
    - `imported_file_names_are_untrusted_in_results` (ARCH-060).
  - **`tests/events.rs` additions:**
    - `snapshot_is_computed_outside_the_bus_lock`: a provider blocked on a gate doesn't delay a publish to another client;
    - `events_published_during_a_snapshot_follow_it_in_order`;
    - `client_unsubscribed_during_a_snapshot_gets_nothing`;
    - `history_is_capped_at_1024_events_per_topic`;
    - `asset_and_project_topics_carry_kinds_and_snapshots` (ARCH-044).
  - Remove the four remaining `versions_trash.rs` entries and the five `asset_record.rs` entries from `expected-fail.toml`.
- [ ] **Step 2: Run them.**

Run: `cargo nextest run -p ww-core --test record --test assets --test events`
Expected: FAIL. Nothing is implemented.

- [ ] **Step 3: Implement it.**
- [ ] **Step 4: Run them, then the spec tests.**

Run: `cargo nextest run -p ww-protocol -p ww-core && cargo clippy -p ww-core --all-targets -- -D warnings && cargo run -q -p ww-cli -- dev spec-tests`
Expected: PASS; `spec tests: 63 passed, 3 expected-fail`. `schema_hash_is_16_hex_and_golden` is unchanged: no wire type changed.

- [ ] **Step 5: Commit.**

```bash
git add crates/ww-protocol crates/ww-core crates/ww-spec-tests/expected-fail.toml
git commit -m "core: asset tools, async Record stage with hand-off blocks, asset:/project: topics with snapshots outside the bus lock"
```

---

### Task 14: `ww doctor` basics, crash recovery and the outside-writers reconciler (ARCH-016, ARCH-085, CORE-004, CORE-026, CORE-038, CORE-040, QA-011, QA-046, QA-055, QA-060; owner decision 5)

**Files:**
- Modify: `crates/ww-core/src/state/mod.rs`.
  - The read-only check opens with `immutable=1` (URI) when no `-wal` file exists, so a refused file never gets fresh `-wal`/`-shm` files.
  - A non-empty `-journal` beside the file is `StateError::HotJournal`.
- Modify: `crates/ww-core/src/config.rs` and `server/mod.rs` (`CoreExit::StateHotJournal(PathBuf)` → exit 7). `CoreConfig` also gains `pub git_env: Vec<(OsString, OsString)>`: empty in `CoreConfig::for_env` (production), set by in-process tests to `TestEnv::git_env()`, and passed to every `git` the core runs, so in-process doctor tests are isolated from the system and user git config too (QA-038). A spawned core needs nothing extra: it inherits `child_env()`'s git variables. `core_registry` (in `tools/mod.rs`) gains a last parameter `git_env: Arc<Vec<(OsString, OsString)>>` for `register_doctor_tools`; `server/mod.rs` passes `CoreConfig.git_env`, and the `core_registry` calls in `tools/tests.rs` pass an empty one.
- Create: `crates/ww-core/src/tools/doctor_tools.rs` (`workspace.doctor`) and `crates/ww-core/src/store/git_config.rs`: the core's only `git` runner, so the repository-config writes are the store's like every other workspace write (ARCH-002, ARCH-112's intent).
- Modify: `crates/ww-core/src/store/write.rs` (factor out the pure `decide_write`, for `tests/reconcile_sim.rs`).
- Modify: `crates/ww-core/Cargo.toml` (dev-dependency `proptest`, for `tests/reconcile_sim.rs`).
- Modify: `crates/ww-cli/tests/user_cli.rs` (`damaged_state_db_message_is_plain`) and `crates/ww-client/tests/client.rs` (the damaged-database test's `expected` text): both assert "`ww doctor` (plan 2b) can set it aside.", which this task changes to "`ww doctor` can set it aside." (neither file is protected).
- Create: `crates/ww-client/src/doctor.rs`. Modify `crates/ww-client/{Cargo.toml (add rusqlite), src/lib.rs}`: `StartFailure::StateHotJournal`, and the messages without "(plan 2b)".
- Create: `crates/ww-cli/src/user/doctor.rs`. Modify `user/mod.rs`.
- Test: `crates/ww-core/tests/{crash_recovery.rs, reconcile_sim.rs, reconcile_ntfs.rs, doctor_tool.rs}`; `crates/ww-client/tests/doctor.rs`; `crates/ww-cli/tests/doctor_cli.rs`.

**Interfaces:**

Produces:

```rust
// ww-client doctor.rs (ww may touch the suite's own local folder; it never writes workspace files, ARCH-002)
pub enum StateFileState { Missing, Healthy, Damaged(String), TooNew { found: i64, known: i64 }, HotJournal }
pub fn inspect_suite_state(env: &Env) -> StateFileState;                 // read-only; immutable=1 without -wal
pub fn set_aside(env: &Env, now_utc: i64) -> io::Result<Vec<PathBuf>>;   // state.db, -wal, -shm, -journal → <name>.damaged-<YYYYMMDDTHHMMSSZ>, same folder
pub fn finish_recovery(env: &Env) -> io::Result<StateFileState>;         // one read-write open (SQLite rolls the journal back), then the check
pub enum SuiteTomlState { Missing, Unreadable(String), NoDefault, DefaultMissing(PathBuf), Ok { default: PathBuf, id: String } }
pub fn inspect_suite_toml(env: &Env) -> SuiteTomlState;                 // ww_schema::SuiteToml; never resolve_workspace_id
pub fn set_repo_autocrlf_false(ws: &Path) -> io::Result<()>;            // git -C <ws> config --local core.autocrlf false (run_hidden);
                                                                         // ww doctor calls it only after an interactive "y"
// ww-core
pub struct DoctorArgs { pub fix: bool }
pub struct DoctorCheck { pub id: String, pub state: String /* ok | fixed | problem */, pub detail: String }
pub fn register_doctor_tools(reg: &mut Registry, probe: Arc<dyn Probe>, git_env: Arc<Vec<(OsString, OsString)>>);
// store/git_config.rs
impl Store { pub async fn git(&self, args: &[&str], git_env: &[(OsString, OsString)]) -> Result<Captured, StoreError>; }
    // run_hidden(find_on_path("git"), args, cwd = the workspace root, git_env, 30 s); StoreError::Io when git is
    // missing or times out. workspace.doctor reads and writes git config only through it
// store/write.rs
pub enum WriteDecision { Proceed, Conflict, TreatAsNew }
pub fn decide_write(expect: &Expect, current: Option<&str /* sha256 or LFS oid */>, generated: Option<&str>,
                    pending: &[String]) -> WriteDecision;   // pure: step 3 of the write algorithm
    // checks: git.repository, git.core_longpaths, git.lfs_setlockablereadonly, git.core_autocrlf, gitignore.entries,
    //         gitattributes.entries, sealed.readonly, workspace.volume, workspace.watcher, workspace.orphans,
    //         workspace.needs_repair, workspace.missing
// ww-cli
pub enum UserCommand { /* … */ Doctor }
```

**`ww doctor`, in order:**
1. **The suite `state.db`** (owner decision 5), with `inspect_suite_state`:
   - `Damaged` or `TooNew` → explain in plain words. With `--non-interactive`: `problem: … — run ww doctor interactively to set it aside`. Otherwise ask "Set it aside as state.db.damaged-<utc> and start with a fresh database? [y/N]". `y` → `set_aside` → `fixed: …`; anything else → `problem`.
   - `HotJournal` → ask "Let SQLite finish its recovery? [y/N]", then `finish_recovery`. If the file is still damaged, the set-aside question follows.
   - If a `problem` remains here (the answer was `n`, or `--non-interactive`), the core can't start, so step 3 can't run: doctor prints `problem: workspace checks skipped — the core can't start` after step 2 and exits 1, not 2 (it did run; a problem remains).
2. **`suite.toml`**, through `ww_client::doctor::inspect_suite_toml` (so `ww-cli` needs no `ww-schema` dependency and its protected `Cargo.toml` stays unchanged), never through `resolve_workspace_id`.
   - `Missing` or `NoDefault` → `ok: no default workspace; workspace checks skipped`, and step 3 doesn't run (nothing is wrong: no workspace is set up yet).
   - `Unreadable` → `problem: … — ` with the parse error; step 3 doesn't run.
   - `DefaultMissing` → `problem: … — run ww workspace use "<path>"`; step 3 doesn't run.
3. **The workspace checks**, through a connection whose hello names the default workspace's id: `workspace.doctor {fix:true}`, printed line by line.
   - The core runs `git config --local core.longpaths true` and `git config --local lfs.setlockablereadonly false` through `Store::git` (CORE-040, ARCH-085), with `CoreConfig.git_env`. Each check is `ok` when the repository's own config already has that value, else `fixed` (or a `problem` without `fix`). Without `.git`, they are `ok` with "not a git repository; skipped".
   - **`git.core_autocrlf`** is report-only in the tool: when the effective `core.autocrlf` (`git config core.autocrlf`, through `Store::git`) is `true` or `input`, it is a `problem` explaining that text files outside `projects/` get CRLF on checkout, and `workspace.doctor` never changes it, whatever `fix` says and whoever calls it (an agent-callable fix tool must not change persistent git config without the person's confirmation; CORE-040 doesn't ask for this setting, and `projects/** -text` already covers the asset trees). An unset value is `ok`.
   - After printing the workspace checks, `ww doctor` asks "Set core.autocrlf=false for this repository only? [y/N]" when that check is a `problem`. Only `y` sets it, with `git -C <ws> config --local core.autocrlf false` through `ww_client::doctor::set_repo_autocrlf_false` (`run_hidden`): git itself writes `.git\config`, which is not a store-managed workspace file (ARCH-002). The line becomes `fixed:`. With `--non-interactive`, or any answer but `y`, nothing is set, the line stays a `problem` and doctor exits 1.
   - The five `.gitignore` entries and the `.gitattributes` line `projects/** -text` are appended through `edit_text` when missing (CORE-040 clarification).
   - `sealed.readonly` (CORE-026; research 5A-6, the doctor part): a sealed file without the read-only attribute whose SHA-256 matches the manifest (a clone or checkout drops the attribute, since git stores only file modes) gets it back with `fix`, through `fsops::set_readonly`; one whose hash doesn't match stays as it is, a human edit, and is listed. Re-applying it automatically after every rescan is deferred (Not in this plan).
   - Orphans, needs-repair manifests and `missing` files are listed as `problem`s, never fixed automatically (CORE-038).
   - A network or cloud volume, or one without stable IDs, is a `problem` with an explanation.
   - A workspace served without a watcher (Task 11) is a `problem`: "outside edits are found only by `ww notify` and at open".

   A second run changes nothing (QA-060).

- [ ] **Step 1: Write the failing tests:**
  - **`tests/crash_recovery.rs`** (QA-055, the fast subset):
    - **`crash_at_each_write_order_step_recovers_cleanly`** (Review Focus 1). For each of `store.after_output`, `store.after_manifest`, `store.after_jobs_log`, `store.after_progress` and `store.after_index`:
      1. `CoreFixture::start_with_env` with that point at `abort`; `test.ingest_run {job_line:true}` → the core exits.
      2. A fresh core, started only after `KillOnDrop::wait_exit` has seen the old one exit (Windows releases a dead process's `LockFileEx` locks when its handles close, and the docs say the timing depends on system resources; if this step flakes on `core.lock`, report it rather than adding a sleep), same key: no `*.ww-tmp-*` or `*.ww-bak-*` remains; `project.status` lists the run in `orphans` only for `after_output`; every manifest parses; every log line parses (`read_lines` drops none); `assets.query` agrees with the manifests.
      3. The retry with the same key runs again (the dead core's `running` row was released under `core.lock`), and both runs' files exist.
    - **`crash_between_seal_and_manifest_lists_the_version_and_loses_nothing`** (Review Focus 1, CORE-038): `asset.import` (copy), then a core with `store.after_seal=abort` and `asset.signoff` → the core exits. A fresh core: `project.status` lists `v1.0.0/` in `orphans`; every file of the run exists exactly once, in `v1.0.0/` or `candidates/<run>/`; the manifest parses and has no `approved_version`; nothing was deleted;
    - `disk_full_mid_commit_keeps_files_consistent`;
    - `held_manifest_during_commit_is_retried` (paused time);
    - `corrupt_or_missing_index_db_is_rebuilt_at_open`;
    - `damaged_workspace_state_db_fails_that_workspace_not_the_core` (`state.unavailable`; the file is unchanged);
    - `refused_wal_db_gets_no_new_side_files` (a WAL-mode db with `user_version` 999 and no `-wal`: after the refused start, no `-wal` or `-shm` exists);
    - `hot_journal_exits_7_with_nothing_changed`.
  - **`tests/reconcile_sim.rs`** (QA-046, simulated, shrinkable): `outside_writers_never_lose_edits`.
    - A `proptest` strategy generates operation sequences, which shrink on failure. They run against an in-memory file map, applying the pure write decision (`store::write::decide_write`, factored out of `store/write.rs` in this task; Interfaces) and `watch::reconcile::decide`.
    - Operations: outside edit, slow outside write (a write observed mid-way, with an unstable identity), delete, rename, folder move, partial manifest edit, invalid manifest, checkout of n files (with overflow, rewriting candidate files whose run records change in the same checkout), LFS pointer, edit between check and replace, core write, core manifest edit, crash at a write-order step, crash after the replace.
    - Invariants: no outside edit is overwritten or left only in a backup file; invalid manifests are marked, never rewritten; no human edit comes from an unstable read, from the core's own pending bytes, or twice for one SHA-256; a removed file the suite still expects (in `hashes`, of an open run, or a manifest) is `missing`, never forgotten, and the suite's own moves never make one `missing`; the index model converges after a rescan; no orphan is deleted; no write happens while `index.lock` exists.
    - `cases` comes from `test-params.toml`, through `ww_testkit::params::proptest_cases()`.
  - **`tests/reconcile_ntfs.rs`** (QA-046, real NTFS with the fence): the same scenarios and invariants, through a real core and `GitOps`. Each scenario ends with a fence; then the core's view (`asset.get`, `assets.query`) and the files on disk are compared.
  - **`tests/doctor_tool.rs`:**
    - `doctor_sets_git_config_and_gitignore_then_changes_nothing` (CORE-040, ARCH-085, QA-060);
    - `doctor_without_git_skips_with_a_note`;
    - `doctor_lists_orphans_and_needs_repair_without_fixing` (and `missing` files);
    - `doctor_reports_a_missing_watcher` (`watch.start=unsupported`; `workspace.watcher` is a `problem` whose detail says outside edits are found only by `ww notify` and at open);
    - `doctor_git_calls_use_the_configured_git_env` (an in-process core whose `CoreConfig.git_env` is `TestEnv::git_env()`; a `core.autocrlf=true` in the test profile's `.gitconfig` is reported as a `problem`, and with an empty profile it is `ok`, so the core reads only the config it was given);
    - `doctor_tool_reports_autocrlf_and_never_sets_it` (CORE-040: a repository whose config says `core.autocrlf=true`; `workspace.doctor {fix:true}` from a `cli` and from an `mcp` caller → `git.core_autocrlf` is a `problem` and `.git\config` is byte-identical; `.gitattributes` gains `projects/** -text`);
    - `doctor_restores_read_only_on_matching_sealed_files` (CORE-026: after `git checkout` leaves sealed files writable, `fix` makes the matching ones read-only and lists an edited one without touching it).
  - **`ww-client/tests/doctor.rs`:**
    - `inspect_never_creates_side_files`;
    - `set_aside_moves_db_wal_and_shm_and_deletes_nothing`;
    - `finish_recovery_rolls_back_a_hot_journal`;
    - `start_failure_messages_name_doctor_without_plan_2b`;
    - `suite_toml_states_cover_missing_unreadable_and_stale_default`.
  - **`ww-cli/tests/doctor_cli.rs`:**
    - `doctor_answers_n_keeps_the_file` (and prints `problem: workspace checks skipped — the core can't start`, exit 1) and `doctor_answers_y_sets_it_aside_and_the_core_starts`;
    - `non_interactive_doctor_never_moves_it`;
    - `doctor_sets_autocrlf_only_after_yes` (a repository with `core.autocrlf=true`: answer `n` → unchanged, exit 1; answer `y` → `git config --local core.autocrlf` is `false`, a `fixed:` line, and a second run is all `ok:`);
    - `non_interactive_doctor_never_sets_autocrlf` (`ww --non-interactive doctor` → no prompt, `.git\config` unchanged, exit 1);
    - `doctor_with_a_stale_default_reports_it_and_still_runs`;
    - `doctor_without_a_default_workspace_skips_the_workspace_checks` (no `suite.toml`, then a `suite.toml` without `default_workspace` → `ok: no default workspace; workspace checks skipped`, exit 0, and no core is started);
    - plan 2a's `damaged_state_db_message_is_plain` (`ww-cli/tests/user_cli.rs`) and the damaged-database test in `ww-client/tests/client.rs` assert the new message without "(plan 2b)".
  - Remove the three `doctor.rs` entries from `expected-fail.toml`. The list then holds no plan-2b test.
- [ ] **Step 2: Run them.**

Run: `cargo nextest run -p ww-core --test crash_recovery --test reconcile_sim --test reconcile_ntfs --test doctor_tool && cargo nextest run -p ww-client -p ww-cli`
Expected: FAIL. Nothing is implemented.

- [ ] **Step 3: Implement it.**
- [ ] **Step 4: Run them, then the spec tests.**

Run: `cargo nextest run --workspace --exclude ww-spec-tests && cargo clippy --workspace --all-targets -- -D warnings && cargo run -q -p ww-cli -- dev spec-tests`
Expected: PASS; `spec tests: 66 passed, 0 expected-fail`.

- [ ] **Step 5: Commit.**

```bash
git add crates/ww-core crates/ww-client crates/ww-cli crates/ww-spec-tests/expected-fail.toml Cargo.lock
git commit -m "doctor: git config and .gitignore basics, damaged or hot-journal state.db set aside only after yes; crash and reconciler tests"
```

---

### Task 15: Plan-2b close-out (QA-028) — **protected: `crates/ww-cli/src/main.rs`**

**Files:**
- Modify (protected): `crates/ww-cli/src/main.rs`. In the `Dev` branch only: when `global.any_set()`, print "`ww dev` doesn't take --workspace, --non-interactive or --no-start." and exit 2. This is the only change to the file. It can't live in `src/user/**`: the flags are clap `global = true` arguments flattened into the top-level `Cli` in `main.rs`, so only the match there knows that the subcommand is `Dev`, and `src/dev/**` is protected too.
- Modify: `crates/ww-cli/src/user/mod.rs` (`GlobalArgs::any_set`).
- Modify: `crates/ww-core/src/{server/mod.rs, server/conn.rs, main.rs, config.rs, tools/dispatcher.rs}` and `crates/ww-platform/src/session_end.rs` (core robustness, below).
- Modify: `dev/rule-classification.toml`.
- Test: `crates/ww-cli/src/user/mod.rs` parse tests; additions to `crates/ww-core/tests/{transport.rs, lifecycle.rs}` and `crates/ww-platform/tests/session_end.rs`.

**Interfaces:**
- Produces: `impl GlobalArgs { pub fn any_set(&self) -> bool; }`, and `SessionEndSim { QueryEnd, End, Close }` as the `simulate` selector, separate from the reported `SessionEnd` events.

- [ ] **Step 1: Write the failing tests:**
  - `dev_commands_reject_user_global_flags` (`ww dev spec-report --no-start` → exit 2 with that message); the existing `dev_commands_still_parse` keeps passing;
  - **`tests/transport.rs`:**
    - `non_utf8_line_is_a_parse_error`;
    - `client_notifications_are_ignored`;
    - `line_of_exactly_max_bytes_is_accepted`;
  - **`tests/lifecycle.rs`:**
    - `failed_parent_watch_registration_exits_4`: `WW_TEST_PARENT_PID=4`, the System process, which `process::open` refuses with access denied rather than `NotFound` → the core begins `Fatal` and exits 4;
    - `pipe_instance_creation_failure_is_fatal_exit_4` (a fault-injected `create_instance` error after the first instance);
    - `file_log_has_no_ansi_escapes` (`with_ansi(false)`; no `\x1b[` in the log);
    - `panic_log_line_carries_the_trace_id`: the tool runs inside a `tracing` span with `trace_id`, so the panic hook's JSON line has it;
  - **`ww-platform/tests/session_end.rs`:** `simulate_selector_is_separate_from_reported_events`.
- [ ] **Step 2: Run them.**

Run: `cargo nextest run -p ww-cli -p ww-core -p ww-platform`
Expected: FAIL for the new tests.

- [ ] **Step 3: Implement it**, with the one protected edit. Document exit 4 for a pipe-instance creation failure in `CoreExit`'s and `start_claimed`'s docs.
- [ ] **Step 4: Classify the rules** (QA-028) in `dev/rule-classification.toml`:
  - **`test`:**
    - every rule a plan-2b test cites (the "rules this plan implements" list, each checked by `grep -rn "spec: .*<ID>" crates`). The two that only one test cites are ARCH-002 (Task 6, `new_file_write_records_generated_hash`) and CORE-025 (Task 13, `record_runs_after_meter_and_applies_the_plan_in_write_order`); CORE-005 and CORE-014 are cited by Task 10's `machine_local_files_never_land_in_the_workspace` and `module_dependencies_are_acyclic_and_store_depends_on_no_consumer`;
    - the eight rules only plan-1 tests cite: ARCH-097, QA-017, QA-020, QA-033, QA-036, QA-038, QA-079, QA-086;
    - CORE-010, which moves from `manual` (Task 10's test).
  - **`lint`:** ARCH-112 and QA-024 stay `lint`. ARCH-002 is `test`, since its tests cite it; the lint backs them up.
- [ ] **Step 5: Run the full gate.**

Run: `cargo fmt --check && cargo clippy --workspace --all-targets -- -D warnings && cargo deny check && cargo nextest run --workspace && cargo run -q -p ww-cli -- dev spec-tests && cargo run -q -p ww-cli -- dev spec-report`
Expected:
- everything passes;
- `ww dev spec-tests` prints `66 passed, 0 expected-fail`;
- `spec-report` shows 0 dangling citations and no `test`-classified rule without a test.

- [ ] **Step 6: Commit, then the privacy scan.**

```bash
git add crates/ww-cli crates/ww-core crates/ww-platform dev/rule-classification.toml
git commit -m "close-out: reject user flags on ww dev, core robustness fixes, rule classification for plans 1-2b (QA-028)"
cargo run -q -p ww-cli -- dev privacy-scan --tree HEAD
```

Expected: the privacy scan exits 0.
