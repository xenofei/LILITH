# Plan 2b — Core Platform I-b: Workspace Data Implementation Plan

> DRAFT IN PROGRESS: all 15 tasks written (no tasks after 15 remain); self-review partly done (all 119 cited IDs exist; no placeholders or personal data found); not yet done: the privacy scan, a final cross-task consistency pass, and the reply.

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
- New workspace dependencies: `proptest` (shrinkable property tests, QA-046) and `toml_edit` (comment-preserving `suite.toml` edits). `serde_json` gains `preserve_order` (ARCH-076).
- `yaml-rust2` (already a workspace dependency) validates targeted YAML edits.
- New `windows` features in `ww-platform` only.

**Spec:** `docs/superpowers/specs/2026-10-12-worldwright-suite-design.md`. Task 1 adds its Appendix E, "Workspace data (plan 2b clarification)". The plan follows roadmap row 2b (`docs/superpowers/plans/2026-10-12-roadmap.md`), which cites §1.5–1.6, §2.1 and §2.3–2.4.

The rules this plan implements, fully or in the part named:
- §1.1: ARCH-002 (store), ARCH-016 (`ww notify`, `ww doctor` basics);
- §1.2: ARCH-028, ARCH-033 (staging on the workspace volume), ARCH-042, ARCH-044;
- §1.3: ARCH-058;
- §1.5: ARCH-073–083, ARCH-085, ARCH-088, ARCH-089, ARCH-090 (writing `suite.toml`);
- §1.6: ARCH-094 (no workspace in the install folder), ARCH-096;
- §1.7: ARCH-098 (the `sketchbook` default project; the app side is plan 4);
- §2.0: CORE-003 (staging handles; document save handles are plan 5), CORE-006, CORE-007 (writes), CORE-009, CORE-010 (the test);
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

Decisions made in this plan (none needs the owner):
- **Targeted YAML edits are line edits**, checked by re-parsing with `yaml-rust2`. No YAML crate on crates.io keeps comments when it writes, and `yaml-rust2` is maintained, MIT/Apache and already a workspace dependency. An edit the line editor can't make safely (anchors, flow mappings at the edited key, several documents) fails with `yaml.unsupported_edit` and changes nothing.
- **`suite.toml` edits use `toml_edit`**, which keeps comments and unknown keys. It is maintained, MIT/Apache and already in `Cargo.lock` through `toml`.
- **Manifests are ordered maps.** A typed struct can't keep the original interleaving of known and unknown keys. `Manifest` is therefore the ordered `serde_json` map (with `preserve_order`), with typed views for reading and setters that edit in place: existing keys keep their position, and new keys are appended.
- **Dates the core writes are UTC.** That covers trash folders, `progress.md` entries, log months, run ids and set-aside names. Per-machine monthly log names (ARCH-081) then don't depend on time zones. Plan 3 adds local-calendar time for caps (CORE-012).
- **Fail points exist only in non-stable builds and are armed only on test channels** (`WW_TEST_FAILPOINTS`), like the test tools. A stable build refuses test channels, so it can never arm one. Task 1 records this reading of QA-066's "compiled only into test builds".
- **`workspace.create` doesn't run `git init`.** Sync (plan 7+) owns repository setup. `ww doctor` skips its git checks, with a note, when the folder isn't a repository.

## Global Constraints

- **Toolchain:** Rust **1.99.0**, `x86_64-pc-windows-msvc`, edition **2024**.
- **Branch and merge:**
  - Work on `plan-2b-core-platform`, cut from `main` after this plan's docs PR merges.
  - Commit after every task; each message ends with `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`.
  - The branch lands as one PR through `ww dev merge`.
  - **Protected files** (QA-019/020): Tasks 2, 5, 10 and 15 modify them, so the owner must run `ww dev approve <PR>` on the final head. New files under `crates/ww-spec-tests/**` (tests and fixtures) are free additions (`add-ok`). This plan changes no workflow file.
- **Crates** (ARCH-110): no new crate.
  - New `ww-core` modules: `index`, `watch`, `graph`, `pipeline`, `projects`, `settings`.
  - `store` grows from `fsops` into the module CORE-014 describes.
- **Dependency boundaries** (ARCH-111, QA-027):
  - `ww-schema` may add only `serde_json`, `yaml-rust2`, `toml_edit` and `sha2`; none of them pulls in a Windows crate. `crates/ww-testkit/tests/dependency_boundaries.rs` stays green.
  - Only `ww-platform` calls Windows APIs.
  - `ww-core` reaches every `ww_platform::fs` function that mutates files only from `store/fsops.rs`. Those functions are added to `crates/ww-core/clippy.toml`'s `disallowed-methods` (Task 6).
- **New dependencies** (OPS-108; `cargo deny check` must pass). Pin each to its current crates.io release at implementation time (`cargo search <name> --limit 1`), with the floors below, and record the versions in the Task 2 report.

  | Crate | Floor | Licence | Reason |
  |---|---|---|---|
  | `proptest` | 1.7 | MIT OR Apache-2.0 | the shrinkable simulated-filesystem reconciler variant (QA-046) and manifest round-trip properties (QA-044). `default-features = false, features = ["std"]`, so `rusty-fork` and `wait-timeout` stay out of the tree |
  | `toml_edit` | 0.22 | MIT OR Apache-2.0 | `suite.toml` edits that keep comments and unknown keys (CORE-007; the core is its only writer) |
  | `serde_json` feature `preserve_order` | — | — | manifests keep key order (ARCH-076). Set on the workspace dependency, so every build of every crate uses the same `serde_json` |

  **New `windows` features in `ww-platform`:** `Win32_System_IO` (`OVERLAPPED`, `CancelIoEx`). `Win32_Storage_FileSystem` is already enabled.
- **File writes in `ww-core`** (ARCH-002, ARCH-112, QA-024): every file-mutating call stays in `crates/ww-core/src/store/fsops.rs`, one `#[allow(clippy::disallowed_methods)]` per function. `crates/ww-testkit/tests/single_write_allowance.rs` keeps enforcing it.
- **Tests** (QA-031): as in plan 2a, plus:
  - Retry backoff, the 60 s git wait, watcher batching and the 30- and 90-day purges are tested with `#[tokio::test(start_paused = true)]` or `FakeClock`, never with real sleeps.
  - E2E tests wait on the watcher with the **watcher fence** (`RawPipe::fence`), never with a sleep.
  - The one wall-clock measurement is the CORE-042 index benchmark (Task 10). It is a performance gate with its budget in `test-params.toml`; it is not a correctness assertion.
  - Tests that need git use `TestEnv::git_init` (real `git`, isolated config, no remote, no network).
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
- **Names that are contracts.** Task 1 copies everything from here to the end of this section into spec Appendix E, except the test-channel tools table.
  - **Workspace-relative paths** use `/` and are checked before anything is written (CORE-040):
    - at most 180 characters;
    - no component may be a reserved Windows name (`CON`, `PRN`, `AUX`, `NUL`, `COM1`–`COM9`, `LPT1`–`LPT9`, and `COM¹`–`COM³`/`LPT¹`–`LPT³`), whatever its case and extension;
    - no component may end in a dot or a space, contain `<>:"\|?*` or a control character, or be `.` or `..`;
    - absolute paths longer than 259 characters are written through `\\?\` long paths.
  - **Store write algorithm** (ARCH-074; ARCH-074 clarification):
    1. Check the path.
    2. If writes are paused for git, wait (ARCH-083).
    3. Re-hash the target and compare it with the expected hash: for a full write, the file's last generated hash (ARCH-078); for a manifest or YAML edit, the hash read at the start of the edit. LFS pointers compare by `oid`.
    4. Write `<dir>\.<name>.ww-tmp-<pid>-<n>` in the same folder, and flush it with `FlushFileBuffers`.
    5. Replace the target. For an existing target, call `ReplaceFileW` with a backup name `<dir>\.<name>.ww-bak-<pid>-<n>`. For a new target, call `MoveFileExW(MOVEFILE_WRITE_THROUGH)` without replace-existing.
    6. Hash the backup. If it differs from the expected hash, an outside edit landed between steps 3 and 5. The outside version is put back with a second `ReplaceFileW`, the core's own new content is discarded (the call failed, so nothing of yours is lost), and the call fails with `store.hash_conflict`. Otherwise the backup is removed.
    7. Record the new SHA-256 as the file's last generated hash.

    Error handling:
    - Sharing and lock violations (Windows errors 32, 33 and 1224) are retried, starting at 10 ms and doubling to a cap of 500 ms, for up to 10 s in total; after that the call fails with `store.busy`.
    - Error 1175 is retried the same way. Error 1177 is recovered by moving the replacement onto the freed target name with `MoveFileExW(MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH)`.
    - Where `ReplaceFileW` is unsupported (errors 1 and 50, for example on some network volumes), the store makes two write-through renames (target → backup, temp → target). If a crash leaves only the backup, the next workspace open restores it.

    `REPLACEFILE_WRITE_THROUGH` is documented as unsupported, so durability comes from the flush in step 4 and from the write-through moves. A replaced file gets the replacement's file ID, so the index records the identity the store reports after each write.
  - **Write order** (ARCH-075): output files → manifest → jobs log line → `progress.md` entry → index → events. On workspace open, the store does three things:
    - outputs with no manifest entry are listed as orphans and never deleted (CORE-038);
    - stale `*.ww-tmp-*` files of processes that are no longer running are removed;
    - a `*.ww-bak-*` file whose target is missing is restored.
  - **Append logs** (ARCH-081, CORE-009):
    - one JSON object per line, ending in `\n`, with `"machine"` and `"seq"` (from 1 per file) first, then the record;
    - each line is one write under `LockFileEx` on the byte range starting at offset 2⁶⁴−2 with length 1 of the log file itself, which `ww` can take too;
    - before appending, a last line without `\n` is closed with `\n`;
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
    - its `state.db` is opened read-only and its rows are never released.
  - **Local data** (ARCH-097, CORE-005): `index.db` sits next to the workspace `state.db`, in `Env::workspace_local_dir(ws)`. It uses WAL, and `PRAGMA user_version` is `1`. A different version, or a failed `quick_check`, means the file is deleted and rebuilt.
  - **`.gitignore` entries** added by `workspace.create` and `ww doctor` (CORE-040, ARCH-096): `.worldwright/`, `.trash/`, `inbox/*`, `projects/*/assets/*/candidates/`, `projects/*/assets/*/work/` (the root-relative form of CORE-040's `assets/*/work/`).
  - **Refs** (ARCH-042, CORE-031, CORE-047):
    - a sealed version: `ww://<workspace id>/<project>/<asset_id>@<major.minor.patch>`, optionally followed by `#sha256=<64 hex>`;
    - a candidate: `ww://<workspace id>/<project>/<asset_id>@candidate/<run_id>/<file>`;
    - manifest link forms (I6): `<asset_id>@<v>`, `proj:<slug>/<asset_id>@<v>`, `lib:<item>@<v>` and `style@<n>`;
    - every new reference to a sealed version carries the hash, and manifest `inputs` entries gain `"sha256"`.

    Name rules: a project slug matches `^[a-z0-9][a-z0-9-]{0,62}$`, an asset id `^[a-z][a-z0-9_]{0,62}$` and a run id `^r_\d{8}_\d{4}(_\d+)?$` (UTC). A version's **content hash** is the SHA-256 of a text with one line per file in `v<version>/`, except `source/**` and `manifest.snapshot.json`. The lines are sorted by `/`-separated relative path in byte order, and each reads `<sha256 hex>  <relative path>\n`.
  - **Manifest fields this plan writes** (`schema_version` 1 on new manifests):
    - `creation_uuid` (UUID v4; CORE-047);
    - `status_before_stale` and `status_before_blocked`;
    - `has_working_doc`;
    - `external: [{"path","sha256"}]` (link-external files);
    - `candidate_runs[].pinned`;
    - `human_edits[]` entries `{"path","sha256","previous_sha256","detected"}`;
    - `signed_off.by`, one of `user` (`ui:*`), `assistant`, `claude-code` (`mcp`) and `cli`.

    Unknown fields are kept in place (CORE-148).
  - **Product tools.** Every one declares the Worldwright module `workspace`, which is always available and never filtered by `modules` (CORE-058). Inputs reject unknown fields. A tool with a `project` argument uses the session's active project when it's omitted, else `sketchbook` (ARCH-088, ARCH-098).

    | Tool | Effect | Arguments | `structured` result |
    |---|---|---|---|
    | `workspace.create` | write | `{path, name?, default?}` | `{id, path, created: bool, default: bool, warnings: [string]}` |
    | `workspace.set_default` | write | `{path}` | `{id, path}` |
    | `workspace.doctor` | write | `{fix}` | `{checks: [{id, state: "ok"\|"fixed"\|"problem", detail}]}` |
    | `watch.notify` | write | `{paths: [string]}` | `{reconciled: [string], ignored: [string]}` |
    | `assets.query` | read | `{project?, status?, type?, text?, limit?}` | `{assets: [AssetSummary]}` |
    | `asset.get` | read | `{project?, asset_id}` | `AssetDetail` |
    | `asset.import` | write | `{project?, asset_id, type, files: [path], mode: "copy"\|"link_external"}` | `{ref, run_id?, handoff}` |
    | `asset.collect` | write | `{project?, asset_id}` | `{run_id, handoff}` |
    | `asset.signoff` | write | `{project?, asset_id, run_id, files: [name], bump?, keep_runs?}` | `{ref, version, handoff}` |
    | `asset.request_signoff` | write | `{project?, asset_id}` | `{status}` |
    | `asset.pin_input` | write | `{project?, asset_id, ref, reason}` | `{pinned_inputs}` |
    | `project.create` | write | `{project, name, type?}` | `{project, path}` |
    | `project.set_status` | write | `{project, status: "active"\|"paused"\|"archived"}` | `{project, status, stale: [ref]}` |
    | `project.status` | read | `{project?}` | `{project, status, assets_by_status, orphans: [path], needs_repair: [path]}` |
    | `project.context` | read | `{project?}` | `{project, settings, links, suggested_tools: [name]}` |
    | `trash.list` | read | `{}` | `{entries: [restore record]}` |
    | `trash.restore` | write | `{entry_id}` | `{path}` |

    - `AssetSummary` is `{ref, project, asset_id, type, status, version, approved_version, needs_repair}`.
    - `AssetDetail` is `AssetSummary` plus `{versions: [{version, ref, legacy}], next_versions: {major, minor, patch}, proposed_bump, candidate_runs, human_edits, external, files: [{path, state: "ok"|"not_fetched"|"cloud_only"|"human_edit"}]}`.
    - Every write result that records an operation carries `handoff: {unlocks: [ref], made_stale: [ref], refs: [ref]}` (CORE-020).
  - **Topics** (ARCH-044, ARCH-032):
    - `asset:ww://<workspace id>/<project>/<asset_id>` carries `{kind: "manifest"|"candidate_run"|"version_sealed"|"human_edit"|"needs_repair"|"status", …}`; its snapshot is the `asset.get` result;
    - `project:<slug>` carries `{kind: "status"|"progress"|"asset"}`; its snapshot is the `project.status` result;
    - `workspace` carries `{kind: "orphans"|"git_paused"|"git_resumed"|"rescan"|"default_changed"}`.

    History is kept per topic for 600 s and at most 1024 events (ARCH-032 clarification).
  - **Error codes.** Added to the Appendix D table:

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
    | `ww doctor` | one line per check, `ok: …`, `fixed: …` or `problem: … — <what to do>`; a damaged, newer or hot-journal suite `state.db` is explained, and is set aside (or recovered) only after you type `y` |

    The core-start messages become:
    - "The core can't start: its state database at <path> is damaged. Nothing was changed. `ww doctor` can set it aside."
    - "The core can't start: its state database at <path> was left mid-write by a crash. Nothing was changed. `ww doctor` can finish the recovery."

    The `workspace.unsupported` and unregistered-workspace hints quote the path (`ww workspace use "<path>"`). A workspace named by `--workspace` is never told to become the default; it is told which workspace the core serves.
  - **Fail points** (QA-066, QA-055): `WW_TEST_FAILPOINTS=<name>=<action>[,…]`. They are read only on test channels, and compiled only into builds whose `WW_BUILD_CHANNEL` isn't `stable`.
    - Actions: `abort` (the process ends at once, as a crash would), `disk_full`, `busy`, `unsupported` and `io`.
    - Names: `store.write_temp`, `store.replace`, `store.suite_toml`, `store.after_output`, `store.after_manifest`, `store.after_jobs_log`, `store.after_progress`, `store.after_index`.
  - **Dates:** `YYYY-MM-DD` and RFC 3339 `…Z`, in UTC. The set-aside suffix is `YYYYMMDDTHHMMSSZ`.
  - **Clarifications recorded in Appendix E** (no new rule IDs):
    - ARCH-074: the write algorithm above, including that `ReplaceFileW` has no write-through flag.
    - ARCH-077, QA-048, QA-066: the golden round-trip uses a synthetic manifest with the lantern manifest's structure (owner decision 6). Fixture workspaces are synthetic; none is copied from a real workspace.
    - CORE-040: the budget is exactly 180 characters, and the `.gitignore` form is root-relative.
    - QA-066: fail points are compiled out of stable builds and armed only on test channels.
    - CORE-042: the index-rebuild budget lives in `test-params.toml`.
    - ARCH-032: the per-topic history also has a count cap of 1024.
