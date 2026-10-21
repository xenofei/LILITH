# Plan 4a — App Shell I: Shell and Plumbing Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Give the suite a runnable app shell, LILITH (the shell) inside `lil-image`, with the plumbing every later panel stands on:
- spikes first (cold start, GUI stalls under ComfyUI load, notifications end to end, the lazily loaded WebEngine library, deploy size, the KDDockWidgets 2.5.0 build check, keep-awake, Windows Hello, the first Narrator smoke check);
- the build (CMake, cxx-qt, KDDockWidgets, the C++ shim), the `lil-qt` bridge and a dev-channel deploy folder;
- the shell with layouts, single instance and the connection lifecycle; the Home, Jobs, Confirmations, Usage & spend, Hardware and Diagnostics panels; the confirm window;
- notifications through classic toasts, `lil:` activation and `.lilref` links, so a confirmation can be answered with no app open;
- the core work behind the Jobs panel: queue order and controls, Pause GPU work, the job-record fields and the stall rule, the estimator, the out-of-memory ladder's plain retry, the ComfyUI launch profile and the model inventory report;
- keep-awake while GPU jobs run, the diagnostics bundle, the new `lil doctor` checks, and Windows Hello for `lil dev approve`.

**Architecture:**
- **Apps:** a C++ `main` (`apps/lil-image/`) boots through Rust (`lil-qt`), which settles single instance and the window mode before Qt starts. QML lives only in `apps/lil-ui-kit/` (module `Lilith.UiKit`) and, for WebEngine, `apps/lil-web/`; the Rust bridge is the QML module `Lilith.Core`; the C++ shim (`apps/lil-qt-shim/`) holds the KDDockWidgets dock factory, the live-preview image provider, window activation and the Qt log bridge. WebEngine lives only in `apps/lil-web/` (module `Lilith.Web`), loaded through a `Loader` (APP-137).
- **`lil-qt`** is a workspace crate whose Qt parts sit behind its `qt` feature (P4a-R1), so the throttle, request table, retry, paging, app-state, shortcut and single-instance logic run under `cargo nextest` with no Qt installed.
- **Core:** the `jobs` module gains queue order, moves, batch cancels, Pause GPU work and the new record fields; `engines` gains the ComfyUI launch profile, the out-of-memory advice, model families and stored environment captures; `telemetry` gains `job_stats`, `telemetry.energy` and GPU users; `interchange` gains the notifier, toasts, `lilref.write` and base64url launch arguments. Two infrastructure parts join Appendix D's list: `desktop` (the fullscreen poll, the power request, the foreground check) and `diagnostics` (the bundle and recent errors).
- **New crate `lil-parse`** (platform-neutral, QA-050): the `lil:` URL grammar, the text normaliser for notifications, and the safetensors and GGUF header readers.
- **`lil-platform`** gains toasts, the AUMID shortcut, per-user handler registration, the fullscreen state, the power request, foreground helpers, Windows Hello and DPAPI, and one shared log setup.

**Tech Stack:**
- Rust 1.99.0 (MSVC), edition 2024, `cargo-nextest` 0.9.146, `cargo-deny`, as in plan 3.
- Qt 6.12.0 (`msvc2022_64`) + Qt WebEngine 6.140.0; cxx-qt 0.10.0 with cxx-qt-cmake 0.10.0 (Corrosion v0.5.2); KDDockWidgets 2.5.0 if Task 0's build check passes, else 2.4.1 (APP-005); CMake 4.4.4 and Ninja 1.13.2; MSVC 14.44 (VS 2022 Build Tools); Qt Quick Test through CTest.
- Windows: classic WinRT toasts through the `windows` crate 0.62 (no Windows App SDK), `KeyCredentialManager` and DPAPI.

**Spec:** `docs/superpowers/specs/2026-10-12-lilith-design.md` (1079 rules today; this plan adds one, ARCH-127, so `EXPECTED_RULE_COUNT` in `crates/lil-cli/src/dev/spec_report.rs` becomes 1080). Terms are those of `docs/GLOSSARY.md`. Task 2 adds the spec's **Appendix G, "App shell and plumbing (plan 4a clarification)"**. The plan follows roadmap row 4a (`docs/superpowers/plans/2026-10-12-roadmap.md`).

**Execution method:** subagent-driven (chosen by the owner), with heavy review on every task: quality over speed, no rush.

**Starts after:** PR #24 (`docs-layout`) is merged to `main`. Every task then branches from `main` (Global Constraints, "Branches and PRs").

**Owner answers amend the plan before Task 3.** The questions at the end (Q1–Q7) are written into the plan with their recommended answers as defaults, each place marked "(owner question Qn; default shown)". A "no" to any of them changes Appendix G, the capability list, the number registry and Task 3's spec tests, so the plan is amended for it before Task 3 starts; Task 2 records the answers in the spec.

**Checkpoint after Task 14:** the Qt-free core (Tasks 0–14) gets a scoped review and an owner check-in before Task 15 starts the Qt work (see "Checkpoint A").