- **Test-channel tools added by this plan** (module `"test"`, registered only on `test-*` channels; a test contract, not copied into the spec):

  | Tool | Effect | Arguments | Result (`structured`) |
  |---|---|---|---|
  | `test.store_write` | write | `{path, text, expect: "new"\|"generated"\|"<sha256>"}` | `{sha256}`, through `Store::write` |
  | `test.ingest_run` | write | `{project?, asset_id, files: [{name, text}], job_line: bool}` | `{run_id, staging_dir, files: [path]}`; it writes the files into a staging handle as an engine would, then commits a candidate run |
  | `test.trash` | write | `{path}` | `{entry_id}` |
  | `test.watch_fence` | read | `{name}` | `{}` once the watcher has handled `<ws>\.worldwright\fence\<name>` |
  | `test.settings` | read | `{project?, settings?}` | the resolved `Settings` with sources |
- **Privacy** (QA-086): no user-profile paths (write `%USERPROFILE%`), no personal names or e-mail addresses in code, fixtures, test names or reports.
  - Non-ASCII test labels use "Ünïcödé space".
  - Fixture manifests are made up. Authors are `"sample-author"`, and hashes are SHA-256 values of the fixture's own bytes.

## Review Focus

1. **The core crashes between writing an output and updating its manifest**: a killed process, a power cut, a debugger stop.
   - **Expected:**
     - no file is half-written, and no temp file survives the next start;
     - an output with no manifest entry stays on disk and is listed as an orphan, never deleted;
     - a crash after the manifest step leaves a normal, readable run;
     - the dead core's `running` idempotency row is released at the restart (the restarted core holds `core.lock`), so a retry runs the operation again. The first attempt's files stay visible as an orphan or a run, and nothing is lost.
   - **Covered by:** Task 14, `crash_at_each_write_order_step_recovers_cleanly`; Task 5, `crash_after_output_lists_orphan_and_keeps_files` (green in Task 12).
2. **A target is held open** by Defender, Explorer's preview pane or an image viewer when the core replaces it.
   - **Expected:** the write retries with backoff and succeeds once the handle is released. If the handle is still held after 10 s, the call fails with `store.busy` and leaves the target unchanged and no temp file behind.
   - **Covered by:** Task 6, `held_target_is_retried_until_released` and `target_held_past_budget_fails_with_store_busy_and_no_temp`.
3. **Long paths**: a relative path near 180 characters under a deep workspace, and an absolute path past 260.
   - **Expected:** a relative path over 180 characters is refused before any file is touched. A legal relative path under a deep workspace is written even when the absolute path passes 260 characters.
   - **Covered by:** Task 6, `relative_path_over_180_chars_is_refused_before_any_write` and `deep_workspace_past_260_chars_writes_through_long_paths`.
4. **A workspace in a OneDrive-synced or network folder.** FileId or ChangeTime may be unstable there, `ReplaceFileW` may be unsupported, and a read may download a cloud-only file.
   - **Expected:**
     - the index trusts FileId and ChangeTime only on local NTFS or ReFS volumes. Elsewhere it ignores their churn and compares size and mtime, hashing only what changed, so a sync client renumbering IDs doesn't trigger a rehash of everything;
     - cloud-only placeholders are listed as `cloud_only` and never read, so the reconciler never downloads them;
     - where `ReplaceFileW` is unsupported, the two-rename fallback still never loses the old or the new content.
   - **Covered by:** Task 10, `volume_without_stable_ids_ignores_id_churn` and `cloud_placeholder_is_indexed_but_never_read`; Task 6, `replace_falls_back_to_two_renames_where_unsupported`.
5. **`git checkout` of thousands of files** overflows the watcher buffer.
   - **Expected:** the overflow triggers a full rescan. Writes wait while `.git/index.lock` exists, and the index converges. A file that matches its (new) manifest hash is never reported as a human edit.
   - **Covered by:** Task 11, `checkout_of_3000_files_overflows_then_rescans_without_false_human_edits`.

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
- **`test-params.toml` changes (protected).** It records the CORE-042 index-rebuild budget, because QA-019 makes it the home of test budgets.
- **`asset.get` and `assets.query` land with the index (Task 10)**, so the watcher's spec tests can observe human edits before Task 13.
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
- **Adversarial review** (QA-023): Tasks 6, 7, 9, 11, 13 and 14 change `store`, `state` or the dispatcher. Their review is done by a reviewer with fresh context, who must try to write a failing test, not just comment.
- **Spec tests** (QA-017, QA-018): Task 5's author reads only the spec (with Appendices D and E), this plan's Global Constraints and the Interfaces blocks. Later tasks only remove `expected-fail.toml` entries; none edits a spec test.

---

### Task 1: Spec changes first (QA-029)

**Files:**
- Modify: `docs/superpowers/specs/2026-10-12-worldwright-suite-design.md`. No new IDs, no renumbering and no new `- **ID** [` lines; the ID check stays `925 925 []`.

**Interfaces:**
- Produces: spec Appendix E and the Appendix D additions, which Tasks 2–15 implement.

- [ ] **Step 1: Append clarifications to rule text.** Mark each "(plan 2b clarification: Appendix E)":
  - **ARCH-074.** "`ReplaceFileW` has no write-through flag (`REPLACEFILE_WRITE_THROUGH` is documented as unsupported). Durability comes from flushing the temp file before the swap and from `MoveFileExW` with `MOVEFILE_WRITE_THROUGH` for new targets and fallbacks. The outside-edit window is closed by checking the replaced file's backup (Appendix E)."
  - **ARCH-077, QA-048 and QA-066.** "(owner decision 2026-10-15) The golden round-trip uses a synthetic manifest with the lantern manifest's structure. Fixture workspaces are synthetic; nothing is copied from a real workspace."
  - **CORE-040.** "The budget is exactly 180 characters. In the workspace's root `.gitignore`, `assets/*/work/` is written `projects/*/assets/*/work/`."
  - **QA-066.** "Fail points are compiled out of stable builds and armed only on test channels."
  - **CORE-042.** "The budget is recorded in `test-params.toml`."
  - **ARCH-032.** "The history is also capped at 1024 events per topic; an older cursor gets a snapshot."
- [ ] **Step 2: Edit Appendix D.**
  - Add the 22 error-code rows from Global Constraints. The rows `store.hash_conflict` and `workspace.required` lose *provisional* and gain "confirmed by plan 2b".
  - Add exit code 7 to the `ww-core` exit-code table.
  - In the `ww` commands paragraph, replace "`ww doctor` (plan 2b) can set it aside." with "`ww doctor` can set it aside.", and add the hot-journal message.
  - In the Events bullet, after "History: 600 s per topic, by monotonic time", add "and at most 1024 events".
- [ ] **Step 3: Write `## Appendix E. Workspace data (plan 2b clarification)`** after Appendix D.
  - It contains, in order, the "Names that are contracts" bullets of this plan's Global Constraints: paths, write algorithm, write order, append logs, areas, `core.lock`, local data, `.gitignore`, refs, manifest fields, product tools, topics, the commands, fail points, dates and the clarifications.
  - The error codes and exit code 7 go into Appendix D (Step 2), not here.
  - **Exception:** the test-channel tools table stays out of the spec.
  - The text is copied as written. No line may start with `- **` followed by a rule ID.
- [ ] **Step 4: Update §0.5's document map** to end with "· Appendix E Workspace data (plan 2b clarification)".
- [ ] **Step 5: Verify the IDs.**

Run: `python -I -c "import re;t=open('docs/superpowers/specs/2026-10-12-worldwright-suite-design.md',encoding='utf-8').read();ids=re.findall(r'(?m)^- \*\*([A-Z]+-\d{3})\*\* \[',t);refs=set(re.findall(r'\b(?:ARCH|CORE|APP|AI|IMG|QA|OPS)-\d{3}\b',t));print(len(ids),len(set(ids)),sorted(refs-set(ids)))"`
Expected: `925 925 []`

- [ ] **Step 6: Commit.**

```bash
git add docs/superpowers/specs/2026-10-12-worldwright-suite-design.md
git commit -m "spec: workspace data appendix and plan-2b clarifications (write path, codes, tools, golden)"
```

---

### Task 2: Workspace dependencies and the platform file layer (ARCH-074, ARCH-097, CORE-047, QA-038) — **protected: root `Cargo.toml`**

**Files:**
- Modify (protected): `Cargo.toml`. In one edit, add `proptest` and `toml_edit` to `[workspace.dependencies]`, and give `serde_json` the `preserve_order` feature.
- Modify: `crates/ww-platform/Cargo.toml` (`windows` feature `Win32_System_IO`).
- Create: `crates/ww-platform/src/{fs.rs, paths.rs}`. Modify `src/{lib.rs, env.rs, ids.rs, process.rs}`.
- Modify: `crates/ww-core/src/{identity.rs, workspace.rs}` and `crates/ww-client/src/connect.rs`. The four private path normalisers become `ww_platform::paths::folder_key`.
- Modify: `crates/ww-testkit/src/ids.rs` (`SeqIds::uuid`).
- Test: `crates/ww-platform/tests/fs.rs`; unit tests in `paths.rs`, `env.rs` and `ids.rs`; `crates/ww-platform/tests/pipes.rs` (hardened).

**Interfaces:**

Produces:

```rust
// ww_platform::fs — every function opens paths through long_path()
pub fn long_path(p: &Path) -> PathBuf;                            // "\\?\" (or "\\?\UNC\") when absolute and > 259 chars
pub fn flush(file: &std::fs::File) -> io::Result<()>;             // FlushFileBuffers
pub fn replace_file(target: &Path, replacement: &Path, backup: &Path) -> io::Result<()>;
    // ReplaceFileW(REPLACEFILE_IGNORE_MERGE_ERRORS | REPLACEFILE_IGNORE_ACL_ERRORS); never REPLACEFILE_WRITE_THROUGH
pub fn move_new(from: &Path, to: &Path) -> io::Result<()>;        // MoveFileExW(MOVEFILE_WRITE_THROUGH); files and folders
pub fn move_replace(from: &Path, to: &Path) -> io::Result<()>;    // MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH
pub fn set_readonly(path: &Path, on: bool) -> io::Result<()>;     // FILE_ATTRIBUTE_READONLY
pub enum RetryClass { SharingViolation, ReplaceRetry, ReplacementNotMoved, Unsupported, DiskFull, Other }
pub fn classify(e: &io::Error) -> RetryClass;                     // 32/33/1224; 1175; 1177; 1/50; 112/39; else
pub struct FileIdentity { pub volume_serial: u64, pub file_id: [u8; 16], pub size: u64,
                          pub last_write: i64, pub change_time: i64, pub attributes: u32 }
impl FileIdentity { pub fn is_cloud_placeholder(&self) -> bool; }  // RECALL_ON_DATA_ACCESS | RECALL_ON_OPEN | OFFLINE
pub fn identity(path: &Path) -> io::Result<FileIdentity>;
    // CreateFileW(FILE_READ_ATTRIBUTES, share read|write|delete, BACKUP_SEMANTICS|OPEN_REPARSE_POINT),
    // then GetFileInformationByHandleEx(FileIdInfo, FileBasicInfo, FileStandardInfo); never reads data
pub struct VolumeInfo { pub fs_name: String, pub serial: u64, pub remote: bool }
impl VolumeInfo { pub fn stable_ids(&self) -> bool; }             // !remote && fs_name is "NTFS" or "ReFS"
pub fn volume_info(path: &Path) -> io::Result<VolumeInfo>;        // GetVolumeInformationByHandleW + GetDriveTypeW
pub struct LockedFile;                                            // Drop unlocks and closes
pub fn try_lock_exclusive(path: &Path) -> io::Result<Option<LockedFile>>;  // creates; LockFileEx EXCLUSIVE|FAIL_IMMEDIATELY on [0,1)
pub struct AppendLock;
pub fn lock_append(file: &std::fs::File, within: Duration) -> io::Result<AppendLock>;  // range [2^64-2, +1), polled
pub enum Share { None, Read, ReadWrite }
pub fn open_shared(path: &Path, share: Share) -> io::Result<std::fs::File>;  // the handle-holding helper's primitive
pub trait Probe: Send + Sync + std::fmt::Debug {                  // injectable, so tests fake volumes and placeholders
    fn identity(&self, path: &Path) -> io::Result<FileIdentity>;
    fn volume_info(&self, path: &Path) -> io::Result<VolumeInfo>;
}
pub struct SystemProbe;                                           // the two functions above
// ww_platform::paths
pub fn folder_key(path: &Path) -> String;  // canonicalise (when it exists), strip \\?\ and \\?\UNC\, `\` separators,
                                           // trim trailing separators, Unicode lowercase; for comparison only, never hashed
// env.rs
impl Env { pub fn machine_id(&self) -> String; }                  // COMPUTERNAME rule; test channels "testmachine"
// ids.rs
pub trait IdSource { fn id(&self, prefix: &str) -> String; fn uuid(&self) -> String; }   // uuid: v4, lowercase, hyphenated
// process.rs
pub struct Captured { pub code: Option<i32>, pub stdout: String, pub stderr: String }
pub fn run_hidden(exe: &Path, args: &[&OsStr], cwd: &Path, env: &[(OsString, OsString)], within: Duration) -> io::Result<Captured>;
    // CREATE_NO_WINDOW, stdio captured, killed after `within`; the core never flashes a console (ARCH-005)
pub fn find_on_path(name: &str) -> Option<PathBuf>;
```

- [ ] **Step 1: Write the failing tests.**
  - **`tests/fs.rs`** (tempdirs only):
    - `replace_file_swaps_content_and_keeps_the_backup`;
    - `replaced_file_takes_the_replacement_file_id` (documented behaviour the index relies on);
    - `move_new_refuses_an_existing_target`;
    - `move_replace_overwrites_with_write_through`;
    - `held_target_is_a_sharing_violation`: `open_shared(target, Share::Read)`, then `replace_file` → `classify == SharingViolation`;
    - `long_path_round_trip_past_260`: a tempdir nested to 300 characters; `move_new`, `identity` and `set_readonly` all work;
    - `identity_is_stable_across_rename_and_changes_on_replace` (NTFS);
    - `identity_never_hydrates`: a file with `FILE_ATTRIBUTE_OFFLINE` set → `is_cloud_placeholder()`, and its last-access time is unchanged;
    - `volume_info_reports_ntfs_for_the_temp_volume`;
    - `exclusive_lock_is_refused_while_held_and_free_after_drop`;
    - `append_lock_excludes_a_second_holder`;
    - `readonly_attribute_round_trip`.
  - **Unit tests:**
    - `folder_key_strips_prefixes_case_and_trailing_separators`: `\\?\C:\Ws\`, `c:\ws` and `\\?\UNC\srv\share\x` → `\\srv\share\x`;
    - `folder_key_resolves_a_junction_to_its_target` (`mklink /J` in a tempdir through `run_hidden` of `%ComSpec%`);
    - `machine_id_is_lowercase_and_restricted`, and `test_channel_machine_id_is_testmachine`;
    - `random_uuid_is_v4` (version nibble 4, variant `8`–`b`);
    - `run_hidden_captures_output_and_exit_code` (`%ComSpec% /c echo hi & exit 3`).
  - **`tests/pipes.rs`:** `instance_dacl_is_user_and_system_only` also asserts each ACE's access mask is `GENERIC_ALL` (`GA`).
  - Every existing test that compares `serde_json` text keeps passing with `preserve_order` (Step 4 runs the whole workspace).
- [ ] **Step 2: Run them.**

Run: `cargo nextest run -p ww-platform`
Expected: FAIL. Nothing is implemented.

- [ ] **Step 3: Implement it.**
  - Make the three root `Cargo.toml` edits.
  - Replace the four normalisers with `folder_key`: `ww-core/src/identity.rs`, `ww-core/src/workspace.rs`, `ww-client/src/connect.rs`, and `ww-platform/src/env.rs`'s comparison use. `normalise_for_hash` stays: the workspace hash keeps its plan-1 ASCII-lowercase contract (QA-038).
  - Every `unsafe` block gets a `// SAFETY:` comment.
- [ ] **Step 4: Run the whole workspace**, the boundary test and the licence check.