The rules this plan implements, fully or in the part named:
- §1.1: ARCH-005 (the core posts notifications), ARCH-010 (the panels of this plan), ARCH-015 (`lil-mcp` launches apps), ARCH-016 (`lil confirm-window`, P4a-R4), and the new ARCH-127 (keep-awake);
- §1.4: ARCH-065 (the doctor's catalogue check), ARCH-072 (the stall rule and the watchdog's history bound), ARCH-126 (the ComfyUI launch profile);
- §1.7: ARCH-098, ARCH-100 (the app keeps working while the core is away; documents are plan 5), ARCH-103 (the Notification hook);
- §1.8: ARCH-114, ARCH-116; §1.9: ARCH-117–119, ARCH-121, ARCH-122;
- §2.8: CORE-117 (the new fields), CORE-130 (the stored captures and the doctor's comparison), CORE-160 (not the live class or the Generate modes), CORE-161, CORE-163 (step 1 and the advice), CORE-164;
- §3.1: APP-001–006, APP-008, APP-011–016, APP-020–022, APP-146, APP-156, and APP-163 and APP-164 in this plan's copy and titles;
- §3.2: APP-024–037 (APP-035's document holds are plan 5; APP-032's clipboard bytes are plan 4b);
- §3.3: APP-039–041, APP-043 (its session, jobs, confirmations, usage, hardware and diagnostics rows), APP-044–049, APP-051, APP-054, APP-140;
- §3.4: APP-065 (its notification), APP-020's launch arguments (the plan-3 carry N6);
- §3.6: APP-111, APP-114 (the queue, previews, progress, ETA, lane state and queue controls), APP-115–121, APP-122 ("tracked only" until plan 6b), APP-123 (its empty state until plan 7+), APP-124, APP-127 (not shared GPU memory or per-engine load), APP-128, APP-129, APP-131, APP-132 (thermal and throttling), APP-133 (an empty state), APP-134;
- §3.7: APP-135–137, APP-139;
- §4.7 A: AI-116's Notification hook;
- §6: QA-016 (the notification path), QA-026, QA-042, QA-050 (`lil:` URLs, model headers), QA-060 (the first-run handler checks), QA-064 (the Narrator smoke check), QA-077, QA-082–085, QA-088.

Already done, nothing to build: APP-047 (an interactive Claude Code session's `destructive` and `exec` calls), which plan 3 settled with `mcp_confirmation = "core"` (Appendix F); this plan's notification path covers those calls too.

Spike results this plan builds on: `docs/spikes/02-qt-stack.md` §7–8 and `docs/spikes/04-frametimes.md` §8.1 (APP-135, IMG-037).

## Number registry

Allocated here, before Task 1, so nothing is renumbered at integration. "Now" was re-checked on `docs-layout` (4930685) on 2026-10-21; re-check it against `main` when Task 2 starts, and stop if it moved.

| Item | Now | Plan 4a allocates | Task |
|---|---|---|---|
| Suite migrations (`SUITE_MIGRATIONS`, `lil_schema::SUITE_STATE_SCHEMA`) | 8 | 9 `JOBS_V2`; 10 `ENGINE_RECORDS` → schema 10 | 6; 8 |
| Workspace migrations (`WORKSPACE_MIGRATIONS`, `WORKSPACE_STATE_SCHEMA`) | 7 | 8 `JOBS_V2` → schema 8 | 6 |
| `telemetry.db` `USER_VERSION` | 1 | 2 (`job_stats`) | 7 |
| `INDEX_SCHEMA_VERSION`, `RESOURCE_SCHEMA_VERSION`, `.lilref` version | 1, 1, 1 | unchanged | — |
| Protocol | 1.1, 5 capabilities | 1.2; capabilities `queue.v1` (Task 6), `desktop.v1` (Task 9), `context.v1` (Task 11), appended in that order | 6, 9, 11 |
| Error codes (`codes.rs` `ALL`) | 66 | 67: `job.not_queued` (`data_conflict`, `never`) | 6 |
| Rule ids | 1079 rules | **ARCH-127** (keep-awake); count 1080 | 2 |
| Rule ids not used | — | the parked AI-surfaces revision's own new ids (the ARCH id after ARCH-127, and the AI ids above AI-233); this plan allocates none of them. ARCH-127 keeps the parked revision's number, but this plan's text drops its director and timer-wait clauses and its Start-dialog line, so unparking that revision must merge its ARCH-127 into this one | — |
| CORE-146 table rows (no new rule ids) | — | `projects.list`, `workspaces.list`, `desktop.get`, `engines.environment`, `telemetry.energy`, `diagnostics.recent` (read); `lilref.write`, `notify.claude_code` (write) | 2 |
| `app/*` methods | 3 | `app/queue.pause`, `app/queue.resume`, `app/desktop.set`, `app/set_context` | 6, 9, 11 |
| Topics | — | `desktop` (new); `jobs` gains `queue`; `job:<id>` gains `unresponsive`; `diagnostics` gains `error` | 9; 6; 6; 12 |
| Fail points | — | `jobs.after_withdraw` | 6 |
| Plugin version | 1.3.0 | 1.3.1 (the Notification hook) | 11 |
| Spike documents | 01–09 | `docs/spikes/10-app-shell.md`, `docs/spikes/11-windows-integration.md` | 0, 1 |
| Ruling ids | — | P4a-R1 to P4a-R35 (below) | — |
| Noted for plan 6a | — | the `jobs` table's `priority` CHECK admits `interactive`, `user` and `agent` only; plan 6a's `live` class needs a table rebuild in its own migration | — |

## Decisions recorded for this plan

The owner decisions and rulings of plans 2a, 2b and 3 bind this plan. These owner decisions bind it too (2026-10-19 to 2026-10-21):
- **KDDockWidgets 2.5.0** after a half-day build check on Qt 6.12.0 (Task 0), else 2.4.1 (APP-005).
- **A minimal accessibility rule** (APP-156) with a Narrator smoke check in each milestone's manual list.
- **Classic WinRT toasts** with a per-user Start-menu AUMID shortcut and protocol activation into `lil:` URLs; no Windows App SDK (APP-022).
- **Windows Hello:** a TPM KeyCredential-derived key-encryption key if Task 1 proves the signatures deterministic, else Windows Hello gating a DPAPI-protected key; the passphrase file stays the fallback (QA-088).
- **CORE-160's queue controls** (class order, `job.move`, batch cancels, Pause GPU work, fullscreen auto-pause) are in this plan with the Jobs panel; the live class and the Generate modes wait for plan 6a.
- **Keep-awake** is in this plan (ARCH-127, added by Task 2), and so is **Home's "While you were away" card** (APP-111, amended by Task 2).
- **VC++ runtime DLLs ship app-local** (OPS-094).
- **The shell is simply LILITH** (APP-164).
- The plan-3 review findings R1–R4 and the `sed` `w`/`W` form, carried under P3-R34, are not in this plan (P4a-R22).

### Rulings made in this plan

Each stands unless the owner says otherwise; the owner questions at the end name the ones that touch them. Rulings made while the plan runs are added below with their reasons (QA-090).

- **P4a-R1** `lil-qt` is a member of the cargo workspace with its Qt parts behind the `qt` feature (off by default, `cxx-qt` optional), and CMake builds it through `cxx_qt_import_crate(... FEATURES qt LOCKED)` against the workspace `Cargo.lock`. Why: `cargo nextest`, `clippy` and `cargo deny` then cover its logic with no Qt on the runner (QA-068). Fallback if Task 0 finds the feature route broken: `lil-qt` leaves the workspace with its own lock file, and its Qt-free modules move to a `lil-qt-core` crate in the workspace. Cost if wrong: one crate split.
- **P4a-R2** Toast activation uses a scheme per channel: `lil` (stable), `lil-dev` (dev), `lil-test-<id>` (test channels, desktop group only). Any channel's app opens a raw `lil://` ref. Why: Windows keeps one handler per scheme, and a dev core's confirmation ids mean nothing to a stable app. Cost if wrong: a dev build's links say `lil-dev:`.
- **P4a-R3** `lil:open/<target>` carries the target as base64url without padding (RFC 4648 §5); a raw `lil://` ref is percent-decoded once; the handler reads the raw command line and never re-splits it. Why: quotes, spaces and percent-encoding differ by launcher, and a base64url argument has none of them.
- **P4a-R4** `lil --confirm-window` is spelled `lil confirm-window [<id>]`. Why: `crates/lil-cli/src/main.rs` (protected) requires a subcommand, so a bare global flag can't run; Task 2 records the spelling as an ARCH-016 and APP-049 clarification.
- **P4a-R5** Pause GPU work is kept in memory with two reasons, `manual` and `fullscreen`; the lane is paused while either holds, `app/queue.resume` clears only `manual`, and leaving fullscreen clears only `fullscreen`. A core restart ends a pause (its queued jobs become `interrupted` anyway, ARCH-022). Why: an automatic resume must never undo your own pause.
- **P4a-R6** The fullscreen poll runs every 2 s, in one poll for the quiet mode and the auto-pause; leaving fullscreen counts after 2 polls in a row. Why: the call is cheap, a game that starts pauses new work within 2 s, and an alt-tab doesn't flap the queue.
- **P4a-R7** No toast is posted while a connected suite window is the foreground window; the app shows the same news itself (owner question Q3; default shown).
- **P4a-R8** Notification text goes through `lil_parse::shown_text`: C0 and C1 controls removed; the bidirectional controls and zero-width characters Appendix G lists shown as `⟨U+XXXX⟩`, except a U+200D ZERO WIDTH JOINER between two `Extended_Pictographic` code points, which is kept; the length limit counts `char`s over the whole text; then XML-escaped into the toast. Why: toast text comes from tool arguments and engine output (ARCH-060); this is the notification part of the parked AI-surfaces revision's normaliser rule, without its id.
- **P4a-R9** A confirmation toast has one button, "Review", and both it and the body open the confirm window; no toast button answers anything (APP-022, APP-044; owner question Q6; default shown).
- **P4a-R10** The confirm dialog takes an answer only from a click, or Space or Enter on the focused button, at least 500 ms after the dialog became visible and while its window is in front. Initial focus is on Deny, and Approve is never the default button. Why: a key already held down, or a click meant for the window underneath, must not answer.
- **P4a-R11** The confirm-window process has its own single-instance pipe, `\\.\pipe\lilith-app-image-confirm-<userSID>-<channel>`, so several toast clicks reuse one window; while the full app runs, `confirm` URLs go to it first.
- **P4a-R12** In this plan `origin` is derived from the caller: `app:<app>`, `claude_code` (`mcp`), `assistant`, `cli` or `core`. The director plan adds `task:<job_id>`; a per-panel origin waits for a `tools/call` field (a later protocol change).
- **P4a-R13** Every job has a `batch_id`; a call that submits one job makes a batch of one. Why: cancel by batch and the batch toast need no special case.
- **P4a-R14** The estimator keys history by `(kind, estimate_key)`; `comfy.prompt`'s key hashes its graph without seeds and filename prefixes; it needs 3 of the newest 20 succeeded runs and uses nearest-rank p50 and p90. Plan 6a refines the key with model profiles.
- **P4a-R15** The ComfyUI launch profile's `--reserve-vram` defaults to 1.5 GB, the canvas, browser and compositor budget IMG-111 uses. Task 0 confirms the shell's own part (its dedicated VRAM with a WebEngine page shown); plans 5 and 6a re-measure the canvas part.
- **P4a-R16** The app keeps its own state (`last_seen_at` per workspace, the last layout) in `<local dir>\apps\<app>\`, never in the core. Why: it is per app and per machine, and the core owns no app state.
- **P4a-R17** Hardware charts are Qt Quick Shapes (`PathPolyline`), never a `QQuickRhiItem` or Qt Graphs. Why: no renderer to rebuild on float (ARCH-116), and a smaller deploy folder.
- **P4a-R18** Plugin 1.3.1 adds the Notification hook (`lil hook notification`, which calls `notify.claude_code`); `lil doctor` updates 1.3.0 in place.
- **P4a-R19** `diagnostics.bundle` writes one zip in `<local dir>\diagnostics\`; it never holds workspace files.
- **P4a-R20** Every process logs through one `lil_platform::logs::init`: JSON lines, a file a day, 14 kept per process, and a day's file stops at 64 MiB with one `log_capped` line. Why: ARCH-117 asks for a size cap that a daily roll alone doesn't give.
- **P4a-R21** In this plan an open request (`open`, a raw ref, a `.lilref` file, an agent's `ui.show`) shows the target in Home's "Opened" card; plan 4b routes it to the Inspector. A `task` URL opens the Jobs panel on that job until the Assistant panel exists.
- **P4a-R22** The plan-3 hook gaps (the final 3b re-review's R1–R4 and `sed` `w`/`W`) are not in this plan. Why: they live in the PreToolUse classifier, which this plan doesn't change (its hook work is the Notification event only), P3-R34 already treats each as a bug fix, and the last attempt to fix them was stopped by a safety classifier, so the owner dispatches that fix deliberately as its own small PR from `main`.
- **P4a-R23** The Windows Hello key-encryption key is SHA-256 over the UTF-8 bytes of `LILITH reviewer KEK v1` followed by the raw bytes of the credential's signature over the enrolment's challenge: 32 random bytes made once at enrolment and kept as hex in `app-key.hello.toml`. Its lowercase hex is the age passphrase of `app-key.hello.age`. The DPAPI route stores `app-key.dpapi`. Why: no new cryptography dependency, and the existing age decryption stays the one path.
- **P4a-R24** `desktop` and `diagnostics` are infrastructure inside `lil-core`, like `server` and `rpc` (Appendix D's CORE-014 clarification); their tools carry the always-available modules `desktop` and `diagnostics`.
- **P4a-R25** The window title is `ATHANOR — <project>` (APP-164; owner question Q7; default shown); the confirm window's is `LILITH — Confirm`.
- **P4a-R26** The channel's scheme, the `.lilref` association and the AUMID shortcut are written only after your own input: `lil doctor` after you type `y`, or the Settings panel's "Set up notifications" button, which the app also offers on its first start while they are missing (owner question Q5; default shown). The core never writes them.
- **P4a-R27** The core tracks each verified `ui:*` connection's process in telemetry with role `app` while it is connected. Why: Hardware's "who's using the GPU" shows the app, and plan 7a's RAM admission reads its working set (IMG-041).
- **P4a-R28** Model families are read only from `.safetensors`, `.sft` and `.gguf` headers, within fixed limits; pickle formats are never opened.
- **P4a-R29** Desktop-touching tests run only when `CI=true`, `LIL_SANDBOX=1` or `LIL_OWNER_DESKTOP=1` (P4a-R31); anywhere else they panic before touching the desktop (QA-042).
- **P4a-R30** `lil-mcp` may also depend on `lil-platform` and `toml`, widening plan 3's list (`lil-client`, `lil-protocol`, `rmcp`, `tokio`, `serde_json`, `tracing`). Why: starting an app without focus and the shared log setup are Windows calls that only `lil-platform` may make (ARCH-111), and the app registrations are TOML; neither adds policy or state, so ARCH-012 still holds. `crates/lil-mcp/Cargo.toml`'s comment and the dependency-boundary test change with it. Cost if wrong: two crates in `lil-mcp`'s tree.
- **P4a-R31** Until a runner that can post toasts and take the foreground exists, the serialised desktop group runs on your PC as an **owner-present** step you start (`LIL_OWNER_DESKTOP=1`, about 10 minutes, with the PC left alone meanwhile); CI runs only the offscreen and Qt-free tests. Why: hosted Windows Server runners have no reliable toast platform, and no Sandbox runner is built in this plan. This needs QA-042's wording ("never in your desktop session") to allow a run you start yourself; Task 2 adds that clarification only with your yes, recorded with the owner answers, and otherwise these tests stay on the manual list by name.
- **P4a-R32** Workflow files (`.github/workflows/**`) are changed only in commits the owner pushes, because agents' tokens can't change workflows (QA-079). If Task 0's CI measurement puts the `pr-ready` job over 50 minutes with Qt in it, Task 15 splits the Qt build into its own job with a Qt cache, and `dev/merge-guard.toml` names the new required check (protected).
- **P4a-R33** Timing values with no spec source: reconnect backoff from 250 ms, doubling to 5 s; the notification path's reachability cached for 10 s; the unresponsive check every 5 s; the layout saved every 60 s while it changes. Why: each is well under the spec's own bounds (the 30 s stall rule, the 30-minute confirmation lifetime) and cheap. Cost if wrong: a tuning change.
- **P4a-R34** Switching workspace is `Session::set_workspace(id)`, a new `core/hello` with that workspace id; it never changes `suite.toml`'s default. The switcher's "Make this the default" is a separate, explicit action that calls `workspace.set_default`. In M1 the core serves one workspace, so switching to another one answers `workspace.unsupported` naming the served one (ARCH-090). Why: `workspace.set_default` changes every `cli` and `mcp` caller's workspace, which a click in the app must not do as a side effect.
- **P4a-R35** If Task 0's S0.9 finds that Qt Quick, KDDockWidgets or the grabs don't work offscreen on Windows, the GUI tests run in hidden real windows (`--no-activate`, never taking focus) as an owner-present step on your PC, about 15 minutes per GUI task, and CI runs only the Qt-free and build tests.

## Global Constraints

- **Toolchain:** Rust **1.99.0**, `x86_64-pc-windows-msvc`, edition **2024**; Qt **6.12.0** `msvc2022_64` + Qt WebEngine **6.140.0** (asserted as a pair, QA-077); cxx-qt, cxx-qt-lib and cxx-qt-build **=0.10.0**, cxx-qt-cmake tag **0.10.0** (Corrosion v0.5.2); KDDockWidgets **2.5.0** or **2.4.1** as Task 0 records, built with `KDDockWidgets_QT6=ON` and `KDDockWidgets_FRONTENDS=qtquick`; CMake **4.4.4**, Ninja **1.13.2**; MSVC 14.44 with `CMAKE_MSVC_RUNTIME_LIBRARY=MultiThreadedDLL`. The build facts of spike 02 §7–8 hold: `extern crate cxx_qt_lib;` in a bridge that uses no `cxx-qt-lib` type; the shim links `Qt6::GuiPrivate` privately; the exe never links `Qt6::WebEngineQuick`.
- **Owner constraints, binding on every task:**
  - nothing may need paid software for the full features: Qt (LGPLv3), KDDockWidgets (GPL), cxx-qt and every new crate are free, and the toasts need no Windows App SDK;
  - no content filtering and no business gating: no task adds an approval gate, a content check or a licence check;
  - usage, cost and token tracking stays as plan 3 built it; caps stay optional, off by default and set only by you (CORE-008); the hardware telemetry stays (§2.5);
  - an AI agent's delete, install or script run still waits for your yes or no, and this plan makes that answerable with no app open (ARCH-055);
  - nothing personal in the repository (QA-086): write `%USERPROFILE%`, `%APPDATA%`, `%LOCALAPPDATA%` and `<owner>`.
- **Branches and PRs** (owner direction): every task is its own branch `p4a-tNN-<slug>`, cut from `main` only after every task its "Depends on" line names has merged, and its own PR through `lil dev merge`. No task builds on an unmerged task. Tasks marked **Qt-free** need no Qt and may run at the same time in separate worktrees, one agent per tree. Agents push and open PRs; the owner approves and merges (QA-036). The owner is asked before any step marked **owner present**, and before any check estimated over 5 minutes; every such check carries its time estimate. A branch whose PR sits while another merges is rebased on `main`, and its shared files (the registry pins in `crates/lil-core/src/tools/tests.rs`, `crates/lil-schema/src/suite.rs`, `crates/lil-core/tests/hello.rs`, `crates/lil-core/tests/module_deps.rs`, `crates/lil-core/Cargo.toml`) are recomputed, never merged by hand. Tasks 7, 8 and 9 merge in that order.
- **Protected files** (QA-019, QA-020; `dev/protected-paths.txt`). These change, so the owner runs `lil dev approve <PR>` on each PR's final head, and each PR says which ones it changed:
  - `crates/lil-cli/src/dev/spec_report.rs`, Task 2: `EXPECTED_RULE_COUNT` 1079 → 1080 and nothing else;
  - `crates/lil-spec-tests/Cargo.toml`, Task 3: the `lil-parse` dev-dependency;
  - `crates/lil-spec-tests/tests/core_session.rs`, Task 6: `hello_major_mismatch_is_refused`'s `core_protocol` becomes `"1.2"`, nothing else;
  - `crates/lil-spec-tests/tests/plugin_hooks.rs` and `crates/lil-spec-tests/tests/doctor_claude.rs`, Task 11: the hook event set gains `"Notification"`, the expected `version` becomes `"1.3.1"`, and the installed-version fixture follows it;
  - `/Cargo.toml`, Task 12 (`zip`) and Task 15 (`cxx`, `cxx-qt`, `cxx-qt-lib`, `cxx-qt-build`);
  - `.github/workflows/ci-pr-ready.yml`, Task 15 (Qt, the CMake build, CTest `qml` and `deploy`, and the Qt job split if P4a-R32 applies) and Task 29 (the budgets' environment); **the owner pushes these commits** (P4a-R32, QA-079), and `dev/merge-guard.toml` changes only with a job split;
  - `crates/lil-cli/src/dev/reviewer.rs` and `crates/lil-cli/src/dev/mod.rs`, Task 28 (Windows Hello);
  - `.config/nextest.toml` and `test-params.toml`, Task 29 (the `desktop` test group and the shell budgets);
  - `crates/lil-testkit/tests/dependency_boundaries.rs` is not protected; Task 11 changes it for P4a-R30.

  New files under `crates/lil-spec-tests/**` are free additions (`add-ok`). No other protected file changes: not `crates/lil-cli/src/main.rs` (P4a-R4), not `crates/lil-cli/Cargo.toml`.
- **Crates and folders** (ARCH-110):
  - new crates `crates/lil-parse` (Qt-free, platform-neutral) and `crates/lil-qt` (Qt only under `qt`, P4a-R1);
  - new CMake tree: `/CMakeLists.txt`, `/CMakePresets.json`, `apps/lil-qt-shim/` (C++), `apps/lil-ui-kit/` (QML only, module `Lilith.UiKit`), `apps/lil-web/` (QML module `Lilith.Web`, the only importer of `QtWebEngine`), `apps/lil-image/` (`main.cpp`, `image.toml`);
  - `Cargo.toml`'s `exclude` stays `["spikes"]`; spike code lives in `spikes/app-shell/` and `spikes/winshell/`, outside the workspace.
- **Dependency boundaries** (ARCH-111, QA-027): `lil-schema` and `lil-protocol` add no dependency. `lil-parse` depends only on `lil-schema`, `base64`, `serde_json` and `sha2` (and `proptest` for tests), and joins `crates/lil-testkit/tests/dependency_boundaries.rs` as a crate with no Qt or Windows dependency. Only `lil-platform` calls Windows APIs (`lil-qt`'s Windows needs go through it). Only `apps/lil-web/` links or imports WebEngine, checked by a dependency walk (Task 15).
- **New dependencies** (OPS-108; `cargo deny check` must pass). Each version is the one on crates.io on the task's day, recorded in the commit message:

  | Crate | Version and features | Licence | Where | Reason |
  |---|---|---|---|---|
  | `cxx` | `1.0.205` | MIT OR Apache-2.0 | `lil-qt`, optional | the bridge (spike 02's pin) |
  | `cxx-qt`, `cxx-qt-lib` | `=0.10.0` | MIT OR Apache-2.0 | `lil-qt`, optional | QObjects and list models (APP-025) |
  | `cxx-qt-build` | `=0.10.0` | MIT OR Apache-2.0 | `lil-qt` build-dependency, optional | code generation |
  | `zip` | the current release, `default-features = false, features = ["deflate"]` | MIT | `lil-core` | the diagnostics bundle (ARCH-121) |

  New `windows` features go in `crates/lil-platform/Cargo.toml` only; Task 1 lists the exact set it used and Task 5 adds that set. KDDockWidgets comes through CMake `FetchContent` at its release tag, never through cargo.
- **File writes in `lil-core`** (ARCH-002, ARCH-112, QA-024): every file-mutating call stays in `crates/lil-core/src/store/fsops.rs`. New homes there: `write_suite_cache_file` (the `.lilref` cache and the diagnostics zip, refusing any path outside `<local dir>\cache\lilref\` and `<local dir>\diagnostics\`), and the `suite.toml` edits of `app/desktop.set` through the existing targeted-edit path. The app's own files (P4a-R16) are written by `lil-qt` through `lil_platform::fs::replace_file`; the app never writes a workspace file.
- **Tests** (QA-031, QA-068):
  - Qt-free crates: `cargo nextest run -p <crate> --retries 0 -E '<filter>'`, with `FakeClock`, `SeqIds`, the fake hardware, `FakeComfy` and the new `FakeDesktop` ("Test fakes" below); never real time, a real ComfyUI, real toasts or the real registry;
  - QML: Qt Quick Test files `apps/lil-ui-kit/tests/tst_<name>.qml`, run by CTest with `QT_QPA_PLATFORM=offscreen` and the backend environment Task 0's S0.9 records (`ctest --preset qml -R tst_<name>`; if S0.9 fails, P4a-R35); every panel takes its core as `required property var core`, so tests pass `FakeCore.qml` (Task 16);
  - offscreen grabs: `ctest --preset grabs -R grab_<panel>` saves `build/dev/grabs/<panel>-<state>.png` and asserts the image has the window's size and isn't one flat colour; the reviewer looks at every grab;
  - the flake guard: every new test runs 10 times with no retry before its task's commit (Execution notes);
  - nothing opens a window that takes focus on your desktop during an agent's run: app starts in tests use `--no-activate`, and anything else is marked owner present;
  - E2E tests over the pipe use `lil-testkit` (`TestEnv`, `TestApp`, `CoreFixture`) on `test-<id>` channels.
- **Isolation and desktop tests** (QA-038, QA-040–042; P4a-R29): no automated test posts a real toast, writes the real `HKCU\Software\Classes`, creates a real Start-menu shortcut, takes the real foreground, holds a real power request or prompts Windows Hello, except the **desktop group**: Rust tests named `desktop_*` and marked `#[ignore = "desktop"]`, and CTest tests labelled `desktop`, run serialised only where QA-042 allows: in this plan that is the owner-present local run of P4a-R31 (about 10 minutes, Task 29), since CI runs only the offscreen and Qt-free tests until a desktop-capable runner exists. Test channels use `FakeDesktop` and the classes root `HKCU\Software\LILITH-test-<id>\Classes`, which the shell never reads.
- **Privacy** (QA-086): no user-profile paths, names or e-mail addresses in code, fixtures, test names, grabs or spike documents. Grabs use fixture workspaces with synthetic names. Spike documents name hardware only as far as a measurement needs, as spikes 02 and 04 do.
- **Copy and terms** (QA-089, APP-163, APP-164): user-facing copy calls a candidate a **take** and uses "version" only for a sealed result; the window title leads with ATHANOR (P4a-R25, owner question Q7; default shown); the shell is LILITH. Text uses the glossary's terms: "ComfyUI launch profile", "Claude Code profile" and "model profile", never "profile" alone; "GPU lease", never "lease" alone; "confirmation", never "approval"; "layout preset", never "preset" alone. Task 2 adds this plan's new terms.
- **Accessibility** (APP-156): every `Lilith.UiKit` control sets `Accessible.role` and a non-empty `Accessible.name`, takes keyboard focus in tab order and works from the keyboard; custom delegates (job rows, gauges, chart tiles, the app's own notification cards) set their role and name themselves. Each panel task runs `A11yWalk` (Task 18) over its panel.
- **Spec values used verbatim:**
  - throttle: leading-edge, then at most **10 Hz** for job progress and **1 Hz** per telemetry series; confirmations never throttled (APP-027);
  - GUI-thread stalls p99 **< 16 ms** under ComfyUI load (APP-135); cold start **< 2 s** to an interactive window, after a reboot and after a Defender first scan (APP-136); event-to-UI **< 100 ms** for job progress, immediate for confirmations (APP-139);
  - stall rule: no progress for **30 s** → "unresponsive", display only; the watchdog at **max(3 × p50, 2 × p90)** of history, else **3 ×** expected (ARCH-072);
  - progress toasts for jobs expected to run over about **30 s**, updated at most **once a second**, replaced at the end (APP-146);
  - notification defaults: progress **off**, every other category **on**; quiet while fullscreen **on** (APP-146); fullscreen auto-pause **off** (CORE-160); keep-awake for GPU jobs **on** (ARCH-127);
  - cap gauges and alerts at **50 / 80 / 100 %** (APP-121, CORE-089);
  - Hardware opens on a **15-minute** live window and zooms out to **24 h** (APP-129); no CPU temperature (APP-128);
  - layout restore order: create the windows → register the dock factory → restore → clamp every window to its screen's available area (APP-003); M1 keeps "Default" plus the last state (APP-004);
  - single instance: `\\.\pipe\lilith-app-<app>-<userSID>-<channel>`, `first_pipe_instance` and the core pipe's user-only descriptor (APP-013);
  - the out-of-memory ladder retries at most **twice** (CORE-163); a pending confirmation expires after **30 minutes** (CORE-115);
  - the AUMID is `LILITH.Suite.<Channel>` (QA-038).
- **Names that are contracts.** Task 2 copies the sub-bullets of this bullet into spec Appendix G, in this order. The protocol bullet goes into Appendix D instead. The two bullets after this one (test-channel tools, test fakes) are not copied.
  - **Protocol 1.2** (Appendix D): `PROTOCOL = 1.2`; `CAPABILITIES` = `events.v1`, `tools.v1`, `jobs.v1`, `confirmations.v1`, `caps.v1`, `queue.v1`, `desktop.v1`, `context.v1`, in that order. A 1.1 client keeps working. `app/queue.pause` and `app/queue.resume` need `queue.v1`, `app/desktop.set` needs `desktop.v1` and `app/set_context` needs `context.v1` in the shared capabilities; without it the call gets `caller.not_allowed`. New error code `job.not_queued` (`data_conflict`, `never`), message "Only a job that is waiting its turn can be moved; this one is with the engine, running or finished."
  - **Jobs** (CORE-117, CORE-160, ARCH-072):
    - `JobSummary` adds, from plan 4a: `batch_id` (`b_<16 hex>`; every job has one, P4a-R13), `index` (its 0-based place in the batch), `status_seq: [{status, substatus, at}]` (every change in order; at most 64 entries, the first and the newest 63), `origin` (`app:<app>`, `claude_code`, `assistant`, `cli` or `core`, P4a-R12), `engine_device?` (the GPU adapter's name, for a job that held the GPU lease), `phase?`, `subtasks?: [{name, value?, max?}]`, `indeterminate?: true`, `last_progress_at?`, `cache_hit_nodes?: [node_id]`, `all_cached?: true`, `unresponsive?: true` and `estimate?: Estimate` (taken when the job was submitted); times are RFC 3339 UTC.
    - **Phase:** from the class type of the node ComfyUI reports `executing` (the core reads it from the job's graph), the first match winning: contains `Loader` → `loading_models`; contains `TextEncode` → `encoding`; contains `Sampler` → `sampling`; contains `Decode` → `decoding`; starts with `Save` or `Preview` → `saving`; anything else → `other`. From `execution_success` until the outputs are staged the phase is `ingesting`. Other job kinds report the phase their handler sets. `progress.node` stays the raw node id.
    - `subtasks` lists each phase the job reached, in order, as `{name}`; the current one also carries `value` and `max` from the newest `progress` event while that phase lasts. `indeterminate: true` while the executing node has sent no `progress` event.
    - `last_progress_at` is the time of the newest `executing` or `progress` event. `unresponsive: true` while the job is `running` with substatus `executing` and `last_progress_at` (or `started`, before any event) is **30 s** or more ago; the core checks every 5 s and publishes `{kind: "unresponsive", on}` on `job:<id>` when it flips; nothing is cancelled.
    - `cache_hit_nodes` are the node ids of ComfyUI's `execution_cached` event; when every node of the prompt was cached, the job succeeds with `all_cached: true` and the summary text "Nothing changed: ComfyUI served every node from its cache."
    - The watchdog sets `watchdog: "slow"` when the time since `execution_start` passes `max(3 × wall_s_p50, 2 × wall_s_p90)` of the job's `estimate` with `source: "measured_history"`, else 3 × `expected_secs`.
    - **Queue order:** class rank `interactive`, `user`, `live`, `agent` (`live` is plan 6a's; no plan-4a job has it); within a class, the lower `order_key` first, where a new job's key is its `seq`. Plan 3's limit of 8 overtakes counts only group moves.
    - `job.move {job_id, to: "next" | "last"}` (`write`) sets the job's `order_key` to one below the lowest, or one above the highest, key among the queued jobs of its lane and class, and returns `{job}`. Only a job in substatus `waiting`, `waiting_for_vram` or `queue_paused` moves; any other gets `job.not_queued`. A move never changes the class and never touches a prompt in the engine.
    - `job.cancel` takes exactly one of `{job_id}` (result `{job}`, as in plan 3), `{ids}` (1–500 job ids), `{batch_id}` or `{all_mine: true}` (results `{jobs: [JobSummary]}`); `all_mine` covers every unfinished job whose idempotency scope names the caller's kind and, for a `ui:*` caller, the same app. None or more than one selector is `params.invalid`.
    - **Pause GPU work:** `app/queue.pause {lane?}` and `app/queue.resume {lane?}` (`lane`: `gpu-heavy`, the default, or `cpu`) return `QueueState {lane, paused, reasons: ["manual" | "fullscreen"], held, withdrawn}`. A paused lane dispatches nothing; each of its queued jobs, whatever its class or caller, shows substatus `queue_paused`; the prompt already executing is never interrupted. A lane is paused while it has any reason; `app/queue.resume` removes only `manual` (P4a-R5). `jobs.list`'s result gains `lanes: [QueueState]`, and the `jobs` topic gains `{kind: "queue", state: QueueState}`.
    - **Withdrawal:** when a lane pauses, each suite job of it in `queued_in_engine` whose prompt `/queue` still lists as pending is cancelled through `POST /api/jobs/{id}/cancel`; its `external_id` is cleared and it returns to `queue_paused`. A prompt that started meanwhile runs on. A job with `external_side_effects` is never withdrawn. On resume, a withdrawn job is posted with a new prompt id only after `/api/jobs/{old}` and `/history/{old}` both show the old one gone (CORE-120). Fail point `jobs.after_withdraw` fires after the cancel's answer and before the row changes.
    - `jobs.list` also takes `since?` (RFC 3339: the jobs whose `updated` is later) and `batch_id?`.
    - **Fullscreen** (P4a-R6): the core polls `SHQueryUserNotificationState` every **2 s**; `QUNS_RUNNING_D3D_FULL_SCREEN`, `QUNS_BUSY` and `QUNS_PRESENTATION_MODE` are fullscreen, and leaving counts after 2 polls in a row that aren't. With `[jobs] fullscreen_autopause = true` (default `false`) fullscreen adds the `fullscreen` reason to `gpu-heavy` and leaving removes it. The `desktop` topic gets `{kind: "fullscreen", on}`.
    - **Keep-awake** (ARCH-127): while any `gpu-heavy` job is `running` and `[power] keep_awake_gpu` is on (default `true`), the core holds one system-required power request through the API Task 1 records, and releases it as soon as none runs. The core logs each sleep it sees (`slept`, with `from` and `to`) and publishes `{kind: "resumed", slept_from, at}` on `desktop`.
  - **Estimates** (CORE-161, APP-140):
    - `job.estimate {tool, args}` (`read`, module `jobs`) validates `args` as a call of `tool` would (`tool.unknown`, `params.invalid`) and returns `Estimate {source: "measured_history" | "price_table" | "prices_yaml" | "none", samples, gpu_s_p50, gpu_s_p90, wall_s_p50, wall_s_p90, wh_p50, wh_p90, cost, currency, energy_cost?}`; with `source: "none"` every number and `cost` are `null`. Plan 4a produces only `measured_history` and `none`; prices arrive with plan 6b.
    - History (P4a-R14): the newest 20 succeeded jobs with the same `kind` and `estimate_key` in `telemetry.db` `job_stats`; fewer than 3 is `none`; p50 and p90 are nearest-rank; seconds are decimal strings with one decimal, Wh with three. A local job's `cost` is `"0"` in the display currency, and `energy_cost` is `wh_p50` at `[power] electricity` when that is set.
    - `estimate_key`: for `comfy.prompt`, the first 16 hex of SHA-256 over the canonical JSON (keys sorted, no whitespace) of its graph with every input named `seed`, `noise_seed` or `filename_prefix` removed; for `test.job`, its `estimate_key` argument (default `"test"`); other tools have none and estimate `none`.
    - `telemetry.db` `user_version = 2` adds `job_stats(job_id TEXT PRIMARY KEY, kind TEXT NOT NULL, estimate_key TEXT NOT NULL, finished_ms INTEGER NOT NULL, wall_ms INTEGER NOT NULL, gpu_seconds_milli INTEGER NOT NULL, system_mj INTEGER NOT NULL)` with an index on `(kind, estimate_key, finished_ms)`; `meter` writes a row when a job with an `estimate_key` succeeds; `wall_ms` is the sum of its execution windows; a version-1 file is migrated forward, keeping its rows.
    - `telemetry.energy {from, to}` (`read`) → `{from, to, gpu_mj, cpu_mj, system_mj, measured: {gpu, cpu}, energy_cost?}` over the samples in the range, both bounds included, from the finest table that still holds the range (`s1`, then `m1`, then `h1`), with `JobEnergy`'s whole-system formula.
    - `Snapshot` adds `gpu_users: [{pid, name, vram_local_mib, sm_permille?, role?}]`: the 8 processes with the most dedicated GPU memory, tracked or not; `name` is untrusted. A verified `ui:*` connection's process is tracked with role `app` while it is connected (P4a-R27).
  - **Out of memory** (CORE-163): the retries are Appendix F's; the final failure is `comfyui.out_of_memory` with the message "ComfyUI ran out of GPU memory, also on 2 retries.", `fix.user_hint` "Close other programs that use the GPU, or choose the model profile's lighter variant, then retry." and `retry: after_resolve`.
  - **ComfyUI launch profile** (ARCH-126; P4a-R15): `[engines.comfyui]` gains `reserve_vram_gb` (a decimal string, default `"1.5"`), `preview_method` (default `"taesd"`), `manager` (default `false`) and `dynamic_vram` (default `true`). An owned start runs `<python> main.py --listen 127.0.0.1 --port <port> --reserve-vram <reserve_vram_gb> --preview-method <preview_method>`, then the Manager flag Task 2 records only when `manager` is true, then the dynamic-VRAM opt-out Task 2 records only when `dynamic_vram` is false, then `<extra_args>`. An `extra_args` entry that is `--enable-assets` or any prefix of it from `--enable-as` on, with or without `=<value>`, stops the start with `engine.not_configured` naming it. `engines.status` entries gain `launch_profile: {reserve_vram_gb, preview_method, manager, dynamic_vram, flags: [string]} | null` (`null` for an attached engine) and `asset_catalogue: "on" | "off" | "unknown"`, probed as Task 2 records.
  - **Model inventory report** (CORE-164; P4a-R28): `models.report {}` (`read`, module `engines`) → `{root, refreshed, models: [{path, size, sha256, family, states, model_id?, model_name?, untrusted?}], missing: [{model_id, model_name?, sha256}]}`. `path` is relative to `<root>\models`, with `\`. `states` is a subset of `["orphan", "duplicate", "unknown_family"]`: `orphan` when no `registry/models.yaml` entry has its SHA-256, `duplicate` when another file has the same SHA-256, `unknown_family` when `family` is `null`. `missing` lists the `registry/models.yaml` entries whose SHA-256 no file has. `family` is the first of the safetensors `__metadata__` keys `modelspec.architecture` and `ss_base_model_version`, and GGUF's `general.architecture`; it is read only from `.safetensors`, `.sft` and `.gguf` files, and a safetensors header over 100 MiB, a GGUF with more than 1,024 keys before `general.architecture`, or a string over 64 KiB gives `null`. Family values and file names are untrusted (ARCH-060). Families are cached in `model_hashes.family`. With no `root`, the result is `engine.not_configured`. Nothing is moved or deleted because of a state.
  - **Engine environments** (CORE-130): each capture is kept in the suite `state.db` table `engine_captures (engine, captured_utc, capture)`, the newest 10 per engine. `engines.environment {}` (`read`, module `engines`) → `{captures: [EnvCapture] (newest first, at most 2), changes: [{what: "comfyui" | "node" | "driver", name?, from?, to?, change: "changed" | "added" | "removed"}]}` between those two.
  - **Notifications** (ARCH-005, APP-022, APP-065, APP-146):
    - categories `job_finished`, `job_failed`, `confirmation_waiting`, `cap_alert`, `director_decision`, `job_progress` and `agent_activity` (owner question Q2; default shown); `job_progress` defaults to off, every other one to on. `suite.toml` `[notifications]` holds each by name, plus `quiet_while_fullscreen = true`; `[jobs] fullscreen_autopause = false`; `[power] keep_awake_gpu = true`.
    - `desktop.get {}` (`read`, module `desktop`) → `DesktopSettings {notifications: {<category>: bool}, quiet_while_fullscreen, fullscreen_autopause, keep_awake_gpu, keep_awake_available, notification_path: {toasts: "ready" | "unavailable", shortcut: "ok" | "missing" | "wrong", handlers: "ok" | "missing" | "other_channel"}}`. `app/desktop.set {notifications?, quiet_while_fullscreen?, fullscreen_autopause?, keep_awake_gpu?}` (verified `ui:*` only; owner question Q1; default shown) edits `suite.toml` with targeted edits and returns `DesktopSettings`; the `desktop` topic gets `{kind: "settings", settings}`. A hand edit of `suite.toml` is read at the next core start, as for caps.
    - **Ready** (the reachability fix): `toasts` is `ready` only when the toast notifier for the channel's AUMID reports `Setting() == Enabled` and the core isn't running elevated; otherwise `unavailable`, with `toasts_reason: "disabled_for_app" | "disabled_for_user" | "disabled_by_policy" | "elevated" | "no_platform"` beside it in `notification_path`.
    - A toast is posted only when its category is on, the channel's toasts are `ready`, and no connected `ui:*` app owns the foreground window (P4a-R7); otherwise nothing is posted or queued. `job_finished` and `job_failed` post once per batch, when its last job ends (`job_failed` when any failed; never for `cancelled`), for jobs of priority `user` or `agent` whose `origin` isn't `core`; `confirmation_waiting` posts for each pending confirmation and is removed when it is answered, expires or is cancelled; `cap_alert` posts for each `caps` alert; `agent_activity` posts for `notify.claude_code` and for an agent caller's `open_in` or `ui.show`; `director_decision` comes with the director plan.
    - **Toast XML** (P4a-R8, P4a-R9): `<toast launch="URL" activationType="protocol"><visual><binding template="ToastGeneric"><text>TITLE</text><text>BODY</text></binding></visual>ACTIONS</toast>`, where ACTIONS is `<actions><action content="Review" arguments="URL" activationType="protocol"/></actions>` for `confirmation_waiting` and empty otherwise; TITLE and BODY are `lil_parse::shown_text` output (C0 and C1 controls removed; U+200E, U+200F, U+202A–U+202E, U+2066–U+2069, U+200B–U+200D, U+2060 and U+FEFF shown as `⟨U+XXXX⟩`, except a U+200D between two `Extended_Pictographic` code points, which is kept; at most 200 `char`s over the whole text, ending `…` when cut), then XML-escaped. A progress toast's binding also holds `<progress value="{progressValue}" valueStringOverride="{progressValueString}" status="{progressStatus}"/>`, and its first post carries `NotificationData` with those three values and `SequenceNumber` 1; each update raises the sequence number by 1. `Tag` is the job, batch or confirmation id; `Group` is the category. A quiet toast has `SuppressPopup = true`. No toast has a "Deny" or "Approve" button (owner question Q6; default shown).
    - **Copy:** `job_finished`: "Job finished" (or "Batch finished (<n> jobs)") / "<kind> · <project, or no project> · <elapsed>", launching `<scheme>:task/<job_id>`; `job_failed`: "Job failed" / "<kind>: <error message>", the same launch; `confirmation_waiting`: "Confirmation waiting" / "<who> wants to run <tool>" (who: "Claude Code", "The assistant" or "A script (lil)"), or for a cap alone "A call would go over your <cap> cap", launching `<scheme>:confirm/<id>`; `cap_alert`: "Spending at <pct> %" / "<cap> cap, <period>"; `agent_activity`: "Claude Code" / the message, or "Claude Code opened <target title>", launching `<scheme>:open/<base64url>`.
    - **Progress toasts** (`job_progress`): for a `user` or `agent` job whose `estimate.wall_s_p50`, else `expected_secs`, is over 30 s; posted at `execution_start` when no suite window is in front; updated at most once a second by Tag and Group (`progressValue` 0–1, `progressValueString` "<pct> %", `progressStatus` the phase); removed when the job ends, after which `job_finished` or `job_failed` follow their own toggles.
    - **Quiet while fullscreen:** while the newest poll says fullscreen and `quiet_while_fullscreen` is on, every toast has `SuppressPopup = true`; none is dropped, and confirmations are made quiet like the rest.
    - **Reachable** (ARCH-055, CORE-113, APP-051, QA-016): the notification path can reach you when `notification_path` is `ready`, `ok` and `ok` and `confirmation_waiting` is on, re-read at most every 10 s (P4a-R33). A call from an agent caller (`assistant`, `mcp` or `cli`) that needs a confirmation then gets `pending_confirmation` with no app connected; a non-interactive caller still gets `confirm.unavailable` or `cap.exceeded`. When the path isn't reachable and no app is connected, the call gets `confirm.unavailable` or `cap.exceeded` at once, so a confirmation never waits where you can't see it; with an app connected, it waits there with the top bar's confirmation badge, whatever the toasts' state.
    - `notify.claude_code {message, title?}` (`write`, module `interchange`): `message` 1–2,000 characters, `title` at most 100; → `{posted, reason?: "category_off" | "foreground" | "duplicate" | "unavailable"}`; the same message within 10 s is a `duplicate`.
  - **Activation and registration** (APP-020–022; P4a-R2, P4a-R26):
    - schemes: `lil` (stable), `lil-dev` (dev), `lil-test-<id>` (test channels, desktop group only). URLs: `<scheme>:confirm/<confirmation id>`, `<scheme>:task/<job id>`, `<scheme>:open/<base64url of the UTF-8 target, without padding>`, and a raw `lil://` ref.
    - per-user registration under the classes root: `<root>\<scheme>` with `(Default) = "URL:LILITH (<Channel>)"`, an empty `URL Protocol`, `DefaultIcon = "<exe>",0` and `shell\open\command = "<exe>" "%1"`; `<root>\.lilref` with `(Default) = "LILITH.lilref.<channel>"`, and `<root>\LILITH.lilref.<channel>\shell\open\command = "<exe>" "%1"`. The classes root is `HKCU\Software\Classes` for stable and dev and `HKCU\Software\LILITH-test-<id>\Classes` for test channels outside the desktop group; `<exe>` is the channel's `lil-image.exe`.
    - writing them: `lil doctor` after `y`, or the app's "Set up notifications" button (owner question Q5; default shown), through `lil_platform::{write_shortcut, register_handlers}`;
    - the AUMID shortcut: `<Programs>\LILITH.lnk` for stable, `<Programs>\LILITH (<Channel>).lnk` otherwise, targeting the channel's `lil-image.exe`, with `System.AppUserModel.ID` = `LILITH.Suite.<Channel>`; `<Programs>` is the per-user Start-menu Programs folder (`FOLDERID_Programs`), or `<test root>\profile\Programs\` on a test channel.
  - **`lil:` URLs** (APP-020, QA-050; P4a-R3), parsed by `lil_parse::parse_lil_url`:
    - the input is one command-line argument from the raw command line (`GetCommandLineW`), with surrounding spaces and one pair of surrounding `"` removed; at most 8,192 characters;
    - the scheme (up to the first `:`) is compared ASCII case-insensitively; `lil://` and a ref is a ref under every channel; `confirm/`, `task/` and `open/` are accepted only under the channel's own scheme;
    - `confirm/<id>` needs `^cf_[0-9a-f]{16}$`, `task/<id>` needs `^j_[0-9a-f]{16}$`, and `open/<b64>` needs base64url without `=`, decoding to 1–4,096 bytes of UTF-8 with no control character, which must parse as a ref when it starts with `lil://`; nothing may follow except one trailing `/`, which is dropped;
    - a raw ref has its `%HH` escapes decoded once as UTF-8 (refusing `%00`–`%1F`, `%7F`, invalid UTF-8 and a decoded `%`), then must parse as a ref;
    - anything else is refused with "This link isn't one LILITH opens: <shown_text of the argument, at most 80 characters>", and nothing happens;
    - in plan 4a (P4a-R21) `confirm` opens the confirm window, `task` opens the Jobs panel on that job, and `open` and a raw ref show the target in Home's "Opened" card;
    - `open_in` and `ui.show` (the plan-3 carry N6): `launch_command.args` is the registered `args`, then `--intent <intent>`, then `<scheme>:open/<base64url(target)>`.
  - **`.lilref` files** (APP-021): an argument whose path ends `.lilref` (any case) is read, at most 4 KiB, with `lil_schema::parse_lilref`, and opened as its ref. `lilref.write {ref, title?}` (`write`, module `interchange`) writes `<local dir>\cache\lilref\<first 16 hex of SHA-256 of the ref>.lilref` and returns `{path}`; the ref must be a sealed ref carrying `#sha256=`, else `ref.invalid`.
  - **Apps** (APP-002, APP-013, APP-014, APP-111):
    - single instance: the running instance writes `{"app": "<app>", "pid": <pid>}` when a client connects; the second launch checks that the server's image path is its own executable, calls `AllowSetForegroundWindow(pid)`, writes `{"args": [<argument>, …]}`, reads `{"ok": true}` and exits 0. Each message is one JSON line of at most 64 KiB, within 2 s. The confirm window's pipe is `\\.\pipe\lilith-app-<app>-confirm-<userSID>-<channel>` (P4a-R11).
    - command line: `lil-image [--intent open | edit | use_as_input] [--no-activate] [--confirm [<id>]] [<URL> | <path>]`; `--confirm` with no id lists every pending confirmation; `--no-activate` shows the window without activating it and flashes its taskbar button.
    - app state (P4a-R16): `<local dir>\apps\<app>\state.json` = `{"state": 1, "workspaces": {"<workspace id>": {"last_seen_at": "<RFC 3339>"}}}`; `<local dir>\apps\<app>\layout.json` = `{"layout": 1, "windows": [{"name", "geometry": {"x", "y", "w", "h"}, "maximized", "screen": {"name", "scale"}}], "kddw": <KDDockWidgets' saved layout as JSON>}` (APP-002). A file that doesn't parse or doesn't restore is renamed to `<name>.bad-<YYYYMMDDTHHMMSSZ>`, and the app uses the Default layout.
    - "While you were away" (APP-111): the jobs of `jobs.list {since: last_seen_at}` that finished or failed, and those in `interrupted_unknown`, plus the pending confirmations; `last_seen_at` becomes now once Home has been visible for 3 s.
  - **Session and listing tools** (ARCH-030, ARCH-088, APP-006, APP-043): `app/set_context {project: <slug> | null}` → `{workspace: {id, path} | null, project}` (`project.unknown` for an unknown slug); the workspace is chosen by the session's `core/hello`, and the app switches it with a new hello (`Session::set_workspace`, P4a-R34), never by changing the default; `projects.list {}` (`read`, module `projects`) → `{projects: [{project, name, status, assets}]}`; `workspaces.list {}` (`read`, module `workspace`) → `{workspaces: [{id, path, name?, default, served, missing}]}`.
  - **`lil-mcp` launches** (ARCH-015, APP-065): when an agent's `open_in` or `ui.show` result has `running: false` and a `launch_command` whose `exe` is a suite app registered beside the core (`apps\<app>.toml`), `lil-mcp` starts it detached, with `--no-activate` before the URL and without focus (`SW_SHOWNOACTIVATE`), and adds the line "Started <app> in the background." to the result text.
  - **Plugin 1.3.1** (ARCH-103, AI-116; P4a-R18): `hooks/hooks.json` gains `Notification` with the exec-form hook `{"type": "command", "command": "${user_config.lil_path}", "args": ["hook", "notification"], "onFailure": "block"}`, or the failure setting Task 2 records for this event; `plugin.json` has `version: "1.3.1"`. `lil hook notification` reads the hook JSON (`message`, `title?`) and calls `notify.claude_code` without starting a core; it prints nothing and exits 0.
  - **Diagnostics** (ARCH-117–122, QA-082–085; P4a-R19, P4a-R20):
    - `diagnostics.bundle {job_id?}` (`write`, module `diagnostics`) writes `<local dir>\diagnostics\lilith-diagnostics-<YYYYMMDDTHHMMSSZ>.zip` → `{path, bytes, files: [string], redactions}`. Entries: `manifest.json` (`{bundle: 1, created, build_id, channel, protocol}`), `versions.json` (the core, each app that connected, ComfyUI, the Qt pair, KDDockWidgets, the driver), `suite.toml` (redacted), `errors.json` (`diagnostics.recent` with limit 200), `hardware.json` (`Snapshot`), `engines.json` (`engines.status` and `engines.environment`), `logs\<file>` (the newest 5 MiB of each process's newest log file) and, with `job_id`, `job.json` (its `JobSummary` and tool arguments, untrusted). Never a workspace file.
    - Redaction: every value under a key holding one of Appendix F's scrub words (`KEY`, `TOKEN`, `SECRET`, `PASSWORD`, `PASSWD`, `CREDENTIAL`, `AUTH`), every Claude Code profile label in `suite.toml`, every e-mail address (`[A-Za-z0-9._%+-]+@[A-Za-z0-9.-]+\.[A-Za-z]{2,}`) and every Credential Manager target becomes `<redacted>`; and (owner question Q4; default shown) the user-profile folder becomes `%USERPROFILE%` and the computer name `<machine>`. `redactions` counts the replacements.
    - `diagnostics.recent {limit?}` (`read`, module `diagnostics`; 1–200, default 50) → `{errors: [{code, count, first_at, last_at, last_message, last_tool?}]}`, one row per code, the newest last error first, from the core's last 1,000 error outcomes in memory; the `diagnostics` topic gets `{kind: "error", code, tool?, at}` for each.
    - Logs: every process (`lil-core`, `lil`, `lil-mcp`, each app) writes `<local dir>\logs\<process>.<YYYY-MM-DD>.log` as JSON lines, a new file each day, 14 kept per process; a day's file stops at 64 MiB with one `log_capped` line; every process installs a panic hook that logs `panic` with its location (ARCH-119); an app's Qt messages are logged with their `qt_category`. Each app call carries a `trace_id` (`tr_<16 hex>`, ARCH-118).
  - **The `lil` commands this plan adds.** Exit codes are as in Appendix D.

    | Command | Output |
    |---|---|
    | `lil confirm-window [<id>]` | starts the channel's `lil-image.exe` from `lil.exe`'s folder with `--confirm [<id>]`, prints `confirm window started (pid <pid>)` and exits 0; with no `lil-image.exe` there, exit 2 and "lil-image.exe isn't beside lil.exe"; with `--non-interactive`, exit 2 and "the confirm window is for interactive use" (P4a-R4) |
    | `lil hook notification` | as in "Plugin 1.3.1" above |
    | `lil doctor` | adds: the AUMID shortcut (`ok:`, or `problem:`, created or repaired only after you type `y`); the channel's scheme and `.lilref` handlers (`ok:`, `problem:` when missing, `warning:` when another channel holds them, written only after `y`); a `warning:` for each `engines.environment` change, ending "Roll back with ComfyUI-Manager's snapshot restore or comfy-cli's version switch; the suite doesn't restore engines (CORE-166)."; the asset catalogue (`ok: ComfyUI's asset catalogue is off`, `warning: ComfyUI's asset catalogue is on`, `info: ComfyUI's asset catalogue is unknown`); and `info:` counts from `models.report` |

  - **Fail points added:** `jobs.after_withdraw`.
  - **Clarifications recorded in Appendix G** (no new rule IDs): ARCH-016 and APP-049, `lil confirm-window`; ARCH-055, CORE-113, APP-051 and QA-016, when the notification path can reach you; APP-020, the activation schemes and the URL grammar; APP-022, the shortcut, the registration and the toast XML; APP-039 and APP-043, `app/desktop.set` and `app/set_context`; APP-065, the agent-activity toast; APP-146, the seventh category and the toast rules; CORE-014, `desktop` and `diagnostics` are infrastructure; CORE-117, the new `JobSummary` fields; CORE-130, the stored captures; CORE-146, the new tool rows; CORE-160, the pause reasons, the withdrawal and the 2 s poll; CORE-161, the history rule and `estimate_key`; CORE-164, the header limits; ARCH-030, switching workspace by a new hello; QA-042, the owner-present local run (only with the owner's yes, P4a-R31); ARCH-072, the watchdog bound; ARCH-117, the log cap; ARCH-126, the launch-profile keys; the plan-3 carry N6, base64url launch arguments.
- **Test-channel tools added by this plan** (module `"test"`, registered only on `test-*` channels; a test contract, not copied into the spec):

  | Tool | Effect | Arguments | Result (`structured`) |
  |---|---|---|---|
  | `test.desktop` | write | `{fullscreen?: "no" \| "d3d" \| "busy" \| "presentation", foreground?: "suite" \| "other", shortcut?: bool, handlers?: bool, toasts?: "ready" \| "disabled_for_app" \| "disabled_for_user" \| "disabled_by_policy" \| "elevated", clear?: bool}` | `{toasts: [{op: "post" \| "update" \| "remove", tag, group, title?, body?, launch?, silent?, progress?}], power_held, polls}`: sets `FakeDesktop` and reads what it recorded |
  | `test.job` (extended) | write | adds `phases?: [string]`, `estimate_key?`, `silent_secs?`, `batch?: n` | `{job}`, or with `batch` `{jobs}` sharing one `batch_id` |

  Test-only overrides, honoured only on test channels: `LIL_TEST_STALL_SECS` (the 30 s of the stall rule) and `LIL_TEST_DESKTOP_POLL_MS` (the 2 s poll).
- **Test fakes** (QA-066; not copied into the spec): `lil_platform::desktop_fake::FakeDesktop` (toasts, fullscreen state, foreground pid, shortcut and handler state, power request), used by every test-channel core; `lil-fake-image` (a test-kit binary copied as `lil-image.exe` into its own folder, which records its arguments to `<test root>\fake-image-args.jsonl` and exits), for `lil confirm-window` and `lil-mcp`'s launches; `FakeComfy` gains `cached_nodes(&[node_id])` (sends `execution_cached`) and `oom_next(n)`; `FakeCore.qml` for Qt Quick Tests (Task 16).

## Review Focus

1. **The core goes away while the app is open:** it crashes, is stopped, restarts after an update, or a newer major replaces it; meanwhile a cap prompt waits inline, a confirm window is open, and Jobs shows progress.
   - **Expected:** a "Reconnecting…" banner within a second; AI and workspace actions are disabled; nothing is sent twice; after the reconnect each topic resumes from its last `seq` or reloads from a snapshot, with no lost or doubled rows; an inline cap prompt and its waiting request survive and still settle once, by retry token, after you answer; a call that was in flight ends with a clear error, never a silent resend; an incompatible core shows "restart the core when it's idle".
   - **Covered by:** Task 14, `session_reconnects_and_resubscribes_from_the_last_seq`, `session_gets_a_snapshot_when_the_history_is_gone` and `in_flight_calls_fail_and_are_never_resent`; Task 16, `pending_call_retries_once_after_an_answer_across_a_reconnect`; Task 20, `tst_banners::test_reconnecting_disables_actions`; Task 21, `tst_confirm_window::test_reconnect_keeps_the_dialog_and_its_state`.
2. **Odd or hostile links reach the handler:** a browser's `lil://` link with `%20` or a trailing `/`, quotes, a 50,000-character argument, `lil:open/` with bad base64url or a target holding control characters, another channel's scheme, an unknown verb, a `.lilref` file of 1 GB or with a bad hash, a UNC path.
   - **Expected:** each is refused with a short message showing at most 80 normalised characters; nothing opens that shouldn't, and no activation ever answers a confirmation; a well-formed link of each kind round-trips.
   - **Covered by:** Task 3's spec tests `every_form_round_trips` and `refuses_everything_else` (turned green by Task 4) and Task 4's property test `any_argument_is_classified_or_refused_without_panic`; Task 19, `args_refuse_a_large_lilref_and_a_bad_hash`, `args_refuse_a_network_lilref` and `tst_open_card::test_refused_link_shows_message`.
3. **A notification is clicked at an awkward moment:** the app is running, or isn't; the confirm window is already open; the confirmation was answered elsewhere, expired, or belongs to a core that restarted; two clicks come quickly.
   - **Expected:** exactly one window comes to the front; the dialog shows the confirmation's live state (pending, answered, expired, unknown) and never answers by itself; a second click reuses the window; a toast is removed when its confirmation is answered or expires.
   - **Covered by:** Task 21, `tst_confirm_window::test_answered_elsewhere_shows_state` and `test_a_second_url_adds_to_the_open_window`; Task 19, `a_confirm_url_goes_to_the_running_full_app_first` and `two_quick_second_launches_reach_one_window`; Task 10, `confirmation_toast_is_removed_when_answered_or_expired`; the desktop group's `desktop_toast_url_brings_the_confirm_window_to_front` (P4a-R31). Task 1's checks S1.4 and S1.5 set the recipe; they are spike code, not coverage.
4. **A fullscreen game starts and stops while work queues,** with one prompt already posted to ComfyUI behind an outside job, and you have also pressed Pause yourself.
   - **Expected:** new work stops within one poll; the executing prompt is never interrupted; the posted prompt is withdrawn only while ComfyUI still lists it pending, and posted again with a new id only after the old one is gone, never twice; leaving fullscreen never undoes your own pause; with auto-pause off (the default) nothing pauses.
   - **Covered by:** Task 6, `withdraw_only_while_pending_and_repost_with_a_new_id_once` and `crash_after_withdraw_never_posts_twice`; Task 9, `fullscreen_reason_never_clears_a_manual_pause` and `autopause_is_off_by_default`.
5. **Screens change between runs:** a monitor is unplugged, the scale differs (a layout saved at 150 % restored at 100 %), the saved layout comes from a newer KDDockWidgets (a blob its restore refuses), or the layout file is damaged or empty.
   - **Expected:** every window lands inside an available screen area; a layout that doesn't parse or restore is set aside as `.bad-<time>` and the Default layout opens, with no crash and no lost panel.
   - **Covered by:** Task 18, `layout_clamps_windows_to_the_available_area` (Qt-free, with the unplugged, rescaled and oversized cases) and `tst_layout::test_bad_layout_falls_back_to_default` (damaged, empty and refused-by-KDDockWidgets files).
6. **Windows won't show the toasts:** notifications are off for LILITH in Windows Settings, off for every app, blocked by policy, or the core runs elevated (toasts then fail silently).
   - **Expected:** `desktop.get` says "not reachable" and why; with no app open, an agent's call that needs a confirmation gets `confirm.unavailable` at once instead of waiting 30 minutes for a toast you never see; with the app open, the confirmation waits in the Confirmations panel with the top bar's badge; Diagnostics and the Settings panel name the reason and what to do.
   - **Covered by:** Task 10, `reachable_is_false_when_windows_turns_the_apps_toasts_off` and `an_elevated_core_is_never_reachable`; Task 25, `test_notification_path_problems_are_named`; Task 27, `test_unreachable_path_shows_the_reason_and_a_fix`.

## Changes from the roadmap row

- **Spec additions first** (Task 2): ARCH-127 (keep-awake) and APP-111's "While you were away" come from the parked AI-surfaces revision's plan-4a parts, without their director clauses; `app/desktop.set` (owner question Q1) and a seventh notification category (owner question Q2) are proposed.
- **A Settings panel** (Task 27) the row doesn't name: the toggles APP-146, CORE-160 and ARCH-127 call settings need a place to change them, and the notification setup of owner question Q5 lives there.
- **A checkpoint after Task 14** (owner direction): the Qt-free core is reviewed and checked in with the owner before the Qt work starts.
- **New core tools** the row implies but no rule lists: `projects.list` and `workspaces.list` (the top bar's switcher), `desktop.get`, `engines.environment` (the doctor's comparison), `telemetry.energy` (APP-131), `diagnostics.recent` (QA-083), `lilref.write` (APP-021) and `notify.claude_code` (ARCH-103).
- **`app/activate`** (APP-043's interchange row) waits for a second app: with one app, `open_requests:image` is enough.
- **The ComfyUI panel** is plan 6a's (APP-009 and IMG-126's lock-down). This plan builds only the lazily loaded `Lilith.Web` module with its named WebEngine profile from `WebEngineProfilePrototype` (the roadmap's carry for 4a and 6a), and keeps it out of the panel list.

## Not in this plan

- **Spec rules in the row's sections that this plan doesn't build**, each with its owner:
  - §3.1: named layout presets (APP-004) [M1.5]; APP-007 [Later]; APP-009 [plan 6a]; APP-010 [plan 4b]; APP-017 [plan 5]; APP-018 [M1b]; APP-019 [plan 4b]; APP-023 [Later]; APP-143's browser, Review and canvas scopes [plans 4b and 5; this plan builds the map, its global keys and the conflict check];
  - §3.2: APP-035's document holds [plan 5]; APP-038 [plan 4b]; APP-032's clipboard bytes [plans 4b and 5];
  - §3.3: APP-042 [plan 5]; APP-050, APP-052, APP-053 [plan 7+]; `app/activate` [with the second app, Later]; `app/thumbs.request` [plan 4b];
  - §3.6: APP-114's filters (status, kind, who started it) [plan 4b, with "Clear inactive" under the same amendment]; APP-112, APP-113, APP-141, APP-142, APP-145, APP-147, APP-148, APP-157 [plan 4b]; APP-114's phases display, unresponsive badge and "Clear inactive" [plan 4b, on this plan's fields]; APP-122's account sync [plan 6b]; APP-123's Engine A usage [plan 7+]; APP-125 [M1.5]; APP-126 and APP-130 [Later]; APP-127's shared GPU memory and per-engine load and APP-132's spill alert [plan 4b, with CORE-059]; APP-133's backups [plan 7+; this plan shows an empty state];
  - §3.7: APP-138 [plan 4b];
  - §2.8: CORE-160's live class and Generate modes [plan 6a]; CORE-163's step 2 [plan 6a]; CORE-165 [plan 6a]; CORE-166 [Later];
  - §1.9: ARCH-120 [Later];
  - the parked AI-surfaces revision's text-normaliser rule beyond notifications, ARCH-127's director-task clauses and the task report [the AI-surface plans]; this plan uses only the notification part (P4a-R8).
- **Plan-3 carries aimed at plan 4 that go elsewhere:**
  - the PreToolUse hook gaps R1–R4 and `sed` `w`/`W` from the final 3b re-review, carried under P3-R34 [a standalone hook-fix PR from `main`, which the owner dispatches; P4a-R22];
  - a manifest lock written as the 8.3 alias of a trashed folder; `trash.restore {to?}`; thumbnails; run and version orphans from index rows; `project.status` at 10,000 assets [plan 4b, with its store and index work];
  - the plan-2b "Wait" items tagged plan 4: the 32-bit serial and `lock_byte` notes, `folder_key` drive-letter tests, NTFS-only assertions, `create_junction` visibility and `$UpCase` agreement, the `suite.toml` fail-point note and failed-create test, `unsafe_entry`'s per-file handle at seal, the `module_deps` cycle guard and the warm-10k setup, `verify_pending`'s test strength, names without a SHA-256 at seal, Q27's core-side set-aside of orphan `-wal`/`-shm` (it needs spec text), `run_folder`'s and `runs_elsewhere`'s identity errors, the `PlaceholderAt`/`CloudProbe` spelling and `workspace.create`'s post-create race [plan 4b's core batch, which changes `store`];
  - the clipboard recipe [plan 4b for copy and drag-out, plan 5 for paste-in and drag-in].
- **Plan 6a:** the ComfyUI panel and its lock-down on `Lilith.Web`; the memory mode per model profile (IMG-155); the Generate panel's cost and time chip.
- **Later CI work** (a workflow the owner pushes): an alert for a Qt WebEngine 6.140.x security release (pre-research §2); a Windows Sandbox runner for the desktop group (P4a-R31).
- **Decided, not built:** the live-preview provider stays in the C++ shim, where APP-031 and ARCH-110 put it, rather than moving to Rust through cxx-qt 0.9's `QQmlImageProviderBase`.
- **Plan 7+:** the Velopack release job and the installer copies (QA-074, OPS-091–101); `lil-oracle` in `identity::APP_EXES`; director-task toasts and the Assistant panel's `task` URLs.

## Execution notes

- **Per-task gate** (targeted tests only, the owner's testing rule), run at the end of every task before its commit. "Run the gate" in a task means:
  1. `cargo nextest run -p <crate> --retries 0 -E '<the task's filter>'` for each crate the task touched; never `--workspace`;
  2. the flake guard on the task's new tests: `pwsh -NoProfile -Command 'foreach ($i in 1..10) { cargo nextest run -p <crate> --retries 0 -E "<filter>"; if ($LASTEXITCODE) { exit 1 } }'` (single quotes, so the calling shell doesn't expand `$i` or `$LASTEXITCODE`), and for QML `ctest --preset qml -R <tests> --repeat until-fail:10`. Tests that start real cores (Tasks 6, 10, 11, 14) take about 10 minutes for the ten runs; say so before running;
  3. `cargo run -q -p lil-cli -- dev spec-tests` (exits 0; the expected-fail count only goes down after Task 3);
  4. `cargo clippy -p <crate> --all-targets -- -D warnings` and `cargo fmt -p <crate> --check` for each crate touched;
  5. `cargo run -q -p lil-cli -- dev spec-report` (exits 0: the rule count `EXPECTED_RULE_COUNT` pins, no dangling citation, no unrecorded ruling);
  6. `cargo run -q -p lil-cli -- dev privacy-scan --tree HEAD` (after the commit).

  A task that changes a `Cargo.toml` or `Cargo.lock` also runs `cargo deny check`. GUI tasks also run `cmake --build --preset dev` (about 25 minutes the first time from a clean tree, 1–3 minutes after), their `qml` and `grabs` tests, and attach the grabs to the review. CI runs the full suite on every PR; the owner runs it locally only if they choose.
- **Commits.** Every task commits all the files it changed, and every commit message ends with the trailer `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`. The commit blocks below give the subject line only.
- **Review on every task** (owner direction: quality over speed): a spec-compliance review and a code-quality review by fresh reviewers, and for GUI tasks a look at every grab. **Adversarial review** (QA-023), where the reviewer must try to write a failing test: Tasks 4 (the untrusted parsers), 6 (jobs, the dispatcher), 9 (pause and keep-awake), 10 (the notification path for confirmations), 11 (launch arguments, `lil-mcp` launches), 19 (activation and single instance), 21 (confirmation answers) and 28 (Windows Hello in `lil dev`). Every review and fix report is saved to a file outside the repository.
- **Spec tests** (QA-017, QA-018): Task 3's author reads only the spec (Appendices D to G), this plan's Global Constraints and the Interfaces blocks. Later tasks only remove `expected-fail.toml` entries; none edits a spec test except the protected one-value changes of Tasks 6 and 11.
- **Tool registration** as in plan 3: each task that adds tools calls its `register_*_tools` from `core_registry` (or `server/mod.rs` for tools that need a running module) and updates the pins in `crates/lil-core/src/tools/tests.rs`; never delete a pin to get green. `crates/lil-core/tests/module_deps.rs` learns `desktop` and `diagnostics` (P4a-R24).
- **GUI builds** run in a Visual Studio developer shell: `pwsh -NoProfile -File tools/dev-shell.ps1 -- <command>` (Task 15) sets up MSVC and puts `LIL_QT_DIR` (default `C:\Qt\6.12.0\msvc2022_64`) on `CMAKE_PREFIX_PATH`.
- **Checkpoint A, after Task 14** (owner direction): when Tasks 0–14 have merged, a scoped review reads the merged core against Appendix G, and the owner checks in on it (the queue controls, the notification path and the desktop settings, through `lil call`) before Task 15 starts. Splitting the plan there into two halves is the owner's call; nothing below depends on it.
- **Types named but not spelled out** in an Interfaces block (`QueueState`, `Estimate`, `DesktopSettings`, `EnvCapture`, `ModelsReport`, `JobStatsRow`) are the Rust forms of the Appendix G shapes of the same name.

---

### Task 0: Spike — the Qt shell stack (APP-005, APP-135, APP-136, APP-137, APP-156, ARCH-114, ARCH-116, IMG-126) — **owner present** for S0.5 (b, c), S0.6, S0.7, and the scratch CI run of S0.9 and S0.10, which the owner pushes

**Depends on:** PR #24 merged. Needs Qt. May run at the same time as Task 1.

**Files:**
- Create: `spikes/app-shell/` — `CMakeLists.txt`, `main.cpp`, `qml/{Main.qml, JobsSim.qml, GridSim.qml, ChartsSim.qml, WebPanel.qml}`, `web/` (the separate QML plugin `SpikeWeb`: `SpikeWebView.qml` and the WebEngine profile from `WebEngineProfilePrototype`), `rust/` (crate `spike_shell_bridge`: a 10,000-row `QAbstractListModel` paged 200 rows at a time; a feed of 8 fake jobs at 10 Hz with JPEG frames; 6 telemetry series at 1 Hz), `shim/` (the KDDockWidgets dock factory, a preview `QQuickImageProvider`, a 4 ms GUI-thread heartbeat logger), `run-spike.ps1` (builds from scratch, deploys, runs each check, writes `results/checks.json`), `comfy-load.ps1` (starts ComfyUI on `127.0.0.1` with the spike's own GPU-saturating test workflow in a loop, and stops it afterwards).
- Create: `docs/spikes/10-app-shell.md` (from `docs/spikes/TEMPLATE.md`); modify `docs/spikes/README.md` (a summary row).

**Interfaces:**
- Produces, for Task 2 and Tasks 15–29: the offscreen test environment and the CI time; the KDDockWidgets version (2.5.0 or 2.4.1); whether P4a-R1's feature route works; the `windeployqt` flags and `--skip-plugin-types` list, and the trimmed folder's size; the cold-start, stall and VRAM numbers; the Narrator answers; the cache location rule.

**Pass bars, fixed before the run** (copied verbatim into §4 of the spike document before anything runs):
- **S0.1 KDDockWidgets 2.5.0** (time box: half a day): `FetchContent` at tag `v2.5.0`; the shim's dock factory themed through ViewFactory QML; 3 docks in 2 main windows plus 1 floating; `LayoutSaver` save and restore; `LayoutSaver::addWindowToSave()` for one window. PASS = configure and build exit 0 with 0 warning lines from our own files at default settings (no warning flag added or removed in them; warnings from KDDockWidgets' own headers are recorded separately and don't fail the check); restore puts each of the 3 windows within 2 px of its saved geometry with the same docks in the same places (by unique name), 3 runs of 3; the per-window save round-trips one window alone; exit code 0. Any FAIL, or the time box running out → the same checks on `v2.4.1`, which then stays (APP-005).
- **S0.2 The `lil-qt` route** (P4a-R1): in a scratch worktree only, a throwaway workspace member with `cxx`, `cxx-qt` and `cxx-qt-lib` optional under a `qt` feature and `cxx-qt-build` an optional build-dependency. PASS = it builds through `cxx_qt_import_crate(MANIFEST_PATH <member>/Cargo.toml CRATES <member> LOCKED FEATURES qt QT_MODULES Qt6::Core Qt6::Gui Qt6::Qml)` against the workspace `Cargo.lock`, and its QML counter round-trips; and `cargo build --workspace --locked` and `cargo clippy -p <member> --all-targets -- -D warnings` pass with no Qt folder on PATH and `QMAKE` unset. FAIL → P4a-R1's fallback. The scratch worktree is deleted; only the result is recorded.
- **S0.3 Lazy WebEngine library** (APP-137, IMG-126): `SpikeWeb` is a separate QML plugin DLL imported only by `WebPanel.qml`, which a `Loader` loads when its dock is first shown; the named persistent WebEngine profile comes from `WebEngineProfilePrototype`. PASS = `dumpbin /dependents` of the exe, the shim DLL and the bridge library list no `Qt6WebEngine*`; `GetModuleHandleW(L"Qt6WebEngineCore.dll")` is null from start until the dock is shown and non-null after; the WebEngine profile's storage is under `%LOCALAPPDATA%\LILITH-spike\image\`; Qt logs no deprecation warning; `about:blank` loads. FAIL → stop; the result goes to the owner before Task 2, because APP-137 (an invariant) can't be met as specified.
- **S0.4 Deploy size** (ARCH-114, OPS-094): the exe and our DLLs (shim, KDDockWidgets, `SpikeWeb`) are copied first, then `windeployqt --release --qmldir <every QML folder> --no-translations --no-opengl-sw --no-system-dxc-compiler --no-ffmpeg --skip-plugin-types <list>`, and the VC++ runtime DLLs are copied app-local from the MSVC redist folder (no `vc_redist` is run). Record the `--dry-run --list` size and file count untrimmed, trimmed, and trimmed without the WebEngine module. PASS (hard) = the trimmed folder runs S0.1's restore and S0.3's lazy load with every Qt, Visual Studio, MSVC and Windows Kits folder removed from PATH and `QTDIR`, `QT_PLUGIN_PATH` and `QML_IMPORT_PATH` unset (spike 02's harness). Target (soft) = trimmed ≤ 300 MiB; above that is reported, not a failure.
- **S0.5 Cold start** (APP-136): the deployed app logs `first_frame` (the first `frameSwapped`) and `interactive` (the first idle of the event loop after the QML root completes), in ms from process start. PASS = `interactive` < 2,000 ms in each of: (a) 10 warm starts (p50 and max recorded); (b) the first start after a reboot (owner present); (c) the first start of a fresh copy of the folder after a Defender scan of it (owner present: `"%ProgramFiles%\Windows Defender\MpCmdRun.exe" -Scan -ScanType 3 -File <folder>`). A repeat of spike 02's 145 s stall goes to the owner at once, with its log. Other FAIL → the start is profiled (QML cache through `qmlcachegen`, the pipeline cache, deferred panel loading) and re-measured once; still over 2 s → the result goes to the owner before Task 2.
- **S0.6 GUI stalls under ComfyUI load** (APP-135): at once, 8 fake jobs each pushing a 512×512 JPEG preview and progress at 10 Hz through the image provider; the 10,000-row paged list scrolled 200 rows a second; 6 charts drawn with Qt Quick Shapes `PathPolyline`, 900 points each, a point a second (P4a-R17); while ComfyUI runs the test loop with NVML GPU utilisation ≥ 95 % throughout. Over 60 s, measure the 4 ms heartbeat's lateness and the frame interval. PASS = p99 < 16 ms for both (max recorded). Recorded, not a bar: the spike's dedicated VRAM (PDH `Local Usage`) with the web dock shown, for P4a-R15. FAIL → previews drop to 5 Hz and charts to one redraw a second, and the check runs again; still failing → the result goes to the owner before Task 2.
- **S0.7 Narrator smoke check** (APP-156; owner present): with Narrator on (Ctrl+Win+Enter), the owner tabs through the spike window and records yes or no for: a KDDockWidgets title bar, a dock tab, the counter button by its accessible name, the chart tile (a custom delegate with `Accessible.role` and `Accessible.name`), and an `Accessible.announce()` message. PASS = the spike's own controls are read (else the pattern is fixed before Task 18). The KDDockWidgets answers are recorded, and any "no" goes to the owner (APP-156).
- **S0.9 Offscreen GUI tests** (QA-068): under `QT_QPA_PLATFORM=offscreen`, a Qt Quick Test with a KDDockWidgets main window and one floating window, a `PathPolyline` Shape, a `Loader` of the `SpikeWeb` plugin, and a grab that isn't one flat colour; the backend environment it needs (for example `QT_QUICK_BACKEND=software`) is recorded. PASS = all of it works locally and in one scratch CI run on `windows-2025` (a workflow commit the owner pushes, P4a-R32). FAIL → P4a-R35 (hidden real windows, owner present) for whatever failed, recorded per item.
- **S0.10 CI time with Qt** (P4a-R32): the scratch CI run of S0.9 also builds the spike with Qt installed by `naqt` and times the whole `pr-ready` job's steps. PASS = the job, with Qt added, stays under 50 minutes. FAIL → Task 15 splits Qt into its own job with a Qt cache, as P4a-R32 says.
- **S0.8 Caches:** with `organizationName` `LILITH-spike` and `applicationName` `image`, PASS = Qt's QML and pipeline caches are under `%LOCALAPPDATA%\LILITH-spike\image\`, and no new folder named after the executable appears in `%LOCALAPPDATA%`. FAIL → the app sets `QML_DISK_CACHE_PATH` and the pipeline-cache path itself from `Env` (ARCH-095), and the check runs again.

- [ ] **Step 1: Write §1–4 of `docs/spikes/10-app-shell.md`** with the bars above, verbatim, and commit before running anything.

```bash
git commit -m "spike 10: the app-shell question, setup and pass bars"
```

- [ ] **Step 2: Build the spike** (about 40 minutes for the first build from scratch), S0.1 first, stopping S0.1 at its time box. Run `pwsh -NoProfile -File spikes/app-shell/run-spike.ps1 -Clean`. Expected: `spikes/app-shell/results/checks.json` with a verdict per check.
- [ ] **Step 3 (owner present; about 1 hour with a reboot):** S0.5 (b) and (c); S0.9's and S0.10's scratch CI run, which the owner pushes; S0.6, with ComfyUI started by `comfy-load.ps1` only after the owner says go, and stopped by it; S0.7.
- [ ] **Step 4: Write §5–8 and the README row.** §7 names the decisions: the KDDockWidgets version, P4a-R1's route, the deploy flags, the offscreen environment (or P4a-R35), the CI time (and whether P4a-R32's split applies), and the measured VRAM for P4a-R15. Run `cargo run -q -p lil-cli -- dev privacy-scan --tree HEAD` after the commit. Expected: clean.

```bash
git commit -m "spike 10: app-shell results (KDDockWidgets, lil-qt route, lazy WebEngine, deploy, cold start, stalls, Narrator)"
```

---

### Task 1: Spike — Windows integration: toasts, activation, foreground, fullscreen, keep-awake and Windows Hello (APP-014, APP-020, APP-022, APP-146, ARCH-127, CORE-160, QA-088) — **owner present**

**Depends on:** PR #24 merged. Qt-free. May run at the same time as Task 0.

**Files:**
- Create: `spikes/winshell/` — a standalone crate (`windows` 0.62.x) with the binaries `winshell-core` (`windows_subsystem = "windows"`, started detached as `lil-client` starts the core), `winshell-app` (a tiny window and a single-instance pipe), `winshell-hello` (a console program), and `run-spike.ps1` (registers, runs, cleans up and writes `results/checks.json`).
- Create: `docs/spikes/11-windows-integration.md`; modify `docs/spikes/README.md`.

**Interfaces:**
- Produces, for Tasks 2, 5, 9, 10, 19, 21 and 28: the `windows` feature list; whether a windowless process can post under a shortcut's AUMID; what a protocol activation's raw command line holds (from a toast, from a browser, from Run); the foreground recipe; the progress-toast behaviour; the fullscreen values; the power-request API; the Windows Hello route (KEK, DPAPI with a gate, or passphrase only).

**Pass bars, fixed before the run** (copied verbatim into §4):
- **S1.1 Crate surface:** the spike builds with 0 warnings using `UI::Notifications::{ToastNotificationManager, ToastNotification, NotificationData}`, `Data::Xml::Dom::XmlDocument`, `Security::Credentials::KeyCredentialManager`, `Win32::System::WinRT::IUserConsentVerifierInterop`, `Win32::UI::Shell::SHQueryUserNotificationState`, `Win32::System::Power::{PowerCreateRequest, PowerSetRequest, PowerClearRequest}`, `Win32::UI::Shell::PropertiesSystem::IPropertyStore` and `Win32::Security::Cryptography::{CryptProtectData, CryptUnprotectData}`. The feature list is recorded for Task 5.
- **S1.2 Windowless sender** (APP-022): `winshell-core.exe`, started with `DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP`, posts a toast under AUMID `LILITH.Spike.Notify`, with a per-user shortcut `<Programs>\LILITH Spike.lnk` carrying that AUMID. PASS = the owner sees it pop, 3 times of 3, and `ToastNotifier::Setting()` is `Enabled`. Recorded, not a bar: whether a toast shows with the shortcut removed and only `HKCU\Software\Classes\AppUserModelId\LILITH.Spike.Notify` present.
- **S1.3 Protocol activation** (APP-020): `HKCU\Software\Classes\lilspike` registered as Appendix G's registration says, with scheme `lilspike`. PASS = a click on the toast's body and one on its button each start `winshell-app.exe` with `GetCommandLineW` exactly `"<exe>" "lilspike:confirm/cf_0123456789abcdef"`, 3 times of 3 each. Recorded: the raw command line when Edge opens `lilspike://0123456789abcdef/sketchbook/lantern@1.0.0#sha256=<64 hex>` from a local HTML file, and when the Run dialog opens `lilspike:open/<base64url>` (encoding, a trailing `/`, quotes). FAIL (the argument arrives split, re-quoted or percent-encoded) → P4a-R3's grammar is changed to match what arrives before Task 2 writes Appendix G, and the change is reported.
- **S1.4 Foreground** (APP-014): with `winshell-app.exe` A running behind another focused window, a toast click starts B, which reads A's pid over the spike's single-instance pipe, calls `AllowSetForegroundWindow(A)`, passes the argument and exits; A calls `SetForegroundWindow`. PASS = `GetForegroundWindow()` is A's window and A has keyboard focus, 5 times of 5; with no A running, B's own window comes to the front, 5 times of 5. FAIL → the foreground recipe changes (the running instance also calls `AllowSetForegroundWindow` for its own windows and uses `SwitchToThisWindow` as a fallback) and the check runs again; still failing → the owner hears before Task 2, and the confirm window falls back to a flashing taskbar button.
- **S1.5 Progress toasts** (APP-146): a toast with Tag `j_spike`, Group `job_progress` and data binding, updated once a second for 20 s. PASS = every `Update` returns `Succeeded` and the toast doesn't pop again; after the owner dismisses it, `Update` returns `NotificationNotFound`; at the end `Remove(tag, group)` then a new toast pops; a toast with `SuppressPopup = true` lands in the notification centre without popping (owner confirms). FAIL → progress toasts are left out of plan 4a (the `job_progress` category stays, off, with no toasts) and the owner hears before Task 2.
- **S1.6 Fullscreen poll** (CORE-160, APP-146): `SHQueryUserNotificationState` every 2 s while the owner (a) works at the desktop, (b) runs a fullscreen Direct3D game or program, (c) plays a fullscreen video in a browser, (d) runs a presentation if one is at hand. PASS = (a) gives `QUNS_ACCEPTS_NOTIFICATIONS`; (b) gives `QUNS_RUNNING_D3D_FULL_SCREEN` or `QUNS_BUSY`; the call's p99 < 1 ms over 100 calls. (c) and (d) are recorded. The poll runs from `winshell-core.exe`, the detached windowless process, as the core will. FAIL → fullscreen auto-pause and the quiet mode use only the values that did work, and the owner hears before Task 2.
- **S1.7 Keep-awake** (ARCH-127): the detached `winshell-core.exe` holds `PowerSetRequest(PowerRequestSystemRequired)` on a `PowerCreateRequest` object with a reason string. PASS = `powercfg /requests`, which the owner runs in an elevated terminal, lists it under SYSTEM while held and not after `PowerClearRequest`; and, when this PC's sleep timeout is finite, the PC stays awake 5 minutes past it with no input while the request is held (owner present; recorded as skipped when the timeout is "never"); `powercfg /a` (owner) records whether Modern Standby is present; and the spike's hidden window receives `PBT_APMSUSPEND` and `PBT_APMRESUMEAUTOMATIC` across one sleep (owner present). If those messages don't arrive, ARCH-127's sleep-gap record falls back to comparing the wall clock with the monotonic clock every 30 s, a jump of more than 60 s counting as a sleep. FAIL → the same checks with `SetThreadExecutionState(ES_CONTINUOUS | ES_SYSTEM_REQUIRED)` from a dedicated thread; if both fail, ARCH-127's "can't be held" path.
- **S1.8 Windows Hello** (QA-088): from `winshell-hello.exe` in Windows Terminal: (a) `KeyCredentialManager::IsSupportedAsync()` is true; (b) `RequestCreateAsync("LILITH-spike-kek", ReplaceExisting)` shows the Hello prompt in front of the terminal; (c) `OpenAsync`, then `RequestSignAsync` over one fixed 32-byte challenge 5 times in one process and 5 times in a second process, gives 10 byte-identical signatures; (d) a TPM 2.0 is present (`Tbsi_GetDeviceInfo`); (e) `IUserConsentVerifierInterop::RequestVerificationForWindowAsync(GetConsoleWindow(), …)` shows the prompt in front and returns `Verified`; (f) `DeleteAsync` removes the credential. The route: **KEK** when (a), (b), (c), (d) and (f) pass; else **DPAPI with the Hello gate** when (e) passes and `CryptProtectData`/`CryptUnprotectData` round-trip; else **passphrase only**, reported to the owner.
- **Cleanup:** PASS = afterwards the spike's shortcut, `HKCU\Software\Classes\lilspike`, `HKCU\Software\Classes\AppUserModelId\LILITH.Spike.Notify` and the credential are gone, checked by the script.

If S1.1 or S1.2 fails, stop: the result goes to the owner before Task 2 (APP-022).

- [ ] **Step 1: Write §1–4 of `docs/spikes/11-windows-integration.md`** with the bars, verbatim, and commit before running anything.

```bash
git commit -m "spike 11: the Windows-integration question, setup and pass bars"
```

- [ ] **Step 2 (owner present, about 1 hour including one sleep and wake; the owner says go first, because the script writes a shortcut, registry keys and a Windows Hello credential, all removed at the end):** run `pwsh -NoProfile -File spikes/winshell/run-spike.ps1`. Expected: `spikes/winshell/results/checks.json` with every check's verdict and the cleanup's PASS.
- [ ] **Step 3: Write §5–8 and the README row**, naming the decisions: the feature list, the activation forms, the power-request API, the Hello route. Run the privacy scan after the commit. Expected: clean.

```bash
git commit -m "spike 11: Windows-integration results (toasts, activation, foreground, fullscreen, keep-awake, Windows Hello)"
```

---

### Task 2: Spec changes first (QA-029) — Qt-free — **protected: `crates/lil-cli/src/dev/spec_report.rs`**

**Depends on:** Tasks 0 and 1 merged; the owner's answers to the questions at the end (each default stands if the owner says nothing else).

**Files:**
- Modify: `docs/superpowers/specs/2026-10-12-lilith-design.md`; `docs/GLOSSARY.md`; `docs/superpowers/plans/2026-10-12-roadmap.md`; `dev/rule-classification.toml` (ARCH-127 classified `test`); `crates/lil-cli/src/dev/spec_report.rs` (`EXPECTED_RULE_COUNT = 1080`, protected).

**Interfaces:**
- Produces: ARCH-127; spec Appendix G and the Appendix D additions, which Tasks 3–29 implement.

- [ ] **Step 1: Re-check the number registry** against `main`: `SUITE_MIGRATIONS.len()`, `WORKSPACE_MIGRATIONS.len()`, telemetry `USER_VERSION`, `PROTOCOL`, `codes::ALL.len()`, the highest ARCH id. Expected: 8, 7, 1, 1.1, 66, ARCH-126. If any moved, stop and tell the controller.
- [ ] **Step 2: Add ARCH-127** after ARCH-023, exactly:

```markdown
- **ARCH-127** [M1] (invariant) **Keep this PC awake while work runs** (owner decision 2026-10-20; plan 4a). While any suite GPU job is running, and only then, the core holds a system-required power request, so the PC doesn't sleep on its idle timer; the display may still turn off. It changes no power plan or power setting, never blocks a sleep or shutdown you start, and releases the request as soon as nothing qualifies.
  - A setting, on by default, turns it off for suite GPU jobs.
  - The core records each gap in which the machine slept anyway, from the system's resume events, in its log and in Diagnostics' event log.
  - Plan 4a's spike settles which Windows API holds the request on this PC and whether it holds through the idle timeouts of Modern Standby machines; this rule fixes the behaviour, not the API. If the request can't be held on this PC, the setting is hidden and Diagnostics says so.
  - Director tasks join this rule with the director plan.
```

- [ ] **Step 3: Amend rules.**
  - APP-111 gains: "(owner decision 2026-10-20; plan 4a) Home has a **"While you were away"** card for the jobs that finished, failed or wait for you, and the confirmations waiting, since you last looked, by a `last_seen_at` time the app records per workspace when Home is shown. The director plan adds a line per director task."
  - Each rule in Global Constraints' "Clarifications recorded in Appendix G" gains one sentence ending "(plan 4a clarification: Appendix G)".
  - The owner's answers: Q1 → APP-039's list and APP-043's map gain `app/desktop.set` (a "Notifications and desktop" row: `app/desktop.set` | `desktop.get` | `desktop`); Q2 → APP-146's categories gain "Claude Code and agent activity", on by default; questions Q3 and Q4 → APP-146 and ARCH-121 each gain the sentence of the default; Q5 → APP-022 gains "the app offers the notification setup on its first start, and writes it only after your click"; Q6 → APP-022 gains "no toast carries an answer button"; Q7 → nothing (APP-164 stands); P4a-R31's run → QA-042 gains "until a runner that can post toasts exists, the owner may start the desktop group in their own session", only with the owner's yes. A "no" is recorded as the owner gave it, and the matching Appendix G line follows it.
  - Spike results as "(plan 4a result, <date>: …)" notes, each citing `docs/spikes/10-app-shell.md` or `11-windows-integration.md`: APP-005, APP-001 and ARCH-114 (the KDDockWidgets version); APP-022 (the windowless sender); APP-135, APP-136, APP-137 and APP-156 (the numbers and the Narrator answers); ARCH-127 (the API, or "can't be held"); QA-088 (the route); ARCH-126 (P4a-R15's VRAM reading).
- [ ] **Step 4: Edit CORE-146's table** (table rows, not rule lines): `projects.list`, `workspaces.list`, `desktop.get`, `engines.environment`, `telemetry.energy`, `diagnostics.recent` (read, M1); `lilref.write`, `notify.claude_code` (write, M1). APP-043's map gains them too: `projects.list` and `workspaces.list` in the Session row, `telemetry.energy` in Hardware, `diagnostics.recent` and `engines.environment` in Diagnostics, `lilref.write` and `notify.claude_code` in Interchange.
- [ ] **Step 5: Edit Appendix D:** protocol 1.2 and the capabilities of Global Constraints; the methods table's `app/*` row names the four plan-4a methods and their capabilities; the error-code row `job.not_queued` | `data_conflict` | `never` | plan 4a; the `lil` command rows `lil confirm-window [<id>]` and `lil hook notification`.
- [ ] **Step 6: Write `## Appendix G. App shell and plumbing (plan 4a clarification)`** after Appendix F: a lead-in like Appendix F's ("…They are contracts… It adds no rule IDs except ARCH-127, which §1.1 holds…"), then the sub-bullets of Global Constraints' "Names that are contracts", in order, as written, without the protocol bullet. Fill each value Tasks 0–2 settle: the Manager flag, the dynamic-VRAM opt-out, the asset-catalogue probe, the power-request API, the Notification hook's failure setting, the trailing-`/` rule. No line may start with `- **` followed by a rule ID. §0.5's map ends "· Appendix G App shell and plumbing (plan 4a clarification)".
- [ ] **Step 7: Re-verify the external facts** Appendix G relies on, from primary sources, each recorded with the date in Appendix B.3: `ToastNotifier.Setting`'s values and that an elevated sender's toasts don't show; ComfyUI 0.42.1's Manager flag, `--reserve-vram`'s unit, `--preview-method taesd`, its dynamic-VRAM opt-out, and how a client tells whether its asset catalogue is on; Claude Code's Notification hook input (`message`, `title`, `notification_type`) and whether `onFailure` applies to it; KDDockWidgets 2.5.0's release notes against Qt 6.12; `SHQueryUserNotificationState`'s values; the toast schema's `activationType="protocol"`, `SuppressPopup` and progress binding; how `ShellExecute` passes a `"%1"` argument. A fact that differs from Appendix G's text: Appendix G follows the fact, and the controller hears of it before Task 3.
- [ ] **Step 8: Glossary rows** (QA-089), in the matching sections:

  | Term | Meaning | Avoid | Defined in |
  |---|---|---|---|
  | Toast | A Windows notification the core posts under the channel's AUMID; Windows draws it, and a click opens a `lil:` URL in an app. | popup, alert (an alert is a Hardware or cap event) | APP-022, ARCH-005 |
  | Notification category | One of the toast kinds, each with its own toggle. | notification type | APP-146 |
  | Activation scheme | The URL scheme a channel's toasts and links use: `lil`, `lil-dev` or `lil-test-<id>`. | protocol (the pipe protocol is another thing) | APP-020, Appendix G |
  | Confirm window | The minimal `lil-image --confirm` window that answers confirmations without the shell. | dialog (alone), popup | APP-048, APP-049 |
  | Pause GPU work | Holding every queued job of a lane, yours and agents', without stopping the running prompt. | suspend, stop | CORE-160 |
  | Power request | What the core holds so the PC doesn't sleep while a GPU job runs. | wake lock, keep-awake lock | ARCH-127 |
  | Cost and time chip | The estimate shown before an action that spends GPU time or money. | price tag, quote | APP-140, CORE-161 |
  | Deploy folder | The trimmed `windeployqt` folder that runs with Qt and MSVC off PATH. | install folder (Velopack's) | ARCH-114, OPS-094 |

- [ ] **Step 9: Roadmap:** row 4a cites the two spike documents; row 4b gains the carries this plan sends it ("Not in this plan"); the carry-forward table gains a row for the standalone hook-fix PR (P4a-R22).
- [ ] **Step 10: Set `EXPECTED_RULE_COUNT` to 1080** (protected) and verify.

Run: `grep -cE '^- \*\*[A-Z]+-[0-9]{3}\*\* \[' docs/superpowers/specs/2026-10-12-lilith-design.md` and `cargo run -q -p lil-cli -- dev spec-report`
Expected: 1080 from both; 0 dangling citations; 0 unrecorded rulings. Then `cargo nextest run -p lil-cli --retries 0 -E 'test(/spec_report/)'`: PASS.

- [ ] **Step 11: Commit.**

```bash
git commit -m "spec: ARCH-127 keep-awake, Appendix G (app shell and plumbing), protocol 1.2 and the plan-4a tool rows"
```

---

### Task 3: Protected spec tests for plan 4a, expected to fail (QA-017, QA-018, QA-021) — Qt-free — **protected: `crates/lil-spec-tests/Cargo.toml`**

**Depends on:** Task 2 merged.

**Files:**
- Create: `crates/lil-parse/` with `Cargo.toml` and `src/{lib.rs, lil_url.rs, text.rs, model_header.rs}` holding the public signatures of Task 4's Interfaces with `todo!()` bodies (QA-017, interface first).
- Create: `crates/lil-spec-tests/tests/{jobs_queue_4a.rs, estimates.rs, comfy_profile.rs, notifications.rs, desktop.rs, interchange_4a.rs, diagnostics.rs, doctor_4a.rs, lil_url.rs}`.
- Modify: `crates/lil-spec-tests/expected-fail.toml` (one entry per new test, grouped under the task that removes it, each with its rules); `crates/lil-spec-tests/Cargo.toml` (protected: `lil-parse` as a dev-dependency).

**Interfaces:**
- Consumes: spec Appendices D–G, Global Constraints, the Interfaces blocks of Tasks 4–14. The author reads nothing else.
- Produces: the expected-fail list Tasks 4–14 shrink.

- [ ] **Step 1: Write the tests**, each citing its rules on its first line (`// spec: …`), through tool calls, `app/*` calls from `TestApp`, the `lil` CLI, `test.desktop`, and `lil-parse`'s public functions (the URL grammar is a format, QA-018). Assertions, by file:
  - `jobs_queue_4a.rs` (removed by Task 6): `job_move_next_and_last_stay_within_the_class` (CORE-160: three queued `agent` jobs behind a gated running job; `job.move {to: "next"}` on the third makes `jobs.list` order it first among `agent`, never ahead of a queued `user` job; `last` puts it last); `job_move_of_a_running_job_is_not_queued` (the code is `job.not_queued`); `job_cancel_by_ids_batch_and_all_mine` (`{ids}` cancels exactly those; `{batch_id}` cancels the batch of `test.job {batch: 3}`; `{all_mine: true}` from `cli` leaves the `ui:test` app's jobs alone; two selectors → `params.invalid`); `queue_pause_holds_every_queued_job_and_never_interrupts_the_running_prompt` (CORE-117, CORE-160: after `app/queue.pause {}` the running gated job keeps running, every queued job shows `queue_paused`, `jobs.list` `lanes[0]` is `{lane: "gpu-heavy", paused: true, reasons: ["manual"]}`; after `app/queue.resume {}` they run in order); `queue_pause_withdraws_a_pending_prompt_and_reposts_it_once` (CORE-160, CORE-120: with an outside prompt running in the fake ComfyUI and a suite prompt `queued_in_engine` behind it, pausing posts exactly one `/api/jobs/{id}/cancel`, the job returns to `queue_paused`, and after resume exactly one new `POST /prompt` carries a different prompt id); `queue_pause_is_only_for_verified_apps` (APP-040: `cli` gets `caller.not_allowed`; a `ui:test` session without `queue.v1` gets `caller.not_allowed`); `job_records_carry_batch_phase_and_last_progress` (CORE-117: a `test.job {phases: ["loading_models", "sampling"]}` shows `batch_id` `b_` plus 16 hex, `index` 0, `origin` `"cli"`, `phase` ending `sampling`, `subtasks` names in order, and `status_seq` starting `queued`); `a_silent_job_is_unresponsive_and_nothing_is_cancelled` (ARCH-072, with `LIL_TEST_STALL_SECS=2`: `test.job {silent_secs: 4}` gets `unresponsive: true` and `job:<id>` `{kind: "unresponsive", on: true}`, then succeeds); `all_cached_run_says_nothing_changed` (CORE-117: with every node cached, `all_cached: true` and the summary text of Appendix G);
  - `estimates.rs` (Task 7): `job_estimate_is_none_without_history` (CORE-161: `source: "none"`, numbers `null`); `job_estimate_uses_measured_history_after_three_runs` (three `test.job {estimate_key: "k"}` runs with `test.hw_tick` → `source: "measured_history"`, `samples: 3`, `wall_s_p50` a decimal string, `cost` `"0"`); `job_estimate_validates_args_like_a_call` (`params.invalid` for an unknown argument, `tool.unknown` for an unknown tool); `telemetry_energy_sums_a_range` (`telemetry.energy` over ticked samples equals the sum of `telemetry.series` `gpu.energy` points); `ui_connections_are_tracked_in_telemetry` (P4a-R27: `Snapshot.processes` holds the test app's pid with role `app` while connected, not after);
  - `comfy_profile.rs` (Task 8): `owned_comfyui_starts_with_the_launch_profile` (ARCH-126: the fake ComfyUI's `system_stats.system.argv` holds `--reserve-vram 1.5` and `--preview-method taesd` and no Manager flag; `engines.status` shows `launch_profile.flags` equal to them); `enable_assets_in_extra_args_stops_the_start` (`engine.not_configured` naming it); `out_of_memory_retries_twice_then_fails_with_advice` (CORE-163: `oom_next(3)` → three `POST /prompt`, then `comfyui.out_of_memory` with the hint of Appendix G); `models_report_names_orphan_duplicate_and_unknown_family` (CORE-164: a fixture root with a safetensors file carrying `modelspec.architecture`, a copy of it, a file with no metadata and a registry entry whose file is absent → those four states, nothing deleted); `engine_environment_reports_a_changed_node` (CORE-130: two captures that differ in one node → one `changes` row `{what: "node", change: "changed"}`);
  - `notifications.rs` (Task 10): `toasts_follow_categories_and_defaults` (APP-146: with `test.desktop {foreground: "other"}`, a finished batch posts one `job_finished` toast; `job_progress` is off by default; after `app/desktop.set {notifications: {job_finished: false}}` none is posted); `no_toast_while_a_suite_window_is_in_front` (P4a-R7); `quiet_while_fullscreen_posts_silently_and_never_drops` (`fullscreen: "d3d"` → `silent: true`, still posted); `progress_toast_updates_at_most_once_a_second_and_is_replaced_at_the_end` (`update` ops at least 1 s apart, then `remove` and a `job_finished` `post`); `with_the_notification_path_an_agent_call_waits_without_an_app` (ARCH-055, QA-016: no app connected, path ready → `test.destructive` from `cli` gets `pending_confirmation` and a `confirmation_waiting` toast with launch `lil-test-<id>:confirm/<id>`); `turning_off_confirmation_toasts_makes_agent_calls_unavailable` (`confirm.unavailable`); `toasts_off_in_windows_make_the_path_unreachable` (`test.desktop {toasts: "disabled_for_app"}` → `desktop.get` shows `toasts: "unavailable"` and `toasts_reason: "disabled_for_app"`, and a `cli` `test.destructive` with no app gets `confirm.unavailable` at once, with no toast); `non_interactive_callers_never_wait` (`lil --non-interactive call test.destructive` → exit 1, `confirm.unavailable`); `toast_text_is_escaped_and_normalised` (a message holding `<`, `&`, U+202E and U+0007 shows `&lt;`, `&amp;`, `⟨U+202E⟩` and no control character); `notify_claude_code_posts_agent_activity_and_drops_duplicates` (ARCH-103); `agent_open_posts_agent_activity` (APP-065: an `mcp` caller's `ui.show` posts an `agent_activity` toast whose launch is an `open/` URL);
  - `desktop.rs` (Task 9): `desktop_settings_are_set_only_by_a_verified_app` (`app/desktop.set` from `cli` → `caller.not_allowed`; from `ui:test` it writes `suite.toml` and `desktop.get` reads it back); `autopause_is_off_by_default`; `fullscreen_autopause_pauses_and_resumes` (CORE-160, with `LIL_TEST_DESKTOP_POLL_MS=50`: after `app/desktop.set {fullscreen_autopause: true}` and `fullscreen: "d3d"`, `reasons` is `["fullscreen"]`; after `"no"` for two polls, the lane runs); `fullscreen_reason_never_clears_a_manual_pause`; `keep_awake_is_held_only_while_a_gpu_job_runs` (ARCH-127: `power_held` true while a gated `gpu-heavy` job runs, false after; false while only `cpu` jobs run; false with `keep_awake_gpu: false`);
  - `interchange_4a.rs` (Task 11): `open_in_launch_args_carry_base64url_targets` (APP-020, N6: `launch_command.args` ends `["--intent", "open", "lil-test-<id>:open/<b64>"]` and the base64url decodes to the target); `lilref_write_makes_a_file_that_parses_back` (APP-021); `lilref_write_refuses_a_candidate_ref` (`ref.invalid`); `app_set_context_sets_the_session_project` (ARCH-088); `projects_and_workspaces_are_listed` (APP-006); `mcp_launches_a_stopped_app_without_focus` (ARCH-015: under the fake `claude`, `ui.show` → `lil-fake-image` recorded `--no-activate` before the URL; the result text has "Started image in the background.");
  - `diagnostics.rs` (Task 12): `diagnostics_bundle_redacts_secrets_labels_and_emails` (ARCH-121, QA-084: a `suite.toml` holding a `pass_env`-style `API_KEY`, a Claude Code profile label and an e-mail address → the zip's `suite.toml` and logs hold none of them, `redactions` ≥ 3, `manifest.json` `bundle` is 1, no workspace path is in `files`); `diagnostics_recent_groups_errors_by_code` (QA-083); `every_process_logs_json_lines` (ARCH-117: after a `lil status`, `<local>\logs\lil.<date>.log` holds a JSON line);
  - `doctor_4a.rs` (Task 13): `doctor_checks_the_aumid_shortcut_and_repairs_after_yes` (APP-022: `problem:` naming the shortcut; `n` changes nothing; `y` → the test root's `Programs` holds `LILITH (Test-<id>).lnk` and a second run prints `ok:`); `doctor_registers_the_handlers_after_yes` (QA-060, under the test classes root); `doctor_reports_engine_changes_and_the_asset_catalogue` (CORE-130, ARCH-065); `confirm_window_starts_the_app_in_confirm_mode` (APP-049: `lil confirm-window cf_0123456789abcdef` with `lil-fake-image` → recorded args `["--confirm", "cf_0123456789abcdef"]`, exit 0; `--non-interactive` → exit 2);
  - `lil_url.rs` (Task 4): `every_form_round_trips` (APP-020: `confirm`, `task`, `open` and a raw ref each parse to their variant, and `open_url` round-trips 1,000 generated targets); `refuses_everything_else` (another channel's verb, an unknown verb, a bad id, padding `=`, a decoded control character, `%00`, a 8,193-character argument → refused, with `shown` at most 80 characters); `raw_refs_are_percent_decoded_once` (`%2520` stays `%20` after one decode and is refused as a ref).
- [ ] **Step 2: Run them.** Run: `cargo run -q -p lil-cli -- dev spec-tests`. Expected: exit 0, with every new test listed as expected-fail and no other change.
- [ ] **Step 3: Gate** (`-p lil-parse -p lil-spec-tests`; `lil-parse` compiles with its `todo!()` bodies). Commit.

```bash
git commit -m "spec tests: plan 4a queue, estimates, launch profile, notifications, desktop, interchange, diagnostics, doctor and lil: URLs (expected to fail)"
```

---

### Task 4: `lil-parse`: the `lil:` URL grammar, the shown-text normaliser and the model-header readers (APP-020, QA-050, CORE-164; P4a-R3, P4a-R8, P4a-R28) — Qt-free, parallel

**Depends on:** Task 3 merged.

**Files:**
- Modify (fill the Task 3 stubs): `crates/lil-parse/src/{lib.rs, lil_url.rs, text.rs, model_header.rs}`.
- Test: `crates/lil-parse/tests/{lil_url.rs, text.rs, model_header.rs}` (with `proptest-regressions/` committed when it appears).
- Modify: `crates/lil-testkit/tests/dependency_boundaries.rs` (`lil-parse` joins the crates with no Qt or Windows dependency); `crates/lil-spec-tests/expected-fail.toml` (remove the `lil_url.rs` entries).

**Interfaces:**
- Produces:

```rust
pub enum LilUrl { Confirm(String), Task(String), Open(String /* decoded target */), Ref(String /* after one percent-decode */) }
pub struct UrlRefusal { pub shown: String /* shown_text(arg, 80) */, pub reason: &'static str }
pub fn strip_argument(raw: &str) -> &str;                       // surrounding spaces, one pair of quotes
pub fn parse_lil_url(arg: &str, channel_scheme: &str) -> Result<LilUrl, UrlRefusal>;
pub fn open_url(channel_scheme: &str, target: &str) -> String;  // "<scheme>:open/<base64url>"
pub fn confirm_url(channel_scheme: &str, id: &str) -> String;
pub fn task_url(channel_scheme: &str, job_id: &str) -> String;
pub fn shown_text(s: &str, max_chars: usize) -> String;         // P4a-R8
pub enum Family { Known(String), Unknown }
pub enum HeaderError { Io(std::io::Error), Malformed(&'static str) }
pub fn safetensors_family(r: &mut dyn std::io::Read) -> Result<Family, HeaderError>;
pub fn gguf_family(r: &mut dyn std::io::Read) -> Result<Family, HeaderError>;
```

- [ ] **Step 1: Write the failing tests:**
  - `lil_url.rs`: `confirm_task_open_and_ref_parse` (each form of Appendix G to its variant, scheme in any case); `other_channel_verbs_are_refused` (`lil:confirm/…` under `lil-dev` → refused; `lil://…` under `lil-dev` → `Ref`); `bad_ids_and_padding_are_refused` (`cf_` + 15 hex, upper-case hex, `open/` with `=`, with `?x`, with `#y`); `decoded_targets_hold_no_control_characters`; `one_trailing_slash_is_dropped_two_are_refused`; `raw_refs_are_percent_decoded_once` (`%20` → space, then the ref grammar decides; `%2520` stays `%20` and is refused; `%00`, `%7F` and invalid UTF-8 refused); `long_arguments_are_refused` (8,193 characters; `shown` at most 80 characters); `strip_argument_removes_one_pair_of_quotes` (`"\"x\""` → `x`, `"\"\"x\"\""` → `"x"`); property tests `open_url_round_trips` (any UTF-8 target of 1–4,096 bytes with no control character) and `any_argument_is_classified_or_refused_without_panic` (any string up to 10,000 characters), with the `[proptest] cases` of `test-params.toml`;
  - `text.rs`: `controls_are_removed` (U+0000–U+001F except none kept, U+007F–U+009F); `bidi_and_zero_width_are_shown` (each listed code point becomes `⟨U+XXXX⟩`); `emoji_sequences_and_rtl_text_survive` (a ZWJ family emoji and Hebrew text unchanged: a U+200D between two `Extended_Pictographic` code points is kept); `other_joiners_are_shown` (a U+200D after a letter and a U+200C in Persian text become `⟨U+200D⟩` and `⟨U+200C⟩`); `cut_at_max_with_an_ellipsis` (201 `char`s → 200, the last `…`, counted over the whole text, markers included);
  - `model_header.rs`: `safetensors_reads_modelspec_architecture` then `ss_base_model_version`; `safetensors_without_metadata_is_unknown`; `safetensors_header_over_100_mib_is_unknown_without_reading_it` (an 8-byte prefix announcing 100 MiB + 1 and a reader that fails after 8 bytes → `Unknown`, no read past 8); `gguf_reads_general_architecture` (versions 2 and 3); `gguf_with_more_than_1024_keys_first_is_unknown`; `gguf_string_over_64_kib_is_unknown`; property test `truncated_or_random_headers_never_panic`.
- [ ] **Step 2: Run them.** Run: `cargo nextest run -p lil-parse --retries 0`. Expected: FAIL (`todo!`).
- [ ] **Step 3: Implement.** `Extended_Pictographic` comes from a range table in `text.rs`, generated once from Unicode 16.0's `emoji-data.txt` and committed with its source line (no new dependency); `base64`'s URL-safe no-pad engine; the parser never allocates more than the input's length plus 4 KiB; the GGUF reader skips values by their declared type and length, with every length checked before it is read.
- [ ] **Step 4: Run them, then the gate** (`-p lil-parse -p lil-testkit -E 'binary(dependency_boundaries)'`). Remove the `lil_url.rs` entries. Expected: PASS, flake guard clean.
- [ ] **Step 5: Commit.**

```bash
git commit -m "lil-parse: lil: URL grammar with base64url targets, shown-text normaliser, safetensors and GGUF family readers"
```

---

### Task 5: Platform: toasts, the AUMID shortcut, handler registration, fullscreen state, the power request, foreground, launches and logs (APP-013, APP-014, APP-022, ARCH-015, ARCH-095, ARCH-117, ARCH-127, CORE-160, QA-038; P4a-R2, P4a-R20, P4a-R26, P4a-R29) — Qt-free, parallel

**Depends on:** Tasks 1 and 3 merged.

**Files:**
- Create: `crates/lil-platform/src/{desktop.rs, toast.rs, shortcut.rs, classes.rs, power.rs, logs.rs, desktop_fake.rs}`.
- Modify: `crates/lil-platform/src/{env.rs, process.rs, session_end.rs, guard.rs, lib.rs}`, `crates/lil-platform/Cargo.toml` (Task 1's `windows` features, each with a comment naming its use).
- Test: `crates/lil-platform/tests/{desktop.rs, logs.rs, env_names.rs}`.

**Interfaces:**
- Consumes: Task 1's feature list and power-request API.
- Produces:

```rust
// lil_platform::env::Env
pub fn activation_scheme(&self) -> String;            // "lil" | "lil-dev" | "lil-test-<id>"
pub fn classes_root(&self) -> String;                 // Appendix G
pub fn start_menu_shortcut(&self) -> PathBuf;         // <Programs>\LILITH[ (<Channel>)].lnk
pub fn app_state_dir(&self, app: &str) -> PathBuf;    // local_dir\apps\<app>
pub fn app_pipe_name(&self, app: &str, confirm: bool) -> String;
pub fn diagnostics_dir(&self) -> PathBuf;             // local_dir\diagnostics
pub fn lilref_dir(&self) -> PathBuf;                  // local_dir\cache\lilref
// lil_platform::desktop
pub enum Fullscreen { No, D3d, Busy, Presentation, Unknown }
pub struct Toast { pub tag: String, pub group: String, pub xml: String, pub silent: bool }
pub struct ToastProgress { pub value_permille: u16, pub value_string: String, pub status: String }
pub enum UpdateResult { Succeeded, NotFound, Failed }
pub enum ToastsState { Ready, Unavailable(&'static str /* disabled_for_app | disabled_for_user | disabled_by_policy | elevated | no_platform */) }
pub enum ShortcutState { Ok, Missing, WrongTarget, WrongAumid }
pub enum HandlerState { Ok, Missing, OtherChannel(PathBuf) }
pub trait Desktop: Send + Sync {
    fn fullscreen(&self) -> Fullscreen;
    fn foreground_pid(&self) -> Option<u32>;
    fn toasts_state(&self) -> ToastsState;    // Ready only when Setting() == Enabled and not elevated
    fn post(&self, t: &Toast) -> Result<(), String>;
    fn update(&self, tag: &str, group: &str, p: &ToastProgress) -> UpdateResult;
    fn remove(&self, tag: &str, group: &str);
    fn set_power_request(&self, held: bool) -> std::io::Result<()>;
    fn power_request_available(&self) -> bool;
    fn shortcut_state(&self, env: &Env, exe: &Path) -> ShortcutState;
    fn handler_state(&self, env: &Env, exe: &Path) -> HandlerState;
}
pub fn real_desktop(env: &Env) -> Arc<dyn Desktop>;   // toasts through CreateToastNotifierWithId(env.notification_aumid())
pub fn write_shortcut(env: &Env, exe: &Path) -> std::io::Result<()>;
pub fn register_handlers(env: &Env, exe: &Path) -> std::io::Result<()>;
pub fn allow_set_foreground(pid: u32) -> bool;
pub fn raw_command_line() -> String;
pub fn split_first_argument(cmdline: &str) -> (String /* exe */, String /* the rest, untouched */);
// lil_platform::process
pub fn spawn_no_activate(exe: &Path, args: &[String]) -> std::io::Result<u32>;   // detached, SW_SHOWNOACTIVATE
// lil_platform::session_end: the hidden window also reports PBT_APMSUSPEND and PBT_APMRESUMEAUTOMATIC
pub enum PowerEvent { Suspending, Resumed }
// lil_platform::logs (P4a-R20)
pub struct LogGuard;
pub fn init(env: &Env, process: &str) -> std::io::Result<LogGuard>;
pub fn init_at(dir: &Path, process: &str, cap_bytes: u64) -> std::io::Result<LogGuard>;  // tests
// lil_platform::guard (P4a-R29)
pub fn require_desktop_group();                       // panics unless CI=true, LIL_SANDBOX=1 or LIL_OWNER_DESKTOP=1 (P4a-R29)
// lil_platform::desktop_fake
pub struct FakeDesktop; // impl Desktop; set_fullscreen, set_foreground, set_shortcut, set_handlers, set_toasts_state; records() -> Vec<ToastRecord>; power_held() -> bool; polls() -> u32
```

- [ ] **Step 1: Write the failing tests:**
  - `env_names.rs`: `names_per_channel` (stable `lil`, `HKCU\Software\Classes`, `<Programs>\LILITH.lnk`; dev `lil-dev`, `LILITH (Dev).lnk`; test `lil-test-abc`, `HKCU\Software\LILITH-test-abc\Classes`, a shortcut under the test root; `app_pipe_name("image", true)` = `\\.\pipe\lilith-app-image-confirm-<sid>-<channel>`); the existing QA-040 guard rejects a test channel whose shortcut or classes root resolves to the real ones;
  - `desktop.rs`: `split_first_argument_keeps_the_rest_untouched` (`"C:\x y\lil-image.exe" "lil-dev:confirm/cf_0123456789abcdef"` → the exe and `"lil-dev:confirm/cf_0123456789abcdef"` with its quotes; an unquoted exe; tabs); `fake_desktop_records_posts_updates_and_removes`; `require_desktop_group_panics_outside_ci` (with `CI`, `LIL_SANDBOX` and `LIL_OWNER_DESKTOP` unset in a child process); `toasts_state_maps_each_notifier_setting` (a fake `Setting()` for each value, and an elevated token, map to Appendix G's reasons);
  - desktop group (`#[ignore = "desktop"]`, each starting with `require_desktop_group()`): `desktop_toast_posts_under_the_shortcut_aumid`, `desktop_toasts_state_is_ready_on_this_pc`, `desktop_shortcut_and_handlers_round_trip_under_a_test_scheme` (write, read back `Ok`, remove), `desktop_power_request_sets_and_clears`;
  - `logs.rs`: `one_file_a_day_and_14_kept` (with an injected date); `a_day_stops_at_the_cap_with_one_capped_line` (`init_at` with a 4 KiB cap: the file ends with exactly one `log_capped` line and stays under 4 KiB plus that line); `panic_hook_logs_the_location` (a child process panics; its log holds `"panic"` and a `file:line`).
- [ ] **Step 2: Run them.** Run: `cargo nextest run -p lil-platform --retries 0 -E 'binary(env_names) | binary(desktop) | binary(logs)'`. Expected: FAIL.
- [ ] **Step 3: Implement**, with Task 1's recipes: the shortcut through `IShellLinkW` and `IPropertyStore` (`PKEY_AppUserModel_ID`); registration with `RegCreateKeyExW` under `classes_root`; `HandlerState::OtherChannel` when the command names another exe; the power request on a `PowerCreateRequest` object (or Task 1's fallback).
- [ ] **Step 4: Run them, then the gate** (`-p lil-platform`). The desktop group isn't run here (QA-042). Expected: PASS.
- [ ] **Step 5: Commit.**

```bash
git commit -m "platform: toasts, AUMID shortcut, per-user handlers, fullscreen state, power request, foreground and no-activate launches, shared logs"
```

---

### Task 6: Jobs: record fields, queue order, `job.move`, batch cancels and Pause GPU work (CORE-117, CORE-120, CORE-160, ARCH-072, APP-039, APP-040, APP-043; P4a-R5, P4a-R12, P4a-R13) — Qt-free, adversarial review — **protected: `crates/lil-spec-tests/tests/core_session.rs`**

**Depends on:** Task 3 merged.

**Files:**
- Create: `crates/lil-core/src/jobs/pause.rs`.
- Modify: `crates/lil-core/src/jobs/{mod.rs, queue.rs, runner.rs, record.rs}`; `crates/lil-core/src/engines/comfyui/{handler.rs, ws.rs}` (`execution_cached`, phases, `last_progress_at`, withdrawal); `crates/lil-core/src/tools/{job_tools.rs, test_tools.rs}`; `crates/lil-core/src/rpc.rs` (the two `app/queue.*` methods); `crates/lil-core/src/state/migrations.rs` (append `JOBS_V2`: suite migration 9, workspace migration 8); `crates/lil-schema/src/suite.rs` (`SUITE_STATE_SCHEMA = 9`, `WORKSPACE_STATE_SCHEMA = 8`); `crates/lil-core/src/store/failpoints.rs` (`jobs.after_withdraw`); `crates/lil-protocol/src/{session.rs, app.rs, codes.rs}` (1.2, `queue.v1`, `QueueParams`, `QueueState`, `job.not_queued`); `crates/lil-testkit/src/fake_comfy.rs` (`cached_nodes`); `crates/lil-core/tests/hello.rs`; protected `crates/lil-spec-tests/tests/core_session.rs` (`"1.2"`).
- Modify (shared, recompute when rebasing): `crates/lil-core/src/tools/tests.rs` (the registry pins), `crates/lil-core/tests/hello.rs` (its capability assertion becomes "the core's list ends with `queue.v1`", not a count).
- Test: `crates/lil-core/tests/{jobs.rs, comfy_client.rs, state.rs}` additions; `crates/lil-protocol` unit tests.

**Interfaces:**
- Produces:

```rust
pub enum PauseReason { Manual, Fullscreen }                     // wire "manual", "fullscreen"
pub enum StartedBy { App(String), ClaudeCode, Assistant, Cli, Core }  // JobSummary.origin; named apart from jobs::JobOrigin, the stored tool call job.retry uses
pub enum MoveTo { Next, Last }
pub enum CancelSelector { One(JobId), Ids(Vec<JobId>), Batch(String), AllMine }
impl Jobs {
    pub async fn pause(&self, lane: Lane, why: PauseReason) -> QueueState;
    pub async fn resume(&self, lane: Lane, why: PauseReason) -> QueueState;   // removes only `why`
    pub fn queue_state(&self, lane: Lane) -> QueueState;
    pub async fn move_job(&self, caller: &JobCaller, id: &JobId, to: MoveTo) -> Result<JobSummary, ErrorBody>;
    pub async fn cancel_selected(&self, caller: &JobCaller, sel: CancelSelector) -> Result<Vec<JobSummary>, ErrorBody>;
}
pub enum WithdrawOutcome { Withdrawn, AlreadyStarted, NotPending }
pub trait Withdraw: Send + Sync {                               // engines implements it for ComfyUI
    fn withdraw<'a>(&'a self, job: &'a JobId, external_id: &'a str) -> BoxFuture<'a, WithdrawOutcome>;
}
impl JobCtx {
    pub fn set_phase(&self, phase: &str);
    pub fn mark_cached(&self, nodes: &[String]);
    pub fn set_engine_device(&self, name: &str);
}
// JOBS_V2 adds to `jobs`: batch_id TEXT, batch_index INTEGER, order_key INTEGER, origin TEXT, status_seq TEXT,
//   engine_device TEXT, phase TEXT, subtasks TEXT, indeterminate INTEGER NOT NULL DEFAULT 0,
//   last_progress_ms INTEGER, cache_hit_nodes TEXT, all_cached INTEGER NOT NULL DEFAULT 0,
//   estimate TEXT, estimate_key TEXT; CREATE INDEX jobs_batch ON jobs(batch_id) WHERE batch_id IS NOT NULL.
// A row from before the migration reads with batch_id "b_<its job id's 16 hex>", index 0, order_key = seq,
//   and origin from its idempotency scope.
```

- [ ] **Step 1: Write the failing tests:**
  - `jobs.rs`: `move_next_and_last_stay_within_the_class` (CORE-160); `move_refuses_engine_running_and_finished_jobs` (`job.not_queued` for `queued_in_engine`, `running`, `succeeded`); `moved_jobs_never_count_as_overtaken_by_groups`; `cancel_selectors_are_exclusive` (`params.invalid` for none or two); `all_mine_is_scoped_by_caller_kind_and_app` (a `ui:test` job survives a `cli` `all_mine`); `cancel_by_batch_and_by_ids`; `pause_holds_every_class_and_caller_as_queue_paused`; `pause_never_interrupts_the_executing_prompt`; `resume_removes_only_its_reason` (paused with `manual` and `fullscreen`; `resume(manual)` leaves it paused with `["fullscreen"]`); `a_restart_ends_a_pause` (P4a-R5); `every_job_has_a_batch_and_an_index` (P4a-R13); `origin_follows_the_caller` (P4a-R12: `ui:test` → `app:test`, `cli` → `cli`, `mcp` → `claude_code`, a core-submitted inventory job → `core`); `status_seq_keeps_the_first_and_newest_63`; `unresponsive_flips_after_30_s_and_back` (`FakeClock`; `job:<id>` gets `on: true` then `on: false` after a `progress`); `old_rows_read_with_derived_batch_and_origin` (`state.rs`: a suite `state.db` at migration 8 holding a job row migrates to 9, and the row reads back with the derived values);
  - `comfy_client.rs`: `phase_follows_the_executing_node_class` (the Appendix G table, including `ingesting` after `execution_success`); `subtasks_and_indeterminate` (a loader node with no `progress` → `indeterminate: true`; a sampler with `progress` → `subtasks` last entry has `value` and `max`); `all_cached_run_says_nothing_changed`; **`withdraw_only_while_pending_and_repost_with_a_new_id_once`** (Review Focus 4: an outside prompt running, a suite prompt held pending; pause → one `/api/jobs/{id}/cancel`; resume → the core reads `/api/jobs/{old}` and `/history/{old}` first, then one `POST /prompt` with a new id; a prompt the fake starts between the `/queue` read and the cancel is left running, `AlreadyStarted`); **`crash_after_withdraw_never_posts_twice`** (fail point `jobs.after_withdraw`, core killed; after the restart the job is `interrupted`, ARCH-022, and the fake saw exactly one `POST /prompt` for it); `external_side_effects_jobs_are_never_withdrawn`;
  - `lil-protocol`: `protocol_is_1_2_and_offers_queue_v1` (`PROTOCOL.to_string() == "1.2"`, `CAPABILITIES` holds `queue.v1` after the five of 1.1, by content, so Tasks 9 and 11 can append; a 1.1 client's shared set lacks it); `job_not_queued_is_in_the_table`;
  - `dispatch.rs`: `queue_methods_need_a_verified_app_and_the_capability` (`cli` → `caller.not_allowed`; `ui:test` without `queue.v1` → `caller.not_allowed`; with it → `QueueState`).
- [ ] **Step 2: Run them.** Run: `cargo nextest run -p lil-core -p lil-protocol --retries 0 -E 'binary(jobs) | binary(comfy_client) | binary(state) | binary(dispatch) | binary(hello) | package(lil-protocol)'`. Expected: FAIL.
- [ ] **Step 3: Implement.** The queue's pick orders by class rank, then `order_key`; the lane's pause is checked in admission, before VRAM. The withdrawal reads `/queue`, then cancels; the job row changes only after the cancel's answer (fail point between). `test.job` gains `phases`, `batch` and `silent_secs`, and a test-channel core honours `LIL_TEST_STALL_SECS`. Change `hello_major_mismatch_is_refused`'s value to `"1.2"` (protected).
- [ ] **Step 4: Run them, then the gate** (`-p lil-core -p lil-protocol -p lil-schema -p lil-testkit`). Remove the `jobs_queue_4a.rs` entries. Expected: PASS, flake guard clean, `dev spec-tests` green with the edited `core_session.rs`.
- [ ] **Step 5: Commit.**

```bash
git commit -m "jobs: batches, origin, phases and stall flag; class order and job.move; batch cancels; Pause GPU work with safe withdrawal; protocol 1.2"
```

---

### Task 7: Telemetry and estimates: `job_stats`, `job.estimate`, `telemetry.energy`, GPU users and app tracking (CORE-161, ARCH-072, APP-127, APP-131, APP-140; P4a-R14, P4a-R27) — Qt-free, parallel

**Depends on:** Task 6 merged (the `estimate` and `estimate_key` columns).

**Files:**
- Create: `crates/lil-core/src/telemetry/stats.rs`, `crates/lil-core/src/jobs/estimate.rs`.
- Modify: `crates/lil-core/src/telemetry/{db.rs (USER_VERSION 2, the migration step, SCHEMA), mod.rs (gpu_users, energy over a range)}`, `crates/lil-core/src/tools/{telemetry_tools.rs (telemetry.energy), job_tools.rs (job.estimate), registry.rs (`Tool::estimate_key`)}`, `crates/lil-core/src/meter/mod.rs` (writes `job_stats`), `crates/lil-core/src/jobs/{mod.rs, scheduler.rs}` (the estimate at submit; the watchdog bound), `crates/lil-core/src/server/mod.rs` (where connections are accepted: track a verified `ui:*` pid with role `app` until it disconnects), `crates/lil-core/src/engines/comfyui/handler.rs` (`estimate_key` of `comfy.prompt`), `crates/lil-core/src/tools/test_tools.rs`.
- Modify (shared): `crates/lil-core/src/tools/tests.rs` (pins).
- Test: `crates/lil-core/tests/{telemetry.rs, jobs.rs, scheduler.rs, meter.rs}` additions.

**Interfaces:**
- Consumes (Task 6): the `estimate` and `estimate_key` columns.
- Produces:

```rust
// the registry's tool trait (crates/lil-core/src/tools/registry.rs) gains, with a default of None:
//     fn estimate_key(&self, args: &serde_json::Value) -> Option<String>
pub struct Estimator;
impl Estimator {
    pub async fn estimate(&self, kind: &str, key: Option<&str>) -> Estimate;     // P4a-R14
}
impl Telemetry {
    pub async fn put_job_stats(&self, row: &JobStatsRow) -> Result<(), ErrorBody>;
    pub async fn history(&self, kind: &str, key: &str, newest: u32) -> Result<Vec<JobStatsRow>, ErrorBody>;
    pub async fn energy(&self, from: i64, to: i64) -> Result<EnergyTotals, ErrorBody>;
}
pub fn comfy_estimate_key(graph: &serde_json::Value) -> String;                  // Appendix G
```

- [ ] **Step 1: Write the failing tests:**
  - `telemetry.rs`: `version_1_migrates_to_2_keeping_rows`; `job_stats_rows_round_trip`; `energy_over_a_range_matches_the_job_energy_formula` (ticked samples; `system_mj` equals `(gpu + cpu + rest) / psu` as `JobEnergy` computes); `energy_picks_the_finest_table_that_holds_the_range`; `gpu_users_lists_the_top_8_by_vram_tracked_or_not`; `a_ui_connection_is_tracked_as_app_until_it_closes` (P4a-R27);
  - `jobs.rs`: `estimate_is_none_below_three_samples`; `estimate_uses_nearest_rank_over_the_newest_20` (wall seconds 1–25 recorded → the newest 20 are 6–25, p50 15.0, p90 23.0); `estimate_key_ignores_seeds_and_prefixes` (two graphs differing only in `seed`, `noise_seed` and `filename_prefix` → one key; a changed `steps` → another); `estimate_is_taken_at_submit` (`JobSummary.estimate` equals `job.estimate` at the time); `job_estimate_validates_args_like_a_call`; `local_cost_is_zero_with_energy_cost_when_a_rate_is_set`;
  - `meter.rs`: `job_stats_written_only_for_succeeded_jobs_with_a_key`;
  - `scheduler.rs`: `watchdog_uses_the_history_bound` (estimate p50 10 s, p90 30 s → `slow` at 60 s, not at 30 s; with `source: none` and `expected_secs` 20 → at 60 s).
- [ ] **Step 2: Run them.** Run: `cargo nextest run -p lil-core --retries 0 -E 'binary(telemetry) | binary(jobs) | binary(meter) | binary(scheduler)'`. Expected: FAIL.
- [ ] **Step 3: Implement** with the Appendix G shapes; decimal strings through the existing `Money` and `JobEnergy` formatting.
- [ ] **Step 4: Run them, then the gate** (`-p lil-core`). Remove the `estimates.rs` entries. Expected: PASS.
- [ ] **Step 5: Commit.**

```bash
git commit -m "telemetry: job_stats and job.estimate from measured history; telemetry.energy; GPU users; apps tracked while connected"
```

---

### Task 8: ComfyUI: the launch profile, the out-of-memory advice, the model inventory report and stored environment captures (ARCH-065, ARCH-126, CORE-130, CORE-163, CORE-164; P4a-R15, P4a-R28) — Qt-free, parallel

**Depends on:** Tasks 4, 6 and 7 merged (Tasks 7, 8 and 9 share files and merge in that order).

**Files:**
- Modify: `crates/lil-core/src/engines/{config.rs, tools.rs}`, `crates/lil-core/src/engines/comfyui/{supervisor.rs, handler.rs, inventory.rs, environment.rs, client.rs (the catalogue probe)}`, `crates/lil-core/src/state/migrations.rs` (append `ENGINE_RECORDS`: suite migration 10), `crates/lil-schema/src/suite.rs` (`SUITE_STATE_SCHEMA = 10`; the `[engines.comfyui]` keys), `crates/lil-core/Cargo.toml` (`lil-parse`), `crates/lil-testkit/src/fake_comfy.rs` (`oom_next`, the catalogue answer).
- Modify (shared): `crates/lil-core/src/tools/tests.rs` (pins), `crates/lil-core/src/engines/comfyui/handler.rs` (also Task 7's: merge after it), `crates/lil-core/src/tools/test_tools.rs`.
- Test: `crates/lil-core/tests/{engines.rs, comfy_observe.rs, comfy_client.rs, state.rs}` additions.

**Interfaces:**
- Consumes: `lil_parse::{safetensors_family, gguf_family}` (Task 4); Task 2's Manager flag, opt-out flag and catalogue probe.
- Produces:

```rust
pub struct LaunchProfile { pub reserve_vram_gb: String, pub preview_method: String, pub manager: bool, pub dynamic_vram: bool }
impl LaunchProfile { pub fn flags(&self) -> Vec<String>; }                       // Appendix G order
pub fn refuse_enable_assets(extra_args: &[String]) -> Result<(), ErrorBody>;    // engine.not_configured
pub enum Catalogue { On, Off, Unknown }
pub async fn models_report(engines: &Engines) -> Result<ModelsReport, ErrorBody>;
pub async fn environment(engines: &Engines) -> Result<EnvironmentView, ErrorBody>;
// ENGINE_RECORDS: ALTER TABLE model_hashes ADD COLUMN family TEXT;
//   ALTER TABLE model_hashes ADD COLUMN family_read INTEGER NOT NULL DEFAULT 0;
//   CREATE TABLE engine_captures (engine TEXT NOT NULL, captured_utc INTEGER NOT NULL, capture TEXT NOT NULL,
//     PRIMARY KEY (engine, captured_utc));
```

- [ ] **Step 1: Write the failing tests:**
  - `engines.rs`: `owned_start_flags_follow_the_profile` (the spawned argument list is `main.py --listen 127.0.0.1 --port <p> --reserve-vram 1.5 --preview-method taesd` then `extra_args`); `manager_flag_only_when_on`; `dynamic_vram_opt_out_only_when_off`; `enable_assets_in_extra_args_is_refused` (`--enable-assets`, an abbreviation ComfyUI accepts, and `--enable-assets=1`); `engines_status_shows_the_launch_profile_and_the_catalogue` (the fake answering 200, 404 and 500 → `on`, `off`, `unknown`; an attached engine's `launch_profile` is `null`);
  - `comfy_client.rs`: `out_of_memory_fails_with_advice_after_two_retries` (`oom_next(3)`: three posts, then `comfyui.out_of_memory` with Appendix G's message and hint, `retry: after_resolve`);
  - `comfy_observe.rs`: `models_report_states` (Task 3's fixture shape: orphan, duplicate, unknown family, and a `missing` registry entry; no file changed); `families_are_cached_by_identity_and_read_once` (a counting probe opens each header once over two reports); `pickle_formats_are_never_opened` (`.ckpt`, `.pt`, `.pth`, `.bin` opened 0 times); `captures_keep_the_newest_10`; `environment_lists_changes_between_two_captures` (node changed, node added, ComfyUI revision changed, driver changed);
  - `state.rs`: `migration_10_adds_family_and_captures`.
- [ ] **Step 2: Run them.** Run: `cargo nextest run -p lil-core --retries 0 -E 'binary(engines) | binary(comfy_observe) | binary(comfy_client) | binary(state)'`. Expected: FAIL.
- [ ] **Step 3: Implement.** The inventory refresh reads a family only for a file whose cached identity changed or whose `family_read` is 0; family text is kept as untrusted.
- [ ] **Step 4: Run them, then the gate** (`-p lil-core -p lil-schema -p lil-testkit`). Remove the `comfy_profile.rs` entries. Expected: PASS.
- [ ] **Step 5: Commit.**

```bash
git commit -m "engines: ComfyUI launch profile, out-of-memory advice, model inventory report with header families, stored environment captures"
```

---

### Task 9: Desktop state: the fullscreen poll, auto-pause, keep-awake and desktop settings (ARCH-127, CORE-160, APP-039, APP-146; P4a-R5, P4a-R6, P4a-R24; owner question Q1; default shown) — Qt-free, adversarial review

**Depends on:** Tasks 5, 6 and 8 merged (after Task 8, for `crates/lil-schema/src/suite.rs`).

**Files:**
- Create: `crates/lil-core/src/desktop.rs` (infrastructure, P4a-R24).
- Modify: `crates/lil-core/src/server/mod.rs` (`real_desktop` on stable and dev, `FakeDesktop` on test channels; `LIL_TEST_DESKTOP_POLL_MS`), `crates/lil-core/src/rpc.rs` (`app/desktop.set`), `crates/lil-core/src/tools/{mod.rs, test_tools.rs}` (`desktop.get`, `test.desktop`), `crates/lil-core/src/config.rs`, `crates/lil-schema/src/{suite.rs, suite_edit.rs}` (`[notifications]`, `[jobs] fullscreen_autopause`, `[power] keep_awake_gpu`, targeted edits), `crates/lil-protocol/src/{session.rs, app.rs}` (`desktop.v1`, `DesktopSetParams`, `DesktopSettings`), `crates/lil-core/tests/module_deps.rs`.
- Modify (shared): `crates/lil-core/src/tools/tests.rs` (pins), `crates/lil-core/tests/hello.rs` (its list ends with `desktop.v1`).
- Test: `crates/lil-core/tests/desktop.rs` (new), `crates/lil-core/tests/dispatch.rs` additions.

**Interfaces:**
- Consumes: `lil_platform::desktop::{Desktop, Fullscreen}`, `session_end::PowerEvent` (Task 5); `Jobs::{pause, resume}` (Task 6).
- Produces:

```rust
pub struct DesktopWatch;
impl DesktopWatch {
    pub fn start(desktop: Arc<dyn Desktop>, clock: Arc<dyn Clock>, every: Duration, events: Arc<EventBus>) -> Arc<DesktopWatch>;
    pub fn fullscreen(&self) -> watch::Receiver<bool>;
    pub fn foreground_is_one_of(&self, pids: &[u32]) -> bool;
}
pub struct DesktopSettingsStore;   // reads suite.toml; app/desktop.set writes it; publishes {kind: "settings"}
impl DesktopSettingsStore { pub fn current(&self) -> watch::Receiver<DesktopSettings>; pub async fn set(&self, p: DesktopSetParams) -> Result<DesktopSettings, ErrorBody>; }
pub struct AutoPause;   pub fn start_autopause(w: &Arc<DesktopWatch>, jobs: Arc<Jobs>, s: watch::Receiver<DesktopSettings>) -> Arc<AutoPause>;
pub struct KeepAwake;   pub fn start_keep_awake(d: Arc<dyn Desktop>, jobs: Arc<Jobs>, s: watch::Receiver<DesktopSettings>, events: Arc<EventBus>) -> Arc<KeepAwake>;
```

- [ ] **Step 1: Write the failing tests** (`desktop.rs`, with `FakeDesktop` and `FakeClock`):
  - `the_poll_runs_every_2_s` (P4a-R6); `leaving_fullscreen_counts_after_two_polls`;
  - `autopause_is_off_by_default`; `autopause_adds_and_removes_the_fullscreen_reason`; **`fullscreen_reason_never_clears_a_manual_pause`** (Review Focus 4); `autopause_never_interrupts_the_executing_prompt`;
  - `keep_awake_is_held_only_while_a_gpu_heavy_job_runs` (held at a gated `gpu-heavy` job's start, released at its end; never for `cpu` jobs); `keep_awake_off_releases_at_once`; `keep_awake_unavailable_is_reported` (`keep_awake_available: false` in `desktop.get`); `a_sleep_gap_is_logged_and_published` (a fake `Suspending` then `Resumed` → one `slept` log line and one `{kind: "resumed"}` event);
  - `desktop_get_reports_the_notification_path` (`toasts`, `shortcut`, `handlers` from `FakeDesktop`);
  - `dispatch.rs`: `desktop_set_needs_a_verified_app_and_the_capability`; `desktop_set_edits_suite_toml_keeping_comments` (a commented `suite.toml` keeps its comments and other keys; `desktop.get` reads the new values).
- [ ] **Step 2: Run them.** Run: `cargo nextest run -p lil-core -p lil-schema --retries 0 -E 'binary(desktop) | binary(dispatch) | binary(module_deps) | package(lil-schema)'`. Expected: FAIL.
- [ ] **Step 3: Implement.** One poll task; fullscreen state on a `watch` channel; the power request is set only on a change of "should hold".
- [ ] **Step 4: Run them, then the gate** (`-p lil-core -p lil-schema -p lil-protocol`). Remove the `desktop.rs` spec-test entries. Expected: PASS.
- [ ] **Step 5: Commit.**

```bash
git commit -m "desktop: 2 s fullscreen poll, opt-in auto-pause that never undoes your pause, keep-awake while GPU jobs run, app/desktop.set"
```

---

### Task 10: Notifications: the notifier, toasts and the notification path for confirmations (ARCH-005, ARCH-055, CORE-113, APP-022, APP-051, APP-146, QA-016; P4a-R7, P4a-R8, P4a-R9) — Qt-free, adversarial review

**Depends on:** Tasks 4, 7 and 9 merged.

**Files:**
- Create: `crates/lil-core/src/interchange/{notify.rs, toast_xml.rs}`.
- Modify: `crates/lil-core/src/interchange/{mod.rs, tools.rs}` (`notify.claude_code`), `crates/lil-core/src/confirm/mod.rs` (`Notifier` as the `NotificationPath`; `UiPresence` keeps the pids), `crates/lil-core/src/server/mod.rs` (wiring), `crates/lil-core/Cargo.toml` (`lil-parse`, if Task 8 hasn't added it).
- Test: `crates/lil-core/tests/notify.rs` (new), `crates/lil-core/tests/confirm.rs` additions.

**Interfaces:**
- Consumes: `DesktopWatch`, `DesktopSettingsStore` (Task 9); `lil_parse::{shown_text, confirm_url, task_url, open_url}` (Task 4); `JobSummary.estimate` (Task 7).
- Produces:

```rust
pub struct ToastText { pub title: String, pub body: String, pub launch: String, pub review_button: bool, pub progress: Option<ToastProgress> }  // Appendix G's progress binding
pub fn toast_xml(t: &ToastText) -> String;                                       // Appendix G
pub enum NotifyOutcome { Posted, CategoryOff, Foreground, Duplicate, Unavailable }
pub struct Notifier;
impl Notifier {
    pub fn start(desktop: Arc<dyn Desktop>, watch: Arc<DesktopWatch>, settings: watch::Receiver<DesktopSettings>,
                 events: Arc<EventBus>, presence: Arc<UiPresence>, env: Env, clock: Arc<dyn Clock>) -> Arc<Notifier>;
    pub async fn claude_code(&self, message: &str, title: Option<&str>) -> NotifyOutcome;
    pub async fn agent_opened(&self, target_title: &str, target: &str) -> NotifyOutcome;
}
impl confirm::NotificationPath for Notifier { fn reachable(&self) -> bool; }    // Appendix G "Reachable"
```

- [ ] **Step 1: Write the failing tests:**
  - `notify.rs`: `toast_xml_escapes_and_normalises` (`<`, `>`, `&`, `"` escaped; U+202E shown; the XML parses); `progress_toast_xml_carries_the_binding_and_initial_data` (the `<progress>` element of Appendix G, and the first post's `NotificationData` with `SequenceNumber` 1); `categories_and_their_defaults`; `one_toast_per_batch_when_its_last_job_ends` (a batch of 3 → one "Batch finished (3 jobs)"; one failed → "Job failed"); `cancelled_and_core_jobs_never_toast`; `a_suite_window_in_front_suppresses_and_nothing_is_queued` (P4a-R7: moving the foreground away later posts nothing old); `quiet_while_fullscreen_sets_silent_and_drops_nothing`; `progress_toast_threshold_and_rate` (`wall_s_p50` 31 → posted, 29 → not; updates at least 1 s apart under 10 Hz progress); `progress_toast_is_replaced_at_the_end` (`remove`, then the finished toast); **`confirmation_toast_is_removed_when_answered_or_expired`** (Review Focus 3); `cap_alert_posts_once_per_threshold`; `claude_code_duplicates_within_10_s_are_dropped`; `agent_open_posts_agent_activity_with_an_open_url`;
  - `confirm.rs`: `reachable_needs_toasts_shortcut_handlers_and_the_category` (each part missing → `false`); **`reachable_is_false_when_windows_turns_the_apps_toasts_off`** (Review Focus 6: each `ToastsState::Unavailable` reason → `false`, and `desktop.get` names it); **`an_elevated_core_is_never_reachable`**; `an_unreachable_path_answers_unavailable_at_once_without_an_app` (no 30-minute wait: the call returns `confirm.unavailable`); `a_call_waits_without_an_app_when_reachable` (ARCH-055: `cli` `test.destructive`, no app → `pending_confirmation` and a `confirmation_waiting` toast whose launch is `<scheme>:confirm/<id>`; an app connected later answers it and the retry runs once); `non_interactive_callers_still_get_unavailable` (QA-016); `turning_the_category_off_makes_calls_unavailable_without_an_app`.
- [ ] **Step 2: Run them.** Run: `cargo nextest run -p lil-core --retries 0 -E 'binary(notify) | binary(confirm)'`. Expected: FAIL.
- [ ] **Step 3: Implement.** The notifier subscribes in-process to `jobs`, `confirmations` and `caps`; it keeps the tags it posted, so it can remove them; reachability is cached for 10 s.
- [ ] **Step 4: Run them, then the gate** (`-p lil-core`). Remove the `notifications.rs` entries. Expected: PASS.
- [ ] **Step 5: Commit.**

```bash
git commit -m "notifications: toasts by category with quiet fullscreen, progress toasts, and the notification path so confirmations wait with no app open"
```

---

### Task 11: Interchange and sessions: base64url launch arguments, `.lilref` links, listings, `app/set_context`, `lil-mcp` launches and the Notification hook (ARCH-015, ARCH-088, ARCH-103, APP-020, APP-021, APP-043, APP-065, AI-116; P4a-R18) — Qt-free, adversarial review — **protected: `crates/lil-spec-tests/tests/plugin_hooks.rs`, `crates/lil-spec-tests/tests/doctor_claude.rs`**

**Depends on:** Tasks 4, 5 and 10 merged.

**Files:**
- Create: `crates/lil-core/src/interchange/lilref.rs`, `crates/lil-mcp/src/launch.rs` (a module of the `lil-mcp` binary; it stays binary-only, and its tests drive the binary).
- Modify: `crates/lil-core/src/interchange/{mod.rs (launch args, N6), tools.rs (lilref.write)}`, `crates/lil-core/src/tools/{project_tools.rs (projects.list), workspace_tools.rs (workspaces.list)}`, `crates/lil-core/src/rpc.rs` and `crates/lil-core/src/session.rs` (`app/set_context`), `crates/lil-core/src/store/fsops.rs` (`write_suite_cache_file`), `crates/lil-protocol/src/{session.rs, app.rs}` (`context.v1`, `SetContextParams`), `crates/lil-mcp/src/main.rs`, `crates/lil-mcp/Cargo.toml` (`lil-platform` and `toml`, P4a-R30, and its dependency comment), `crates/lil-testkit/tests/dependency_boundaries.rs` (`lil-mcp`'s allowed list, P4a-R30), the other `1.3.0` references (`crates/lil-cli/tests/doctor_claude.rs`, `crates/lil-cli/tests/plugin_files.rs`, `crates/lil-testkit/tests/test_app.rs`), `crates/lil-core/tests/hello.rs` (its list ends with `context.v1`), `crates/lil-core/src/tools/tests.rs` (pins), `crates/lil-cli/src/user/{hook.rs, mod.rs, doctor.rs (PLUGIN_VERSION "1.3.1")}`, `plugin/worldwright/{hooks/hooks.json, .claude-plugin/plugin.json, skills/lilith/SKILL.md}` (the skill gains one line: to give the owner a clickable link to a sealed version, call `lilref_write` and print the path it returns, APP-021), `crates/lil-testkit/src/bin/lil-fake-image.rs` (new) and `crates/lil-testkit/src/bins.rs`, protected `crates/lil-spec-tests/tests/{plugin_hooks.rs, doctor_claude.rs}`.
- Test: `crates/lil-core/tests/interchange.rs`, `crates/lil-core/tests/projects.rs`, `crates/lil-core/tests/workspace_tools.rs`, `crates/lil-mcp/tests/launch.rs` (new), `crates/lil-cli/tests/{hook_notification.rs (new), plugin_files.rs}`.

**Interfaces:**
- Consumes: `lil_parse::open_url` (Task 4), `lil_platform::process::spawn_no_activate` (Task 5), `Notifier::{claude_code, agent_opened}` (Task 10).
- Produces:

```rust
pub fn launch_args(reg: &AppRegistration, target: &Target, intent: Intent, scheme: &str) -> Vec<String>;  // N6
pub fn write_lilref(env: &Env, r: &AssetRef, title: Option<&str>) -> Result<PathBuf, ErrorBody>;  // through fsops::write_suite_cache_file
// lil-mcp, inside the binary (src/launch.rs): after an agent's open_in or ui.show, start the launch_command
//   through lil_platform::process::spawn_no_activate when its exe is a registered suite app (P4a-R30)
// lil-cli: `lil hook notification`
```

- [ ] **Step 1: Write the failing tests:**
  - `interchange.rs`: `launch_args_are_intent_then_a_base64url_url` (decoding the last argument gives the target back, for a ref, a `doc:` id and a path with spaces and `#`); `lilref_write_path_and_contents` (the file parses with `parse_lilref` and its name is the ref's hash prefix); `lilref_write_refuses_unsealed_refs`; `lilref_write_stays_in_the_cache_folder`;
  - `projects.rs`: `projects_list_counts_assets`; `workspace_tools.rs`: `workspaces_list_marks_the_default_and_the_served_one` (and a missing folder `missing: true`);
  - `dispatch.rs`: `set_context_changes_the_session_project` (a later `project.context {}` names it); `set_context_unknown_project_is_project_unknown`; `set_context_needs_context_v1`;
  - `launch.rs` (`lil-mcp`, under the fake `claude`): `launches_a_stopped_registered_app_without_focus` (`lil-fake-image` records `["--intent", "open", "--no-activate", "<scheme>:open/<b64>"]`; the result text holds "Started image in the background."); `never_launches_an_unregistered_exe` (a `launch_command` naming another exe → no spawn); `never_launches_for_a_cli_caller` (`lil call ui.show` launches nothing);
  - `hook_notification.rs`: `notification_hook_calls_notify_without_starting_a_core` (with a core: one `agent_activity` record in `test.desktop`; without one: nothing, exit 0, no core started); `malformed_notification_input_prints_nothing`;
  - `plugin_files.rs`: `plugin_1_3_1_has_the_notification_hook`; protected spec tests: the event set becomes `["Notification", "PostToolUse", "PreToolUse", "SessionStart"]`, the version `"1.3.1"`, and the doctor's installed-version fixture `"1.3.1"` (a `1.3.0` install is updated in place after `y`).
- [ ] **Step 2: Run them.** Run: `cargo nextest run -p lil-core -p lil-mcp -p lil-cli --retries 0 -E 'binary(interchange) | binary(projects) | binary(workspace_tools) | binary(dispatch) | binary(launch) | binary(hook_notification) | binary(plugin_files)'`. Expected: FAIL.
- [ ] **Step 3: Implement.** `lil-mcp` checks the `exe` against the `apps\*.toml` beside the core it is connected to (the core's folder from `lil_status`) before spawning. The two protected spec tests change only in the values named.
- [ ] **Step 4: Run them, then the gate** (`-p lil-core -p lil-mcp -p lil-cli -p lil-protocol -p lil-testkit`). Remove the `interchange_4a.rs` entries. Expected: PASS, including the edited protected tests.
- [ ] **Step 5: Commit.**

```bash
git commit -m "interchange: base64url launch args, lilref.write, project and workspace lists, app/set_context, lil-mcp launches without focus, plugin 1.3.1 Notification hook"
```

---

### Task 12: Diagnostics core: the bundle, recent errors and logs for every process (ARCH-117–122, QA-082–085; P4a-R19, P4a-R20, P4a-R24) — Qt-free — **protected: `/Cargo.toml`**

**Depends on:** Tasks 5, 8 and 11 merged (Task 11 creates `fsops::write_suite_cache_file`, which the bundle uses, and widens `lil-mcp`'s dependencies, P4a-R30).

**Files:**
- Create: `crates/lil-core/src/diagnostics/{mod.rs, bundle.rs, redact.rs, recent.rs}`.
- Modify: `/Cargo.toml` (protected: `zip` in `[workspace.dependencies]`), `crates/lil-core/Cargo.toml`, `crates/lil-core/src/{main.rs (lil_platform::logs::init), tools/dispatcher.rs (each error outcome to `Diagnostics::record_error`), store/fsops.rs}`; `crates/lil-cli/src/user/mod.rs` (the logs for user commands; `main.rs` is protected and unchanged); `crates/lil-mcp/src/main.rs`; `crates/lil-core/tests/module_deps.rs`.
- Modify (shared): `crates/lil-core/src/tools/tests.rs` (pins).
- Test: `crates/lil-core/tests/diagnostics.rs` (new); `crates/lil-cli/tests/logs.rs` (new).

**Interfaces:**
- Consumes: `lil_platform::logs` (Task 5); `engines.environment` (Task 8).
- Produces:

```rust
pub struct Diagnostics;
impl Diagnostics {
    pub fn record_error(&self, code: &str, tool: Option<&str>, message: &str, at_ms: i64);   // ring of 1,000
    pub fn recent(&self, limit: u32) -> Vec<ErrorGroup>;
    pub async fn bundle(&self, ctx: &CallCtx, job: Option<&JobId>) -> Result<BundleResult, ErrorBody>;
}
pub struct Redactor { pub labels: Vec<String>, pub profile_dir: Option<String>, pub machine: Option<String> }
impl Redactor { pub fn text(&self, s: &str) -> (String, u32); pub fn toml(&self, doc: &str) -> (String, u32); }
```

- [ ] **Step 1: Write the failing tests:**
  - `diagnostics.rs`: `bundle_holds_the_listed_entries_and_no_workspace_file`; `redaction_covers_scrub_words_labels_emails_and_targets`; `profile_folder_and_machine_name_are_rewritten` (owner question Q4; default shown; skipped as "answered no" if the owner said so in Task 2); `bundle_writes_only_under_the_diagnostics_folder`; `bundle_with_a_job_adds_its_summary_and_untrusted_args`; `recent_groups_by_code_newest_first_over_the_last_1000`; `each_error_is_published_on_the_diagnostics_topic`; `nothing_is_sent_anywhere` (a bundle run with the test network guard makes no outbound connection, ARCH-122);
  - `logs.rs` (`lil-cli`): `lil_and_lil_mcp_write_json_logs` (after `lil status` and an `lil-mcp` session, both files exist and hold JSON lines).
- [ ] **Step 2: Run them.** Run: `cargo nextest run -p lil-core -p lil-cli --retries 0 -E 'binary(diagnostics) | binary(logs) | binary(module_deps)'`. Expected: FAIL.
- [ ] **Step 3: Implement.** The zip is written to a temporary name in the diagnostics folder, then renamed; logs are copied with shared read access and redacted line by line.
- [ ] **Step 4: Run them, then the gate** (`-p lil-core -p lil-cli -p lil-mcp`), plus `cargo deny check`. Remove the `diagnostics.rs` entries. Expected: PASS.
- [ ] **Step 5: Commit.**

```bash
git commit -m "diagnostics: redacted bundle zip, recent errors by code, one log setup with a size cap and panic hooks in every process"
```

---

### Task 13: `lil doctor` checks and `lil confirm-window` (APP-022, APP-049, ARCH-016, ARCH-065, CORE-130, CORE-164, QA-060; P4a-R4, P4a-R26) — Qt-free

**Depends on:** Tasks 5, 8, 9 and 11 merged.

**Files:**
- Create: `crates/lil-cli/src/user/confirm_window.rs`.
- Modify: `crates/lil-cli/src/user/{doctor.rs, mod.rs}`.
- Test: `crates/lil-cli/tests/{doctor_desktop.rs, confirm_window.rs}` (new).

**Interfaces:**
- Consumes: `desktop.get`, `engines.environment`, `engines.status`, `models.report` (Tasks 8, 9); `write_shortcut`, `register_handlers` (Task 5).
- Produces: the `lil doctor` lines and `lil confirm-window` of Appendix G.

- [ ] **Step 1: Write the failing tests:**
  - `doctor_desktop.rs`: `shortcut_problem_then_fixed_after_yes` (`n` changes nothing; `y` writes the test-root shortcut; a second run says `ok:`); `handlers_missing_then_registered_after_yes` (under the test classes root); `handlers_held_by_another_channel_are_a_warning_and_kept`; `non_interactive_never_writes`; `engine_changes_warn_with_the_rollback_hint`; `asset_catalogue_lines` (on, off, unknown); `models_report_counts_are_info`; `a_second_run_changes_nothing` (QA-060);
  - `confirm_window.rs`: `starts_the_app_with_confirm_and_an_id`; `without_an_id_lists_all` (`["--confirm"]`); `non_interactive_exits_2`; `no_app_beside_lil_exits_2`.
- [ ] **Step 2: Run them.** Run: `cargo nextest run -p lil-cli --retries 0 -E 'binary(doctor_desktop) | binary(confirm_window)'`. Expected: FAIL.
- [ ] **Step 3: Implement.** `lil doctor` writes the shortcut and the handlers itself, through `lil-platform`, only after a `y` from a terminal (P3-R28's rule for answers).
- [ ] **Step 4: Run them, then the gate** (`-p lil-cli`). Remove the `doctor_4a.rs` entries. Expected: PASS.
- [ ] **Step 5: Commit.**

```bash
git commit -m "doctor: AUMID shortcut, lil: and .lilref handlers, engine-environment changes, asset catalogue, model report; lil confirm-window"
```

---

### Task 14: `lil-client` session: reconnect, resubscribe and app calls (APP-024, APP-033–036, ARCH-100, QA-026) — Qt-free, parallel

**Depends on:** Task 3 merged.

**Files:**
- Create: `crates/lil-client/src/session.rs`.
- Modify: `crates/lil-client/src/{lib.rs, connect.rs (Client::app_call), blocking.rs (mark_gui_thread and the debug assert)}`.
- Test: `crates/lil-client/tests/session.rs` (new).

**Interfaces:**
- Produces:

```rust
pub enum SessionState { Connecting, Ready(Box<HelloResult>), Reconnecting { attempt: u32 }, Incompatible { core_protocol: String }, Unavailable { reason: String } }
pub struct Session;
impl Session {
    pub fn start(cfg: ClientConfig, topics: Vec<String>) -> Session;           // on the current tokio runtime
    pub fn state(&self) -> tokio::sync::watch::Receiver<SessionState>;
    pub async fn call_tool(&self, name: &str, args: serde_json::Map<String, Value>, key: Option<String>, trace_id: Option<String>) -> Result<Outcome<ToolOutput>, ClientError>;
    pub async fn app_call(&self, method: &str, params: Value) -> Result<Outcome<Value>, ClientError>;
    pub async fn subscribe(&self, topics: Vec<String>) -> Result<(), ClientError>;
    pub async fn unsubscribe(&self, topics: Vec<String>) -> Result<(), ClientError>;
    pub fn events(&self) -> tokio::sync::mpsc::Receiver<EventParams>;          // across reconnects, in seq order per topic
    pub fn last_seq(&self, topic: &str) -> Option<u64>;
    pub async fn set_workspace(&self, id: &str) -> Result<HelloResult, ClientError>;   // a new core/hello, P4a-R34
    pub async fn close(self);
}
impl Client { pub async fn app_call(&self, method: &str, params: Value) -> Result<Outcome<Value>, ClientError>; }
pub fn mark_gui_thread();                                                     // QA-026
```

- [ ] **Step 1: Write the failing tests:**
  - **`session_reconnects_and_resubscribes_from_the_last_seq`** (Review Focus 1: events 1–5, the core killed, a new core started; the state goes `Reconnecting` then `Ready`; each topic resumes after its last seq, with no gap and no duplicate);
  - **`session_gets_a_snapshot_when_the_history_is_gone`** (a cursor older than the 1,024-event history → a `snapshot` event);
  - **`in_flight_calls_fail_and_are_never_resent`** (a gated call when the core dies → `ClientError::Closed`; the new core saw no second call with that key);
  - `incompatible_core_reports_incompatible_and_is_left_running` (APP-036, ARCH-014; with `LIL_TEST_CLIENT_PROTOCOL`);
  - `unavailable_when_no_core_can_start`;
  - `backoff_doubles_from_250_ms_to_5_s` (paused tokio time; P4a-R33);
  - `set_workspace_rehellos_and_never_changes_the_default` (P4a-R34: after `set_workspace`, `suite.toml`'s default is byte-identical; another workspace than the served one answers `workspace.unsupported`);
  - `app_call_reaches_app_methods` (`app/confirmations.list` with a `ui:test`-trusted test client);
  - `blocking_calls_on_the_gui_thread_panic_in_debug` (QA-026).
- [ ] **Step 2: Run them.** Run: `cargo nextest run -p lil-client --retries 0 -E 'binary(session)'`. Expected: FAIL.
- [ ] **Step 3: Implement.** One task owns the connection and the merged event stream; reconnect re-hellos with the same caller kind and workspace, then resubscribes with `since_seq` from `last_seq`.
- [ ] **Step 4: Run them, then the gate** (`-p lil-client`). Expected: PASS, flake guard clean (these start and kill real cores, so run the guard).
- [ ] **Step 5: Commit.**

```bash
git commit -m "client: reconnecting session with resubscribe from the last seq, snapshots, app calls, and the GUI-thread guard"
```

---

### Task 15: Build: CMake, cxx-qt, KDDockWidgets, the shim, the `lil-qt` skeleton, the deploy folder and Qt in CI (ARCH-114, APP-001, APP-005, APP-030, APP-137, OPS-094, QA-068, QA-077, QA-078; P4a-R1) — needs Qt — **protected: `/Cargo.toml`, `.github/workflows/ci-pr-ready.yml`**

**Depends on:** Tasks 0 and 2 merged, and Checkpoint A passed (Execution notes).

**Files:**
- Create: `/CMakeLists.txt` (Corrosion imports `lil-core`, `lil-cli`, `lil-mcp` and `lil-qt`; `add_subdirectory(apps/…)`; every exe and DLL lands in one `bin/`), `/CMakePresets.json` (configure `dev` and `ci`: Ninja, `RelWithDebInfo`, `build/<preset>`, `CMAKE_PREFIX_PATH` from `LIL_QT_DIR`; build presets of the same names; test presets `qml`, `grabs`, `deploy`, `desktop`, `bench`, and `ci-qml`, `ci-deploy`, `ci-desktop`, each by CTest label, `qml` and `grabs` with `QT_QPA_PLATFORM=offscreen`).
- Create: `apps/lil-qt-shim/{CMakeLists.txt, include/lilshim/{Activation.h, DockFactory.h, LogBridge.h, PreviewProvider.h, shim_global.h}, src/{Activation.cpp, DockFactory.cpp, LogBridge.cpp, PreviewProvider.cpp}}` (a `SHARED` `qt_add_library` linking `Qt6::GuiPrivate` privately; bodies Tasks 17–19 fill are stubs here), and the QML singleton `LilShim` (module `Lilith.Shim`) that exposes `isModuleLoaded(dll)` and `showInExplorer(path)` to QML.
- Create: `apps/lil-ui-kit/{CMakeLists.txt, qml/Theme.qml, qml/shell/ShellWindow.qml, tests/{CMakeLists.txt, main.cpp, tst_boot.qml}}` (module `Lilith.UiKit` through `qt_add_qml_module`; `QUICK_TEST_MAIN_WITH_SETUP`).
- Create: `apps/lil-web/{CMakeLists.txt, qml/SuiteWebProfile.qml, qml/WebPanel.qml}` (module `Lilith.Web`, its own plugin DLL; the WebEngine profile from `WebEngineProfilePrototype`, P4a's carry for IMG-126; not in any panel list).
- Create: `apps/lil-image/{CMakeLists.txt, main.cpp, image.toml}` (`image.toml`: `app = "image"`, `exe = "lil-image.exe"`, `opens` for `lil://` refs and the image formats; installed as `apps\image.toml` beside the core).
- Create: `crates/lil-qt/{Cargo.toml, build.rs, src/lib.rs, src/qt/mod.rs}` (feature `qt`; the `Lilith.Core` QML module with one `Core` object holding `buildId`; `boot(args) -> BootPlan` returning `Mode::Shell` until Task 19).
- Create: `tools/dev-shell.ps1` (enters the VS 2022 developer environment and runs its arguments), `tools/deploy/check-deploy.ps1` (runs the deploy folder with Qt, Visual Studio, MSVC and Windows Kits removed from PATH, as spike 02's harness; checks the size against `test-params.toml`'s budget once Task 29 adds it, until then against Task 0's figure plus 10 %), `tools/ci/check-qt-pair.ps1` (QA-077: the installed `Qt6Core.dll` is 6.12.0 and `Qt6WebEngineCore.dll` is 6.140.0, and no `Qt6Core5Compat.dll` was installed, else exit 1).
- Modify: `/Cargo.toml` (protected: `cxx`, `cxx-qt`, `cxx-qt-lib`, `cxx-qt-build` in `[workspace.dependencies]`), `.github/workflows/ci-pr-ready.yml` (protected, and pushed by the owner, P4a-R32: in the `pr-ready` job, or in a separate Qt job with a Qt cache when Task 0's S0.10 says so, with `dev/merge-guard.toml` (protected) naming that job; after the Rust steps, `naqt` pinned by commit `43da90f1` installs Qt 6.12.0 `win64_msvc2022_64` with `--extensions qtwebengine@6.140.0`, cached by the key `qt-6.12.0-webengine-6.140.0-msvc2022_64`; `tools/ci/check-qt-pair.ps1`; `cmake --preset ci`; `cmake --build --preset ci`; `ctest --preset ci-qml`; `ctest --preset ci-deploy`), `.gitignore` (`/build/`).
- Test: CTest `deps_walk` (label `deploy`), `deploy_runs_off_path` (label `deploy`), `tst_boot` and `lazy_web` (label `qml`); `crates/lil-qt` unit tests.

**Interfaces:**
- Consumes: Task 0's KDDockWidgets version, `windeployqt` flags, `--skip-plugin-types` list and the P4a-R1 route.
- Produces:

```cpp
// apps/lil-qt-shim/include/lilshim/Activation.h
namespace lilshim {
void activate(QWindow* w, bool takeFocus);   // takeFocus false: show without activating, flash the taskbar button
bool isModuleLoaded(const QString& dll);     // tests: GetModuleHandleW
void showInExplorer(const QString& path);
}
// apps/lil-image/main.cpp: QCoreApplication::setAttribute(Qt::AA_ShareOpenGLContexts) before QGuiApplication;
//   lil_qt::boot(argv) first (a forwarded launch exits 0 before Qt starts, Task 19);
//   organizationName = the channel's data-folder name ("LILITH", "LILITH-dev"), applicationName = "image";
//   KDDockWidgets::initFrontend(QtQuick); LogBridge installed; load Lilith.UiKit ShellWindow (or ConfirmWindow, Task 21).
```

```text
cmake --build --preset dev --target deploy   →  build/dev/deploy/:
  lil-image.exe, lil-core.exe, lil.exe, lil-mcp.exe, apps\image.toml, the shim DLL, the KDDockWidgets DLL,
  the Lilith.Web plugin, the VC++ runtime DLLs copied from the MSVC redist folder (OPS-094), then windeployqt
  with Task 0's flags.
```

- [ ] **Step 1: Write the failing tests:**
  - `deps_walk`: `dumpbin /dependents` of `lil-image.exe`, the shim DLL and the `lil-qt` library lists no `Qt6WebEngine*` (APP-137); the deploy folder holds `vcruntime140.dll`, `vcruntime140_1.dll` and `msvcp140.dll` (OPS-094: System32 has them too, so presence is checked, not use);
  - `lazy_web` (`tst_lazy_web.qml`): before a `Loader` loads `Lilith.Web`'s `WebPanel`, `LilShim.isModuleLoaded("Qt6WebEngineCore.dll")` is false; after, true; the WebEngine profile's storage path is under the app's data folder;
  - `tst_boot.qml`: `Theme` loads and `ShellWindow` opens offscreen with a KDDockWidgets main window;
  - `deploy_runs_off_path`: `check-deploy.ps1` starts the deployed `lil-image.exe --smoke-exit --no-activate` (shows the window without taking focus, logs `first_frame` and `interactive`, exits 0) with the stripped PATH; exit 0, both log lines present, the folder within its size budget;
  - `lil-qt` (no Qt): `boot_returns_shell_mode_by_default`.
- [ ] **Step 2: Run them.** Run, each through `tools/dev-shell.ps1` (Execution notes): `cmake --preset dev`, `cmake --build --preset dev` (about 25 minutes from clean; ask first), `ctest --preset qml -R "tst_boot|lazy_web"`, `ctest --preset deploy`; and `cargo nextest run -p lil-qt --retries 0`. Expected: FAIL (nothing built yet).
- [ ] **Step 3: Implement** with spike 02's CMake pattern and Task 0's results; `extern crate cxx_qt_lib;` where needed; the shim links `Qt6::GuiPrivate` privately; nothing but `apps/lil-web/` names `WebEngineQuick`.
- [ ] **Step 4: Run them, then the gate** (`-p lil-qt`, `cargo deny check`, `cargo clippy --workspace --all-targets -- -D warnings` once, because the workspace gained a member, and `cargo build --workspace --locked` with no Qt on PATH). Expected: PASS; the build has 0 warning lines at default settings.
- [ ] **Step 5: Commit.**

```bash
git commit -m "build: CMake with cxx-qt and KDDockWidgets, the shim, lil-qt behind its qt feature, the lazy Lilith.Web module, the dev deploy folder, Qt in CI"
```

---

### Task 16: `lil-qt` bridge: the `core` object, requests, the throttle, event delivery and automatic retry (APP-024, APP-026–029, APP-139, ARCH-117, ARCH-118, ARCH-119, QA-026) — needs Qt

**Depends on:** Tasks 14 and 15 merged.

**Files:**
- Create: `crates/lil-qt/src/{throttle.rs, requests.rs, retry.rs}` (Qt-free), `crates/lil-qt/src/qt/{core_object.rs, runtime.rs}`.
- Create: `apps/lil-ui-kit/qml/core/CoreCall.js`, `apps/lil-ui-kit/tests/{FakeCore.qml, tst_core_call.qml}`; `crates/lil-qt/tests/e2e.rs` (feature `qt`): Corrosion builds no test targets, so CTest runs it through `add_test(NAME lil_qt_e2e COMMAND cargo test --locked -p lil-qt --features qt --test e2e)` with `QMAKE` set to the Qt pair's `qmake.exe` and the label `qml`; the test starts its own `QGuiApplication` on a thread for `CxxQtThread::queue` (about 4 minutes the first time, for the `qt` build).
- Modify: `apps/lil-image/main.cpp` (app logs through `lil_platform::logs::init(env, "image")`, Qt messages through `LogBridge`), `apps/lil-qt-shim/src/LogBridge.cpp`.

**Interfaces:**
- Consumes: `lil_client::session::Session` and `mark_gui_thread` (Task 14).
- Produces:

```rust
// Qt-free
pub struct LeadingEdge { pub min_interval: Duration }
pub enum Offer { Now, At(Instant) }
impl LeadingEdge { pub fn offer(&mut self, now: Instant) -> Offer; }
pub enum ThrottleClass { JobProgress(String /* job id */), Telemetry(String /* series */), Never, Plain }
pub fn classify(topic: &str, data: &serde_json::Value) -> ThrottleClass;  // APP-027: job:<id> progress 10 Hz, telemetry 1 Hz, confirmations never
pub struct Coalescer; impl Coalescer { pub fn push(&mut self, ev: EventParams, now: Instant) -> Vec<EventParams>; pub fn due(&mut self, now: Instant) -> Vec<EventParams>; }
pub enum PendingStep { Wait, Resend { retry_token: String }, Settle(serde_json::Value /* outcome */) }
pub struct PendingCall;
impl PendingCall {
    pub fn new(confirmation_id: String, retry_token: String) -> PendingCall;
    pub fn on_event(&mut self, ev: &EventParams) -> PendingStep;        // confirmations topic
    pub fn on_snapshot(&mut self, pending_ids: &[String], answered: &[(String, String /* state */)]) -> PendingStep;
    pub fn on_close(&mut self) -> PendingStep;                          // the app closes: cancelled
}
```

```text
QML singleton Core (module Lilith.Core), APP-028:
  properties: connection ("connecting" | "ready" | "reconnecting" | "incompatible" | "unavailable"),
              coreProtocol, workspace (map or null), project, channelScheme,
              startTimes ({firstFrameMs, interactiveMs} of this start, APP-136)
  invokables: call(tool, argsJson) -> requestId; appCall(method, paramsJson) -> requestId; cancel(requestId);
              subscribe(topic); unsubscribe(topic); shownText(text, maxChars) -> string (lil_parse::shown_text)
  signals:    callFinished(id, resultJson), callFailed(id, errorJson), callPendingConfirmation(id, pendingJson),
              event(topic, kind, seq, dataJson)
  workspace switching: setWorkspace(id) -> requestId (Session::set_workspace, P4a-R34)
CoreCall.js: call(core, tool, args, {ok, error, pending, autoRetry = true}) -> {cancel()}
FakeCore.qml: the same API, scripted by tests (respond(id, outcome), emit(topic, kind, data))
```

- [ ] **Step 1: Write the failing tests:**
  - Qt-free (`cargo nextest run -p lil-qt`): `first_progress_applies_now_then_at_most_10_hz`; `telemetry_is_1_hz_per_series`; `confirmations_are_never_throttled`; `coalescing_keeps_the_newest_value_per_key`; `cancel_of_an_unknown_id_is_false`; `pending_call_resends_once_after_approval`; `pending_call_settles_denied_and_expired_without_resending`; **`pending_call_retries_once_after_an_answer_across_a_reconnect`** (Review Focus 1: the answered event missed during a reconnect; the snapshot shows it answered → `Resend` once, never twice); `pending_call_is_cancelled_when_the_app_closes`;
  - QML `tst_core_call.qml`: `test_ok_error_and_pending_reach_their_callbacks`; `test_auto_retry_resends_with_the_retry_token`; `test_cancel_drops_the_callback`; `test_no_request_object_is_returned_to_qml` (the result is a string id, APP-028);
  - `lil_qt_e2e` (a real test-channel core from `lil-testkit`): `call_finishes_through_the_core_object`; `a_cap_pending_call_retries_after_an_app_answer`; `progress_events_arrive_at_most_10_per_second`; `trace_ids_reach_the_core_log` (ARCH-118: the core's log line for the call carries the `tr_` id the app sent).
- [ ] **Step 2: Run them.** Run: `cargo nextest run -p lil-qt --retries 0`, then `cmake --build --preset dev` and `ctest --preset qml -R "tst_core_call|lil_qt_e2e"`. Expected: FAIL.
- [ ] **Step 3: Implement.** A background tokio runtime owns the `Session`; results and events reach the GUI thread through `CxxQtThread::queue`, and a queued task whose object is gone is dropped (APP-026); the GUI thread is marked with `mark_gui_thread()`.
- [ ] **Step 4: Run them, then the gate** (`-p lil-qt`; QML flake guard `--repeat until-fail:10`). Expected: PASS.
- [ ] **Step 5: Commit.**

```bash
git commit -m "lil-qt: core object with request ids, leading-edge throttle, GUI-thread event delivery, automatic retry after a confirmation"
```

---

### Task 17: List models and the live-preview provider (APP-025, APP-031, APP-037) — needs Qt

**Depends on:** Task 16 merged.

**Files:**
- Create: `crates/lil-qt/src/paging.rs` (Qt-free), `crates/lil-qt/src/qt/{json_list_model.rs, preview_feed.rs}`.
- Modify: `apps/lil-qt-shim/src/PreviewProvider.cpp`.
- Test: `crates/lil-qt` unit tests; `apps/lil-ui-kit/tests/{tst_json_list_model.qml, tst_preview.qml}`.

**Interfaces:**
- Produces:

```rust
pub struct PageState; impl PageState { pub fn can_fetch_more(&self) -> bool; pub fn next_args(&mut self, base: &serde_json::Value, page: u32) -> Option<serde_json::Value>; pub fn on_page(&mut self, next_cursor: Option<String>); pub fn reset(&mut self); }
pub enum Change { Inserted(usize), Updated(usize), Removed(usize) }
pub struct Rows; impl Rows { pub fn upsert(&mut self, row: serde_json::Value, id_key: &str) -> Change; pub fn remove(&mut self, id: &str) -> Option<Change>; }
```

```text
JsonListModel (QAbstractListModel, #[base] / #[inherit] / #[cxx_override]): properties core, tool, argsJson,
  listKey, idKey, pageSize (default 100), topic, roleNames (list); invokables reload(), get(row) -> map;
  canFetchMore()/fetchMore() page through `cursor`/`next_cursor`; topic events upsert by idKey; a snapshot or
  resync_required reloads.
EventLogModel: the newest 1,000 events of every topic the app follows, from the start of the app (Diagnostics' event log).
Preview provider: image://lilpreview/<job_id>/<seq>, caching off; one frame per job kept; frames from
  preview:<job_id> events (format, data_b64) pushed from Rust; dropped when the job ends.
```

- [ ] **Step 1: Write the failing tests:** Qt-free `page_state_fetches_until_the_cursor_is_null`, `rows_upsert_by_id_updates_in_place`, `rows_ignore_events_older_than_the_row`; QML `tst_json_list_model::test_fetch_more_pages_through_the_cursor`, `test_topic_upsert_updates_one_row`, `test_snapshot_reloads`, `test_resync_required_reloads`; `tst_event_log::test_keeps_the_newest_1000`; `tst_preview::test_each_frame_has_a_unique_url_and_only_the_newest_is_kept`, `test_a_finished_job_drops_its_frame`, `test_no_image_bytes_reach_qml_properties` (APP-037: model roles hold paths and URLs only).
- [ ] **Step 2: Run them.** Run: `cargo nextest run -p lil-qt --retries 0 -E 'test(/page_state|rows_/)'` and `ctest --preset qml -R "tst_json_list_model|tst_preview"`. Expected: FAIL.
- [ ] **Step 3: Implement** the model over `Rows` and `PageState`; the provider's map is lock-protected (APP-031).
- [ ] **Step 4: Run them, then the gate.** Expected: PASS.
- [ ] **Step 5: Commit.**

```bash
git commit -m "lil-qt: paged JSON list model with live upserts, and the live-preview image provider"
```

---

### Task 18: The shell: windows, layouts, theme, the shortcut map and minimal accessibility (APP-001–004, APP-008, APP-011, APP-012, APP-143's map, APP-156, ARCH-010, ARCH-116; P4a-R16, P4a-R25) — needs Qt

**Depends on:** Task 16 merged.

**Files:**
- Create: `crates/lil-qt/src/{layout_file.rs, shortcuts.rs, app_state.rs}` (Qt-free), `crates/lil-qt/src/qt/layout.rs`; `apps/lil-qt-shim/src/LayoutBridge.cpp` (KDDockWidgets `LayoutSaver` save and restore, the dock factory registration, a test hook that logs the restore order).
- Modify: `apps/lil-ui-kit/qml/Theme.qml` and `apps/lil-ui-kit/qml/shell/ShellWindow.qml` (Task 15 created them).
- Create: `apps/lil-ui-kit/qml/{controls/{LButton.qml, LIconButton.qml, LToggle.qml, LTextField.qml, LBanner.qml, LBadge.qml, LGauge.qml, LCard.qml}, shell/{DockedPanel.qml, Panels.js, PlaceholderPanel.qml}, a11y/A11yWalk.qml}`; `Panels.js` registers a named placeholder for each panel of this plan (Home, Jobs, Confirmations, Hardware, Usage & spend, Diagnostics, Settings), which Tasks 21–27 replace one by one.
- Test: `crates/lil-qt` unit tests; `apps/lil-ui-kit/tests/{tst_controls.qml, tst_layout.qml, tst_a11y.qml, tst_theme.qml}`; grab `grab_shell`.

**Interfaces:**
- Produces:

```rust
pub struct Rect { pub x: i32, pub y: i32, pub w: i32, pub h: i32 }
pub struct Screen { pub name: String, pub available: Rect, pub scale: f64 }
pub fn clamp_to_screens(window: Rect, saved_screen: &str, screens: &[Screen]) -> Rect;   // APP-003
pub struct LayoutFile; impl LayoutFile { pub fn load(path: &Path) -> Result<LayoutFile, LayoutError>; pub fn save(&self, path: &Path) -> std::io::Result<()>; }
pub fn set_aside(path: &Path, utc: &str) -> std::io::Result<PathBuf>;                     // <name>.bad-<utc>
pub enum Scope { Global, BrowserReview, Canvas }
pub struct Binding { pub keys: String, pub action: String, pub scope: Scope }
pub struct ShortcutMap; impl ShortcutMap { pub fn plan_4a() -> ShortcutMap; pub fn conflicts(&self) -> Vec<(Binding, Binding)>; }
pub struct AppState; impl AppState { pub fn load(env: &Env, app: &str) -> AppState; pub fn last_seen(&self, ws: &str) -> Option<String>; pub fn set_last_seen(&mut self, ws: &str, at: &str) -> std::io::Result<()>; }
```

```text
Default layout: one main window "main": Home in the centre; Jobs and Confirmations tabbed on the right;
  Hardware, Usage & spend and Diagnostics tabbed at the bottom (ARCH-010's panels of this plan).
Global keys (plan 4a): Ctrl+1 Home, Ctrl+2 Jobs, Ctrl+3 Confirmations, Ctrl+4 Hardware, Ctrl+5 Usage & spend,
  Ctrl+6 Diagnostics, Ctrl+Shift+P Pause or resume GPU work, F6 / Shift+F6 next / previous panel.
  Scopes BrowserReview and Canvas are declared empty (plans 4b and 5).
Theme tokens (dark, APP-012): bg, surface, surfaceRaised, text, textMuted, accent, danger, warning, ok, focus.
A11yWalk: visits every item of a panel; each visible interactive item must have a non-empty Accessible.name,
  an Accessible.role other than NoRole, and be in the tab chain.
Window title: "ATHANOR — <project>" (P4a-R25).
```

- [ ] **Step 1: Write the failing tests:**
  - Qt-free: **`layout_clamps_windows_to_the_available_area`** (Review Focus 5: a window on an unplugged screen moves onto the primary's available area; a layout saved at 150 % restored on a 100 % screen keeps the window inside the area; one larger than its area shrinks to it; a maximized window restores onto its own screen); `layout_file_round_trips`; `a_bad_layout_file_is_set_aside`; `the_plan_4a_map_has_no_conflicts`; `a_scope_key_equal_to_a_global_key_is_a_conflict`; `app_state_round_trips_last_seen_per_workspace`;
  - QML: `tst_layout::test_restore_runs_in_order` (the shim's hook logs: windows created → factory registered → restore → clamp); **`test_bad_layout_falls_back_to_default`** (a damaged file, an empty file, and a KDDockWidgets blob its restore refuses, as a newer version's would; each is set aside and the Default layout opens); `test_float_and_redock_a_placeholder_keeps_its_state` (ARCH-116; each real panel adds its own, and Task 29 runs the full set); `tst_a11y::test_every_control_has_a_role_a_name_and_focus` (A11yWalk over a page of every control); `tst_theme::test_text_contrast_is_at_least_4_5_to_1` (`text` and `textMuted` on `bg`, `surface` and `surfaceRaised`; `focus` at least 3:1); `tst_controls::test_keyboard_operates_every_control` (Space and Enter press buttons, toggles flip, Escape closes a banner); `grab_shell` (the Default layout).
- [ ] **Step 2: Run them.** Run: `cargo nextest run -p lil-qt --retries 0 -E 'test(/layout|shortcut|app_state/)'` and `ctest --preset qml -R "tst_layout|tst_a11y|tst_theme|tst_controls"`; `ctest --preset grabs -R grab_shell`. Expected: FAIL.
- [ ] **Step 2b: Owner present:** a Narrator check of the shell (Task 0's S0.7 list on the real shell), recorded in the PR.
- [ ] **Step 3: Implement.** The layout is saved on close and every 60 s while it changes (P4a-R33), through `replace_file` (P4a-R16).
- [ ] **Step 4: Run them, then the gate.** Expected: PASS.
- [ ] **Step 5: Commit.**

```bash
git commit -m "shell: KDDockWidgets windows with ordered layout restore and clamping, dark theme tokens, the shortcut map, accessible base controls"
```

---

### Task 19: Single instance, the command line, activation and the Opened card (APP-013–016, APP-020, APP-021, APP-063, APP-065, ARCH-098; P4a-R2, P4a-R3, P4a-R11, P4a-R21) — needs Qt, adversarial review

**Depends on:** Tasks 4, 5, 11 and 18 merged.

**Files:**
- Create: `crates/lil-qt/src/{single_instance.rs, args.rs}` (Qt-free), `crates/lil-qt/src/qt/boot.rs`; `apps/lil-ui-kit/qml/panels/home/OpenedCard.qml`.
- Modify: `apps/lil-image/main.cpp` (a forwarded launch exits 0 before `QGuiApplication`), `apps/lil-qt-shim/src/Activation.cpp`, `apps/lil-ui-kit/qml/shell/ShellWindow.qml` (routes open requests and `open_requests:image` events).
- Test: `crates/lil-qt/tests/instance.rs` (Qt-free, two processes through a probe binary); `apps/lil-ui-kit/tests/tst_open_card.qml`; CTest `desktop_second_launch_comes_to_front` (label `desktop`).

**Interfaces:**
- Consumes: `lil_parse::{strip_argument, parse_lil_url}`, `lil_schema::parse_lilref`; `raw_command_line`, `split_first_argument`, `allow_set_foreground` (Task 5).
- Produces:

```rust
pub enum Mode { Shell, Confirm { id: Option<String> } }
pub enum Intent { Open, Edit, UseAsInput }                    // lil-qt's own; wire as in APP-066
pub enum Opened { Confirm(String), Task(String), Target(String), Ref(String), LilRef { reference: String, title: Option<String> } }
pub struct BootPlan { pub mode: Mode, pub intent: Intent, pub no_activate: bool, pub opened: Option<Opened>, pub refused: Option<String>, pub forwarded: bool, pub org: String, pub app: String }
pub fn plan_boot(raw_cmdline: &str, env: &Env) -> BootPlan;
pub struct InstanceServer;
impl InstanceServer { pub fn claim(env: &Env, app: &str, confirm: bool) -> std::io::Result<Option<InstanceServer>>; pub fn next(&self) -> Option<Forwarded>; }
pub struct Forwarded { pub args: Vec<String> }
pub fn forward(env: &Env, app: &str, confirm: bool, args: &[String]) -> Result<u32 /* pid */, ForwardError>;
```

- [ ] **Step 1: Write the failing tests:**
  - Qt-free `args.rs` tests: `args_parse_every_form` (`--intent`, `--no-activate`, `--confirm` with and without an id, a URL, a path, a `.lilref`); **`args_refuse_a_large_lilref_and_a_bad_hash`** (Review Focus 2: a 1 GB sparse `.lilref` is refused after reading at most 4 KiB; a `.lilref` naming a ref the core answers with `ref.hash_mismatch` shows that message); `args_refuse_a_network_lilref` (a `\\server\share\x.lilref` path is refused unread: "LILITH opens .lilref files from this PC only"); `a_refused_argument_never_answers_or_opens`;
  - `instance.rs`: `a_second_launch_forwards_and_exits` (the probe's second process exits 0 within 2 s and the first receives its arguments); **`two_quick_second_launches_reach_one_window`** (Review Focus 3: two launches started 10 ms apart both forward to the one running instance, which gets both arguments in order, and no second window appears); `forward_checks_the_server_image_path` (a server run from another exe → `ForwardError::Untrusted`, nothing written); `forward_gives_up_after_2_s`; `a_confirm_url_goes_to_the_running_full_app_first` (P4a-R11); `the_confirm_window_has_its_own_pipe`;
  - QML `tst_open_card.qml`: `test_shows_the_target_and_its_actions`; **`test_refused_link_shows_message`**; `test_no_activate_flashes_instead_of_focusing` (the fake shim records `activate(w, false)`); `test_task_url_opens_jobs_on_that_job`; `test_agent_open_request_never_takes_focus` (an `open_requests:image` event with `focus: false`, APP-065);
  - desktop group: `desktop_second_launch_comes_to_front` (Task 1's S1.4 recipe: `GetForegroundWindow` is the first instance's window).
- [ ] **Step 2: Run them.** Run: `cargo nextest run -p lil-qt --retries 0 -E 'binary(instance) | test(/args_/)'` and `ctest --preset qml -R tst_open_card`. Expected: FAIL.
- [ ] **Step 3: Implement.** The single-instance check runs before Qt starts (fast second launches); the raw command line is split once (Task 5) and its rest given to `strip_argument` and `parse_lil_url` unchanged.
- [ ] **Step 4: Run them, then the gate.** Expected: PASS.
- [ ] **Step 5: Commit.**

```bash
git commit -m "app: single instance with foreground hand-off, raw-command-line lil: and .lilref handling, the Opened card"
```

---

### Task 20: The top bar, connection banners, switchers and the no-workspace state (APP-006, APP-015, APP-016, APP-034, APP-036, ARCH-098, ARCH-100) — needs Qt

**Depends on:** Tasks 11, 14, 17 and 18 merged.

**Files:**
- Create: `apps/lil-ui-kit/qml/shell/{TopBar.qml, StatusIndicator.qml, ProjectSwitcher.qml, WorkspaceSwitcher.qml, ConnectionBanner.qml, NoWorkspaceBanner.qml}`.
- Test: `apps/lil-ui-kit/tests/{tst_top_bar.qml, tst_banners.qml}`; grab `grab_top_bar`.

**Interfaces:**
- Consumes: `Core.connection`, `projects.list`, `workspaces.list`, `app/set_context`, `workspace.create`, `workspace.set_default`, `jobs.list` `lanes`, `caps.get`, `telemetry` (Tasks 6, 11, 16, 17).
- Produces: `TopBar` with the indicators of APP-006 (jobs running and queued with a paused badge; spend this month against the highest cap ratio; GPU load, VRAM and temperature; the core connection); the app switcher (ATHANOR only, with the plain name "Image studio" first, APP-164); the assistant toggle shown disabled ("The assistant arrives in a later version"); an action marked `requiresCore: true` is disabled unless `Core.connection` is `ready`.

- [ ] **Step 1: Write the failing tests:** `tst_top_bar::test_indicators_follow_events` (FakeCore emits a `jobs` event with 2 running and 3 queued, a `telemetry` sample at 63 % GPU, 9.5 of 16 GiB and 71 °C, and a `caps` change at 412 ‰: the indicators read "2 running · 3 queued", "GPU 63 % · 9.5/16 GiB · 71 °C" and "41 % of cap"); `test_workspace_switch_rehellos_and_keeps_the_default` (P4a-R34: choosing another workspace calls `Core.setWorkspace`, never `workspace.set_default`; "Make this the default" calls `workspace.set_default` only after its own click); `test_spend_shows_the_highest_cap_ratio`; `test_paused_badge_names_the_reasons`; `test_project_switch_calls_set_context`; `test_sketchbook_is_the_default_project` (ARCH-098); `tst_banners::`**`test_reconnecting_disables_actions`** (Review Focus 1: within one frame of `connection` becoming `reconnecting` the banner shows and every `requiresCore` action is disabled; they return on `ready`); `test_incompatible_says_restart_the_core_when_it_is_idle` (APP-036); `test_no_workspace_banner_and_the_create_flow` (APP-016: "Create or choose a workspace" → `workspace.create` with the chosen folder; the banner leaves); A11yWalk over the top bar; `grab_top_bar` (ready, reconnecting, no workspace).
- [ ] **Step 2: Run them.** Run: `ctest --preset qml -R "tst_top_bar|tst_banners"`. Expected: FAIL.
- [ ] **Step 3: Implement.** The indicators bind to the `jobs`, `telemetry` and `caps` topics through the throttle (APP-027); the switchers read `projects.list` and `workspaces.list` when opened.
- [ ] **Step 4: Run them, then the gate.** Expected: PASS.
- [ ] **Step 5: Commit.**

```bash
git commit -m "shell: top bar indicators, project and workspace switchers, reconnecting, incompatible and no-workspace banners"
```

---

### Task 21: The Confirmations panel, the inline cap prompt and the confirm window (APP-044–049, APP-054, APP-115, CORE-111; P4a-R9, P4a-R10, P4a-R11) — needs Qt, adversarial review

**Depends on:** Tasks 17 and 19 merged.

**Files:**
- Create: `crates/lil-qt/src/answer_gate.rs` (Qt-free); `apps/lil-ui-kit/qml/panels/confirmations/{ConfirmationsPanel.qml, ConfirmationCard.qml, HistoryList.qml}`, `apps/lil-ui-kit/qml/dialogs/{ConfirmDialog.qml, CapPrompt.qml}`, `apps/lil-ui-kit/qml/shell/ConfirmWindow.qml`.
- Modify: `apps/lil-image/main.cpp` (`Mode::Confirm` loads `ConfirmWindow`; title "LILITH — Confirm").
- Test: `crates/lil-qt` unit tests; `apps/lil-ui-kit/tests/{tst_confirmations.qml, tst_cap_prompt.qml, tst_confirm_window.qml}`; CTest `desktop_toast_url_brings_the_confirm_window_to_front` (label `desktop`); grabs.

**Interfaces:**
- Consumes: `app/confirmations.list`, `app/confirmation.answer`, the `confirmations` topic, `Core.shownText`.
- Produces:

```rust
pub enum InputKind { PointerRelease, Space, Enter }
pub struct AnswerGate { pub visible_since_ms: Option<u64>, pub in_front: bool }   // in_front is QML's Window.active
impl AnswerGate { pub fn accept(&self, kind: InputKind, focused_on_button: bool, now_ms: u64) -> bool; }   // P4a-R10
```

- [ ] **Step 1: Write the failing tests:**
  - Qt-free: `refuses_within_500_ms_of_showing`; `refuses_when_not_in_front`; `accepts_a_release_or_space_or_enter_on_the_focused_button_after_500_ms`;
  - `tst_confirmations.qml`: `test_card_shows_who_the_exact_action_and_the_files` (APP-115); `test_untrusted_arguments_are_shown_normalised` (U+202E shown as a marker; control characters gone); `test_initial_focus_is_deny_and_enter_does_not_approve`; `test_one_answer_sends_one_app_call`; `test_answered_elsewhere_clears_the_card` (APP-054); `test_history_tab_shows_confirmed_via` (APP-046); `test_expiry_countdown`;
  - `tst_cap_prompt.qml` (APP-054): `test_raise_allow_once_and_cancel_send_their_answers` (`raise` sends `raise_to`; `allow_once` sends `cap.action`; cancel sends `deny`); `test_answering_in_the_panel_clears_the_inline_prompt`;
  - `tst_confirm_window.qml`: **`test_answered_elsewhere_shows_state`** (Review Focus 3: pending → answered by another app → "Answered in the app at <time>" and no buttons); `test_expired_and_unknown_ids_say_so`; **`test_reconnect_keeps_the_dialog_and_its_state`** (Review Focus 1: `Core.connection` goes `reconnecting` while the dialog is open; its buttons disable, the banner shows, and after `ready` it re-reads the confirmation and shows its live state); `test_without_an_id_lists_every_pending_confirmation`; `test_a_second_url_adds_to_the_open_window` (P4a-R11);
  - desktop group: `desktop_toast_url_brings_the_confirm_window_to_front` (from a `lil-test-<id>:confirm/<id>` launch, with no app running and with one running);
  - grabs: a pending card, a cap prompt, the confirm window.
- [ ] **Step 2: Run them.** Run: `cargo nextest run -p lil-qt --retries 0 -E 'test(/answer_gate|refuses_|accepts_/)'` and `ctest --preset qml -R "tst_confirmations|tst_cap_prompt|tst_confirm_window"`. Expected: FAIL.
- [ ] **Step 3: Implement.** Approve and Deny call `AnswerGate::accept` before `app/confirmation.answer`; the window's `in_front` is its `Window.active`.
- [ ] **Step 4: Run them, then the gate.** Expected: PASS.
- [ ] **Step 5: Commit.**

```bash
git commit -m "confirmations: panel with history, inline cap prompt, and the confirm window that answers only on a real, deliberate input"
```

---

### Task 22: The Jobs panel and the cost and time chip (APP-114, APP-139, APP-140, CORE-160, CORE-161) — needs Qt

**Depends on:** Tasks 6, 7, 17 and 18 merged.

**Files:**
- Create: `crates/lil-qt/src/format.rs` (Qt-free); `apps/lil-ui-kit/qml/panels/jobs/{JobsPanel.qml, JobRow.qml, LaneHeader.qml, QueueControls.qml}`, `apps/lil-ui-kit/qml/controls/CostChip.qml`.
- Modify: `apps/lil-ui-kit/qml/shell/Panels.js` (the panel replaces its placeholder; shared with the other panel tasks, so rebase and keep every entry).
- Test: `crates/lil-qt` unit tests; `apps/lil-ui-kit/tests/{tst_jobs_panel.qml, tst_cost_chip.qml}`; grabs.

**Interfaces:**
- Consumes: `jobs.list` (with `lanes`), the `jobs`, `job:<id>` and `preview:<job_id>` topics, `job.move`, `job.cancel`, `job.retry`, `app/queue.pause`, `app/queue.resume`, `engines.status` (`outside_jobs`), `caps.get`.
- Produces:

```rust
pub fn chip_text(e: &Estimate, cap_share_permille: Option<u32>) -> String;  // "local · ~8 s · ~2 Wh" | "≈ $0.04 · 3 % of cap" | "no estimate yet"
pub fn eta_text(e: &Estimate, elapsed_s: f64) -> String;                    // "about 12 s left" | "—"
pub fn elapsed_text(s: f64) -> String;
```

- [ ] **Step 1: Write the failing tests:** Qt-free `chip_text_local`, `chip_text_money_with_its_cap_share`, `chip_text_none`, `eta_never_goes_below_zero`; `tst_jobs_panel.qml`: `test_rows_update_in_place_at_most_10_hz` (FakeCore sends 50 progress events in a second: the row's value is the newest and at most 11 updates applied); `test_preview_url_changes_with_each_frame`; `test_lane_header_shows_waiting_for_vram_and_outside_jobs`; `test_run_next_and_last_call_job_move`; `test_cancel_batch_and_all_mine`; `test_pause_and_resume_call_app_queue`; `test_paused_header_names_manual_and_fullscreen`; `test_retry_shows_the_cost_chip_from_the_jobs_estimate`; `test_float_and_redock_keeps_jobs_state` (ARCH-116); A11yWalk (job rows set role and name); `tst_cost_chip.qml`: `test_debounced_estimate_call` (one `job.estimate` 300 ms after the last change); grabs: running, paused, empty.
- [ ] **Step 2: Run them.** Run: `cargo nextest run -p lil-qt --retries 0 -E 'test(/chip_text|eta_/)'` and `ctest --preset qml -R "tst_jobs_panel|tst_cost_chip"`. Expected: FAIL.
- [ ] **Step 3: Implement.** One `JsonListModel` over `jobs.list` with the `jobs` topic; the lane header reads `jobs.list`'s `lanes` and `engines.status`'s `outside_jobs`; previews come from the provider of Task 17.
- [ ] **Step 4: Run them, then the gate.** Expected: PASS.
- [ ] **Step 5: Commit.**

```bash
git commit -m "jobs panel: live previews and progress, ETA, lane state, run next or last, batch cancels, Pause GPU work, the cost and time chip"
```

---

### Task 23: The Hardware panel (APP-127, APP-128, APP-129, APP-131, APP-132; P4a-R17) — needs Qt

**Depends on:** Tasks 7, 17 and 18 merged.

**Files:**
- Create: `crates/lil-qt/src/series.rs` (Qt-free); `apps/lil-ui-kit/qml/panels/hardware/{HardwarePanel.qml, ChartTile.qml, GpuUsers.qml, EnergyTable.qml, AlertsStrip.qml}`.
- Modify: `apps/lil-ui-kit/qml/shell/Panels.js` (the panel replaces its placeholder; shared with the other panel tasks, so rebase and keep every entry).
- Test: `crates/lil-qt` unit tests; `apps/lil-ui-kit/tests/tst_hardware.qml`; grabs.

**Interfaces:**
- Consumes: `telemetry.snapshot`, `telemetry.series`, `telemetry.energy`, `telemetry.jobs`, the `telemetry` topic.
- Produces:

```rust
pub fn resolution_for(window_s: u64) -> &'static str;                        // ≤ 1 h "1s", else "1m" (24 h max here)
pub fn downsample(points: &[(i64, f64, f64, f64)], width_px: usize) -> Vec<(i64, f64, f64)>;   // min and max kept per pixel
pub fn energy_periods(now_utc_ms: i64, offset_at: &dyn Fn(i64) -> i64 /* local offset in s at a UTC time */, session_start_utc_ms: i64) -> [(&'static str, i64, i64); 4]; // session, day, month, year
```

- [ ] **Step 1: Write the failing tests:** Qt-free `fifteen_minutes_use_1s_and_24_hours_use_1m`, `downsampling_keeps_min_and_max_per_pixel`, `energy_periods_follow_the_local_calendar` (DST day included); `tst_hardware.qml`: `test_live_values_from_the_snapshot` (a fixture `Snapshot` with GPU 63 %, 9,728 of 16,384 MiB, 71 °C, 285 W, CPU 18 % on 16 cores and 64 W, RAM 41 of 64 GiB shows "63 %", "9.5 / 16 GiB", "71 °C", "285 W", 16 core bars, "64 W" and "41 / 64 GiB"; disks, network, clocks, fan and throttle reasons each show their fixture value); `test_cpu_temperature_is_never_shown` (APP-128); `test_opens_on_15_minutes_and_zooms_to_24_hours` (APP-129); `test_energy_table_calls_telemetry_energy_for_each_period` (APP-131, with cost when set); `test_alerts_show_and_clear` (APP-132); `test_gpu_users_list_names_the_app_and_engines`; `test_estimated_and_missing_parts_are_labelled`; `test_float_and_redock_keeps_hardware_state` (ARCH-116); A11yWalk (chart tiles have role and name); grabs.
- [ ] **Step 2: Run them.** Run: `cargo nextest run -p lil-qt --retries 0 -E 'test(/resolution|downsampl|energy_periods/)'` and `ctest --preset qml -R tst_hardware`. Expected: FAIL.
- [ ] **Step 3: Implement.** Charts are `PathPolyline` shapes (P4a-R17); series update at 1 Hz through the throttle.
- [ ] **Step 4: Run them, then the gate.** Expected: PASS.
- [ ] **Step 5: Commit.**

```bash
git commit -m "hardware panel: live GPU, CPU, RAM, disk and network, 15-minute to 24-hour charts, energy by period, alerts"
```

---

### Task 24: The Usage & spend panel (APP-116–124) — needs Qt

**Depends on:** Tasks 17 and 18 merged.

**Files:**
- Create: `crates/lil-qt/src/csv.rs` (Qt-free); `apps/lil-ui-kit/qml/panels/usage/{UsagePanel.qml, SpendBreakdown.qml, CapGauges.qml, AccountsList.qml, ClaudeCodeSection.qml}`.
- Modify: `apps/lil-ui-kit/qml/shell/Panels.js` (the panel replaces its placeholder; shared with the other panel tasks, so rebase and keep every entry).
- Test: `crates/lil-qt` unit tests; `apps/lil-ui-kit/tests/tst_usage.qml`; grabs.

**Interfaces:**
- Consumes: `usage.summary`, `caps.get`, the `usage` and `caps` topics.
- Produces: `pub fn csv(rows: &[serde_json::Value], columns: &[&str]) -> String` (RFC 4180; a cell starting with `=`, `+`, `-` or `@` is prefixed with `'`); the export is written to the file you pick, through `replace_file`.

- [ ] **Step 1: Write the failing tests:** Qt-free `csv_quotes_and_guards_formulas`; `tst_usage.qml`: `test_month_and_day_totals_by_each_dimension` (APP-116: a fixture `usage.summary` grouped by provider, model, project, workspace (with "loose"), caller kind, account and tool shows one row each; caller kinds read "You" (`ui:*`), "Assistant", "Claude Code" (`mcp`) and "lil" (`cli`)); `test_tokens_images_and_seconds_by_model` (APP-117); `test_estimated_and_reconciled_are_marked`; `test_unattributed_spend_is_separate`; `test_billing_bases_are_never_added_together` (APP-118: subscription quota, money and local work in their own sections); `test_fixed_costs_are_in_the_month_total` (APP-119); `test_per_currency_and_display_totals` (APP-120); `test_cap_gauges_and_the_50_80_100_alerts` (APP-121); `test_accounts_say_tracked_only` (APP-122, until plan 6b); `test_claude_code_section_is_empty_until_engine_a` (APP-123); `test_accounts_show_opaque_ids_when_no_label_exists` (APP-124); `test_csv_export_writes_the_shown_rows`; `test_float_and_redock_keeps_usage_state` (ARCH-116); A11yWalk; grabs.
- [ ] **Step 2: Run them.** Run: `cargo nextest run -p lil-qt --retries 0 -E 'test(/csv_/)'` and `ctest --preset qml -R tst_usage`. Expected: FAIL.
- [ ] **Step 3: Implement.** One `usage.summary` call per tab (month, day) with `group_by` from the dimension picker; money, subscription quota and local work stay in separate sections; CSV through `csv.rs`.
- [ ] **Step 4: Run them, then the gate.** Expected: PASS.
- [ ] **Step 5: Commit.**

```bash
git commit -m "usage and spend panel: totals by every dimension, billing bases apart, fixed costs, currencies, cap gauges, CSV export"
```

---

### Task 25: The Diagnostics and Backups panels (ARCH-121, ARCH-126, ARCH-127, APP-133, APP-134, CORE-164, QA-083) — needs Qt

**Depends on:** Tasks 8, 9, 12, 17 and 18 merged.

**Files:**
- Create: `apps/lil-ui-kit/qml/panels/diagnostics/{DiagnosticsPanel.qml, VersionsCard.qml, EnginesCard.qml, RecentErrors.qml, EventLog.qml, TelemetrySummary.qml, ModelsReport.qml, NotificationPathCard.qml, StatusLines.qml}`, `apps/lil-ui-kit/qml/panels/backups/BackupsPanel.qml`.
- Modify: `apps/lil-ui-kit/qml/shell/Panels.js` (the panel replaces its placeholder; shared with the other panel tasks, so rebase and keep every entry).
- Test: `apps/lil-ui-kit/tests/tst_diagnostics.qml`; grabs.

**Interfaces:**
- Consumes: `core/hello` (versions), `engines.status` (health, launch profile, catalogue), `engines.environment`, `diagnostics.recent` and the `diagnostics` topic, `diagnostics.bundle`, `models.report`, `desktop.get`, `telemetry.snapshot`; the app's own `first_frame` and `interactive` times.
- Produces: Diagnostics with versions, engine health and VRAM, the queue, recent errors by code, the event log (the newest 1,000 events of every topic the app follows), a telemetry summary, the backup status line ("No backups yet: backups arrive in a later version of LILITH"), the keep-awake line, the notification path ("Run `lil doctor` to fix" when a part is missing), this start's times, the model inventory, and "Export diagnostics bundle" with "Show in Explorer"; the Backups panel opens from it with the same empty state.

- [ ] **Step 1: Write the failing tests:** `tst_diagnostics.qml`: `test_versions_and_engine_health` (from `core/hello`'s `build_id` and `engines.status`: "Core <build>", "ComfyUI <version> · owned · ready", and "attached" for an attached engine); `test_launch_profile_flags_and_catalogue_are_shown` (ARCH-126, ARCH-065); `test_recent_errors_are_grouped_by_code`; `test_event_log_keeps_the_newest_1000`; `test_export_calls_diagnostics_bundle_and_shows_the_path`; `test_models_report_states_are_listed`; `test_backup_status_line_and_the_backups_panel_empty_state` (APP-133, APP-134); `test_notification_path_problems_are_named` (Review Focus 6: each `notification_path` problem shows its sentence: "Notifications are off for LILITH in Windows Settings", "Notifications are off in Windows Settings", "A policy blocks notifications", "The core runs as administrator, so Windows won't show its notifications", "The Start-menu shortcut is missing", "Another LILITH build handles lil: links"; each ends with what to do, `lil doctor` or the Settings panel's "Set up notifications"); `test_keep_awake_unavailable_is_said` (ARCH-127); `test_start_times_are_shown` (APP-136); `test_float_and_redock_keeps_diagnostics_state` (ARCH-116); A11yWalk; grabs.
- [ ] **Step 2: Run them.** Run: `ctest --preset qml -R tst_diagnostics`. Expected: FAIL.
- [ ] **Step 3: Implement.** Cards read their tools once on open and follow their topics; the event log is Task 17's `EventLogModel`; the start times are `Core.startTimes`.
- [ ] **Step 4: Run them, then the gate.** Expected: PASS.
- [ ] **Step 5: Commit.**

```bash
git commit -m "diagnostics panel: versions, engines and launch profile, errors by code, event log, model inventory, bundle export; backups empty state"
```

---

### Task 26: Home (minimal) with "While you were away" (APP-111, APP-016, APP-163; P4a-R16, P4a-R21) — needs Qt

**Depends on:** Tasks 18, 19 and 20 merged.

**Files:**
- Create: `crates/lil-qt/src/away.rs` (Qt-free); `apps/lil-ui-kit/qml/panels/home/{HomePanel.qml, ProjectsCard.qml, RecentAssets.qml, StatusCounts.qml, OpenJobs.qml, AwayCard.qml}`.
- Modify: `apps/lil-ui-kit/qml/panels/home/OpenedCard.qml` (placed in Home); `apps/lil-ui-kit/qml/shell/Panels.js` (Home replaces its placeholder).
- Test: `crates/lil-qt` unit tests; `apps/lil-ui-kit/tests/tst_home.qml`; grabs.

**Interfaces:**
- Consumes: `projects.list`, `project.status`, `assets.query {sort: {field: "updated", dir: "desc"}, limit: 12}`, `jobs.list {since}`, `app/confirmations.list`, `AppState::{last_seen, set_last_seen}` (Task 18).
- Produces:

```rust
pub struct AwayItem { pub kind: &'static str /* "finished" | "failed" | "needs_you" | "confirmation" */, pub id: String, pub title: String, pub at: String }
pub fn away_items(jobs: &[serde_json::Value], pending: &[serde_json::Value], last_seen: Option<&str>) -> Vec<AwayItem>;
```

- [ ] **Step 1: Write the failing tests:** Qt-free `away_selects_finished_failed_unknown_and_pending_since_last_seen`, `away_is_empty_without_a_last_seen_time` (first run shows no card), `away_orders_newest_first`; `tst_home.qml`: `test_projects_and_status_counts` (APP-111: a fixture with projects `sketchbook` (12 assets) and `lantern` (3) and `assets_by_status` of 4 candidate, 2 in review, 1 stale, 0 blocked shows both projects with their counts and "Takes waiting 4 · In review 2 · Stale 1 · Blocked 0"); `test_status_copy_says_takes` (APP-163: the `candidate` count reads "Takes waiting"); `test_open_jobs_list`; `test_away_card_lists_items_and_last_seen_is_written_after_3_s_visible`; `test_away_card_dismiss_clears_it`; `test_no_workspace_shows_recent_loose_files_instead` (APP-016; empty until plan 5); `test_opened_card_sits_in_home` (P4a-R21); `test_float_and_redock_keeps_home_state` (ARCH-116); A11yWalk; grabs.
- [ ] **Step 2: Run them.** Run: `cargo nextest run -p lil-qt --retries 0 -E 'test(/away_/)'` and `ctest --preset qml -R tst_home`. Expected: FAIL.
- [ ] **Step 3: Implement.** Home reads its cards on open and after each `jobs` or `confirmations` event; `last_seen_at` is written through `AppState` after 3 s of visibility.
- [ ] **Step 4: Run them, then the gate.** Expected: PASS.
- [ ] **Step 5: Commit.**

```bash
git commit -m "home: projects, recent assets, status counts, open jobs and the While you were away card"
```

---

### Task 27: The Settings panel: notifications, quiet mode, auto-pause, keep-awake and notification setup (APP-022, APP-146, ARCH-127, CORE-160; P4a-R26; owner questions Q1, Q2 and Q5) — needs Qt

**Depends on:** Tasks 5, 9, 10, 18 and 25 merged.

**Files:**
- Create: `apps/lil-ui-kit/qml/panels/settings/{SettingsPanel.qml, NotificationToggles.qml, DesktopToggles.qml, NotificationSetup.qml}`, `apps/lil-ui-kit/qml/shell/FirstStartCard.qml`; `crates/lil-qt/src/qt/setup.rs` (calls `lil_platform::{write_shortcut, register_handlers}` for the channel's `lil-image.exe`, then asks the core to re-read `desktop.get`).
- Modify: `apps/lil-ui-kit/qml/shell/Panels.js` (Settings replaces its placeholder); `apps/lil-ui-kit/qml/shell/ShellWindow.qml` (the first-start card, owner question Q5; default shown).
- Test: `apps/lil-ui-kit/tests/{tst_settings.qml, tst_first_start.qml}`; grabs.

**Interfaces:**
- Consumes: `desktop.get`, `app/desktop.set` and the `desktop` topic (Task 9); `notification_path` and its reasons (Tasks 9, 10); `lil_platform::{write_shortcut, register_handlers}` (Task 5).
- Produces: the Settings panel, with one toggle per notification category (seven with owner question Q2's default), "Quiet while fullscreen", "Pause GPU work while a game or video is fullscreen" (fullscreen auto-pause), "Keep this PC awake while GPU work runs" (hidden when `keep_awake_available` is false, ARCH-127), and a "Notifications on this PC" card showing `notification_path` with a "Set up notifications" button while the shortcut or the handlers are missing (P4a-R26). The first start's card offers the same button while they are missing, and "Not now" hides it until the next start.

- [ ] **Step 1: Write the failing tests:** `tst_settings.qml`: `test_toggles_show_desktop_get` (a fixture `DesktopSettings` with `job_progress` off and the rest on shows exactly that); `test_each_toggle_calls_app_desktop_set_with_one_key` (flipping "Job progress" sends `app/desktop.set {notifications: {job_progress: true}}` and nothing else; the auto-pause and keep-awake toggles send `{fullscreen_autopause: true}` and `{keep_awake_gpu: false}`); `test_keep_awake_toggle_hidden_when_unavailable`; `test_settings_event_updates_the_toggles` (a `desktop` `{kind: "settings"}` event from another app changes the switches); `test_a_refused_set_restores_the_toggle_and_says_why` (a `caller.not_allowed` or `params.invalid` answer puts the switch back and shows the error); **`test_unreachable_path_shows_the_reason_and_a_fix`** (Review Focus 6: `toasts_reason: "disabled_for_app"` shows "Notifications are off for LILITH in Windows Settings" and an "Open Windows notification settings" link, `ms-settings:notifications`); `test_set_up_button_writes_then_rereads` (the button calls the setup bridge once, then `desktop.get`; the card leaves when `shortcut` and `handlers` are `ok`); `tst_first_start.qml`: `test_card_shows_only_while_setup_is_missing`, `test_not_now_hides_it_for_this_start`; `test_float_and_redock_keeps_settings_state` (ARCH-116); A11yWalk over the panel; grabs: settings, unreachable, first start.
- [ ] **Step 2: Run them.** Run: `ctest --preset qml -R "tst_settings|tst_first_start"`. Expected: FAIL.
- [ ] **Step 3: Implement.** Each toggle sends one key and shows the answer the core returns (never its own guess); the setup bridge runs on the background runtime, never on the GUI thread (QA-026).
- [ ] **Step 4: Run them, then the gate.** The setup bridge's real write is a desktop-group test, `desktop_set_up_button_writes_the_shortcut_and_handlers` (owner present, P4a-R31). Expected: PASS.
- [ ] **Step 5: Commit.**

```bash
git commit -m "settings panel: notification categories, quiet while fullscreen, fullscreen auto-pause, keep-awake, notification setup and the first-start card"
```

---

### Task 28: Windows Hello for `lil dev approve` (QA-088, QA-020; P4a-R23) — Qt-free, adversarial review — **protected: `crates/lil-cli/src/dev/reviewer.rs`, `crates/lil-cli/src/dev/mod.rs`**

**Depends on:** Tasks 1 and 5 merged.

**Files:**
- Create: `crates/lil-platform/src/hello.rs`.
- Modify: `crates/lil-platform/{Cargo.toml, src/lib.rs}`; protected `crates/lil-cli/src/dev/{reviewer.rs, mod.rs}` (`lil dev reviewer-hello enroll | status | remove`; `approve` tries Windows Hello first when enrolled).
- Test: `crates/lil-cli/tests/reviewer_hello.rs` (new; not a protected path).

**Interfaces:**
- Consumes: Task 1's route. Only that route is built; the others stay out.
- Produces:

```rust
// lil_platform::hello
pub fn supported() -> bool;
pub fn kek_enroll(credential: &str) -> std::io::Result<()>;                                   // KEK route
pub fn kek_derive(credential: &str, challenge: &[u8; 32]) -> std::io::Result<Zeroizing<[u8; 32]>>; // P4a-R23
pub fn kek_remove(credential: &str) -> std::io::Result<()>;
pub fn consent(message: &str) -> std::io::Result<bool>;                                       // DPAPI route's gate
pub fn dpapi_protect(data: &[u8]) -> std::io::Result<Vec<u8>>;
pub fn dpapi_unprotect(blob: &[u8]) -> std::io::Result<Zeroizing<Vec<u8>>>;
// lil-cli dev: <home>\reviewer\app-key.hello.toml {route = "kek" | "dpapi", credential = "LILITH-reviewer-kek", challenge = "<64 hex>"}
//   beside app-key.hello.age (KEK route: the PEM under age's scrypt with passphrase = hex(KEK)) or app-key.dpapi
pub trait Unlocker { fn unlock(&self) -> Result<Zeroizing<Vec<u8>>, KeyError>; }   // a fake in tests
```

- [ ] **Step 1: Write the failing tests** (no real Windows Hello: an injected `Unlocker`):
  - `enroll_needs_the_existing_passphrase_key` (enrolling decrypts `app-key.age` with your passphrase first, then writes the Hello file; the passphrase file stays);
  - `approve_uses_hello_when_enrolled` and `approve_falls_back_to_the_passphrase_when_hello_is_cancelled_or_fails`;
  - `no_network_call_before_the_key_is_unlocked` (the existing invariant, through Hello too);
  - `status_names_the_route_and_whether_the_key_is_hardware_held` (never "hardware-bound" without a TPM, QA-088);
  - `remove_deletes_the_hello_files_and_the_credential_and_keeps_the_passphrase_file`;
  - `a_damaged_hello_file_never_blocks_the_passphrase_path`.
- [ ] **Step 2: Run them.** Run: `cargo nextest run -p lil-cli --retries 0 -E 'binary(reviewer_hello)'`. Expected: FAIL.
- [ ] **Step 3: Implement.** The decrypted PEM, the KEK and the signature stay in `Zeroizing` buffers and are never logged.
- [ ] **Step 4: Run them, then the gate** (`-p lil-cli -p lil-platform`). **Owner present (about 10 minutes):** `lil dev reviewer-hello enroll`, then `lil dev reviewer-hello status`, then one real `lil dev approve` on a test PR, recorded in the PR body. Expected: PASS; the GitHub review posts after the Hello prompt; with Hello cancelled, the passphrase prompt follows.
- [ ] **Step 5: Commit.**

```bash
git commit -m "dev: Windows Hello unlocks the reviewer key for lil dev approve (route from spike 11); the passphrase file stays the fallback"
```

---

### Task 29: Close-out: the desktop group, the layout and responsiveness checks, and the milestone checks (APP-001, APP-003, APP-135, APP-136, APP-139, ARCH-116, QA-030, QA-042, QA-064, QA-071; P4a-R31) — **protected: `.config/nextest.toml`, `test-params.toml`, `.github/workflows/ci-pr-ready.yml` (pushed by the owner)**

**Depends on:** Tasks 4–28 merged.

**Files:**
- Modify (protected): `.config/nextest.toml` (a nextest `desktop` profile with a `desktop` test group, `max-threads = 1`, filter `test(/^desktop_/)`, run-ignored only); `test-params.toml` (`[budgets]` `gui_stall_p99_ms = 16`, `event_to_ui_ms = 100`, `cold_start_ms = 2000`, `deploy_folder_mib = <Task 0's trimmed size + 10 %>`); `.github/workflows/ci-pr-ready.yml` (the `bench` and `deploy` presets read the budgets; no desktop group in CI, P4a-R31; the owner pushes it, P4a-R32).
- Create: `apps/lil-ui-kit/bench/{bench_gui_stalls.qml, bench_event_to_ui.qml}`, CTest `bench_cold_start` (label `bench`), `tools/desktop/run-desktop-group.ps1` (sets `LIL_OWNER_DESKTOP=1`, runs `cargo nextest run --profile desktop` and `ctest --preset desktop`, writes a summary for the PR body); `apps/lil-ui-kit/tests/tst_layout_full.qml`.
- Modify: `apps/lil-ui-kit/CMakeLists.txt` (CMake reads `test-params.toml`'s `[budgets]` lines with `file(STRINGS)` and passes them to the benchmarks as `LIL_BUDGET_<NAME>` environment variables); `README.md` (a short "Build and run the app" section: the developer shell, `cmake --preset dev`, the deploy target, and the Settings panel or `lil doctor` for the notification setup).

**Interfaces:**
- Consumes: every earlier task.

- [ ] **Step 1: Write the checks:** `tst_layout_full::test_default_layout_has_every_panel_of_this_plan` and `test_float_and_redock_every_panel_keeps_its_state` (APP-001, ARCH-116: the seven real panels, now that they exist); `bench_gui_stalls` (the shell with Jobs at 8 × 10 Hz previews, Hardware live and Home, under the fake feed; p99 heartbeat lateness and frame interval < `LIL_BUDGET_GUI_STALL_P99_MS`); `bench_event_to_ui` (fake events stamped at receipt; job progress p99 < `LIL_BUDGET_EVENT_TO_UI_MS`; a confirmation shows in the frame after it arrives, APP-139); `bench_cold_start` (the deploy folder, 10 warm starts with `--no-activate`, `interactive` < `LIL_BUDGET_COLD_START_MS`).
- [ ] **Step 2: Run them.** Run: `ctest --preset qml -R tst_layout_full` and `ctest --preset bench` (about 6 minutes; ask first). Expected: PASS.
- [ ] **Step 3 (owner present, about 10 minutes, with the PC left alone):** the desktop group, `pwsh -NoProfile -File tools/desktop/run-desktop-group.ps1` (P4a-R31): `desktop_toast_posts_under_the_shortcut_aumid`, `desktop_toasts_state_is_ready_on_this_pc`, `desktop_shortcut_and_handlers_round_trip_under_a_test_scheme`, `desktop_power_request_sets_and_clears`, `desktop_second_launch_comes_to_front`, `desktop_toast_url_brings_the_confirm_window_to_front` and `desktop_set_up_button_writes_the_shortcut_and_handlers`. Expected: each passes, and the script removes everything it registered. If the owner answered no to P4a-R31's QA-042 clarification, these stay on the owner's manual list by name instead.
- [ ] **Step 4 (owner present, about 30 minutes with a reboot):** `bench_gui_stalls` again while `spikes/app-shell/comfy-load.ps1` saturates the GPU (APP-135), and the cold start after a reboot and after a Defender scan (APP-136); results go in the PR body.
- [ ] **Step 5: The final checks.** Run: `cargo run -q -p lil-cli -- dev spec-tests` (no plan-4a entry left in `expected-fail.toml`), `cargo run -q -p lil-cli -- dev spec-report` (1080 rules, 0 dangling), `cargo run -q -p lil-cli -- dev privacy-scan --tree HEAD`. Expected: all exit 0.
- [ ] **Step 6: Commit.**

```bash
git commit -m "close-out: full layout checks, owner-run desktop group, GUI stall, event-to-UI and cold-start benchmarks, build notes"
```

---

## Owner manual checks

Short checks for the owner, at the end of the plan (and on the PRs that name them), each recorded in the PR body or the milestone report (QA-064):
1. **Toasts:** on the app's first start, use "Set up notifications" (or run `lil doctor` and answer `y` to the shortcut and the handlers). With no LILITH window in front, start a suite job you would run anyway (in this plan the only product jobs are node installs, `lil call engines.comfyui.install_node`, which asks for your confirmation first, as an `exec` call should; plan 6a adds generations). A "Job finished" toast pops when it ends; a click brings the app to the front on the Jobs panel. (Progress toasts and keep-awake need a GPU job; this plan has no product GPU tool, so the desktop group in CI checks them, and you see them with plan 6a's Generate panel.)
2. **A confirmation with no app open:** close the app; run `lil call resource.delete` on a throwaway resource. A "Confirmation waiting" toast appears; "Review" brings the confirm window to the front with Deny focused, and Enter alone does nothing; answer with a click. Run the same `lil call` again with `--idempotency-key <the retry token it printed>`: it runs once.
3. **Focus and fullscreen:** with a game or a fullscreen video running, toasts land silently in the notification centre; turn on "Pause GPU work while a game or video is fullscreen" in the Settings panel (Task 27), and Jobs shows the lane paused for "fullscreen" and resumes after you leave; a Pause you pressed yourself stays.
4. **Narrator:** with Narrator on, tab through the top bar, each panel and the confirm window; every control is read by name; record whether KDDockWidgets' title bars and tabs are read (APP-156).
5. **Deployed folder off PATH:** copy `build/dev/deploy` somewhere fresh, run `lil-image.exe` from a plain terminal with no developer environment; it starts in under 2 s and finds its core beside it.
6. **Cold start:** after a reboot, and after a Defender scan of a fresh copy, the first start is under 2 s (APP-136).
7. **Windows Hello:** `lil dev approve` on a real PR unlocks with Windows Hello, and with Hello cancelled falls back to your passphrase (QA-088).

## Questions for the owner

Each has the answer this plan uses unless you say otherwise; the places it touches are marked "(owner question Qn; default shown)". A "no" amends the plan before Task 3 (see the header).

1. **Q1. Who may change the notification, quiet-mode, auto-pause and keep-awake settings?** Recommended: **only you**, from the Settings panel (Task 27), through a new `app/desktop.set` that only a verified app may call, as with caps. It adds one method to APP-039's closed list, so it needs your yes. If no, they become an ordinary tool that agents may call too.
2. **Q2. Claude Code's own notices (ARCH-103) and "an agent opened something" (APP-065) need a toggle that APP-146's six categories don't have.** Recommended: **a seventh category, "Claude Code and agent activity", on by default.**
3. **Q3. Toasts while a LILITH window is in front.** Recommended: **don't post them;** the app already shows the same news (badges, the Confirmations panel, the "While you were away" card), so a toast would repeat it (P4a-R7).
4. **Q4. The diagnostics bundle and your machine.** QA-084 already removes secrets, Claude Code profile labels and e-mail addresses. Recommended: **also replace your Windows user-profile folder with `%USERPROFILE%` and the computer name with `<machine>`,** so a bundle you might attach to a public issue doesn't carry them.
5. **Q5. Setting up notifications.** Toasts, and answering confirmations with no app open, need the Start-menu shortcut and the `lil:` handlers. Recommended: **the app offers "Set up notifications" on its first start and in the Settings panel, and writes them only after your click;** `lil doctor` keeps its own `y` path (P4a-R26).
6. **Q6. Answer buttons on toasts.** APP-022 allows a "Deny" button, since denying isn't irreversible, but a toast click carries no caller identity and shows none of the details. Recommended: **no answer buttons; only "Review", which opens the confirm window** (P4a-R9).
7. **Q7. The window title in plan 4a,** while the window shows only shell panels and the Start-menu entry says LILITH. Recommended: **"ATHANOR — <project>",** as APP-164 says for the Image studio's window, so it doesn't change when plan 5 adds the canvas (P4a-R25).