Run: `cargo nextest run --workspace --exclude ww-spec-tests && cargo nextest run -p ww-testkit --test dependency_boundaries && cargo deny check && cargo run -q -p ww-cli -- dev spec-tests`
Expected: PASS.
- The report lists the pinned versions, and states that `schema_hash` is unchanged (`canonical_json` sorts keys, so `preserve_order` doesn't affect it).
- Any test that compared `serde_json` output as text is fixed to compare values, and listed in the report.

- [ ] **Step 5: Commit.**

```bash
git add Cargo.toml Cargo.lock crates/ww-platform crates/ww-core/src/identity.rs crates/ww-core/src/workspace.rs crates/ww-client/src/connect.rs crates/ww-testkit/src/ids.rs
git commit -m "platform: replace/move with write-through, file identity, volume facts, locks, long paths, folder_key; deps proptest, toml_edit"
```

---

### Task 3: The workspace format in `ww-schema` (ARCH-042, ARCH-076, ARCH-077, CORE-031, CORE-033, CORE-047, CORE-048, CORE-148, QA-044, QA-048)

**Files:**
- Modify: `crates/ww-schema/Cargo.toml` (`serde_json`, `yaml-rust2`, `toml_edit`, `sha2`; dev: `proptest`).
- Create: `crates/ww-schema/src/{manifest.rs, refs.rs, status.rs, version.rs, content_hash.rs, paths.rs, yaml_edit.rs, workspace_yaml.rs, suite_edit.rs, logs.rs, time.rs}`. Modify `src/lib.rs`.
- Create (free additions, `add-ok`): the synthetic fixture workspace `crates/ww-spec-tests/fixtures/sample-workspace/**` (below).
- Test: unit tests in each file; `crates/ww-schema/tests/golden.rs`; `crates/ww-schema/tests/props.rs` (proptest, `cases` from `test-params.toml` `[proptest]`).

**Interfaces:**

Produces:

```rust
// manifest.rs: the ordered map is the truth; typed views read it, setters edit it in place
pub struct Manifest { /* serde_json::Map with preserve_order */ }
impl Manifest {
    pub fn parse(text: &str) -> Result<(Manifest, Vec<SchemaWarning>), serde_json::Error>;  // schema violations are warnings
    pub fn new(project: &str, asset_id: &str, asset_type: &str, creation_uuid: &str) -> Manifest; // schema_version 1, status "planned", version "1.0.0"
    pub fn to_text(&self) -> String;                       // serde_json pretty (2 spaces) + "\n"
    pub fn view(&self) -> ManifestView;                    // typed read
    pub fn get(&self, key: &str) -> Option<&serde_json::Value>;
    pub fn set(&mut self, key: &str, value: serde_json::Value);           // existing key keeps its position, else appended
    pub fn runs_mut(&mut self) -> Vec<&mut serde_json::Map<String, serde_json::Value>>;
    pub fn push(&mut self, list_key: &str, value: serde_json::Value);    // creates the list at the end if missing
}
pub struct ManifestView { pub schema_version: Option<u32>, pub asset_id: String, pub project: String, pub asset_type: Option<String>,
    pub status: Option<Status>, pub version: Option<Version>, pub approved_version: Option<Version>,
    pub creation_uuid: Option<String>, pub inputs: Vec<Input>, pub pinned_inputs: Vec<PinnedInput>,
    pub candidate_runs: Vec<CandidateRun>, pub hashes: BTreeMap<String, String>, pub locks: Vec<String>,
    pub human_edits: Vec<HumanEdit>, pub external: Vec<ExternalFile>, pub has_working_doc: bool }
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
pub fn apply(text: &str, edits: &[YamlEdit]) -> Result<String, YamlEditError>;   // line edits; re-parsed with yaml-rust2
pub enum YamlEditError { Unsupported(String), Invalid(String) }
// workspace_yaml.rs
pub struct WorkspaceYaml { pub name: Option<String>, pub defaults: serde_json::Map<String, serde_json::Value> }
pub struct ProjectYaml { pub name: Option<String>, pub slug: Option<String>, pub status: Option<String>, pub modules: Vec<String>,
                         pub links: Vec<String>, pub pipeline: serde_json::Map<String, serde_json::Value>,
                         pub overrides: serde_json::Map<String, serde_json::Value> }
pub fn read_workspace_yaml(text: &str) -> Result<WorkspaceYaml, String>; pub fn read_project_yaml(text: &str) -> Result<ProjectYaml, String>;
pub fn new_workspace_yaml(name: &str, created: &str) -> String; pub fn new_project_yaml(slug: &str, name: &str, kind: &str, created: &str) -> String;
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
| any but `deprecated` | `Block(reason)` | `blocked` | the reason must be non-empty; `status_before_blocked` = from |
| `blocked` | `Unblock` | `status_before_blocked` | |
| any | `Deprecate` | `deprecated` | |

**The synthetic fixture** (owner decision 6). `crates/ww-spec-tests/fixtures/sample-workspace/` contains:
- `worldwright.yaml`, with comments;
- `projects/sketchbook/{project.yaml, progress.md}`;
- two asset folders under `projects/sketchbook/assets/`:
  - **`prop_signal_lamp`**, the lantern's structure (CORE-033). A pre-suite `v1.0.0/` folder holds the candidates, the chosen file and a derived file together, and has no `manifest.snapshot.json`. The manifest has no `candidate_runs` and no `schema_version`, but it does have fields the suite no longer uses (`license_scope`, `commercial_use`, `content_rating`, `budget_usd`; CORE-147/148), an unknown nested object and a `null` field.
  - **`chr_paper_crane`**, in the modern I5 shape: a sealed `v1.0.0/` with `manifest.snapshot.json`, `hashes`, one `selected` and one open run, `inputs` with a `proj:` ref, and `locks`.
- small made-up text and PNG files whose SHA-256 values are the ones recorded in the manifests.

Both manifests are stored in `Manifest::to_text` format.

- [ ] **Step 1: Write the failing tests.**
  - **`tests/golden.rs`** (ARCH-076, ARCH-077, CORE-148, QA-048):
    - `golden_manifests_round_trip_byte_for_byte`: parse then `to_text` equals the fixture bytes, for both manifests;
    - `one_field_change_is_a_one_line_diff`: setting `status` changes exactly one line;
    - `unknown_and_retired_fields_survive_edits`: `license_scope`, the nested unknown object and the `null` field are still present and in place after `set` and `push`;
    - `hand_formatted_manifest_round_trips_by_value_and_order`: a compact one-line copy parses and keeps its key order;
    - `legacy_manifest_reads_with_warnings_not_errors`: `prop_signal_lamp` gives a `SchemaWarning` for the missing `schema_version` and parses.
  - **Unit tests:**
    - `version_parse_order_and_bump` (`1.4.0` minor → `1.5.0`; `1.4` and `v1.4.0` don't parse);
    - `link_refs_round_trip_all_four_forms`;
    - `asset_ref_with_hash_round_trips_and_names_its_live_topic`: `ww://0123456789abcdef/sketchbook/chr_paper_crane@1.0.0#sha256=<64 hex>`, topic `asset:ww://0123456789abcdef/sketchbook/chr_paper_crane`;
    - `candidate_ref_is_view_only_form`;
    - `bad_refs_are_rejected` (uppercase slug, `@1.0`, a 63-hex hash);
    - `content_hash_follows_the_contract`: a hand-built two-line text hashed independently equals `version_content_hash`, and `source/a.png` and `manifest.snapshot.json` are ignored;
    - `status_transitions_follow_the_table` (every row, plus three refusals: `planned` → `Signoff`, `deprecated` → anything but `Deprecate`, `Block("")`);
    - `path_budget_is_180` (180 is ok, 181 is `TooLong`);
    - `reserved_names_are_refused_in_any_case_and_extension` (`con.png`, `Lpt1`, `com¹.txt`, `nul.tar.gz`);
    - `trailing_dot_space_and_bad_chars_are_refused`;
    - `yaml_set_scalar_keeps_comments_and_order`;
    - `yaml_append_to_block_and_flow_lists`;
    - `yaml_edit_refuses_anchor_and_flow_mapping_targets` (the text is unchanged and the error is `Unsupported`);
    - `suite_register_keeps_comments_and_unknown_tables` (a `# note` line and `[caps] monthly_usd = 50` survive; a second register is a no-op);
    - `log_lines_put_machine_and_seq_first`;
    - `reader_skips_a_torn_last_line_and_dedupes`;
    - `utc_dates_cover_leap_years_and_month_ends` (`2028-02-29`, `1999-12-31T23:59:59Z`);
    - `run_ids_use_utc_minutes`.
  - **`tests/props.rs`** (QA-044): `any_json_object_round_trips_by_order_and_value`, and `edits_never_reorder_existing_keys`.
- [ ] **Step 2: Run them.**

Run: `cargo nextest run -p ww-schema`
Expected: FAIL. Nothing is implemented.

- [ ] **Step 3: Implement it**, and add the fixture files.
- [ ] **Step 4: Run them, and the boundary test.**

Run: `cargo nextest run -p ww-schema && cargo nextest run -p ww-testkit --test dependency_boundaries && cargo clippy -p ww-schema --all-targets -- -D warnings`
Expected: PASS.

- [ ] **Step 5: Commit.**

```bash
git add crates/ww-schema crates/ww-spec-tests/fixtures Cargo.lock
git commit -m "schema: lossless manifests, ww:// refs with content hashes, status machine, targeted YAML/TOML edits, synthetic golden"
```

---

### Task 4: `ww-testkit`: fixture workspaces, fail points, handle holder, git-ops simulator, fence and hygiene (QA-031, QA-046, QA-055, QA-066)

**Files:**
- Modify: `crates/ww-testkit/Cargo.toml` (add `ww-schema` and `serde`).
- Create: `crates/ww-testkit/src/{workspace.rs, handles.rs, gitops.rs, failpoints.rs}`. Modify `src/{lib.rs, env.rs, raw_pipe.rs, bins.rs, files.rs}`.
- Test: unit tests in the new files.

**Interfaces:**

Produces:

```rust
impl TestEnv {
    pub fn copy_fixture_workspace(&self, name: &str) -> PathBuf;   // sample-workspace → <root>\ws-<name>
    pub fn workspace_id(&self, ws: &Path) -> String;               // Env::workspace_hash(canonicalize(ws))
    pub fn workspace_local_dir(&self, ws: &Path) -> PathBuf;
    pub fn registered(&self) -> (Option<PathBuf>, Vec<PathBuf>);  // parsed from this channel's suite.toml
    pub fn git_init(&self, ws: &Path);                             // git init + one commit; GIT_CONFIG_NOSYSTEM=1, HOME and
                                                                   // XDG_CONFIG_HOME under <root>\profile; no remote
    pub fn git(&self, ws: &Path, args: &[&str]) -> String;         // same isolation; panics on failure
    pub fn ww_spawn(&self, args: &[&str]) -> WwChild;              // now a wrapper: Drop kills and waits
}
pub struct WwChild; impl WwChild { pub fn stdout_lines(&mut self) -> &mut dyn Iterator<Item = String>;
                                   pub fn stderr_so_far(&self) -> String; }   // stderr drained on a thread
pub fn sha256_bytes(b: &[u8]) -> String;
pub fn version_content_hash_of(dir: &Path) -> String;             // the Appendix E rule, written independently of ww-schema's
pub struct HandleHolder; impl HandleHolder { pub fn hold(path: &Path, share: ww_platform::fs::Share) -> HandleHolder;
                                             pub fn release(self); }   // Defender- or viewer-style open handle
pub mod failpoints { pub const ENV: &str = "WW_TEST_FAILPOINTS"; pub fn spec(points: &[(&str, &str)]) -> String; }
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
  - `fixture_workspace_copies_with_recorded_hashes` (every `hashes` entry matches `sha256_file`);
  - `workspace_id_matches_platform_hash`;
  - `git_init_is_isolated_from_user_config` (`git config --show-origin user.name` names only the test profile);
  - `handle_holder_blocks_replace_until_released`;
  - `gitops_holds_index_lock_until_finish`;
  - `failpoint_spec_formats_pairs`;
  - `version_content_hash_of_ignores_source_and_snapshot`;
  - `drop_without_a_core_returns_without_waiting` (a fresh `TestEnv`; dropping it makes no hello attempt, checked through a counter in the drop path, not a timer);
  - `call_returns_on_a_null_id_error` (a fake pipe server answers `{nope` with -32700);
  - `ww_child_is_killed_on_drop`;
  - `ensure_built_rebuilds_when_flagged_but_missing` (pure helper `needs_build(flag, exists) -> bool`).
- [ ] **Step 2: Run them.**

Run: `cargo nextest run -p ww-testkit`
Expected: FAIL. Nothing is implemented.

- [ ] **Step 3: Implement it.**
- [ ] **Step 4: Run them again.**

Run: `cargo nextest run -p ww-testkit && cargo clippy -p ww-testkit --all-targets -- -D warnings`
Expected: PASS.

- [ ] **Step 5: Commit.**

```bash
git add crates/ww-testkit Cargo.lock
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
  - `cli_basics.rs`: `status_no_start_reports_not_running` also asserts that `env.suite_state_db()` doesn't exist afterwards (a started core always creates it).
- No change to `crates/ww-spec-tests/Cargo.toml`: everything comes through `ww-testkit`, `ww-protocol` and `serde_json`.

**Interfaces:**
- Consumes: Task 4's testkit API, the Global Constraints contracts and test-channel tools, and the fixture of Task 3.
- Produces: the scenario tests that Tasks 7–14 turn green; each task removes its entries. The last column says which task.

- [ ] **Step 1: Write the tests.**

  **`ws_tools.rs`**

  | Test | Rules | Assertion | Green in |
  |---|---|---|---|
  | `workspace_init_writes_layout_and_registers_default` | CORE-007, ARCH-098 | `ww workspace init "<root>\Ünïcödé space ws"` → exit 0; `worldwright.yaml`, `.gitignore` (all five entries), `projects/sketchbook/project.yaml` and `progress.md` exist; `registered().0` is that path; `ww status --json` `hello.workspace.path` equals it | 8 |
  | `workspace_use_switches_default_and_old_sessions_get_unsupported` | ARCH-090 | session A says hello with workspace 1; `ww workspace use <ws2>` → exit 0; A's next `test.store_write` → `workspace.unsupported` naming ws2; a new hello with ws2's id is ok | 8 |
  | `unsupported_workspace_error_carries_set_default_hint` | QA-003, ARCH-090 | a hello naming a registered non-default workspace → `error.fix.agent_hint.tool == "workspace.set_default"`, `arguments.path` = that path | 8 |
  | `suite_toml_edits_keep_comments_and_unknown_keys` | CORE-007, ARCH-076 | `suite.toml` holding `# my note` and `[caps]\nmonthly_usd = 50` before `ww workspace init`; afterwards both lines are unchanged, and the file parses with the new entry | 8 |

  **`store_writes.rs`**

  | Test | Rules | Assertion | Green in |
  |---|---|---|---|
  | `outside_edit_is_never_overwritten` | ARCH-074, ARCH-073 | `test.store_write {path:"notes/a.txt", text:"core", expect:"new"}` → ok; the test writes "mine" to the file; `test.store_write {…, text:"core 2", expect:"generated"}` → `store.hash_conflict`; the file reads "mine" | 7 |
  | `failed_write_leaves_no_half_written_file` | QA-011, ARCH-074 | core started with `store.write_temp=disk_full`; a write over an existing file → `store.disk_full`; the file's SHA-256 is unchanged and no `*.ww-tmp-*` file exists anywhere under the workspace | 7 |
  | `relative_path_over_budget_is_refused` | CORE-040 | a 181-character path → `store.path_invalid` and nothing exists under its first folder; `con.txt` → `store.path_invalid`; a 180-character path → ok | 7 |
  | `registry_ledger_csv_is_never_written` | ARCH-082 | a fixture `registry/ledger.csv`; writing to it → `store.path_invalid`; its bytes are unchanged | 7 |

  **`store_order.rs`**

  | Test | Rules | Assertion | Green in |
  |---|---|---|---|
  | `crash_after_output_lists_orphan_and_keeps_files` | ARCH-075, CORE-038, QA-055 | core with `store.after_output=abort`; `test.ingest_run` → the core exits within 30 s; the run's files exist under `candidates/<run>/`; a restarted core's `project.status` lists that folder in `orphans`; the files still exist | 12 |
  | `append_log_lines_are_per_machine_per_month_with_seq` | ARCH-081, CORE-009 | two `test.ingest_run {job_line:true}` → exactly one `queue/jobs.testmachine.<yyyy-mm>.jsonl`; its lines parse with `machine == "testmachine"` and `seq` 1 and 2, `machine` and `seq` being the first two keys | 7 |
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
  | `trash_keeps_restore_record_and_restores` | ARCH-058, CORE-037, CORE-110 | `test.trash {path}` → the file is gone; `trash.list` has an entry with `original_path` and a `purge_after` 30 days after `moved_utc`; the folder is `.trash\<YYYY-MM-DD>\…`; `trash.restore` puts the bytes back | 9 |

  **`index_watch.rs`** (each watcher assertion follows a `RawPipe::fence`)

  | Test | Rules | Assertion | Green in |
  |---|---|---|---|
  | `damaged_index_is_rebuilt` | ARCH-089 | stop the core; write 4 KiB of `0xAB` over `workspace_local_dir(ws)\index.db`; restart → `assets.query` lists both fixture assets | 10 |
  | `outside_edit_to_sealed_file_is_a_human_edit` | CORE-034, ARCH-079 | clear the read-only bit of a `chr_paper_crane/v1.0.0/` file and change it; fence → `asset.get` `human_edits` has that path with the new SHA-256; the file keeps the edited bytes | 11 |
  | `invalid_manifest_is_marked_needs_repair_and_kept` | ARCH-079, ARCH-076 | write `{` into a manifest; fence → `asset.get` `needs_repair == true`; the file is still exactly `{` | 11 |
  | `writes_wait_while_git_holds_index_lock` | ARCH-083 | subscribe to `workspace`; create `.git\index.lock`; send `test.store_write` → a `git_paused` event arrives; remove the lock → `git_resumed` arrives before the write's ok response | 11 |
  | `lfs_pointer_is_not_fetched_not_corrupt` | ARCH-085 | replace a sealed file with an LFS pointer whose `oid` is its recorded SHA-256; fence → `asset.get` `files[].state == "not_fetched"`, no human edit | 11 |
  | `ww_notify_reconciles_and_is_silent_without_core` | ARCH-016, ARCH-073 | with no core, `ww notify --no-start <file>` → exit 0, empty output; with a core running, an outside edit then `ww notify <file>` (no fence) → `asset.get` shows the human edit | 11 |

  **`projects_settings.rs`**

  | Test | Rules | Assertion | Green in |
  |---|---|---|---|
  | `project_create_status_and_context` | CORE-055 | `project.create {project:"harbor", name:"Harbor"}` → `projects/harbor/project.yaml` and `progress.md`; `project.status` ok; `project.context` lists settings and `suggested_tools`; a second create → `project.exists` | 12 |
  | `settings_cascade_overrides_workspace_then_project_then_request` | CORE-057 | `defaults.models.image_generate: a` in `worldwright.yaml`, `b` in `project.yaml`; `test.settings` → `b` from `project`; with `settings {"models.image_generate":"c"}` → `c` from `request`; `{"caps.monthly_usd":1}` → `params.invalid` | 12 |
  | `progress_entries_go_under_done` | CORE-039 | after `project.create`, `progress.md` has `- YYYY-MM-DD: ` (today, UTC) as the first line under `## Done`; the other sections are byte-equal to the template | 12 |
  | `archived_project_restore_runs_staleness_check` | CORE-055, CORE-048 | archive `sketchbook`, give an input a newer sealed version on disk, restore → `stale` lists the dependent's ref, and its manifest `status == "stale"` with `status_before_stale` | 12 |
  | `active_project_is_per_session_with_state_json_hint` | ARCH-088 | connection A calls `project.status {project:"harbor"}`; connection B's `project.status {}` → the hint (`harbor`), not A's choice made later; after B names `sketchbook`, A's `project.status {}` is still `harbor`; `.worldwright\state.json` names the last one used | 12 |

  **`asset_record.rs`**

  | Test | Rules | Assertion | Green in |
  |---|---|---|---|
  | `import_creates_candidate_run_with_handoff_and_uuid` | CORE-020, CORE-023, CORE-047 | `asset.import` → `structured.handoff` has `unlocks`, `made_stale` and `refs`; the manifest has `creation_uuid` (UUID v4), one run and status `candidate`; `progress.md` has one new `## Done` line | 13 |
  | `request_signoff_sets_in_review` | CORE-053 | → `status == "in_review"` in both the result and the manifest | 13 |
  | `asset_topic_delivers_events_and_snapshots` | ARCH-044, ARCH-032 | subscribe to the asset's live topic; sign-off → an event with `kind == "version_sealed"`, `version == "1.0.0"`; a second connection subscribing with `since_seq` 999 gets one `snapshot` whose `data.asset_id` matches | 13 |
  | `proj_input_is_recorded_in_project_links` | CORE-019 | `asset.pin_input {ref:"proj:harbor/chr_x@1.0.0"}` → `project.yaml` `links` gains `proj:harbor`, with the file's comments kept | 13 |
  | `legacy_manifest_edit_is_a_minimal_diff` | ARCH-076, ARCH-077, CORE-033, CORE-148 | `asset.get prop_signal_lamp` lists `v1.0.0` as `legacy: true`; `asset.request_signoff` on it changes only the `status` line, apart from new keys appended at the end; the retired fields are byte-identical | 13 |

  **`doctor.rs`**

  | Test | Rules | Assertion | Green in |
  |---|---|---|---|
  | `doctor_fixes_git_config_and_gitignore_then_changes_nothing` | CORE-040, ARCH-085, QA-060 | `git_init`, then `.gitignore` emptied; `ww doctor` → exit 0 with `fixed:` lines; `git config core.longpaths` is `true` and `lfs.setlockablereadonly` is `false`; a second run prints only `ok:` lines and the SHA-256 values of `.gitignore` and `.git\config` are unchanged | 14 |
  | `doctor_sets_damaged_state_db_aside_only_after_yes` | CORE-004 | a corrupt suite `state.db`; `ww status` stderr says "`ww doctor` can set it aside." and not "(plan 2b)"; `ww doctor` with stdin `n` → exit 1, file unchanged; with `y` → `state.db.damaged-<YYYYMMDDTHHMMSSZ>` beside it with the old SHA-256; `ww status` → exit 0 | 14 |
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
- Modify: `crates/ww-core/clippy.toml`. Add `ww_platform::fs::{replace_file, move_new, move_replace, set_readonly, try_lock_exclusive, lock_append, open_shared, flush}` to `disallowed-methods`, with the ARCH-112 reason.
- Modify: `crates/ww-core/src/state/{mod.rs, migrations.rs}`.
  - Migration 2, both scopes: `CREATE INDEX idempotency_prune ON idempotency(state, created_utc);`, so the prune in `idem_begin` no longer scans the table inside `BEGIN IMMEDIATE`.
  - Migration 3, workspace scope: `generated(path TEXT PRIMARY KEY, sha256 TEXT NOT NULL, written_utc INTEGER NOT NULL)`.
- Modify: `crates/ww-core/src/config.rs` (`failpoints`).
- Test: unit tests in `store/`; `crates/ww-core/tests/store_write.rs` (paused runtime).

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
    pub fn from_env(env: &Env, var: &dyn Fn(&str) -> Option<String>) -> Arc<FailPoints>; // none unless test channel and BUILD_CHANNEL != "stable"
    pub fn hit(&self, name: &str) -> io::Result<()>;                         // Abort → std::process::abort()
    pub fn on(&self, name: &str, hook: Box<dyn Fn() + Send + Sync>);         // in-process tests: run code at a point
}
// CoreConfig gains `pub failpoints: Arc<FailPoints>` (from_env in CoreConfig::for_env)
// store/paths.rs
pub struct WsPath(String);                                     // validated, '/' separators
impl WsPath { pub fn parse(rel: &str) -> Result<WsPath, StoreError>; pub fn as_str(&self) -> &str;
              pub fn abs(&self, root: &Path) -> PathBuf; pub fn join(&self, segment: &str) -> Result<WsPath, StoreError>; }
pub fn writable(rel: &WsPath) -> Result<(), StoreError>;       // refuses registry/ledger.csv and anything under projects/*/assets/*/v<semver>/
// store/write.rs
pub enum Expect { New, Generated, Hash(String) }
pub struct Written { pub path: WsPath, pub sha256: String, pub identity: FileIdentity }
pub enum StoreError { HashConflict(WsPath), Busy(WsPath), PathInvalid { path: String, why: String }, DiskFull(WsPath),
                      Io { path: String, why: String }, Locked(WsPath), GitBusy, ReadOnly, NeedsRepair(WsPath),
                      YamlUnsupported(String), VersionExists(String) }
impl StoreError { pub fn to_error(&self, trace_id: &str) -> ErrorBody; }   // the Appendix D codes
pub enum ChangeKind { Written, Created, Removed, MovedFrom(WsPath) }
pub struct StoreEvent { pub path: WsPath, pub kind: ChangeKind, pub sha256: Option<String>, pub identity: Option<FileIdentity> }
pub trait ChangeSink: Send + Sync {                            // the index step (Task 10); Store emits, the index consumes (CORE-010)
    fn applied<'a>(&'a self, events: &'a [StoreEvent]) -> Pin<Box<dyn Future<Output = ()> + Send + 'a>>;
}
pub fn lfs_pointer_oid(bytes: &[u8]) -> Option<String>;        // "version https://git-lfs.github.com/spec/v1" … "oid sha256:<hex>"
pub struct Store;
impl Store {
    pub fn new(root: PathBuf, state: Arc<StateDb>, failpoints: Arc<FailPoints>, clock: Arc<dyn Clock>, ids: Arc<dyn IdSource>) -> Store;
    pub fn root(&self) -> &Path;
    pub fn set_sink(&self, sink: Arc<dyn ChangeSink>);
    pub async fn read(&self, rel: &WsPath) -> Result<Option<Vec<u8>>, StoreError>;
    pub async fn write(&self, rel: &WsPath, bytes: Vec<u8>, expect: Expect) -> Result<Written, StoreError>;
    pub async fn edit_manifest(&self, rel: &WsPath, f: &(dyn Fn(&mut Manifest) -> Result<(), StoreError> + Send + Sync))
        -> Result<(Manifest, Written), StoreError>;            // read → f → write with Expect::Hash(read); an outside change re-reads and re-applies, at most 3 times
    pub async fn edit_yaml(&self, rel: &WsPath, edits: &[YamlEdit]) -> Result<Written, StoreError>;
    pub async fn edit_text(&self, rel: &WsPath, f: &(dyn Fn(&str) -> Result<String, StoreError> + Send + Sync)) -> Result<Written, StoreError>;
    pub async fn recover(&self) -> Result<RecoveryReport, StoreError>;   // stale temps removed, orphaned backups restored
}
pub struct RecoveryReport { pub temps_removed: Vec<WsPath>, pub backups_restored: Vec<WsPath> }
// state additions
impl StateDb { pub async fn generated(&self, path: &str) -> Result<Option<String>, StateError>;
               pub async fn set_generated(&self, path: &str, sha256: &str, utc: i64) -> Result<(), StateError>; }
// store/fsops.rs additions (one #[allow] each)
pub fn write_temp(dir: &Path, name: &str, bytes: &[u8], fp: &FailPoints) -> io::Result<PathBuf>;   // create, write, ww_platform::fs::flush
pub fn replace(target: &Path, temp: &Path, backup: &Path) -> io::Result<()>;
pub fn move_new(from: &Path, to: &Path) -> io::Result<()>; pub fn move_replace(from: &Path, to: &Path) -> io::Result<()>;
pub fn remove_file(path: &Path) -> io::Result<()>; pub fn remove_dir_all(path: &Path) -> io::Result<()>;
pub fn set_readonly(path: &Path, on: bool) -> io::Result<()>; pub fn copy_new(from: &Path, to: &Path) -> io::Result<u64>;
pub fn lock_exclusive(path: &Path) -> io::Result<Option<ww_platform::fs::LockedFile>>;
pub fn append_locked(path: &Path, line: &str, within: Duration) -> io::Result<()>;   // lock_append, close a torn line, one write_all
```

**The write path** is exactly the Global Constraints algorithm. Blocking file calls run in `spawn_blocking`, and the retry sleeps are `tokio::time::sleep`, so paused-time tests drive them.

- [ ] **Step 1: Write the failing tests** (`tests/store_write.rs`, `#[tokio::test(start_paused = true)]`, tempdir workspaces):
  - `new_file_write_records_generated_hash` (ARCH-078);
  - `full_write_over_outside_edit_is_hash_conflict` (ARCH-074);
  - `edit_between_check_and_replace_puts_the_outside_version_back` (ARCH-074): a hook on `store.replace` writes "outside" to the target; the result is `HashConflict`, the file reads "outside", and no backup or temp file remains;
  - **`held_target_is_retried_until_released`** (Review Focus 2): `HandleHolder::hold(target, Share::Read)`; start the write; `advance(3 s)`; release; the write is ok;
  - **`target_held_past_budget_fails_with_store_busy_and_no_temp`** (Review Focus 2): `advance(11 s)` → `Busy`; the target's SHA-256 is unchanged; no `*.ww-tmp-*` file;
  - **`relative_path_over_180_chars_is_refused_before_any_write`** (Review Focus 3): no folder of the path was created;
  - **`deep_workspace_past_260_chars_writes_through_long_paths`** (Review Focus 3): the workspace root is nested so that the target's absolute path is 320 characters; the write and the read are ok;
  - **`replace_falls_back_to_two_renames_where_unsupported`** (Review Focus 4): `store.replace=unsupported` → ok with the new content;
  - `fallback_crash_between_renames_is_restored_by_recover`: a hook stops after target → backup; `recover()` restores the old content;
  - `replace_errors_map_to_retry_recover_or_fail` (pure: `RetryClass` → next step, for 1175, 1177, 32 and 112);
  - `disk_full_leaves_no_temp_and_the_target_unchanged` (QA-011);
  - `ledger_csv_and_sealed_folders_are_refused` (ARCH-082, CORE-026);
  - `lfs_pointer_compares_by_oid` (ARCH-078): a pointer whose `oid` equals the generated hash passes `Expect::Generated`;
  - `manifest_edit_rereads_and_reapplies_after_an_outside_change`;
  - `invalid_manifest_is_needs_repair_and_untouched`;
  - `yaml_edit_keeps_comments_and_is_hash_checked`;
  - `stale_temps_of_dead_processes_are_removed_by_recover` (a temp named for a live PID is kept);
  - `failpoints_arm_only_on_test_channels_of_non_stable_builds` (QA-066);
  - `store_errors_map_to_appendix_d_codes`;
  - `idempotency_prune_uses_its_index` (`EXPLAIN QUERY PLAN` of the prune names `idempotency_prune`).
- [ ] **Step 2: Run them.**

Run: `cargo nextest run -p ww-core --test store_write`
Expected: FAIL. Nothing is implemented.

- [ ] **Step 3: Implement it.**
- [ ] **Step 4: Run them, lint, and do a deliberate-violation check.**

Run: `cargo nextest run -p ww-core -p ww-testkit && cargo clippy -p ww-core --all-targets -- -D warnings`
Expected: PASS.

Then the deliberate-violation check: in a scratch copy, a `ww_platform::fs::replace_file` call in `store/write.rs` must make clippy fail. Record the output, then remove the line.

- [ ] **Step 5: Commit.**

```bash
git add crates/ww-core Cargo.lock
git commit -m "store: hash-checked atomic writes with ReplaceFileW backup check, sharing-violation retries, path budget, fail points"
```

---

### Task 7: `store` order and areas, `WorkspaceHub` and `core.lock` (ARCH-028, ARCH-033, ARCH-075, ARCH-080, ARCH-081, CORE-003, CORE-009, CORE-038, QA-012)

**Files:**
- Create: `crates/ww-core/src/store/{order.rs, logs.rs, staging.rs}`.
- Modify: `crates/ww-core/src/workspace.rs` (`WorkspaceHub`, `OpenWorkspace`, `Access`).
- Modify: `crates/ww-core/src/state/mod.rs`. `for_workspace` takes an `Access`; read-only opens never migrate or release rows.
- Modify: `crates/ww-core/src/tools/{registry.rs, dispatcher.rs, test_tools.rs, mod.rs}` and `crates/ww-core/src/server/mod.rs`. The hub opens the default workspace at start and `close_all` runs before the state databases close.
- Test: unit tests in the new files; `crates/ww-core/tests/{store_order.rs, workspace_hub.rs}`; additions to `tests/state.rs` and `tests/dispatch.rs`.

**Interfaces:**

Consumes: Task 6's `Store`.

Produces:

```rust
// store/order.rs
pub struct OutputMove { pub from: PathBuf /* inside a staging handle */, pub to: WsPath }
pub type ManifestEdit = Arc<dyn Fn(&mut Manifest) -> Result<(), StoreError> + Send + Sync>;
pub type TextEdit = Arc<dyn Fn(&str) -> Result<String, StoreError> + Send + Sync>;
pub struct CommitOp { pub outputs: Vec<OutputMove>, pub manifest: Option<(WsPath, ManifestEdit)>,
                      pub job_line: Option<serde_json::Map<String, Value>>, pub progress: Option<(WsPath, TextEdit)> }
pub struct CommitReport { pub outputs: Vec<Written>, pub manifest: Option<Manifest>, pub job_seq: Option<u64> }
impl Store {
    pub async fn commit(&self, op: CommitOp) -> Result<CommitReport, StoreError>;  // ARCH-075 order; fail points between steps
    pub async fn orphans(&self) -> Result<Vec<WsPath>, StoreError>;               // candidates/<run>/ folders missing from their manifest
}
// store/logs.rs
pub enum LogKind { Jobs, Usage }
impl Store { pub async fn append_log(&self, kind: LogKind, record: serde_json::Map<String, Value>) -> Result<u64, StoreError>; }
pub async fn append_loose_usage(env: &Env, record: serde_json::Map<String, Value>) -> Result<u64, StoreError>; // local_dir\usage-loose.jsonl
// store/staging.rs
pub struct StagingHandle { pub id: String, pub dir: PathBuf }
impl Store { pub async fn staging_handle(&self) -> Result<StagingHandle, StoreError>;
             pub async fn release_handle(&self, h: StagingHandle) -> Result<(), StoreError>; }   // removes an emptied handle folder
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
```

**Rules:**
- **`WorkspaceHub::open`**, in order:
  1. `check_workspace(path)`;
  2. `fsops::ensure_dir(<ws>\.worldwright)` and `fsops::lock_exclusive(core.lock)`: `Some` → `ReadWrite`, `None` → `ReadOnly`;
  3. `states.for_workspace(ws, access)`;
  4. `Store::new`, then `store.recover()` (read-write only);
  5. `orphans()`, published once on `workspace` as `{kind:"orphans", paths}` when not empty.

  The lock lives as long as the `OpenWorkspace`.
- **Dispatcher, Resolve.** With a session workspace:
  - its id differs from `hub.default_id()` → `workspace.unsupported` (owner decision 2, after a switch);
  - otherwise `hub.open`, and `CallCtx.workspace` is set;
  - an effect other than `read` on a `ReadOnly` workspace → `workspace.read_only`, before `idem_begin`.
- **`commit`** steps:
  1. outputs: `move_new` each `OutputMove`;
  2. `edit_manifest`;
  3. `append_log(Jobs)`;
  4. `edit_text(progress)`;
  5. `sink.applied(events of 1–4)`.

  The fail points are `store.after_output`, `store.after_manifest`, `store.after_jobs_log`, `store.after_progress` and `store.after_index`. An error after step 1 leaves the outputs where they are (they become orphans); nothing is ever deleted.
- **Test tools:** `test.store_write` and `test.ingest_run`, per the Global Constraints table. `test.ingest_run` uses `staging_handle`, writes the files through `fsops::write_temp` as an engine would, and commits one candidate run (`Manifest::new` if missing, status via `transition(…, CandidateRun)`).

- [ ] **Step 1: Write the failing tests:**
  - **`tests/store_order.rs`:**
    - `commit_writes_output_manifest_log_progress_then_index` (ARCH-075): a recording `ChangeSink` and hooks see the steps in that order;
    - `error_after_output_leaves_a_listed_orphan_never_deleted` (ARCH-075, CORE-038), for each fail point after step 1;
    - `append_takes_the_shared_lock_and_numbers_lines` (ARCH-081): a second `lock_append` holder delays the append until released;
    - `month_rollover_starts_a_new_file` (`FakeClock::shift_utc`);
    - `torn_last_line_is_closed_and_skipped_by_readers`;
    - `loose_usage_goes_to_the_local_folder` (ARCH-081);
    - `staging_handles_are_in_the_workspace_and_ingest_keeps_file_ids` (ARCH-080, CORE-003, ARCH-033): the `FileIdentity.file_id` is equal before and after the move, and the volume serial equals the workspace's.
  - **`tests/workspace_hub.rs`:**
    - `core_lock_gives_read_write_and_releases_running_rows` (ARCH-028);
    - `second_hub_without_the_lock_is_read_only_and_keeps_rows` (ARCH-028): two `StateRegistry`s whose envs map the workspace to the same local folder; the second gets `ReadOnly`; the first's `running` row survives; the second's `state.db` connection is read-only;
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
Expected: FAIL. Nothing is implemented.

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
- Modify: `crates/ww-core/src/tools/{registry.rs, dispatcher.rs, mod.rs}` and `crates/ww-core/src/workspace.rs` (the agent hint, quoted paths).
- Modify: `crates/ww-client/src/lib.rs` (hint quoting, the `--workspace` wording) and its tests (`tests/client.rs`).
- Create: `crates/ww-cli/src/user/workspace.rs`. Modify `user/mod.rs` (`Workspace` subcommand; the `--no-start` probe).
- Test: unit tests; `crates/ww-core/tests/{workspace_tools.rs, workspace_create_guard.rs}`; `crates/ww-cli/tests/workspace_cli.rs`; `crates/ww-cli/tests/user_cli.rs` (hygiene).

**Interfaces:**

Consumes: Task 7's `WorkspaceHub`, Task 3's `suite_edit::register` and YAML templates.

Produces:

```rust
// store/suite_toml.rs: the core is the only writer of suite.toml (owner decision 1)
pub async fn register_workspace(env: &Env, path: &Path, make_default: bool, fp: &FailPoints) -> Result<bool, StoreError>;
    // read → suite_edit::register → fail point "store.suite_toml" → write_temp + replace with the read hash;
    // an outside change re-reads once
// tools/workspace_tools.rs
pub fn register_workspace_tools(reg: &mut Registry, hub: Arc<WorkspaceHub>, env: Env, probe: Arc<dyn Probe>);
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
  1. Make the path absolute; `fsops::ensure_dir`; canonicalise; `check_workspace`.
  2. Refuse with `workspace.path_invalid` a path inside `install_dir` (ARCH-094), inside another registered workspace, or one that is a file.
  3. If `worldwright.yaml` exists, register only (`created: false`); nothing is written in the folder.
  4. Otherwise `hub.open(ws)`. Through its store, write `projects/sketchbook/project.yaml`, `projects/sketchbook/progress.md` (`PROGRESS_TEMPLATE`, ARCH-098), and `.gitignore` (the five entries, appended to an existing file through `edit_text`), and **last** `worldwright.yaml`, whose presence marks the folder as a workspace.
  5. `register_workspace` (`default` = the argument, else true when no default is set); `hub.refresh_default()`; `hub.close(id)` unless it is the default.

  Warnings: `probe.volume_info` is remote or has no stable ids, or the folder carries cloud-file attributes → "This folder is on a network or cloud-synced volume…" (Review Focus 4).
- **`workspace.set_default`:**
  - the path must hold `worldwright.yaml`, else `workspace.path_invalid`;
  - register with `make_default`;
  - `refresh_default`;
  - publish `{kind:"default_changed", id, path}` on `workspace`.
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
    - `create_refuses_install_dir_nested_workspace_and_files` (ARCH-094);
    - `create_warns_on_network_or_cloud_volumes` (fake `Probe`);
    - `set_default_registers_and_switches` (ARCH-090);
    - `suite_toml_keeps_comments_and_unknown_keys_and_is_hash_checked` (CORE-007): a `FailPoints::on("store.suite_toml", …)` hook changes the file between read and replace → re-read once → both edits are present;
    - `unsupported_fix_has_the_agent_hint_and_a_quoted_path` (QA-003).
  - **`tests/workspace_create_guard.rs`:** `create_on_a_real_registered_workspace_trips_the_guard` (QA-040, its own binary, ordered as plan 2a's guard test).
  - **Unit tests:**
    - `registry_refuses_schemas_that_allow_unknown_properties`;
    - `every_registered_tool_forbids_additional_properties`;
    - `idem_finish_failure_says_what_happens`;
    - `ww-client`: `unregistered_hint_quotes_the_path_and_flag_source_is_not_told_to_switch`.
  - **`ww-cli` parse:** `workspace_init_and_use_parse`.
  - **`tests/workspace_cli.rs`:**
    - `init_creates_and_prints_the_default`;
    - `use_works_with_a_stale_default` (the default folder is deleted first);
    - `no_start_with_an_unresolvable_workspace_while_a_core_runs`: `ww --no-start --workspace <not a workspace> call test.echo '{"text":"x"}'` with a core running → exit 2 with the plain message; the `--no-start` probe passes on errors other than `NotRunning` instead of swallowing them.
  - **`ww-client` test gaps:**
    - `older_client_drain_and_wait_is_incompatible`;
    - `racing_starts_yield_one_core` also asserts exactly one `core.*.log` under the logs folder;
    - the fake-server test's raw-newline literals become `\n` escapes.
  - **Hygiene:** `tests/user_cli.rs` `events_follow_prints_ndjson` uses `WwChild`, so its child's stderr is drained and shown on failure.
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
- Modify: `crates/ww-core/src/store/write.rs` (the `locks` check), `tools/{mod.rs, test_tools.rs}` (`test.trash`), and `workspace.rs` (maintenance at open and every 24 h).
- Test: unit tests; `crates/ww-core/tests/{versions.rs, trash.rs}`.

**Interfaces:**

Consumes: Task 6–7 `Store`; Task 3's `Version`, `version_content_hash`.

Produces:

```rust
// store/versions.rs
pub struct SealRequest { pub project: String, pub asset_id: String, pub version: Version, pub run_id: String,
                         pub files: Vec<String>, pub snapshot: Manifest /* the manifest as it stands after sign-off */ }
pub struct Sealed { pub dir: WsPath, pub files: Vec<(String, String)> /* rel, sha256 */, pub content_hash: String }
pub struct VersionDir { pub version: Version, pub dir: WsPath, pub legacy: bool, pub content_hash: String }
impl Store {
    pub fn version_lock(&self, project: &str, asset_id: &str) -> Arc<tokio::sync::Mutex<()>>;   // per asset
    pub async fn seal(&self, req: SealRequest) -> Result<Sealed, StoreError>;
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
    pub async fn maintenance(&self, now_utc: i64) -> Result<MaintenanceReport, StoreError>;
}
pub fn register_trash_tools(reg: &mut Registry);       // trash.list, trash.restore
```

**`seal`** (CORE-026). It holds `version_lock` for the whole call.
1. Refuse when `v<version>/` exists (`version.exists`).
2. Create `<ws>\.worldwright\staging\seal-<sh_…>\`.
3. Move the chosen files and their `derived` files from `candidates/<run>/`. A chosen file that another run file was derived from is copied instead (I5). Move `source/` if the run has one.
4. Write `manifest.snapshot.json` (`snapshot.to_text()`).
5. `set_readonly` on every file.
6. `move_new` the folder to `projects/<p>/assets/<a>/v<version>/`.
7. Record the generated hashes, and compute `content_hash`.

Each move hits the fail point `store.replace`, which covers every swap or move of a file into place, and retries sharing violations as in Task 6. On an error, already-moved files are moved back to the run folder, and the staging folder (if not empty) is left for the orphan listing. A half-filled `v<version>/` never exists.

**Other rules:**
- **`versions`:** a `v*/` folder without `manifest.snapshot.json`, in an asset whose manifest has no matching `candidate_runs` entry, is `legacy` (CORE-033). It is read as it is and never moved.
- **Locks** (CORE-035): `Store::write` and `edit_*` refuse a path under an asset folder that its manifest lists in `locks` → `store.locked`.
- **Trash** (ARCH-058, CORE-037, CORE-110). `trash` moves the file or folder to `.trash\<date>\<te_…>\payload\<name>` and writes `restore.json` next to it.
- **`maintenance`:**
  - It removes entries whose `purge_after` has passed. This is the core's own maintenance, not an agent call.
  - It moves the folders of `rejected` runs to trash 90 days after their version's sign-off date, unless `pinned` (CORE-032), and records `"trashed_utc"` on the run.
  - The only hard deletes in `ww-core` are this purge, stale temps (Task 6) and the index rebuild (Task 10).

- [ ] **Step 1: Write the failing tests:**
  - **`tests/versions.rs`:**
    - `seal_moves_files_and_writes_a_read_only_snapshot` (CORE-026);
    - `derived_from_files_are_copied_not_moved`;
    - `source_moves_with_the_version_and_is_outside_the_content_hash` (CORE-028);
    - `seals_of_one_asset_are_serialised`: two concurrent seals for `1.1.0` and `1.2.0` both succeed, and a second `1.1.0` → `VersionExists`;
    - `held_file_during_seal_is_retried` (`HandleHolder`, paused time);
    - `failed_seal_leaves_no_half_version`: `store.replace=io` midway → no `v<version>/`, and every file exists exactly once, in the run or the staging folder;
    - `writes_into_sealed_folders_are_refused`;
    - `locked_paths_are_refused_with_store_locked` (CORE-035);
    - `legacy_folder_is_listed_as_legacy` (fixture `prop_signal_lamp`, CORE-033).
  - **`tests/trash.rs`:**
    - `trash_moves_with_a_restore_record` (ARCH-058, CORE-037);
    - `restore_refuses_an_occupied_path` (`trash.restore_conflict`);
    - `trash_purges_after_30_days_only` (`FakeClock`: 29 days kept, 31 days purged);
    - `rejected_runs_go_to_trash_90_days_after_signoff_unless_pinned` (CORE-032);
    - `hard_deletes_happen_only_in_purge_temp_cleanup_and_index_rebuild` (CORE-110): a source scan of `crates/ww-core/src` finds `fsops::remove_` calls only in `store/trash.rs`, `store/write.rs` (`recover`) and `index/`;
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

### Task 10: `index` and the asset read tools (ARCH-089, ARCH-096, CORE-006, CORE-010, CORE-041, CORE-042) — **protected: `test-params.toml`**

**Files:**
- Create: `crates/ww-core/src/index/{mod.rs, schema.rs, reconcile.rs, query.rs}` and `crates/ww-core/src/tools/asset_read_tools.rs` (`assets.query`, `asset.get`).
- Modify: `crates/ww-core/src/workspace.rs`. `OpenWorkspace` gains `pub index: Arc<Index>`; `open` opens it after the store and sets it as the store's `ChangeSink`. The first reconcile runs in the background, so `open` doesn't wait for it.
- Modify (protected): `test-params.toml`. Add `[budgets]` with `index_rebuild_2000_assets_s = 30` and a comment naming CORE-042 and `crates/ww-core/tests/index_bench.rs`.
- Test: unit tests; `crates/ww-core/tests/{index.rs, index_bench.rs, module_deps.rs}`.

**Interfaces:**

Consumes: Task 6's `ChangeSink`, `StoreEvent`; Task 2's `Probe`.

Produces:

```rust
pub const INDEX_SCHEMA_VERSION: i64 = 1;
pub enum IndexError { Sql(String), Io(String), ActorGone }
pub enum Scope { All, Paths(Vec<WsPath>) }
pub enum FileState { Ok, NotFetched, CloudOnly, HumanEdit, NeedsRepair }
pub struct FileRow { pub path: WsPath, pub size: u64, pub sha256: Option<String>, pub state: FileState, pub identity: Option<FileIdentity> }
pub struct AssetRow { pub project: String, pub asset_id: String, pub asset_type: Option<String>, pub status: Option<String>,
                      pub version: Option<String>, pub approved_version: Option<String>, pub manifest: WsPath, pub needs_repair: bool }
pub struct AssetQuery { pub project: Option<String>, pub status: Option<String>, pub asset_type: Option<String>,
                        pub text: Option<String>, pub limit: usize }
pub struct ReconcileReport { pub unchanged: usize, pub hashed: usize, pub changed: Vec<WsPath>, pub removed: Vec<WsPath>,
                             pub cloud_only: Vec<WsPath>, pub needs_repair: Vec<WsPath> }
pub struct Index;                                    // one connection on its own thread ("ww-index-<id>")
impl Index {
    pub fn open(path: &Path, root: &Path, probe: Arc<dyn Probe>) -> Result<Index, IndexError>;
    pub async fn reconcile(&self, scope: Scope) -> Result<ReconcileReport, IndexError>;
    pub async fn rebuild(&self) -> Result<ReconcileReport, IndexError>;
    pub async fn verify_pending(&self, max_files: usize) -> Result<usize, IndexError>;
    pub async fn assets(&self, q: AssetQuery) -> Result<Vec<AssetRow>, IndexError>;
    pub async fn asset(&self, project: &str, asset_id: &str) -> Result<Option<AssetRow>, IndexError>;
    pub async fn files_of(&self, project: &str, asset_id: &str) -> Result<Vec<FileRow>, IndexError>;
    pub async fn dependents(&self, project: &str, asset_id: &str) -> Result<Vec<(String, String)>, IndexError>; // (project, asset_id)
    pub async fn set_state(&self, path: &WsPath, state: FileState) -> Result<(), IndexError>;
}
impl ChangeSink for Index {}
pub fn register_asset_read_tools(reg: &mut Registry);  // assets.query, asset.get (AssetSummary, AssetDetail)
```

**Rules:**
- **Open:** `index.db` in `workspace_local_dir(ws)`, WAL, `synchronous=NORMAL` (it is disposable).
  - A `user_version` other than `INDEX_SCHEMA_VERSION`, a failed `quick_check` or a file that isn't a database means it is closed, removed with its `-wal`/`-shm` through `fsops::remove_file`, and created afresh with a cold rebuild (ARCH-089).
  - Tables: `files`, `assets`, `edges(from_project, from_asset, to_project, to_asset, to_version, kind)` and `assets_fts` (FTS5).
- **Reconcile per file** (CORE-041), walking the workspace without `.git/`, `.worldwright/`, `.trash/` and the temp and backup names:
  - `probe.identity` first.
  - A cloud placeholder is `CloudOnly` and is never opened for reading (Review Focus 4).
  - With `volume_info(root).stable_ids()`, the file is unchanged when size, last write, FileId and ChangeTime all match the row. Otherwise only size and last write are compared, and FileId/ChangeTime churn is ignored.
  - Anything else is hashed.
  - A manifest that changes is re-parsed into `assets` and `edges`; one that doesn't parse is `NeedsRepair` and its asset row is flagged.
  - An LFS pointer's row keeps the pointer's `oid` as `sha256`, with state `NotFetched`.
- **Cold rebuild:** manifests are parsed, their `hashes` are trusted as the files' SHA-256 and marked unverified, and `verify_pending` hashes them in the background (at most 200 files per tick, on a 1 s tokio interval).
- **`ChangeSink::applied`:** takes the store's `sha256` and `identity` as is, without rehashing the file the store just wrote.
- **`asset.get`** reads the manifest through the store, the versions through `Store::versions` (with `legacy`), the file states from the index, and `next_versions` from `approved_version` (or `1.0.0` when there is none). `proposed_bump` is `patch` when the open run's `target_version` says so, else `minor`.

- [ ] **Step 1: Write the failing tests:**
  - **`tests/index.rs`:**
    - `fresh_index_is_wal_and_versioned` (ARCH-089);
    - `version_mismatch_or_failed_quick_check_rebuilds` (ARCH-089);
    - `reconcile_hashes_only_changed_files` (CORE-041; `hashed` counts);
    - `store_writes_feed_the_index_without_rehashing`;
    - **`volume_without_stable_ids_ignores_id_churn`** (Review Focus 4): the fake `Probe` returns a new FileId and ChangeTime on every call and `stable_ids() == false`; two reconciles → the second has `hashed == 0`. On a stable volume, a FileId change alone → hashed;
    - **`cloud_placeholder_is_indexed_but_never_read`** (Review Focus 4): the fake probe sets `FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS`; the counting read hook records 0 opens; the state is `CloudOnly`;
    - `cold_rebuild_trusts_manifest_hashes_then_verifies` (CORE-041);
    - `manifest_inputs_become_edges_and_dependents`;
    - `invalid_manifest_marks_needs_repair`;
    - `machine_local_files_never_land_in_the_workspace` (ARCH-096, CORE-006): after open, writes and reconciles, no `*.db*` file exists under the workspace, and `index.db` is in `workspace_local_dir`;
    - `asset_get_reports_legacy_versions_and_next_versions`;
    - `assets_query_filters_by_project_status_type_and_text`.
  - **`tests/module_deps.rs`:** `module_dependencies_are_acyclic_and_store_depends_on_no_consumer` (CORE-010). It scans `use crate::<module>` in each CORE-014 module's files and asserts:
    - the graph is acyclic;
    - `store` uses none of `index`, `watch`, `graph`, `pipeline`, `projects`, `settings`, `tools`;
    - `index` uses none of `watch`, `pipeline`, `projects`, `settings`, `tools`.
  - **`tests/index_bench.rs`:** `cold_rebuild_of_2000_assets_fits_the_budget` (CORE-042). It builds 2000 assets × 3 files in a tempdir, times `rebuild()` with `std::time::Instant` (the stated exception to QA-031), and compares against `[budgets] index_rebuild_2000_assets_s`, read from `test-params.toml`. It prints the measured time.
  - Remove `index_watch::damaged_index_is_rebuilt` from `expected-fail.toml`.
- [ ] **Step 2: Run them.**

Run: `cargo nextest run -p ww-core --test index --test module_deps --test index_bench`
Expected: FAIL. Nothing is implemented.

- [ ] **Step 3: Implement it**, with the protected `test-params.toml` edit.
- [ ] **Step 4: Run them, then the spec tests.**

Run: `cargo nextest run -p ww-core && cargo clippy -p ww-core --all-targets -- -D warnings && cargo run -q -p ww-cli -- dev spec-tests`
Expected: PASS; `spec tests: 43 passed, 23 expected-fail`. The report quotes the benchmark's measured time.

- [ ] **Step 5: Commit.**

```bash
git add crates/ww-core test-params.toml crates/ww-spec-tests/expected-fail.toml
git commit -m "index: disposable index.db reconciled by path/size/mtime/FileId/ChangeTime, cloud-safe; asset read tools; rebuild benchmark"
```

---

### Task 11: `watch`, the git gate and `ww notify` (ARCH-016, ARCH-073, ARCH-079, ARCH-083, ARCH-085, CORE-034)

**Files:**
- Create: `crates/ww-platform/src/watcher.rs` (`ReadDirectoryChangesW` on its own thread, stopped with `CancelIoEx`).
- Create: `crates/ww-core/src/watch/{mod.rs, reconcile.rs, git.rs}`, `crates/ww-core/src/store/git_gate.rs` and `crates/ww-core/src/tools/watch_tools.rs`.
- Modify: `store/write.rs` (step 2 waits on the gate), `workspace.rs` (`OpenWorkspace.watch`, started for `ReadWrite` workspaces and stopped by `close`), and `tools/test_tools.rs` (`test.watch_fence`).
- Create: `crates/ww-cli/src/user/notify.rs`. Modify `user/mod.rs`.
- Test: `crates/ww-platform/tests/watcher.rs`; unit tests; `crates/ww-core/tests/watch.rs`; `crates/ww-cli/tests/notify_cli.rs`.

**Interfaces:**

Consumes: Task 10's `Index`; Task 6's `Store::edit_manifest`.

Produces:

```rust
// ww-platform watcher.rs
pub enum DirChange { Added(PathBuf), Removed(PathBuf), Modified(PathBuf), RenamedFrom(PathBuf), RenamedTo(PathBuf), Overflow }
pub struct DirWatcher;          // FILE_NOTIFY_CHANGE_{FILE_NAME,DIR_NAME,SIZE,LAST_WRITE,ATTRIBUTES}, subtree; relative paths
impl DirWatcher { pub fn start(root: &Path, buffer_bytes: u32) -> io::Result<(DirWatcher, tokio::sync::mpsc::UnboundedReceiver<DirChange>)>;
                  pub fn stop(&self); }                  // 0 bytes returned or ERROR_NOTIFY_ENUM_DIR → Overflow
// store/git_gate.rs (store owns it; watch sets it: CORE-010)
pub const GIT_WAIT: Duration = Duration::from_secs(60);
pub struct GitGate;
impl GitGate { pub fn set_busy(&self, busy: bool); pub fn is_busy(&self) -> bool;
               pub async fn wait_clear(&self, budget: Duration) -> Result<(), StoreError>; }   // GitBusy after the budget
impl Store { pub fn git_gate(&self) -> &Arc<GitGate>; }
// watch
pub struct WatchConfig { pub buffer_bytes: u32 /* 65536 */, pub batch: Duration /* 200 ms */ }
pub struct Watcher;
impl Watcher {
    pub fn start(store: Arc<Store>, index: Arc<Index>, events: Arc<EventBus>, topic_ws: String, cfg: WatchConfig) -> io::Result<Arc<Watcher>>;
    pub async fn notify(&self, paths: Vec<WsPath>) -> NotifyReport;
    pub async fn fence(&self, name: &str);              // returns once `.worldwright/fence/<name>` and everything before it is handled
    pub fn stop(&self);
}
pub struct NotifyReport { pub reconciled: Vec<String>, pub ignored: Vec<String> }
// watch/reconcile.rs: pure, shared with the simulated QA-046 variant (Task 14)
pub enum FileKind { Candidate, Sealed, Work, Manifest, Other }
pub struct Recorded { pub sha256: String, pub kind: FileKind, pub manifest: Option<WsPath> }
pub enum Observed { Changed { path: WsPath, sha256: String, lfs_oid: Option<String> }, CloudOnly(WsPath), Removed(WsPath), BadManifest(WsPath) }
pub enum Action { Reindex(WsPath), HumanEdit { manifest: WsPath, path: WsPath, sha256: String, previous: String },
                  NeedsRepair(WsPath), NotFetched(WsPath), DeferUntilGitDone(WsPath) }
pub fn decide(obs: &Observed, recorded: Option<&Recorded>, git_busy: bool) -> Action;
pub fn register_watch_tools(reg: &mut Registry);       // watch.notify
// ww-cli
pub enum UserCommand { /* … */ Notify { #[arg(required = true)] paths: Vec<PathBuf> } }
```

**`decide`** (ARCH-079, CORE-034, ARCH-085):

| Observed | Recorded | git busy | Action |
|---|---|---|---|
| any change | — | yes | `DeferUntilGitDone` (decided by the rescan after git) |
| `Changed`, `lfs_oid == Some(recorded)` | Candidate/Sealed | no | `NotFetched` |
| `Changed`, `sha256 != recorded` | Candidate/Sealed | no | `HumanEdit` (the file is never touched) |
| `Changed` | Work, Other or none | no | `Reindex` |
| `BadManifest` | any | no | `NeedsRepair` (never rewritten) |
| `CloudOnly` | any | either | `Reindex` (state `CloudOnly`, not read) |
| `Removed` | any | either | `Reindex` |

The recorded hash of a candidate or sealed file is its manifest's `hashes` entry, else its run's file record, else its last generated hash.

**Watcher rules:**
- Changes are batched for `batch` (tokio time).
- `.worldwright\staging\` and `.trash\` are ignored. Of `.worldwright\` only `fence\` is watched; of `.git\` only `index.lock` and `HEAD`.
- **Git** (ARCH-083):
  - `index.lock` created, or `HEAD` changed → `git_gate.set_busy(true)` and `{kind:"git_paused"}`;
  - `index.lock` gone and `HEAD` unchanged for one batch → a full `index.reconcile(All)`, `decide` for every deferred path, then `set_busy(false)` and `{kind:"git_resumed"}`.

  Store writes wait in `wait_clear(GIT_WAIT)` and fail with `store.git_busy` after 60 s.
- **Overflow** → the same full rescan, and `{kind:"rescan"}`.
- `HumanEdit` → `edit_manifest` pushes `{path, sha256, previous_sha256, detected}` to `human_edits` (a human edit is offered as a new version; the UI is plan 4), then `asset:` `{kind:"human_edit"}`. `NeedsRepair` → `index.set_state`, then `asset:` `{kind:"needs_repair"}`.
- **`ww notify <path>…`** maps each path to the session workspace's relative path (with `folder_key`); a path outside it goes to `ignored`. It calls `watch.notify` and prints nothing on success. With `--no-start` and no core, it is a silent no-op (ARCH-018, ARCH-103).

- [ ] **Step 1: Write the failing tests:**
  - **`ww-platform/tests/watcher.rs`:**
    - `dir_watcher_reports_create_modify_rename_delete`;
    - `tiny_buffer_overflows_into_an_overflow_event` (4096 bytes, 2000 renames);
    - `stop_ends_the_thread`.
  - **Unit tests:**
    - `decide_follows_the_table` (every row);
    - `git_gate_waits_up_to_60_s_then_git_busy` (paused time: 59 s pending, 61 s → `GitBusy`);
    - `notify_maps_paths_and_ignores_outside_ones`.
  - **`ww-core/tests/watch.rs`** (real NTFS tempdir, fence):
    - `outside_edit_to_candidate_or_sealed_file_is_recorded_never_overwritten` (CORE-034);
    - `work_folder_edits_are_indexed_not_human_edits` (ARCH-079);
    - `staging_trash_and_git_internals_are_not_watched` (ARCH-079);
    - `invalid_manifest_is_needs_repair_and_edits_refuse` (`manifest.needs_repair`; the bytes are unchanged);
    - `lfs_pointer_with_matching_oid_is_not_fetched` (ARCH-085);
    - `index_lock_pauses_store_writes_until_the_rescan` (ARCH-083);
    - **`checkout_of_3000_files_overflows_then_rescans_without_false_human_edits`** (Review Focus 5). With `WatchConfig.buffer_bytes = 4096`, `GitOps` rewrites 2,990 plain files, plus 5 sealed files and their manifests with matching new `hashes`, then finishes. After the fence:
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
- Modify: `tools/stages.rs` (`Settings`, `SettingsStage`, `PipelineHooks::plan2` uses `CascadeSettings`), `tools/{registry.rs, dispatcher.rs, mod.rs, test_tools.rs}` (`CallCtx.project`; `test.settings`), and `rpc.rs` (`ActiveProjects::forget` in `Router::closed`).
- Test: unit tests; `crates/ww-core/tests/{projects.rs, settings.rs}`.

**Interfaces:**

Consumes: Task 3's `WorkspaceYaml`, `ProjectYaml`, `PROGRESS_TEMPLATE`; Task 10's `Index::dependents`.

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
pub fn add_done_entry(text: &str, date: &str, entry: &str) -> Result<String, String>;  // first line under "## Done"
pub async fn restore_staleness(ws: &OpenWorkspace, project: &str) -> Result<Vec<String /* ref */>, StoreError>;
// graph (pure)
pub fn is_stale(input_version: &Version, approved: Option<&Version>, pinned: bool) -> bool;
pub fn cycles(edges: &[((String, String), (String, String))]) -> Vec<Vec<(String, String)>>;
pub fn register_project_tools(reg: &mut Registry, tools: Arc<OnceLock<Vec<ToolDescriptor>>>);   // filled by core_registry at the end
```

**Rules:**
- **Settings cascade** (CORE-057):
  - workspace `defaults` → `project.yaml` → the request's `settings` object: a plain override chain over dotted leaf keys (`models.<task>`, `providers.<…>`, `qa_profile`, `auto_approve`, `skip_stages`, `auto_candidate_on_save`, `modules`);
  - lists replace whole lists;
  - a request key from `SUITE_LEVEL_KEYS` or an unknown key → `params.invalid` ("… is a suite-level setting");
  - `check` or `record` in `skip_stages` is dropped with a `SchemaWarning` (CORE-054);
  - `auto_candidate_on_save` defaults to `false`, and nothing in this plan acts on it (CORE-049).
- **Resolve** (ARCH-088, ARCH-098). The project is chosen in this order:
  1. the `project` argument;
  2. `ActiveProjects` for the session;
  3. `state.json`'s hint;
  4. `sketchbook`.

  A call that names a project and ends `ok` sets the session's active project and, on a `ReadWrite` workspace, writes the hint.
- **Project tools** (CORE-055):
  - `project.create` writes `project.yaml` (from the template), then `progress.md` with the entry `- <date>: project created` → `project.exists` / `params.invalid` (bad slug);
  - `project.set_status` edits `status` with `edit_yaml`. Moving from `archived` or `paused` to `active` runs `restore_staleness`: each asset of the project whose input's `approved_version` is newer than its ref and isn't pinned goes `MarkStale` (manifest `status_before_stale`), and is listed in `stale`. Archived projects are skipped when dependents are scanned (I6);
  - `project.status` gives counts by status, `orphans` (from `Store::orphans`) and `needs_repair`;
  - `project.context` gives the settings with sources, `links`, and `suggested_tools`: the tools whose module is `workspace` or in `modules()` (CORE-058).
- **`progress.md`** (CORE-039): the new entry is the first line under `## Done`; every other section is left byte for byte. A file without `## Done` gets the section appended at the end.

- [ ] **Step 1: Write the failing tests:**
  - **`tests/settings.rs`:**
    - `cascade_is_a_plain_override_chain` (CORE-057);
    - `request_cannot_set_suite_level_or_unknown_keys`;
    - `check_and_record_cannot_be_skipped` (CORE-054);
    - `auto_candidate_on_save_defaults_off_and_has_working_doc_survives_edits` (CORE-049).
  - **`tests/projects.rs`:**
    - `resolve_explicit_then_session_then_hint_then_sketchbook` (ARCH-088, ARCH-098);
    - `active_project_is_forgotten_when_the_connection_closes`;
    - `hint_is_written_only_on_read_write_workspaces`;
    - `progress_entry_goes_first_under_done_and_other_sections_are_untouched` (CORE-039);
    - `project_create_refuses_existing_and_bad_slugs`;
    - `restore_marks_unpinned_dependents_stale_with_status_before_stale` (CORE-055, CORE-048);
    - `archived_projects_are_skipped_in_dependents`;
    - `project_context_suggests_tools_of_enabled_modules` (CORE-058);
    - `project_status_lists_orphans_and_needs_repair` (CORE-038);
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
- Create: `crates/ww-core/src/pipeline/{mod.rs, record.rs, signoff.rs, topics.rs}` and `crates/ww-core/src/tools/asset_tools.rs`.
- Modify: `tools/stages.rs` (`RecordStage` becomes asynchronous; `PipelineHooks::plan2` uses `WorkspaceRecord`), `tools/{registry.rs, dispatcher.rs}` (`CallCtx.record`), `events.rs` (snapshots outside the locks; history cap), `rpc.rs` (`ww/subscribe` awaits the bus; no connection lock is held across it), and `crates/ww-protocol/src/events.rs` (`HISTORY_MAX_EVENTS`).
- Test: unit tests; `crates/ww-core/tests/{assets.rs, record.rs}`; additions to `tests/events.rs`.

**Interfaces:**

Consumes: Tasks 7, 9, 10 and 12.

Produces:

```rust
// ww-protocol
pub const HISTORY_MAX_EVENTS: usize = 1024;
// pipeline/record.rs
pub enum RecordBranch { CandidateRun { run_id: String }, SealedVersion { version: Version } }
pub struct HandOff { pub unlocks: Vec<String>, pub made_stale: Vec<String>, pub refs: Vec<String> }
pub struct RecordPlan { pub branch: RecordBranch, pub project: String, pub asset_id: String, pub commit: CommitOp,
                        pub progress_entry: String, pub refs: Vec<String> }
pub struct RecordSlot; impl RecordSlot { pub fn set(&self, plan: RecordPlan); pub fn take(&self) -> Option<RecordPlan>; }
pub struct WorkspaceRecord;
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
// pipeline/topics.rs
pub struct AssetSnapshots; pub struct ProjectSnapshots;          // impl SnapshotProvider
// events.rs (changed)
pub trait SnapshotProvider: Send + Sync { fn snapshot<'a>(&'a self, topic: &'a str) -> Pin<Box<dyn Future<Output = Value> + Send + 'a>>; }
impl EventBus { pub async fn subscribe(&self, client: ClientId, topics: &[String], since: &BTreeMap<String, u64>) -> Vec<TopicState>; }
pub fn register_asset_tools(reg: &mut Registry);  // asset.import, asset.collect, asset.signoff, asset.request_signoff, asset.pin_input
```

**The Record stage** (CORE-015 stage 9, CORE-020) runs only when the outcome is `ok` and the tool set a plan:
1. `store.commit(plan.commit)`, with `progress` = `add_done_entry(today, plan.progress_entry)`. That covers output → manifest → jobs log → progress → index.
2. For `SealedVersion`, the dependents from `index.dependents` that aren't pinned and are `approved`, `exported` or `released` go `MarkStale` (`edit_manifest`), into `made_stale`. `unlocks` lists dependents in `planned` or `blocked` whose every input is now sealed.
3. Events go out: `asset:` `{kind:"candidate_run"|"version_sealed", version?, run_id?}` and `project:` `{kind:"progress"}`.
4. `structured.handoff` is set.

A commit error replaces the outcome with the store error. Outputs already moved stay as listed orphans (ARCH-075). Repairs and QA (CORE-025's middle steps) are the empty stages between ingest and record.

**Tools** (Global Constraints table):
- **`asset.import`.** A missing asset gets `Manifest::new` with `ids.uuid()` as `creation_uuid` (CORE-047).
  - `copy` copies each file into a staging handle with `fsops::copy_new`, then plans a candidate run (CORE-023).
  - `link_external` records `external: [{path, sha256}]` and no run; the status goes to `in_progress`. Untrusted file names are marked untrusted in the result (ARCH-060).
- **`asset.collect`** re-hashes each external file. A mismatch is `ref.hash_mismatch`; otherwise the file is copied in as a candidate run and `external` is cleared.
- **`asset.signoff`:**
  - with no session workspace → `workspace.required`;
  - with a non-empty `external` → `asset.link_external`, with `fix.ui_action {id:"asset.collect", label:"Collect into workspace"}` and `agent_hint {tool:"asset.collect", arguments:{project, asset_id}}`;
  - otherwise, under `version_lock`: `plan_signoff` (the version is `bump(approved)` or the manifest's `version`, CORE-027), `Store::seal` with the planned manifest as the snapshot, then a `SealedVersion` plan whose manifest edit re-applies `plan_signoff` to the re-read manifest. Open runs for the version become `rejected` unless in `keep_runs`, which become `pinned: true`.

  The result `ref` is `ww://…@<v>#sha256=<content hash>` (ARCH-042).
- **`asset.request_signoff`:** `RequestSignoff`, through `edit_manifest` (CORE-053). It writes no `progress.md` entry: it isn't a Record branch.
- **`asset.pin_input`** pushes `{ref, reason, by, date}` to `pinned_inputs`. A `proj:` or `lib:` ref whose source isn't in `project.yaml` `links` is appended there with `edit_yaml` (CORE-019).

**Event bus changes** (before the first real snapshot provider):
- **`subscribe`:**
  1. Under the lock: record the topics; queue the replays the history covers; for each topic that needs a snapshot, remember `s0 = current seq` and mark the topic *pending* for that client (live events for a pending topic are kept in the history, not queued). Release the lock.
  2. Await each provider, holding no lock.
  3. Re-lock. If the client still subscribes to the topic, queue the snapshot (seq `s0`), then the history events with seq > `s0`, and clear *pending*.
- **History** is pruned by age (600 s) and count (`HISTORY_MAX_EVENTS`). A cursor older than the retained first event gets a snapshot.

- [ ] **Step 1: Write the failing tests:**
  - **`tests/record.rs`:**
    - `record_runs_after_meter_and_applies_the_plan_in_write_order` (CORE-015, CORE-020);
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
    - `refs_exclude_source_and_carry_the_content_hash` (CORE-028, CORE-031);
    - `pin_input_records_proj_and_lib_links_keeping_comments` (CORE-019);
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

### Task 14: `ww doctor` basics, crash recovery and the outside-writers reconciler (ARCH-016, ARCH-085, CORE-004, CORE-040, QA-011, QA-046, QA-055, QA-060; owner decision 5)

**Files:**
- Modify: `crates/ww-core/src/state/mod.rs`.
  - The read-only check opens with `immutable=1` (URI) when no `-wal` file exists, so a refused file never gets fresh `-wal`/`-shm` files.
  - A non-empty `-journal` beside the file is `StateError::HotJournal`.
- Modify: `crates/ww-core/src/config.rs` and `server/mod.rs` (`CoreExit::StateHotJournal(PathBuf)` → exit 7).
- Create: `crates/ww-core/src/tools/doctor_tools.rs` (`workspace.doctor`).
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
// ww-core
pub struct DoctorArgs { pub fix: bool }
pub struct DoctorCheck { pub id: String, pub state: String /* ok | fixed | problem */, pub detail: String }
pub fn register_doctor_tools(reg: &mut Registry, probe: Arc<dyn Probe>);
    // checks: git.repository, git.core_longpaths, git.lfs_setlockablereadonly, gitignore.entries,
    //         workspace.volume, workspace.orphans, workspace.needs_repair
// ww-cli
pub enum UserCommand { /* … */ Doctor }
```

**`ww doctor`, in order:**
1. **The suite `state.db`** (owner decision 5), with `inspect_suite_state`:
   - `Damaged` or `TooNew` → explain in plain words. With `--non-interactive`: `problem: … — run ww doctor interactively to set it aside`. Otherwise ask "Set it aside as state.db.damaged-<utc> and start with a fresh database? [y/N]". `y` → `set_aside` → `fixed: …`; anything else → `problem`.
   - `HotJournal` → ask "Let SQLite finish its recovery? [y/N]", then `finish_recovery`. If the file is still damaged, the set-aside question follows.
2. **`suite.toml`**, read with `ww_schema::SuiteToml`, never through `resolve_workspace_id`. If the default workspace is missing: `problem: … — run ww workspace use "<path>"`.
3. **The workspace checks**, through a connection whose hello names the default workspace's id: `workspace.doctor {fix:true}`, printed line by line.
   - The core runs `git config --local core.longpaths true` and `git config --local lfs.setlockablereadonly false` with `run_hidden(find_on_path("git"))` (CORE-040, ARCH-085). Without `.git`, both are `ok` with "not a git repository; skipped".
   - The five `.gitignore` entries are appended through `edit_text`.
   - Orphans and needs-repair manifests are listed as `problem`s, never fixed automatically (CORE-038).
   - A network or cloud volume is a `problem` with an explanation.

   A second run changes nothing (QA-060).

- [ ] **Step 1: Write the failing tests:**
  - **`tests/crash_recovery.rs`** (QA-055, the fast subset):
    - **`crash_at_each_write_order_step_recovers_cleanly`** (Review Focus 1). For each of `store.after_output`, `store.after_manifest`, `store.after_jobs_log`, `store.after_progress` and `store.after_index`:
      1. `CoreFixture::start_with_env` with that point at `abort`; `test.ingest_run {job_line:true}` → the core exits.
      2. A fresh core, same key: no `*.ww-tmp-*` or `*.ww-bak-*` remains; `project.status` lists the run in `orphans` only for `after_output`; every manifest parses; every log line parses (`read_lines` drops none); `assets.query` agrees with the manifests.
      3. The retry with the same key runs again (the dead core's `running` row was released under `core.lock`), and both runs' files exist.
    - `disk_full_mid_commit_keeps_files_consistent`;
    - `held_manifest_during_commit_is_retried` (paused time);
    - `corrupt_or_missing_index_db_is_rebuilt_at_open`;
    - `damaged_workspace_state_db_fails_that_workspace_not_the_core` (`state.unavailable`; the file is unchanged);
    - `refused_wal_db_gets_no_new_side_files` (a WAL-mode db with `user_version` 999 and no `-wal`: after the refused start, no `-wal` or `-shm` exists);
    - `hot_journal_exits_7_with_nothing_changed`.
  - **`tests/reconcile_sim.rs`** (QA-046, simulated, shrinkable): `outside_writers_never_lose_edits`.
    - A `proptest` strategy generates operation sequences, which shrink on failure. They run against an in-memory file map, applying the pure write decision (`store::write::decide_write(expect, current) -> WriteDecision`, factored out in this task) and `watch::reconcile::decide`.
    - Operations: outside edit, delete, rename, partial manifest edit, invalid manifest, checkout of n files (with overflow), LFS pointer, edit between check and replace, core write, core manifest edit, crash at a write-order step.
    - Invariants: no outside edit is overwritten; invalid manifests are marked, never rewritten; the index model converges after a rescan; no orphan is deleted; no write happens while `index.lock` exists.
    - `cases` comes from `test-params.toml`.
  - **`tests/reconcile_ntfs.rs`** (QA-046, real NTFS with the fence): the same scenarios and invariants, through a real core and `GitOps`. Each scenario ends with a fence; then the core's view (`asset.get`, `assets.query`) and the files on disk are compared.
  - **`tests/doctor_tool.rs`:**
    - `doctor_sets_git_config_and_gitignore_then_changes_nothing` (CORE-040, ARCH-085, QA-060);
    - `doctor_without_git_skips_with_a_note`;
    - `doctor_lists_orphans_and_needs_repair_without_fixing`.
  - **`ww-client/tests/doctor.rs`:**
    - `inspect_never_creates_side_files`;
    - `set_aside_moves_db_wal_and_shm_and_deletes_nothing`;
    - `finish_recovery_rolls_back_a_hot_journal`;
    - `start_failure_messages_name_doctor_without_plan_2b`.
  - **`ww-cli/tests/doctor_cli.rs`:**
    - `doctor_answers_n_keeps_the_file` and `doctor_answers_y_sets_it_aside_and_the_core_starts`;
    - `non_interactive_doctor_never_moves_it`;
    - `doctor_with_a_stale_default_reports_it_and_still_runs`.
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
- Modify (protected): `crates/ww-cli/src/main.rs`. In the `Dev` branch only: when `global.any_set()`, print "`ww dev` doesn't take --workspace, --non-interactive or --no-start." and exit 2. This is the only change to the file.
- Modify: `crates/ww-cli/src/user/mod.rs` (`GlobalArgs::any_set`).
- Modify: `crates/ww-core/src/{server/mod.rs, server/conn.rs, main.rs, config.rs, tools/dispatcher.rs}` and `crates/ww-platform/src/session_end.rs` (core robustness, below).
- Modify: `dev/rule-classification.toml`.
- Test: `crates/ww-cli/src/user/mod.rs` parse tests; additions to `crates/ww-core/tests/{transport.rs, lifecycle.rs}` and `crates/ww-platform/tests/session_end.rs`.

**Interfaces:**
- Produces: `impl GlobalArgs { pub fn any_set(&self) -> bool; }`, and `SessionEndSim { QueryEnd, End, Close }` as the `simulate` selector, separate from the reported `SessionEnd` events.

- [ ] **Step 1: Write the failing tests:**
  - `dev_commands_reject_user_global_flags` (`ww dev spec-report --no-start` → exit 2 with that message), and `dev_commands_still_parse`;
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
    - every rule a plan-2b test cites (the "rules this plan implements" list, each checked by `grep -rn "spec: .*<ID>" crates`);
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
