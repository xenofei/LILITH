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
- Rust 1.99.0 (MSVC), edition 2024, `cargo-nextest` 0.9.146 in CI (the Sandbox runner uses whichever version the host has, for both archive and run, and records it; its flags were checked on 0.9.148), `cargo-deny`, as in plan 3.
- Qt 6.12.0 (`msvc2022_64`) + Qt WebEngine 6.140.0; cxx-qt 0.10.0 with cxx-qt-cmake 0.10.0 (Corrosion v0.5.2); KDDockWidgets 2.5.0 if Task 0's build check passes, else 2.4.1 (APP-005); CMake 4.4.4 and Ninja 1.13.2; MSVC 14.44 (VS 2022 Build Tools); Qt Quick Test through CTest.
- Windows: classic WinRT toasts through the `windows` crate 0.62 (no Windows App SDK), `KeyCredentialManager` and DPAPI.

**Spec:** `docs/superpowers/specs/2026-10-12-lilith-design.md` (1079 rules today; this plan adds one, ARCH-127, so `EXPECTED_RULE_COUNT` in `crates/lil-cli/src/dev/spec_report.rs` becomes 1080). Terms are those of `docs/GLOSSARY.md`. Task 2 adds the spec's **Appendix G, "App shell and plumbing (plan 4a clarification)"**. The plan follows roadmap row 4a (`docs/superpowers/plans/2026-10-12-roadmap.md`).

**Execution method:** subagent-driven (chosen by the owner), with heavy review on every task: quality over speed, no rush.

**Starts after:** PR #24 (`docs-layout`) is merged to `main`. Every task then branches from `main` (Global Constraints, "Branches and PRs").

**Owner decisions 2026-10-22:** the owner answered all eight questions; each is recorded below as a decision, and the places it touches are marked "(owner decision 2026-10-22)". **Owner decisions 2026-10-23** (rounds 12 and 13) and **2026-10-24** (round 14) follow them at the end; with the rulings of 2026-10-24 they change Tasks 2, 3, 5, 6, 7, 10, 18, 22, 26, 27, 28, 29 and 30 (P4a-R36 to P4a-R41).

**Checkpoint after Task 14:** the Qt-free core (Tasks 0–14, and Task 30's Sandbox runner) gets a scoped review and an owner check-in before Task 15 starts the Qt work (see "Checkpoint A").

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
| `lil dev` commands | — | `desktop-tests` (the Sandbox runner, and `--host` for the toast and foreground tests, P4a-R39); `merge-flow` (the one-click merge, P4a-R38) | 30; 28 |
| Spike documents | 01–09 | `docs/spikes/10-app-shell.md`, `docs/spikes/11-windows-integration.md` | 0, 1 |
| Ruling ids | — | P4a-R1 to P4a-R41 (below; P4a-R36 to P4a-R40 added on 2026-10-23, P4a-R41 on 2026-10-24) | — |
| Noted for plan 6a | — | the `jobs` table's `priority` CHECK admits `interactive`, `user` and `agent` only; plan 6a's `live` class needs a table rebuild in its own migration | — |

## Decisions recorded for this plan

The owner decisions and rulings of plans 2a, 2b and 3 bind this plan. These owner decisions bind it too (2026-10-19 to 2026-10-21):
- **KDDockWidgets 2.5.0** after a half-day build check on Qt 6.12.0 (Task 0), else 2.4.1 (APP-005).
- **A minimal accessibility rule** (APP-156) with a Narrator smoke check in each milestone's manual list.
- **Classic WinRT toasts** with a per-user Start-menu AUMID shortcut and protocol activation into `lil:` URLs; no Windows App SDK (APP-022).
- **Windows Hello:** a TPM KeyCredential-derived key-encryption key if Task 1 proves the signatures deterministic, else Windows Hello gating a DPAPI-protected key; the passphrase file stays the fallback (QA-088). (2026-10-23: Task 1 left the route open, so Task 28 starts with a route step, and the prompt must come to the front of every application; P4a-R38.)
- **CORE-160's queue controls** (class order, `job.move`, batch cancels, Pause GPU work, fullscreen auto-pause) are in this plan with the Jobs panel; the live class and the Generate modes wait for plan 6a.
- **Keep-awake** is in this plan (ARCH-127, added by Task 2), and so is **Home's "While you were away" card** (APP-111, amended by Task 2).
- **VC++ runtime DLLs ship app-local** (OPS-094).
- **The shell is simply LILITH** (APP-164).
- The plan-3 review findings R1–R4 and the `sed` `w`/`W` form, carried under P3-R34, are not in this plan (P4a-R22).

### Rulings made in this plan

Each stands unless the owner says otherwise. Rulings made while the plan runs are added below with their reasons (QA-090).

- **P4a-R1** `lil-qt` is a member of the cargo workspace with its Qt parts behind the `qt` feature (off by default, `cxx-qt` optional), and CMake builds it through `cxx_qt_import_crate(... FEATURES qt LOCKED)` against the workspace `Cargo.lock`. Why: `cargo nextest`, `clippy` and `cargo deny` then cover its logic with no Qt on the runner (QA-068). Fallback if Task 0 finds the feature route broken: `lil-qt` leaves the workspace with its own lock file, and its Qt-free modules move to a `lil-qt-core` crate in the workspace. Cost if wrong: one crate split.
- **P4a-R2** Toast activation uses a scheme per channel: `lil` (stable), `lil-dev` (dev), `lil-test-<id>` (test channels, desktop group only). Any channel's app opens a raw `lil://` ref. Why: Windows keeps one handler per scheme, and a dev core's confirmation ids mean nothing to a stable app. Cost if wrong: a dev build's links say `lil-dev:`.
- **P4a-R3** `lil:open/<target>` carries the target as base64url without padding (RFC 4648 §5); a raw `lil://` ref is percent-decoded once; the handler reads the raw command line and never re-splits it. Why: quotes, spaces and percent-encoding differ by launcher, and a base64url argument has none of them.
- **P4a-R4** `lil --confirm-window` is spelled `lil confirm-window [<id>]`. Why: `crates/lil-cli/src/main.rs` (protected) requires a subcommand, so a bare global flag can't run; Task 2 records the spelling as an ARCH-016 and APP-049 clarification.
- **P4a-R5** Pause GPU work is kept in memory with two reasons, `manual` and `fullscreen`; the lane is paused while either holds, `app/queue.resume` clears only `manual`, and leaving fullscreen clears only `fullscreen`. A core restart ends a pause; the jobs it held that never reached an engine are then held by the restart hold instead (P4a-R41). Why: an automatic resume must never undo your own pause. (owner decision 2026-10-23) Each reason has a scope, `all` or `agent_only`; both of this plan's reasons are `all`; `manual` is the review's "owner" reason under this plan's name; a held job lists its reasons in `pause_reasons` (Appendix G).
- **P4a-R6** The fullscreen poll runs every 2 s, in one poll for the quiet mode and the auto-pause; leaving fullscreen counts after 2 polls in a row. Why: the call is cheap, a game that starts pauses new work within 2 s, and an alt-tab doesn't flap the queue.
- **P4a-R7** No toast is posted while a connected suite window is the foreground window; the app shows the same news itself (owner decision 2026-10-22).
- **P4a-R8** Notification text goes through `lil_parse::shown_text`: C0 and C1 controls removed; the bidirectional controls and zero-width characters Appendix G lists shown as `⟨U+XXXX⟩`, except a U+200D ZERO WIDTH JOINER inside an emoji ZWJ sequence, which is kept: the element before it is an `Extended_Pictographic` code point, optionally followed by one `Emoji_Modifier` (U+1F3FB–U+1F3FF) or by U+FE0F, and the code point after it is `Extended_Pictographic`; every other U+200D is shown (plan 4a clarification, 2026-10-24: follows UTS #51 emoji ZWJ sequences); the length limit counts `char`s over the whole text; then XML-escaped into the toast. Why: toast text comes from tool arguments and engine output (ARCH-060); this is the notification part of the parked AI-surfaces revision's normaliser rule, without its id.
- **P4a-R9** A confirmation toast has two buttons, "Open" and "Deny" (owner decision 2026-10-22). "Open" and the body open the confirm window, where approving still needs a real click (APP-044). "Deny" denies straight from the toast: denying isn't irreversible, so APP-022 lets it act without a window. A URI activation carries no caller identity, so the deny is authenticated by a token: the core makes a 128-bit random token for each confirmation toast it posts, puts it only in that toast's Deny URL (`<scheme>:deny/<confirmation id>/<token>`), keeps it in memory bound to that confirmation, and accepts it once. The app that receives the URL answers through its verified `ui:*` connection with `app/confirmation.answer {answer: "deny", toast_token}`. The token comes from the OS random source, as the ids do. It stops web pages and other launchers that don't hold the toast. It doesn't stop a program already running as you: such a program can read the activation's command line while it lives, and probably Action Center's store, and so can deny a confirmation that has a toast. That stays inside the trust boundary of §1.3 (such a program can already stop the core) and the harm is deny-only, one confirmation per token, which the caller can ask again; it can never approve. No log line holds a token: the app's and the core's logs cut a `deny/<id>/<token>` tail to `deny/<id>/…`, and the diagnostics `Redactor` scrubs the form too.
- **P4a-R10** The confirm dialog takes an answer only from a click, or Space or Enter on the focused button, at least 500 ms after the dialog became visible and while its window is in front. Initial focus is on Deny, and Approve is never the default button. Why: a key already held down, or a click meant for the window underneath, must not answer.
- **P4a-R11** The confirm-window process has its own single-instance pipe, `\\.\pipe\lilith-app-image-confirm-<userSID>-<channel>`, so several toast clicks reuse one window; while the full app runs, `confirm` URLs go to it first.
- **P4a-R12** In this plan `origin` is derived from the caller and typed (owner decision 2026-10-23): `{surface: "panel", id: "<app>"}` for a verified `ui:<app>`, `{surface: "mcp"}` for Claude Code, `{surface: "agent_session"}` for the in-app assistant, `{surface: "cli"}`, and `{surface: "core"}` for a job the core submits itself. The director plan adds `{surface: "director_task", id: "<job_id>", step?}`; a per-panel id waits for a `tools/call` field (a later protocol change). The jobs log lines carry it too.
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
- **P4a-R25** The window title is `LILITH — <project>` until the Image studio exists (plan 5 adds its canvas), then `ATHANOR — <project>` (APP-164; owner decision 2026-10-22); the confirm window's is `LILITH — Confirm`.
- **P4a-R26** The channel's scheme, the `.lilref` association and the AUMID shortcut are written only after your own input: `lil doctor` after you type `y`, or the "Set up" button: in the Settings panel always while they are missing, and on a card on the app's first start (no `state.json` yet) while they are missing (owner decision 2026-10-22). The core never writes them.
- **P4a-R27** The core tracks each verified `ui:*` connection's process in telemetry with role `app` while it is connected. Why: Hardware's "who's using the GPU" shows the app, and plan 7a's RAM admission reads its working set (IMG-041).
- **P4a-R28** Model families are read only from `.safetensors`, `.sft` and `.gguf` headers, within fixed limits; pickle formats are never parsed: they are opened only to hash them (ruling of 2026-10-24).
- **P4a-R29** Desktop-touching tests run only when `LIL_SANDBOX=1` (inside the Sandbox runner, P4a-R31) or, for the toast and foreground tests, when `LIL_DESKTOP_HOST=1` (the host run, P4a-R39; owner decision 2026-10-23); anywhere else they panic before touching the desktop (QA-042).
- **P4a-R30** `lil-mcp` may also depend on `lil-platform` and `toml`, widening plan 3's list (`lil-client`, `lil-protocol`, `rmcp`, `tokio`, `serde_json`, `tracing`). Why: starting an app without focus and the shared log setup are Windows calls that only `lil-platform` may make (ARCH-111), and the app registrations are TOML; neither adds policy or state, so ARCH-012 still holds. `crates/lil-mcp/Cargo.toml`'s comment and the dependency-boundary test change with it. Cost if wrong: two crates in `lil-mcp`'s tree.
- **P4a-R31** The desktop group runs in a **Windows Sandbox runner** (owner decision 2026-10-22, Q8; Task 30): `lil dev desktop-tests` checks that Windows Sandbox is available, writes a `.wsb` that maps a read-only bundle (a nextest archive of the desktop tests, the source copy its `--workspace-remap` needs, the deployed app and the VC++ DLLs) and a writable results folder, starts the Sandbox, waits for a results-folder sentinel within a timeout, collects the JUnit files and closes the Sandbox. CI runs only the offscreen and Qt-free tests, because hosted runners can't run Windows Sandbox. (owner decision 2026-10-23, superseding Q8 for these tests) S1.9 found the Sandbox's logon token elevated, with no de-elevated launch that works, and the suite never runs elevated, so the toast and foreground tests run on your own PC instead (P4a-R39), under QA-042's new exception; the Sandbox runner keeps the Qt window grabs, protocol activation, the power request, the Rust tests and its own lifecycle, which passed 3 runs of 3 at 132 s a run (`docs/spikes/11-windows-integration.md`).
- **P4a-R32** Workflow files (`.github/workflows/**`) are changed only in commits the owner pushes, because agents' tokens can't change workflows (QA-079). If Task 0's CI measurement puts the `pr-ready` job over 50 minutes with Qt in it, Task 15 splits the Qt build into its own job with a Qt cache, and `dev/merge-guard.toml` names the new required check (protected).
- **P4a-R33** Timing values with no spec source: reconnect backoff from 250 ms, doubling to 5 s; the notification path's reachability cached for 10 s; the unresponsive check every 5 s; the layout saved every 60 s while it changes. Why: each is well under the spec's own bounds (the 30 s stall rule, the 30-minute confirmation lifetime) and cheap. Cost if wrong: a tuning change.
- **P4a-R34** Switching workspace is `Session::set_workspace(id)`, a new `core/hello` with that workspace id; it never changes `suite.toml`'s default. The switcher's "Make this the default" is a separate, explicit action that calls `workspace.set_default`. In M1 the core serves one workspace, so switching to another one answers `workspace.unsupported` naming the served one (ARCH-090). Why: `workspace.set_default` changes every `cli` and `mcp` caller's workspace, which a click in the app must not do as a side effect.
- **P4a-R35** If Task 0's S0.9 finds that Qt Quick, KDDockWidgets or the grabs don't work offscreen on Windows, those GUI tests run in the Sandbox runner on the Sandbox's own screen: `lil dev desktop-tests --suite qml` or `--suite grabs` (Task 30) maps the CMake build tree and the Qt pair's folder read-only at their host paths and runs the CTest commands exported with `ctest --show-only=json-v1`, without `QT_QPA_PLATFORM`; about 8 minutes a run, and the ten-run flake guard repeats each command inside one Sandbox run (about 20 minutes). CI then runs only the Qt-free and build tests. Task 1's S1.9 checks that a Qt window renders in the Sandbox.
- **P4a-R36** No progress toasts (Task 1's S1.5 FAIL path; `docs/spikes/11-windows-integration.md` §7), and (owner decision 2026-10-24, round 14) no live progress toasts later either: a job's progress is shown in the Jobs panel, and toasts go out only for finished work or for what needs you. Every update returned `Succeeded` and the documented calls behaved, but a second pop was seen while a progress toast updated on Windows 11 25H2, and a `SuppressPopup` toast was seen to pop on a 26H2 pre-release build, possibly the control toast posted just before it. So the `job_progress` category is withdrawn: Task 5 leaves out the toast update call, Task 10 the progress toast, the category and their tests, and Task 27 has no "Job progress" toggle; an old `job_progress` key in `suite.toml` is kept and ignored, like any unknown category key. Appendix G's progress bullet says so instead of copying the contract in Global Constraints.
- **P4a-R37** The AUMID shortcut carries `System.AppUserModel.ToastActivatorCLSID`, built directly by Task 5 (Task 1's S1.2p; spike 11 §7). Spike 11's rule adds the CLSID when a test that reads the notification centre itself fails or can't run, and on the test PC it can't run: UI Automation saw no toast and no centre entry there. So instead of an untestable condition, Task 5 writes a CLSID fixed per channel into the shortcut and registers it per user, `<classes root>\CLSID\{<CLSID>}\LocalServer32 = "<exe>"`, with the shortcut, and with it come P4a-R26's writes, the QA-038 test roots, `lil doctor`'s shortcut check and `notification_path.shortcut` (`wrong` without the CLSID); APP-022 records that no COM activator runs. Toasts activate by protocol; the CLSID only keeps a missed toast in the centre. Why: the evidence is mixed (on 26H2 both forms were in History 15 s after the banner, the centre unchecked; on 25H2 a missed plain-shortcut toast wasn't found in the centre), a missed confirmation toast that vanishes would hide "Confirmation waiting", and the CLSID is the safer side of the spike's rule. Cost if wrong: one registry key per channel that a plain shortcut wouldn't need.
- **P4a-R38** Windows Hello's route is Task 28's first step (Task 1's S1.8 left it open; spike 11 §7). Task 28 binds the window-owned calls `KeyCredentialManager.RequestCreateForWindowAsync` and `KeyCredential.RequestSignForWindowAsync`, which `windows` 0.62.2 lacks (bindings generated from newer metadata with `windows-bindgen`, or a raw COM binding), and proves the prompt's placement: owned by a real window that is in front, never `GetConsoleWindow()`, the prompt comes to the front of every other application, a fullscreen one included, with no input during the call. Then it builds the KEK route (P4a-R23), else DPAPI with the Windows Hello gate if that prompt is proven in front, with the passphrase as the fallback either way (owner decision 2026-10-23, round 12; QA-088). Task 28 also builds the one-click merge flow (owner decision 2026-10-23; QA-036): you approve a PR's card; `lil dev merge-flow <PR> --summary <file> --audit <file>` then shows, in a window in front, the summary (title, files, the protected files touched, CI on the exact head) and the independent audit (a review written by an agent that didn't make the change), and asks for your Windows Hello or passphrase authentication; only after it succeeds does it post the reviewer approval (when protected files changed) and run `lil dev merge`. `lil dev merge` itself asks for the same authentication on every merge, protected files or not (owner decision 2026-10-23: you are always the one who authenticates a merge), so no merge happens without it, whoever runs the command; Task 28 changes QA-036's settings deny patterns and QA-061's hook-pattern tests only as far as the flow needs. Until the flow is merged, you run `lil dev approve` and `lil dev merge` yourself.
- **P4a-R39** The host run (owner decision 2026-10-23, round 12; QA-042's exception): `lil dev desktop-tests --host` (Task 30) runs the toast and foreground tests on your PC: `desktop_toast_posts_under_the_shortcut_aumid`, `desktop_toasts_state_is_ready_on_this_pc`, `desktop_second_launch_comes_to_front` and `desktop_toast_url_brings_the_confirm_window_to_front`. It refuses when elevated; shows the hands-off pop-up (topmost, click-through, never takes the focus, at least 10 s of warning and a countdown, then a "done" message); runs the tests serialised with `LIL_DESKTOP_HOST=1` on a test channel; and removes, and checks gone, every shortcut, registry key, CLSID and toast they wrote. The foreground tests start the second launch with `ShellExecuteW` from a window that holds the front, as S1.9's foreground item did, because no synthetic click on a toast was ever tried and UI Automation found no toast on the test PC; a real toast click stays in the owner's manual check 1 (notifications). Toast tests read `ToastNotificationManager::History` and the toast's own `Activated`, `Dismissed` and `Failed` events, never UI Automation. The Sandbox runner keeps the rest of the desktop group.
- **P4a-R40** The 2026-10-23 review's plan-4a items (owner decision 2026-10-23, round 13: all its reservations, as one recorded exception to the spec freeze). Task 2 writes them into CORE-117, CORE-160, CORE-161, APP-146 and Appendix G, which governs where Global Constraints' contract bullets differ:
  - `origin` is typed, `{surface, id?, step?}` (P4a-R12), and is written into the jobs log lines; `estimate` is the review's `estimate_at_submit`, never changed after submission (Tasks 6 and 7);
  - `Estimate` comes in parts with an `unknown` list, estimates add within a part only, and a queued job's ETA counts the work ahead of it (Tasks 7 and 22); the chip's text doesn't change;
  - pause reasons carry a scope and a held job lists them in `pause_reasons` (P4a-R5; Tasks 6 and 22);
  - notifications: open categories, ask-toasts with a lasting in-app copy, FYI toasts for jobs not started from a panel coalesced per origin by replacement (not by the in-place update P4a-R36 rules out), and the reserved confirmation `group` (Task 10);
  - the review's plan-writer notes for this plan: Jobs and Home show an unknown kind, substatus, origin, lane or class by its raw name, in the `queued` group, never failing or hiding the row (Tasks 22 and 26); the `lil:` activation and the confirm window sit in shared shell code, with the target app resolved through the handler registry (APP-061), not hard-wired to `lil-image` (Tasks 19 and 21); the top bar keeps a "Needs you" slot with the pending-confirmation count (Task 20); the Backups panel and the status line say "No backups yet: your work exists only on this disk" until a backup exists (Task 25); every `lil-ui-kit` action declares the tool it calls, or `owner_only`, `view_only` or `manual_only`, and a CTest check fails on an undeclared one (Task 18).
- **P4a-R41** The restart hold (a finding of the same review, ruled into this plan on 2026-10-24; ARCH-022, CORE-160, Appendix G). **It changes an invariant:** it is an exception to ARCH-022's first sentence, since a queued job held by a restart isn't `interrupted`. It is a ruling, not an owner decision, so it needs your OK through Task 2's merge: Task 2's PR body and its merge card's owner summary name this exception. A core restart never turns queued batches into failed jobs. At startup each job that never reached an engine (substatus `waiting`, `waiting_for_vram` or `queue_paused`, and no `external_id`) stays queued with `pause_reasons: ["restart"]` instead of becoming `interrupted`. A director task (`assistant_task`) is never held and stays under ARCH-022 until the director plan (roadmap N33), and a held job keeps its cap reservation. `restart` holds those jobs only, never the lane, so new jobs run. `QueueState` gains `restart_held`, and `held` counts those jobs too. Jobs shows "Resume held jobs (<n>)" whenever `restart_held` is above 0, whatever `paused` says (Task 22); it calls `app/queue.resume {held_only: true}`, which releases only the lane's restart-held jobs and never removes `manual` or any other reason, so your own pause survives it (a plain `app/queue.resume` releases them too, besides clearing `manual`). Jobs labels the reason "Held after a restart". The hold lives in memory, so another restart holds them again. A start that held jobs posts one `job_failed`-category toast ("Jobs held after a restart" / "<n> jobs wait until you resume them in Jobs"), and Home's "While you were away" lists them (Task 26). `job.retry {batch_id}` retries only the newest attempt at each index that failed, was interrupted or was cancelled. It is idempotent: a second call creates nothing new for jobs already retried. The retries keep their `batch_id` and `index`. Every other job without a final state follows ARCH-022 and Appendix F as before. Why: a reboot for a Windows update would otherwise turn a 200-job batch into 200 failed jobs, and the hold keeps ARCH-022's "never re-run by itself". Tasks 6, 10, 22 and 26 build it. Cost if wrong: held jobs wait for a click that an `interrupted` job would have needed anyway.

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
  - `crates/lil-cli/src/dev/desktop_tests.rs` and `crates/lil-cli/src/dev/mod.rs`, Task 30 (the Sandbox runner);
  - `test-params.toml`, Task 29 (the shell budgets);
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
- **Isolation and desktop tests** (QA-038, QA-040–042; P4a-R29): no automated test posts a real toast, writes the real `HKCU\Software\Classes`, creates a real Start-menu shortcut, takes the real foreground, holds a real power request or prompts Windows Hello, except the **desktop group**: Rust tests named `desktop_*` and marked `#[ignore = "desktop"]` (the ones that need the app start the deployed `lil-image.exe` from `LIL_DEPLOY_DIR` and observe it through `lil-platform`, so none needs the `qt` feature or Qt's test DLLs), run serialised only where QA-042 allows: in this plan, the Windows Sandbox runner of Task 30 (`lil dev desktop-tests`, about 8 minutes a run, P4a-R31), except the toast and foreground tests, which run on your PC through `lil dev desktop-tests --host` with the hands-off pop-up (P4a-R39; owner decision 2026-10-23, QA-042's exception); CI runs only the offscreen and Qt-free tests. Test channels use `FakeDesktop` and the classes root `HKCU\Software\LILITH-test-<id>\Classes`, which the shell never reads.
- **Privacy** (QA-086): no user-profile paths, names or e-mail addresses in code, fixtures, test names, grabs or spike documents. Grabs use fixture workspaces with synthetic names. Spike documents name hardware only as far as a measurement needs, as spikes 02 and 04 do.
- **Copy and terms** (QA-089, APP-163, APP-164): user-facing copy calls a candidate a **take** and uses "version" only for a sealed result; the window title is "LILITH — <project>" until the Image studio exists (P4a-R25, owner decision 2026-10-22); the shell is LILITH. Text uses the glossary's terms: "ComfyUI launch profile", "Claude Code profile" and "model profile", never "profile" alone; "GPU lease", never "lease" alone; "confirmation", never "approval"; "layout preset", never "preset" alone. Task 2 adds this plan's new terms.
- **Accessibility** (APP-156): every `Lilith.UiKit` control sets `Accessible.role` and a non-empty `Accessible.name`, takes keyboard focus in tab order and works from the keyboard; custom delegates (job rows, gauges, chart tiles, the app's own notification cards) set their role and name themselves. Each panel task runs `A11yWalk` (Task 18) over its panel.
- **Spec values used verbatim:**
  - throttle: leading-edge, then at most **10 Hz** for job progress and **1 Hz** per telemetry series; confirmations never throttled (APP-027);
  - GUI-thread stalls p99 **< 16 ms** under ComfyUI load (APP-135); cold start **< 2 s** to an interactive window, after a reboot and after a Defender first scan (APP-136); event-to-UI **< 100 ms** for job progress, immediate for confirmations (APP-139);
  - stall rule: no progress for **30 s** → "unresponsive", display only; the watchdog at **max(3 × p50, 2 × p90)** of history, else **3 ×** expected (ARCH-072);
  - progress toasts: none (owner decision 2026-10-24): progress shows in the Jobs panel, and the `job_progress` category is withdrawn (APP-146; P4a-R36);
  - notification defaults: progress **off**, every other category **on**; quiet while fullscreen **on** (APP-146); fullscreen auto-pause **off** (CORE-160); keep-awake for GPU jobs **on** (ARCH-127);
  - cap gauges and alerts at **50 / 80 / 100 %** (APP-121, CORE-089);
  - Hardware opens on a **15-minute** live window and zooms out to **24 h** (APP-129); no CPU temperature (APP-128);
  - layout restore order: create the windows → register the dock factory → restore → clamp every window to its screen's available area (APP-003); M1 keeps "Default" plus the last state (APP-004);
  - single instance: `\\.\pipe\lilith-app-<app>-<userSID>-<channel>`, `first_pipe_instance` and the core pipe's user-only descriptor (APP-013);
  - the out-of-memory ladder retries at most **twice** (CORE-163); a pending confirmation expires after **30 minutes** (CORE-115);
  - the AUMID is `LILITH.Suite.<Channel>` (QA-038).
- **Names that are contracts.** Task 2 copies the sub-bullets of this bullet into spec Appendix G, in this order. The protocol bullet goes into Appendix D instead. The two bullets after this one (test-channel tools, test fakes) are not copied. (2026-10-23) Appendix G also carries the owner decisions and Task 1 results of 2026-10-23 and the decisions of 2026-10-24, and governs where the sub-bullets below differ: the typed `origin`, `pause_reasons` and the reasons' scopes, the restart hold and `job.retry {batch_id}`, estimates in parts and their sums, open categories, ask-toasts and per-origin coalescing, the confirmation `group`, no progress toasts and no `job_progress` category, the CLSID on the shortcut, the power-request API, the sleep record and the foreground hand-off (P4a-R36, P4a-R37, P4a-R40, P4a-R41).
  - **Protocol 1.2** (Appendix D): `PROTOCOL = 1.2`; `CAPABILITIES` = `events.v1`, `tools.v1`, `jobs.v1`, `confirmations.v1`, `caps.v1`, `queue.v1`, `desktop.v1`, `context.v1`, in that order. A 1.1 client keeps working. `app/queue.pause` and `app/queue.resume` need `queue.v1`, `app/desktop.set` needs `desktop.v1` and `app/set_context` needs `context.v1` in the shared capabilities; without it the call gets `caller.not_allowed`. New error code `job.not_queued` (`data_conflict`, `never`), message "Only a job that is waiting its turn can be moved; this one is with the engine, running or finished."
  - **Jobs** (CORE-117, CORE-160, ARCH-072):
    - `JobSummary` adds, from plan 4a: `batch_id` (`b_<16 hex>`; every job has one, P4a-R13), `index` (its 0-based place in the batch), `status_seq: [{status, substatus, at}]` (every change in order; at most 64 entries, the first and the newest 63), `origin` (`app:<app>`, `claude_code`, `assistant`, `cli` or `core`, P4a-R12), `engine_device?` (the GPU adapter's name, for a job that held the GPU lease), `phase?`, `subtasks?: [{name, value?, max?}]`, `indeterminate?: true`, `last_progress_at?`, `cache_hit_nodes?: [node_id]`, `all_cached?: true`, `unresponsive?: true` and `estimate?: Estimate` (taken when the job was submitted); times are RFC 3339 UTC.
    - **Phase:** from the class type of the node ComfyUI reports `executing` (the core reads it from the job's graph), the first match winning: contains `Loader` → `loading_models`; contains `TextEncode` → `encoding`; contains `Sampler` → `sampling`; contains `Decode` → `decoding`; starts with `Save` or `Preview` → `saving`; anything else → `other`. From `execution_success` until the outputs are staged the phase is `ingesting`. Other job kinds report the phase their handler sets. `progress.node` stays the raw node id.
    - `subtasks` lists each phase the job reached, in order, as `{name}`; the current one also carries `value` and `max` from the newest `progress` event while that phase lasts. `indeterminate: true` while the executing node has sent no `progress` event.
    - `last_progress_at` is the time of the newest `executing` or `progress` event. `unresponsive: true` while the job is `running` with substatus `executing` and `last_progress_at` (or `started`, before any event) is **30 s** or more ago; the core checks every 5 s and publishes `{kind: "unresponsive", on}` on `job:<id>` when it flips; nothing is cancelled.
    - `cache_hit_nodes` are the node ids of ComfyUI's `execution_cached` event; when every node of the prompt was cached, the job succeeds with `all_cached: true` and the summary text "Nothing changed: ComfyUI served every node from its cache."
    - The watchdog sets `watchdog: "slow"` when the time since `execution_start` passes `max(3 × p50, 2 × p90)` of the job's `estimate.wall_s` (measured history), else 3 × `expected_secs` (plan 4a clarification, 2026-10-24: a history bound of 0 s, from similar jobs that reported no execution window, so no time was measured, counts as no history; the bound only changes how far the clock runs, never when it starts).
    - **Queue order:** class rank `interactive`, `user`, `live`, `agent` (`live` is plan 6a's; no plan-4a job has it); within a class, the lower `order_key` first, where a new job's key is its `seq`. Plan 3's limit of 8 overtakes counts only group moves.
    - `job.move {job_id, to: "next" | "last"}` (`write`) sets the job's `order_key` to one below the lowest, or one above the highest, key among the queued jobs of its lane and class, and returns `{job}`. Only a job in substatus `waiting`, `waiting_for_vram` or `queue_paused` moves; any other gets `job.not_queued`. A move never changes the class and never touches a prompt in the engine.
    - `job.cancel` takes exactly one of `{job_id}` (result `{job}`, as in plan 3), `{ids}` (1–500 job ids), `{batch_id}` or `{all_mine: true}` (results `{jobs: [JobSummary]}`); `all_mine` covers every unfinished job whose idempotency scope names the caller's kind and, for a `ui:*` caller, the same app. None or more than one selector is `params.invalid`.
    - (superseded for `restart_held`, `held_only` and the restart hold, P4a-R41; Appendix G governs) **Pause GPU work:** `app/queue.pause {lane?}` and `app/queue.resume {lane?}` (`lane`: `gpu-heavy`, the default, or `cpu`) return `QueueState {lane, paused, reasons: ["manual" | "fullscreen"], held, withdrawn}`. A paused lane dispatches nothing; each of its queued jobs, whatever its class or caller, shows substatus `queue_paused`; the prompt already executing is never interrupted. A lane is paused while it has any reason; `app/queue.resume` removes only `manual` (P4a-R5). `jobs.list`'s result gains `lanes: [QueueState]`, and the `jobs` topic gains `{kind: "queue", state: QueueState}`.
    - **Withdrawal:** when a lane pauses, each suite job of it in `queued_in_engine` whose prompt `/queue` still lists as pending is cancelled through `POST /api/jobs/{id}/cancel`; its `external_id` is cleared and it returns to `queue_paused`. A prompt that started meanwhile runs on. A job with `external_side_effects` is never withdrawn. On resume, a withdrawn job is posted with a new prompt id only after `/api/jobs/{old}` and `/history/{old}` both show the old one gone (CORE-120). Fail point `jobs.after_withdraw` fires after the cancel's answer and before the row changes.
    - `jobs.list` also takes `since?` (RFC 3339: the jobs whose `updated` is later) and `batch_id?`.
    - **Fullscreen** (P4a-R6): the core polls `SHQueryUserNotificationState` every **2 s**; `QUNS_RUNNING_D3D_FULL_SCREEN`, `QUNS_BUSY` and `QUNS_PRESENTATION_MODE` are fullscreen, and leaving counts after 2 polls in a row that aren't. With `[jobs] fullscreen_autopause = true` (default `false`) fullscreen adds the `fullscreen` reason to `gpu-heavy` and leaving removes it. The `desktop` topic gets `{kind: "fullscreen", on}`.
    - **Keep-awake** (ARCH-127): while any `gpu-heavy` job is `running` and `[power] keep_awake_gpu` is on (default `true`), the core holds one system-required power request through the API Task 1 records, and releases it as soon as none runs. The core logs each sleep it sees (`slept`, with `from` and `to`) and publishes `{kind: "resumed", slept_from, at}` on `desktop`.
  - **Estimates** (CORE-161, APP-140):
    - `job.estimate {tool, args}` (`read`, module `jobs`) validates `args` as a call of `tool` would (`tool.unknown`, `params.invalid`) and returns an `Estimate` in parts (owner decision 2026-10-23): `{local?: {source, samples, gpu_s_p50, gpu_s_p90, wh_p50, wh_p90, energy_cost?}, wall_s?: {source, samples, p50, p90}, money?: {source, p50, p90, upper_bound, currency}, simulated?: {source, p50, p90, currency}, quota?: {source, unit, p50, p90}, unknown: [{part, reason}]}`. A part is present only when it was estimated, with its own `source` (`measured_history`, `price_table` or `prices_yaml`). `unknown` names each part the call needs but couldn't estimate, with `reason` `no_history` (fewer than 3 similar jobs), `no_key` (the tool has no `estimate_key`) or `no_price`; a call that spends no money has no `money` part and doesn't list it. Plan 4a defines `local`, `wall_s` and `money` and produces only `local` and `wall_s`, from `measured_history`; prices arrive with plan 6b, and `simulated` (CORE-171) and `quota` are reserved. An estimate with no part is "no estimate yet". (plan 4a clarification, 2026-10-24) A call of a tool with no `estimate_key` lists `local` and `wall_s` with `no_key`; with a key and too little history, both with `no_history`. `local` and `wall_s` come from the same history rows, and `samples` is their count (at most 20); a similar job whose energy wasn't attributed (it reported no execution window) counts 0 GPU-seconds and 0 Wh. `energy_cost` is `{amount, currency}`, a decimal string and its currency code, given only while `[power] electricity`'s currency is the display currency (plan 4a converts no rate). The estimate a job takes at submission is its handler's: the key is computed from the job's own arguments, so every way of submitting the job, a retry and an automatic re-run included, keys it alike.
    - History (P4a-R14): the newest 20 succeeded jobs with the same `kind` and `estimate_key` in `telemetry.db` `job_stats`; fewer than 3 is `no_history`; p50 and p90 are nearest-rank; seconds are decimal strings with one decimal, Wh with three (plan 4a clarification, 2026-10-24: the newest by `finished_ms`, then by `job_id`; values are rounded half to even). `energy_cost` is `wh_p50` at `[power] electricity`, in the display currency, when that is set.
    - **Adding estimates** (owner decision 2026-10-23): a part adds only to the same part, and `money` and `simulated` only in the same currency; nothing adds across parts. A sum over jobs adds their p50s, and its p90 is that sum plus the square root of the sum of each job's (p90 − p50)²; p90s are never added. A job without the part is counted, never taken as zero: the sum carries `of` (the jobs) and `not_estimated` (those without it), shown as "3 of 12 not estimated". A queued job's ETA is its finish time: the remaining `wall_s.p50` of the job running in its lane (never below 0), plus the `wall_s.p50` of each job ahead of it in lane order (class, then `order_key`), plus its own; jobs among them with no `wall_s` are counted and named ("after 2 jobs with no estimate"). (plan 4a clarification, 2026-10-24) A sum in which no job has the part has no p50 or p90: it reads as not estimated, never as 0. A figure that isn't a finite number of 0 or more counts as not estimated. `money` (and `simulated`) sums are carried per currency, each with its own `of` and `not_estimated`, never converted into one.
    - `estimate_key`: for `comfy.prompt`, the first 16 hex of SHA-256 over the canonical JSON (keys sorted, no whitespace) of its graph with every input named `seed`, `noise_seed` or `filename_prefix` removed; for `test.job`, its `estimate_key` argument (default `"test"`); other tools have none and estimate `none`. (plan 4a ruling, 2026-10-24) The `comfy.prompt` graph also leaves out each node's `_meta` and its free-text inputs: every input named `text`, `text_g`, `text_l`, `prompt` or `negative`, and every string input of a node whose `class_type` contains `TextEncode`. The model names, the sizes and the numeric inputs, which drive the time, stay in, so a new prompt text for the same work is a similar job.
    - `telemetry.db` `user_version = 2` adds `job_stats(job_id TEXT PRIMARY KEY, kind TEXT NOT NULL, estimate_key TEXT NOT NULL, finished_ms INTEGER NOT NULL, wall_ms INTEGER NOT NULL, gpu_seconds_milli INTEGER NOT NULL, system_mj INTEGER NOT NULL)` with an index on `(kind, estimate_key, finished_ms)`; `meter` writes a row when a job with an `estimate_key` succeeds; `wall_ms` is the sum of its execution windows; a version-1 file is migrated forward, keeping its rows. (plan 4a ruling, 2026-10-24) A run with `all_cached: true` measured the cache, not the work, and adds no row; a partly cached run does. The row is written once the job's final state `succeeded` is in (after its Record step), at most once per job id, so a job whose Record step fails, or one interrupted and run again, never counts twice. `job_stats` keeps the newest 20 rows of each `(kind, estimate_key)`, the ones the estimator reads: older rows are deleted in the same transaction as the insert. There is no age-based retention.
    - `telemetry.energy {from, to}` (`read`) → `{from, to, gpu_mj, cpu_mj, system_mj, measured: {gpu, cpu}, energy_cost?}` over the samples in the range, both bounds included, from the finest table that still holds the range (`s1`, then `m1`, then `h1`), with `JobEnergy`'s whole-system formula. (plan 4a clarification, 2026-10-24) A table holds the range while its retention still covers `from`; from `m1` or `h1` a row counts when its bucket holds any of the range, as in `telemetry.series`, so the result equals the sum of that table's `*.energy` points over the range. The rest-of-system part is `rest_of_system_w` over the samples' time: their `dt` in `s1`, a nominal second per sample in `m1` and `h1`. From `m1` and `h1`, `measured` says only that every sample had that figure, since the rolled-up rows don't keep the estimated mark. `energy_cost` is `{amount, currency}` at `[power] electricity`'s rate, in that rate's currency.
    - `Snapshot` adds `gpu_users: [{pid, name, vram_local_mib, sm_permille?, role?}]`: the 8 processes with the most dedicated GPU memory, tracked or not; `name` is untrusted. A verified `ui:*` connection's process is tracked with role `app` while it is connected (P4a-R27).
  - **Out of memory** (CORE-163): the retries are Appendix F's; the final failure is `comfyui.out_of_memory` with the message "ComfyUI ran out of GPU memory, also on 2 retries.", `fix.user_hint` "Close other programs that use the GPU, or choose the model profile's lighter variant, then retry." and `retry: after_resolve`. (plan 4a clarification, 2026-10-24) `user_hint` is an optional text of the fix (QA-003), shown to you beside its UI action ("Retry the job" here) and its agent hint. (plan 4a ruling, 2026-10-24) A job with `external_side_effects`, which is never retried, fails at its first out of memory with the message "ComfyUI ran out of GPU memory. The job has external side effects, so it was not retried." and `fix.user_hint` "Close other programs that use the GPU, or choose the model profile's lighter variant. Retrying runs the paid call again, and it is charged again."; its fix's UI action is "Show the job" (`show_job`), and it gets no `job.retry` agent hint, since a fix of its own other than "Retry the job" keeps the runner from adding one.
  - **ComfyUI launch profile** (ARCH-126; P4a-R15): `[engines.comfyui]` gains `reserve_vram_gb` (a decimal string, default `"1.5"`), `preview_method` (default `"taesd"`), `manager` (default `false`) and `dynamic_vram` (default `true`). An owned start runs `<python> main.py --listen 127.0.0.1 --port <port> --reserve-vram <reserve_vram_gb> --preview-method <preview_method>`, then the Manager flag Task 2 records only when `manager` is true, then the dynamic-VRAM opt-out Task 2 records only when `dynamic_vram` is false, then `<extra_args>`. An `extra_args` entry that is `--enable-assets` or any prefix of it from `--enable-as` on, with or without `=<value>`, stops the start with `engine.not_configured` naming it. `engines.status` entries gain `launch_profile: {reserve_vram_gb, preview_method, manager, dynamic_vram, flags: [string]} | null` (`null` for an attached engine) and `asset_catalogue: "on" | "off" | "unknown"`, probed as Task 2 records. (plan 4a clarification, 2026-10-24) `preview_method` is one of `none`, `auto`, `latent2rgb` and `taesd`, the values ComfyUI takes. When no engine runs, `launch_profile` is the profile an owned start would use; it is also `null` when ComfyUI isn't configured. (plan 4a rulings, 2026-10-24) `reserve_vram_gb` is a decimal string (digits with at most one `.`) or a TOML number, finite, from 0 to 256, kept as its decimal text. A launch-profile value that is out of range, of another type or not one of the listed values refuses only an owned start, with `engine.not_configured` naming the key; `launch_profile` shows that key's default, and attaching, the inventory, `models.report` and the outside-jobs watch go on as usual. An `extra_args` entry that is `--windows-standalone-build` or any prefix of it from `--wi` on, with or without `=<value>`, stops the start in the same way, since that flag turns the asset catalogue on. An owned start always ends with `--disable-assets`, after `<extra_args>`, which wins over anything that would turn the catalogue on; `flags` lists the profile's own flags, those before `<extra_args>`. The catalogue is probed when the engine becomes ready. Only a 404, or a JSON answer without `assets`, is a final `unknown`. A probe that fails in any other way (it can't connect, times out after 2 s, gets another 4xx or a 5xx, or its answer can't be read as JSON) stays `unknown` and is made again on a later supervisor pass, at least 30 s after the last one and at most 5 times in all while the engine stays ready, until the first `on` or `off` (plan 4a ruling, 2026-10-24, fix round 2).
  - **Model inventory report** (CORE-164; P4a-R28): `models.report {}` (`read`, module `engines`) → `{root, refreshed, models: [{path, size, sha256, family, states, model_id?, model_name?, untrusted?}], missing: [{model_id, model_name?, sha256}]}`. `path` is relative to `<root>\models`, with `\`. `states` is a subset of `["orphan", "duplicate", "unknown_family"]`: `orphan` when no `registry/models.yaml` entry has its SHA-256, `duplicate` when another file has the same SHA-256, `unknown_family` when `family` is `null`. `missing` lists the `registry/models.yaml` entries whose SHA-256 no file has. `family` is the first of the safetensors `__metadata__` keys `modelspec.architecture` and `ss_base_model_version`, and GGUF's `general.architecture`; it is read only from `.safetensors`, `.sft` and `.gguf` files, and a safetensors header over 100 MiB, a GGUF with more than 1,024 keys before `general.architecture`, or a string over 64 KiB gives `null`. Family values and file names are untrusted (ARCH-060). Families are cached in `model_hashes.family`. With no `root`, the result is `engine.not_configured`. Nothing is moved or deleted because of a state. (plan 4a clarification, 2026-10-24) The report is the newest inventory of this core run, kept after the engine stops; before the first one, `refreshed` is `null` and both lists are empty. Every `models` entry carries `untrusted: true`, since its `path` and `family` are outside text; `model_id` and `model_name` come from the registry and are trusted. A file whose hash wasn't taken (a cloud placeholder, never read) has `sha256: null`, is `orphan` and has no family. Each `missing` entry is one SHA-256 of a registry entry. A file's family is read again only when its cached size, mtime or file identity changed, or when the last read could not open or read the file (`model_hashes.family_read` is 0); a header that is malformed or over the limits is a read whose answer is `null`. (plan 4a rulings, 2026-10-24) The result also has `registry: "ok" | "missing" | "unreadable" | "no_workspace"`: the default workspace's `registry/models.yaml` was read, doesn't exist, couldn't be read (an I/O error, not YAML or not a list), or there is no default workspace. Unless it is `ok`, no file is `orphan`, `missing` is empty and no entry has a `model_id`: the report doesn't guess from a registry it couldn't read. `duplicate` counts files, not paths: two paths to one file (links with the same volume serial and FileId) are one file. A pickle format (`.ckpt`, `.pt`, `.pth`, `.bin`) is opened only to hash it (CORE-130), never parsed.
  - **Engine environments** (CORE-130): each capture is kept in the suite `state.db` table `engine_captures (engine, captured_utc, capture)`, the newest 10 per engine. `engines.environment {}` (`read`, module `engines`) → `{captures: [EnvCapture] (newest first, at most 2), changes: [{what: "comfyui" | "node" | "driver", name?, from?, to?, change: "changed" | "added" | "removed"}]}` between those two. (plan 4a clarification, 2026-10-24) A capture is taken each time the engine becomes ready (the `comfy.inventory` job); `captured_utc` is Unix milliseconds, one more than the engine's newest when the clock gives no later time. A `node` change names the `custom_nodes` folder, and its `from` and `to` are `<version> (<commit>)`, or whichever of the two is known; `comfyui`'s are ComfyUI's commit, else its version, and `driver`'s the NVML driver version. A value only the newer capture has is `added`, one only the older has `removed`. With fewer than two captures, `changes` is empty. (plan 4a rulings, 2026-10-24) A capture is stored only when its `comfyui`, its nodes or its `driver` differ from the engine's newest stored capture; otherwise nothing is stored, so the newest two are the last two different environments and the 10 kept are the last 10 changes. A driver that isn't known (no GPU reading when the capture is taken) is never a difference and shows no change; but a known driver arriving while the newest stored capture's driver is unknown is stored as a new capture (stored captures are never changed), so a later driver change is compared with it (plan 4a ruling, 2026-10-24, fix round 2). A `custom_nodes` folder whose name starts with `.` (ComfyUI-Manager's `.disabled`) or is `__pycache__` isn't a node. In a capture's `system_stats.system.argv`, an entry that is a path inside `root` is written relative to `root`, so stored and served captures hold no absolute path into the ComfyUI folder.
  - **Notifications** (ARCH-005, APP-022, APP-065, APP-146):
    - (withdrawn 2026-10-24 for `job_progress`, P4a-R36; Appendix G governs) categories `job_finished`, `job_failed`, `confirmation_waiting`, `cap_alert`, `director_decision`, `job_progress` and `agent_activity` (owner decision 2026-10-22); `job_progress` defaults to off, every other one to on. `suite.toml` `[notifications]` holds each by name, plus `quiet_while_fullscreen = true`; `[jobs] fullscreen_autopause = false`; `[power] keep_awake_gpu = true`.
    - `desktop.get {fresh?}` (`read`, module `desktop`; `fresh: true` recomputes `notification_path` instead of serving the 10 s cache, P4a-R33) → `DesktopSettings {notifications: {<category>: bool}, quiet_while_fullscreen, fullscreen_autopause, keep_awake_gpu, keep_awake_available, notification_path: {toasts: "ready" | "not_yet_shown" | "unavailable", shortcut: "ok" | "missing" | "wrong", handlers: "ok" | "missing" | "other_channel"}}`. `app/desktop.set {notifications?, quiet_while_fullscreen?, fullscreen_autopause?, keep_awake_gpu?}` (verified `ui:*` only; owner decision 2026-10-22) edits `suite.toml` with targeted edits and returns `DesktopSettings`; the `desktop` topic gets `{kind: "settings", settings}`. A hand edit of `suite.toml` is read at the next core start, as for caps.
    - **Ready** (the reachability fix): `toasts` is `ready`, `not_yet_shown` or `unavailable`. It is `ready` only when the toast notifier for the channel's AUMID reports `Setting() == Enabled` and the core isn't running elevated, and otherwise `unavailable`, with `toasts_reason: "disabled_for_app" | "disabled_for_user" | "disabled_by_policy" | "elevated" | "no_platform"` beside it in `notification_path`. (plan 4a ruling 2026-10-24; Appendix G governs) `not_yet_shown` is `Setting()` answering `ERROR_NOT_FOUND`, from `Setting()` only, for an AUMID that has never shown a toast: it allows posting and counts as `ready` for Reachable; a failed post, or a later `DisabledForApplication`, `DisabledForUser`, `DisabledByGroupPolicy` or `DisabledByManifest` reading, then decides. It ends as `unavailable` with `no_platform` when this process's first post that returned `Ok` while `not_yet_shown` (counted afresh after any other reading) is 5 s old and `Setting()` still fails so (`lil_platform::desktop::NotShownEnd`, `NOT_SHOWN_GRACE`).
    - A toast is posted only when its category is on, the channel's toasts can post (`ready` or `not_yet_shown`: `ToastsState::can_post()`, with `shortcut: ok`; plan 4a ruling 2026-10-24), and no connected `ui:*` app owns the foreground window (P4a-R7); otherwise nothing is posted or queued. `job_finished` and `job_failed` post once per batch, when its last job ends (`job_failed` when any failed; never for `cancelled`), for jobs of priority `user` or `agent` whose `origin` isn't `core`; `confirmation_waiting` posts for each pending confirmation and is removed when it is answered, expires or is cancelled; `cap_alert` posts for each `caps` alert; `agent_activity` posts for `notify.claude_code` and for an agent caller's `open_in` or `ui.show`; `director_decision` comes with the director plan.
    - **Toast XML** (P4a-R8, P4a-R9): `<toast launch="URL" activationType="protocol"><visual><binding template="ToastGeneric"><text>TITLE</text><text>BODY</text></binding></visual>ACTIONS</toast>`, where ACTIONS is `<actions><action content="Open" arguments="<scheme>:confirm/<id>" activationType="protocol"/><action content="Deny" arguments="<scheme>:deny/<id>/<token>" activationType="protocol"/></actions>` for `confirmation_waiting` (P4a-R9) and empty otherwise; TITLE and BODY are `lil_parse::shown_text` output (C0 and C1 controls removed; U+200E, U+200F, U+202A–U+202E, U+2066–U+2069, U+200B–U+200D, U+2060 and U+FEFF shown as `⟨U+XXXX⟩`, except a U+200D inside an emoji ZWJ sequence, which is kept: the element before it is an `Extended_Pictographic` code point, optionally followed by one `Emoji_Modifier` (U+1F3FB–U+1F3FF) or by U+FE0F, and the code point after it is `Extended_Pictographic`; every other U+200D is shown (plan 4a clarification, 2026-10-24: follows UTS #51 emoji ZWJ sequences); at most 200 `char`s over the whole text, ending `…` when cut), then XML-escaped. A progress toast's binding also holds `<progress value="{progressValue}" valueStringOverride="{progressValueString}" status="{progressStatus}"/>`, and its first post carries `NotificationData` with those three values and `SequenceNumber` 1; each update raises the sequence number by 1. `Tag` is the job, batch or confirmation id; `Group` is the category. A quiet toast has `SuppressPopup = true`. No toast approves anything (owner decision 2026-10-22).
    - **Copy:** `job_finished`: "Job finished" (or "Batch finished (<n> jobs)") / "<kind> · <project, or no project> · <elapsed>", launching `<scheme>:task/<job_id>`; `job_failed`: "Job failed" / "<kind>: <error message>", the same launch; `confirmation_waiting`: "Confirmation waiting" / "<who> wants to run <tool>" (who: "Claude Code", "The assistant" or "A script (lil)"), or for a cap alone "A call would go over your <cap> cap", launching `<scheme>:confirm/<id>`; `cap_alert`: "Spending at <pct> %" / "<cap> cap, <period>"; `agent_activity`: "Claude Code" / the message, or "Claude Code opened <target title>", launching `<scheme>:open/<base64url>`.
    - (withdrawn 2026-10-24, P4a-R36; Appendix G governs) **Progress toasts** (`job_progress`): for a `user` or `agent` job whose `estimate.wall_s_p50`, else `expected_secs`, is over 30 s; posted at `execution_start` when no suite window is in front; updated at most once a second by Tag and Group (`progressValue` 0–1, `progressValueString` "<pct> %", `progressStatus` the phase); removed when the job ends, after which `job_finished` or `job_failed` follow their own toggles.
    - **Quiet while fullscreen:** while the newest poll says fullscreen and `quiet_while_fullscreen` is on, every toast has `SuppressPopup = true`; none is dropped, and confirmations are made quiet like the rest.
    - **Reachable** (ARCH-055, CORE-113, APP-051, QA-016): the notification path can reach you when `notification_path` is `ready`, `ok` and `ok` and `confirmation_waiting` is on, re-read at most every 10 s (P4a-R33). A call that needs a confirmation then gets `pending_confirmation` with no app connected; a non-interactive caller still gets `confirm.unavailable` or `cap.exceeded`. When the path isn't reachable and no app is connected, the call gets `confirm.unavailable` or `cap.exceeded` at once, so a confirmation never waits where you can't see it; with an app connected, it waits there with the top bar's confirmation badge, whatever the toasts' state.
    - **Deny from a toast** (P4a-R9): `app/confirmation.answer` takes an optional `toast_token` (with `desktop.v1` in the shared capabilities). With it, only `answer: "deny"` and no `cap` are accepted (`params.invalid` otherwise). A token is live only while its confirmation is pending: answering it by any route (the window, a toast, expiry, cancel) and a core stop kill it at once. The core checks the token first: an unknown, dead or reused token is `confirm.unknown`, whatever state the confirmation is in, so an old toast's Deny after an answer elsewhere, an expiry or a core restart is `confirm.unknown`, and the app shows nothing and logs the outcome (with the token cut). `confirm.already_answered` and `confirm.expired` belong to the window's path only: a live id answered without a token. A deny from a toast is recorded `confirmed_via: app` like any app answer, and the toast is removed.
    - `notify.claude_code {message, title?}` (`write`, module `interchange`): `message` 1–2,000 characters, `title` at most 100; → `{posted, reason?: "category_off" | "foreground" | "duplicate" | "unavailable"}`; the same message within 10 s is a `duplicate`.
  - **Activation and registration** (APP-020–022; P4a-R2, P4a-R26):
    - schemes: `lil` (stable), `lil-dev` (dev), `lil-test-<id>` (test channels, desktop group only). URLs: `<scheme>:confirm/<confirmation id>`, `<scheme>:task/<job id>`, `<scheme>:open/<base64url of the UTF-8 target, without padding>`, and a raw `lil://` ref.
    - per-user registration under the classes root: `<root>\<scheme>` with `(Default) = "URL:LILITH (<Channel>)"`, an empty `URL Protocol`, `DefaultIcon = "<exe>",0` and `shell\open\command = "<exe>" "%1"`; `<root>\.lilref` with `(Default) = "LILITH.lilref.<channel>"`, and `<root>\LILITH.lilref.<channel>\shell\open\command = "<exe>" "%1"`. The classes root is `HKCU\Software\Classes` for stable and dev and `HKCU\Software\LILITH-test-<id>\Classes` for test channels outside the desktop group; `<exe>` is the channel's `lil-image.exe`.
    - writing them: `lil doctor` after `y`, or the app's "Set up" button (P4a-R26) (owner decision 2026-10-22), through `lil_platform::{write_shortcut, register_handlers}`;
    - the AUMID shortcut: `<Programs>\LILITH.lnk` for stable, `<Programs>\LILITH (<Channel>).lnk` otherwise, targeting the channel's `lil-image.exe`, with `System.AppUserModel.ID` = `LILITH.Suite.<Channel>`; `<Programs>` is the per-user Start-menu Programs folder (`FOLDERID_Programs`), or `<test root>\profile\Programs\` on a test channel.
  - **`lil:` URLs** (APP-020, QA-050; P4a-R3), parsed by `lil_parse::parse_lil_url`:
    - the input is one command-line argument from the raw command line (`GetCommandLineW`), with surrounding spaces and one pair of surrounding `"` removed; at most 8,192 characters;
    - the scheme (up to the first `:`) is compared ASCII case-insensitively; `lil://` and a ref is a ref under every channel; `confirm/`, `deny/`, `task/` and `open/` are accepted only under the channel's own scheme;
    - `confirm/<id>` needs `^cf_[0-9a-f]{16}$`, `deny/<id>/<token>` needs that id and `^[0-9a-f]{32}$`, `task/<id>` needs `^j_[0-9a-f]{16}$`, and `open/<b64>` needs base64url without `=`, decoding to 1–4,096 bytes of UTF-8 with no control character, which must parse as a ref when it starts with `lil://`; nothing may follow except one trailing `/`, which is dropped;
    - a raw ref has its `%HH` escapes decoded once as UTF-8 (refusing `%00`–`%1F`, `%7F`, invalid UTF-8 and a decoded `%`), then must parse as a ref;
    - anything else is refused with "This link isn't one LILITH opens: <shown_text of the argument, at most 80 characters>", and nothing happens;
    - in plan 4a (P4a-R21) `confirm` opens the confirm window; `deny` opens no window and takes no focus: the running app, or else the confirm-window process started without a window, sends the deny and exits (P4a-R9, P4a-R11); `task` opens the Jobs panel on that job, and `open` and a raw ref show the target in Home's "Opened" card;
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
    - Redaction: every value under a key holding one of Appendix F's scrub words (`KEY`, `TOKEN`, `SECRET`, `PASSWORD`, `PASSWD`, `CREDENTIAL`, `AUTH`), every Claude Code profile label in `suite.toml`, every e-mail address (`[A-Za-z0-9._%+-]+@[A-Za-z0-9.-]+\.[A-Za-z]{2,}`) and every Credential Manager target becomes `<redacted>`; and (owner decision 2026-10-22) the user-profile folder becomes `%USERPROFILE%` and the computer name `<machine>`. `redactions` counts the replacements.
    - `diagnostics.recent {limit?}` (`read`, module `diagnostics`; 1–200, default 50) → `{errors: [{code, count, first_at, last_at, last_message, last_tool?}]}`, one row per code, the newest last error first, from the core's last 1,000 error outcomes in memory; the `diagnostics` topic gets `{kind: "error", code, tool?, at}` for each.
    - Logs: every process (`lil-core`, `lil`, `lil-mcp`, each app) writes `<local dir>\logs\<process>.<YYYY-MM-DD>.log` as JSON lines, a new file each day, 14 kept per process; a day's file stops at 64 MiB with one `log_capped` line; every process installs a panic hook that logs `panic` with its location (ARCH-119); an app's Qt messages are logged with their `qt_category`. Each app call carries a `trace_id` (`tr_<16 hex>`, ARCH-118).
  - **The `lil` commands this plan adds.** Exit codes are as in Appendix D.

    | Command | Output |
    |---|---|
    | `lil confirm-window [<id>]` | starts the channel's `lil-image.exe` from `lil.exe`'s folder with `--confirm [<id>]`, prints `confirm window started (pid <pid>)` and exits 0; with no `lil-image.exe` there, exit 2 and "lil-image.exe isn't beside lil.exe"; with `--non-interactive`, exit 2 and "the confirm window is for interactive use" (P4a-R4) |
    | `lil hook notification` | as in "Plugin 1.3.1" above |
    | `lil doctor` | adds: the AUMID shortcut (`ok:`, or `problem:`, created or repaired only after you type `y`); the channel's scheme and `.lilref` handlers (`ok:`, `problem:` when missing, `warning:` when another channel holds them, written only after `y`); a `warning:` for each `engines.environment` change, ending "Roll back with ComfyUI-Manager's snapshot restore or comfy-cli's version switch; the suite doesn't restore engines (CORE-166)."; the asset catalogue (`ok: ComfyUI's asset catalogue is off`, `warning: ComfyUI's asset catalogue is on`, `info: ComfyUI's asset catalogue is unknown`); and `info:` counts from `models.report` |

  - **Fail points added:** `jobs.after_withdraw`.
  - **Clarifications recorded in Appendix G** (no new rule IDs): ARCH-016 and APP-049, `lil confirm-window`; ARCH-055, CORE-113, APP-051 and QA-016, when the notification path can reach you; APP-020, the activation schemes and the URL grammar; APP-022, the shortcut, the registration and the toast XML; APP-039 and APP-043, `app/desktop.set` and `app/set_context`; APP-065, the agent-activity toast; APP-146, the seventh category and the toast rules; CORE-014, `desktop` and `diagnostics` are infrastructure; CORE-117, the new `JobSummary` fields; CORE-130, the stored captures; CORE-146, the new tool rows; CORE-160, the pause reasons, the withdrawal and the 2 s poll; CORE-161, the history rule and `estimate_key`; CORE-164, the header limits; ARCH-030, switching workspace by a new hello; ARCH-072, the watchdog bound; ARCH-117, the log cap; ARCH-126, the launch-profile keys; the plan-3 carry N6, base64url launch arguments.
- **Test-channel tools added by this plan** (module `"test"`, registered only on `test-*` channels; a test contract, not copied into the spec):

  | Tool | Effect | Arguments | Result (`structured`) |
  |---|---|---|---|
  | `test.desktop` | write | `{fullscreen?: "no" \| "d3d" \| "busy" \| "presentation", foreground?: "suite" \| "other", shortcut?: bool, handlers?: bool, toasts?: "ready" \| "not_yet_shown" \| "disabled_for_app" \| "disabled_for_user" \| "disabled_by_policy" \| "elevated" \| "no_platform", clear?: bool}` | `{toasts: [{op: "post" \| "remove", tag, group, title?, body?, launch?, silent?, xml?}], power_held, polls}` (no `update`: P4a-R36): sets `FakeDesktop` and reads what it recorded; each `post` carries `xml`, the `Toast.xml` handed to the desktop (Task 3's toast tests read its actions, the Deny token and the escaping from it); `foreground: "suite"` puts a connected verified `ui:*` app's pid in front, `"other"` another process's |
  | `test.job` (extended) | write | adds `phases?: [string]`, `estimate_key?`, `silent_secs?`, `batch?: n` | `{job}`, or with `batch` `{jobs}` sharing one `batch_id` |

  Test-only overrides, honoured only on test channels: `LIL_TEST_STALL_SECS` (the 30 s of the stall rule) and `LIL_TEST_DESKTOP_POLL_MS` (the 2 s poll).
- **Test fakes** (QA-066; not copied into the spec): `lil_platform::desktop_fake::FakeDesktop` (toasts, fullscreen state, foreground pid, shortcut and handler state, power request), used by every test-channel core; `lil-fake-image` (a test-kit binary with `#![windows_subsystem = "windows"]`, so no console window opens, built with the test binaries as `target\testbins\debug\lil-fake-image.exe` and copied as `lil-image.exe` into its own folder; each run appends one JSON line `{"args": [<argument>, …]}` to `%LIL_TEST_ROOT%\fake-image-args.jsonl` and exits; Task 3's spec tests also read a bare array line), for `lil confirm-window` and `lil-mcp`'s launches; `FakeComfy` gains `cached_nodes(&[node_id])` (sends `execution_cached`) and `oom_next(n)`; `FakeCore.qml` for Qt Quick Tests (Task 16).

## Review Focus

1. **The core goes away while the app is open:** it crashes, is stopped, restarts after an update, or a newer major replaces it; meanwhile a cap prompt waits inline, a confirm window is open, and Jobs shows progress.
   - **Expected:** a "Reconnecting…" banner within a second; AI and workspace actions are disabled; nothing is sent twice; after the reconnect each topic resumes from its last `seq` or reloads from a snapshot, with no lost or doubled rows; an inline cap prompt and its waiting request survive and still settle once, by retry token, after you answer; a call that was in flight ends with a clear error, never a silent resend; an incompatible core shows "restart the core when it's idle".
   - **Covered by:** Task 14, `session_reconnects_and_resubscribes_from_the_last_seq`, `session_gets_a_snapshot_when_the_history_is_gone` and `in_flight_calls_fail_and_are_never_resent`; Task 16, `pending_call_retries_once_after_an_answer_across_a_reconnect`; Task 20, `tst_banners::test_reconnecting_disables_actions`; Task 21, `tst_confirm_window::test_reconnect_keeps_the_dialog_and_its_state`.
2. **Odd or hostile links reach the handler:** a browser's `lil://` link with `%20` or a trailing `/`, quotes, a 50,000-character argument, `lil:open/` with bad base64url or a target holding control characters, another channel's scheme, an unknown verb, a `.lilref` file of 1 GB or with a bad hash, a UNC path.
   - **Expected:** each is refused with a short message showing at most 80 normalised characters; nothing opens that shouldn't; no activation ever approves a confirmation, and a `deny/` link denies only with its toast's token, once; a well-formed link of each kind round-trips.
   - **Covered by:** Task 3's spec tests `every_form_round_trips` and `refuses_everything_else` (turned green by Task 4) and Task 4's property test `any_argument_is_classified_or_refused_without_panic`; Task 19, `args_refuse_a_large_lilref_and_a_bad_hash`, `args_refuse_a_network_lilref` and `tst_open_card::test_refused_link_shows_message`.
3. **A notification is clicked at an awkward moment:** the app is running, or isn't; the confirm window is already open; the confirmation was answered elsewhere, expired, or belongs to a core that restarted; two clicks come quickly.
   - **Expected:** exactly one window comes to the front; the dialog shows the confirmation's live state (pending, answered, expired, unknown) and never approves by itself; a "Deny" click denies once and opens nothing, and a second Deny (or a Deny after an answer elsewhere) does nothing but log; a second click reuses the window; a toast is removed when its confirmation is answered or expires.
   - **Covered by:** Task 21, `tst_confirm_window::test_answered_elsewhere_shows_state` and `test_a_second_url_adds_to_the_open_window`; Task 19, `a_confirm_url_goes_to_the_running_full_app_first` and `two_quick_second_launches_reach_one_window`; Task 10, `confirmation_toast_is_removed_when_answered_or_expired`; the desktop group's `desktop_toast_url_brings_the_confirm_window_to_front` (P4a-R31). Task 1's checks S1.3 and S1.4 set the activation recipe; they are spike code, not coverage. The deny: Task 10, `a_toast_deny_needs_its_token_and_works_once`; Task 19, `deny_url_opens_no_window_and_answers_once`.
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

- **Spec additions first** (Task 2): ARCH-127 (keep-awake) and APP-111's "While you were away" come from the parked AI-surfaces revision's plan-4a parts, without their director clauses; `app/desktop.set` and a seventh notification category are added (owner decision 2026-10-22).
- **A Windows Sandbox runner** (Task 30; owner decision 2026-10-22, Q8) runs the desktop group, which QA-042 needs somewhere to run. (2026-10-23: the toast and foreground tests run on your PC instead, P4a-R39.)
- **No progress toasts** (P4a-R36), although the row names them: Task 1's S1.5 took its FAIL path, and the owner then dropped live progress toasts altogether (round 14).
- **The restart hold** (P4a-R41): a core restart holds never-started jobs instead of failing them. **This changes an invariant (ARCH-022's first sentence) by a ruling, not by an owner decision, so it needs your OK through Task 2's merge;** Task 2's PR body and merge card say so.
- **The one-click merge flow** (Task 28, P4a-R38; owner decision 2026-10-23) and the review's plan-4a items (P4a-R40).
- **A Settings panel** (Task 27) the row doesn't name: the toggles APP-146, CORE-160 and ARCH-127 call settings need a place to change them, and the notification setup (owner decision 2026-10-22) lives there.
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
- **Later CI work** (a workflow the owner pushes): an alert for a Qt WebEngine 6.140.x security release (pre-research §2).
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
- **Spec tests** (QA-017, QA-018): Task 3's author reads only the spec (Appendices D to G), this plan's Global Constraints and the Interfaces blocks. Later tasks only remove `expected-fail.toml` entries; none edits a spec test except the protected one-value changes of Tasks 6 and 11. One exception (coordinator ruling, 2026-10-24, at Task 3's review): Task 6 adds the spec test `all_cached_run_says_nothing_changed` in a new file, `crates/lil-spec-tests/tests/jobs_cache_4a.rs`, together with `FakeComfy::cached_nodes`, because the test can't compile before Task 6's fake exists. It is written first, from Appendix G only, and enters `expected-fail.toml` under Task 6; Task 6 removes the entry once it passes.
- **Tool registration** as in plan 3: each task that adds tools calls its `register_*_tools` from `core_registry` (or `server/mod.rs` for tools that need a running module) and updates the pins in `crates/lil-core/src/tools/tests.rs`; never delete a pin to get green. `crates/lil-core/tests/module_deps.rs` learns `desktop` and `diagnostics` (P4a-R24).
- **GUI builds** run in a Visual Studio developer shell: `pwsh -NoProfile -File tools/dev-shell.ps1 -- <command>` (Task 15) sets up MSVC and puts `LIL_QT_DIR` (default `C:\Qt\6.12.0\msvc2022_64`) on `CMAKE_PREFIX_PATH`.
- **Checkpoint A, after Task 14** (owner direction): when Tasks 0–14 and 30 have merged, a scoped review reads the merged core against Appendix G, and the owner checks in on it (the queue controls, the notification path and the desktop settings, through `lil call`) before Task 15 starts. Splitting the plan there into two halves is the owner's call; nothing below depends on it.
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
- **S0.9 Offscreen GUI tests** (QA-068): under `QT_QPA_PLATFORM=offscreen`, a Qt Quick Test with a KDDockWidgets main window and one floating window, a `PathPolyline` Shape, a `Loader` of the `SpikeWeb` plugin, and a grab that isn't one flat colour; the backend environment it needs (for example `QT_QUICK_BACKEND=software`) is recorded. PASS = all of it works locally and in one scratch CI run on `windows-2025` (a workflow commit the owner pushes, P4a-R32). FAIL → P4a-R35 for whatever failed, recorded per item: those tests run in the Sandbox runner's `--suite qml` or `--suite grabs` mode (Task 30), on the Sandbox's own screen.
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
- Create: `spikes/winshell/` — a standalone crate (`windows` 0.62.x) with the binaries `winshell-core` (`windows_subsystem = "windows"`, started detached as `lil-client` starts the core), `winshell-app` (a tiny window and a single-instance pipe), `winshell-hello` (a console program), and `run-spike.ps1` (registers, runs, cleans up and writes `results/checks.json`). It also holds `run-sandbox.ps1` and `inside.ps1` for S1.9.
- Create: `docs/spikes/11-windows-integration.md`; modify `docs/spikes/README.md`.

**Interfaces:**
- Produces, for Tasks 2, 5, 9, 10, 19, 21, 28 and 30: the `windows` feature list; whether a windowless process can post under a shortcut's AUMID; what a protocol activation's raw command line holds (from a toast, from a browser, from Run); the foreground recipe; the progress-toast behaviour; the fullscreen values; the power-request API; the Windows Hello route (KEK, DPAPI with a gate, or passphrase only); and which desktop checks work inside Windows Sandbox, with a run's time (for Task 30).

**Pass bars, fixed before the run** (copied verbatim into §4):
- **S1.1 Crate surface:** the spike builds with 0 warnings using `UI::Notifications::{ToastNotificationManager, ToastNotification, NotificationData}`, `Data::Xml::Dom::XmlDocument`, `Security::Credentials::KeyCredentialManager`, `Win32::System::WinRT::IUserConsentVerifierInterop`, `Win32::UI::Shell::SHQueryUserNotificationState`, `Win32::System::Power::{PowerCreateRequest, PowerSetRequest, PowerClearRequest}`, `Win32::UI::Shell::PropertiesSystem::IPropertyStore` and `Win32::Security::Cryptography::{CryptProtectData, CryptUnprotectData}`. The feature list is recorded for Task 5.
- **S1.2 Windowless sender** (APP-022): `winshell-core.exe`, started with `DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP`, posts a toast under AUMID `LILITH.Spike.Notify`, with a per-user shortcut `<Programs>\LILITH Spike.lnk` carrying that AUMID. PASS = the owner sees it pop, 3 times of 3, and `ToastNotifier::Setting()` is `Enabled`. Recorded, not a bar: whether a toast shows with the shortcut removed and only `HKCU\Software\Classes\AppUserModelId\LILITH.Spike.Notify` present.
- **S1.3 Protocol activation** (APP-020): `HKCU\Software\Classes\lilspike` registered as Appendix G's registration says, with scheme `lilspike`. PASS = a click on the toast's body and one on its button each start `winshell-app.exe` with `GetCommandLineW` exactly `"<exe>" "lilspike:confirm/cf_0123456789abcdef"`, 3 times of 3 each. Recorded: the raw command line when Edge opens `lilspike://0123456789abcdef/sketchbook/lantern@1.0.0#sha256=<64 hex>` from a local HTML file, and when the Run dialog opens `lilspike:open/<base64url>` (encoding, a trailing `/`, quotes). FAIL (the argument arrives split, re-quoted or percent-encoded) → P4a-R3's grammar is changed to match what arrives before Task 2 writes Appendix G, and the change is reported.
- **S1.4 Foreground** (APP-014): with `winshell-app.exe` A running behind another focused window, a toast click starts B, which reads A's pid over the spike's single-instance pipe, calls `AllowSetForegroundWindow(A)`, passes the argument and exits; A calls `SetForegroundWindow`. PASS = `GetForegroundWindow()` is A's window and A has keyboard focus, 5 times of 5; with no A running, B's own window comes to the front, 5 times of 5. FAIL → the foreground recipe changes (the running instance also calls `AllowSetForegroundWindow` for its own windows and uses `SwitchToThisWindow` as a fallback) and the check runs again; still failing → the owner hears before Task 2, and the confirm window falls back to a flashing taskbar button.
- **S1.5 Progress toasts** (APP-146): a toast with Tag `j_spike`, Group `job_progress` and data binding, updated once a second for 20 s. PASS = every `Update` returns `Succeeded` and the toast doesn't pop again; after the owner dismisses it, `Update` returns `NotificationNotFound`; at the end `Remove(tag, group)` then a new toast pops; a toast with `SuppressPopup = true` lands in the notification centre without popping (owner confirms). FAIL → progress toasts are left out of plan 4a (the `job_progress` category stays, off, with no toasts) and the owner hears before Task 2.
- **S1.6 Fullscreen poll** (CORE-160, APP-146): `SHQueryUserNotificationState` every 2 s while the owner (a) works at the desktop, (b) runs a fullscreen Direct3D game or program, (c) plays a fullscreen video in a browser, (d) runs a presentation if one is at hand. PASS = (a) gives `QUNS_ACCEPTS_NOTIFICATIONS`; (b) gives `QUNS_RUNNING_D3D_FULL_SCREEN` or `QUNS_BUSY`; the call's p99 < 1 ms over 100 calls. (c) and (d) are recorded. The poll runs from `winshell-core.exe`, the detached windowless process, as the core will. FAIL → fullscreen auto-pause and the quiet mode use only the values that did work, and the owner hears before Task 2.
- **S1.7 Keep-awake** (ARCH-127): the detached `winshell-core.exe` holds `PowerSetRequest(PowerRequestSystemRequired)` on a `PowerCreateRequest` object with a reason string. PASS = `powercfg /requests`, which the owner runs in an elevated terminal, lists it under SYSTEM while held and not after `PowerClearRequest`; and, when this PC's sleep timeout is finite, the PC stays awake 5 minutes past it with no input while the request is held (owner present; recorded as skipped when the timeout is "never"); `powercfg /a` (owner) records whether Modern Standby is present; and the spike's hidden window receives `PBT_APMSUSPEND` and `PBT_APMRESUMEAUTOMATIC` across one sleep (owner present). If those messages don't arrive, ARCH-127's sleep-gap record falls back to comparing the wall clock with the monotonic clock every 30 s, a jump of more than 60 s counting as a sleep. FAIL → the same checks with `SetThreadExecutionState(ES_CONTINUOUS | ES_SYSTEM_REQUIRED)` from a dedicated thread; if both fail, ARCH-127's "can't be held" path.
- **S1.8 Windows Hello** (QA-088): from `winshell-hello.exe` in Windows Terminal: (a) `KeyCredentialManager::IsSupportedAsync()` is true; (b) `RequestCreateAsync("LILITH-spike-kek", ReplaceExisting)` shows the Hello prompt in front of the terminal; (c) `OpenAsync`, then `RequestSignAsync` over one fixed 32-byte challenge 5 times in one process and 5 times in a second process, gives 10 byte-identical signatures; (d) a TPM 2.0 is present (`Tbsi_GetDeviceInfo`); (e) `IUserConsentVerifierInterop::RequestVerificationForWindowAsync(GetConsoleWindow(), …)` shows the prompt in front and returns `Verified`; (f) `DeleteAsync` removes the credential. The route: **KEK** when (a), (b), (c), (d) and (f) pass; else **DPAPI with the Hello gate** when (e) passes and `CryptProtectData`/`CryptUnprotectData` round-trip; else **passphrase only**, reported to the owner.
- **S1.9 Inside Windows Sandbox** (QA-042, P4a-R31; owner decision 2026-10-22). It runs after Task 0's Step 2, because its Qt item and its deploy mapping use Task 0's deploy folder, `spikes/app-shell/build/deploy`. **Owner step first** (about 10 minutes with the restart): turn on the Windows Sandbox feature once (Settings → Optional features → "More Windows features" → Windows Sandbox), which needs virtualization on in the firmware; agents change no system setting. The scripts inside are written for Windows PowerShell 5.1 (`pwsh` isn't in the Sandbox image), and the `LogonCommand` is `powershell.exe -ExecutionPolicy Bypass -File C:\lil\in\inside.ps1`. The spike's `run-sandbox.ps1` then writes a `.wsb` that maps the spike's bundle read-only, a copy of the spike's deploy folder read-only at the same absolute path as on the host (to check that a `SandboxFolder` outside the default places works), and an empty results folder writable (made before the start, readable by SYSTEM, as Sandbox shares run as SYSTEM). Its logon command runs `inside.ps1`, which writes `started.json` at once, runs every item below, writes one JSON file per item and `done.json` to the results folder, then shuts the Sandbox down. Every bar is read from those files by `run-sandbox.ps1`, unattended:
  - **elevation:** `elevation.json` records the logon process's `TokenElevation` and integrity level. If it is elevated, `inside.ps1` repeats the toast and foreground items under a de-elevated launch (a scheduled task with `/RL LIMITED`, then `runas /trustlevel:0x20000` if that fails), records which launch worked, and the items below are judged on that run. If neither gives a non-elevated token, the toast and foreground items FAIL and go to the owner.
  - **toast:** `toast.json` holds `ToastNotifier::Setting()` and whether `ToastNotificationManager::History().GetHistory("LILITH.Spike.Notify")` holds the posted Tag. PASS = `Enabled` and the Tag is there.
  - **protocol:** `protocol.json` holds whether `HKCU\Software\Classes\lilspike\shell\open\command` exists after registration, and the raw `GetCommandLineW` that `ShellExecuteW("lilspike:confirm/cf_0123456789abcdef")` delivered. PASS = present, and the argument exactly `"lilspike:confirm/cf_0123456789abcdef"`.
  - **foreground:** `foreground.json` holds A's pid and window, the pid of a second window the script itself brought to the front first, and `GetForegroundWindow()`'s owner pid after B's hand-off. PASS = A's pid.
  - **power:** `power.json` holds `PowerSetRequest`'s result and whether `powercfg /requests`, run from the elevated logon context, lists it. PASS = listed. This proves only the API inside a VM; S1.7 on the host stays the real check that the PC stays awake.
  - **Rust exe:** `rust.json` holds the exit code of one dynamically linked Rust test exe run once with the three VC++ DLLs on `PATH` and once without them. PASS = it runs with them (the runner bundles them either way); the run without them is recorded.
  - **Qt windows:** `qt.json` holds the spike app's grab statistics offscreen (`QT_QPA_PLATFORM=offscreen`) and on the Sandbox's own screen (visible window, non-flat grab), and the RHI backend Qt chose. PASS = both grabs are non-flat.
  - **lifecycle:** `lifecycle.json` and the host log record the Sandbox's version and kind (inbox, or the Store version with the `wsb` CLI), which availability check worked (`wsb list` exiting 0 where the CLI exists; else `WindowsSandbox.exe` present and a run reaching `started.json` within 120 s), which stop worked (`shutdown /s /t 0` inside; `wsb stop --id`; ending the `WindowsSandbox.exe` process the script started and its client), the Sandbox's process names while it runs (for `running()`), and the time from start to closed. PASS = a detection and a stop that both work, and a whole run under 10 minutes (part of PASS).
  - **host state** (one owner-present observation, about 15 minutes): the owner watches one run's toast pop in the Sandbox window (the only on-screen item), then the toast, foreground and Qt items run again with the Sandbox window behind other windows, and once more with the host locked. Recorded, not a bar; Task 30 tells the owner what not to do during a run from these results.
  - PASS = every bar above, 3 runs of 3. FAIL on any item → that item's tests can't use the runner: stop and ask the owner before Task 2 (running them in the owner's own session would need a QA-042 amendment, which is the owner's call); the other items still use the runner.
- **Cleanup:** PASS = afterwards the spike's shortcut, `HKCU\Software\Classes\lilspike`, `HKCU\Software\Classes\AppUserModelId\LILITH.Spike.Notify` and the credential are gone, checked by the script.

If S1.1 or S1.2 fails, stop: the result goes to the owner before Task 2 (APP-022).

- [ ] **Step 1: Write §1–4 of `docs/spikes/11-windows-integration.md`** with the bars, verbatim, and commit before running anything.

```bash
git commit -m "spike 11: the Windows-integration question, setup and pass bars"
```

- [ ] **Step 2 (owner present, about 1 hour including one sleep and wake; the owner says go first, because the script writes a shortcut, registry keys and a Windows Hello credential, all removed at the end):** run `pwsh -NoProfile -File spikes/winshell/run-spike.ps1`. Expected: `spikes/winshell/results/checks.json` with every check's verdict and the cleanup's PASS.
- [ ] **Step 2b (owner present, about 1 hour and 15 minutes, after the owner has turned on Windows Sandbox and restarted):** S1.9, `pwsh -NoProfile -File spikes/winshell/run-sandbox.ps1 -Runs 3`, then its host-state observation. Expected: the per-item result files and `done.json` for each run, and every bar judged from them.
- [ ] **Step 3: Write §5–8 and the README row**, naming the decisions: the feature list, the activation forms, the power-request API, the Hello route, the Sandbox result. Run the privacy scan after the commit. Expected: clean.

```bash
git commit -m "spike 11: Windows-integration results (toasts, activation, foreground, fullscreen, keep-awake, Windows Hello)"
```

---

### Task 2: Spec changes first (QA-029) — Qt-free — **protected: `crates/lil-cli/src/dev/spec_report.rs`**

**Depends on:** Tasks 0 and 1 merged; the owner decisions of 2026-10-22 ("Owner decisions" at the end).

**Files:**
- Modify: `docs/superpowers/specs/2026-10-12-lilith-design.md`; `docs/GLOSSARY.md`; `docs/superpowers/plans/2026-10-12-roadmap.md`; `docs/superpowers/plans/2026-10-21-plan-4a-shell-and-plumbing.md` (the 2026-10-23 and 2026-10-24 amendments, P4a-R36 to P4a-R41); `dev/rule-classification.toml` (ARCH-127 classified `test`); `crates/lil-cli/src/dev/spec_report.rs` (`EXPECTED_RULE_COUNT = 1080`, protected).

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
  - The owner's answers: Q1 → APP-039's list and APP-043's map gain `app/desktop.set` (a "Notifications and desktop" row: `app/desktop.set` | `desktop.get` | `desktop`); Q2 → APP-146's categories gain "Claude Code and agent activity", on by default; questions Q3 and Q4 → APP-146 and ARCH-121 each gain the sentence of the default; Q5 → APP-022 gains "the app offers the notification setup with one Set up button on its first start and in its settings, and writes it only after your click"; Q6 → APP-022 gains "a confirmation toast has Open and Deny: Deny denies from the toast, authenticated by a single-use token in its URL; approving needs the window" (P4a-R9), and ARCH-053, CORE-111 and APP-044 each gain one owner-decided exception, their other wording unchanged: "(owner decision 2026-10-22) One exception, for a **deny** only: a deny of an agent's confirmation may also come from a confirmation toast's Deny action, relayed by a verified `ui:*` connection with that toast's single-use token (P4a-R9). Approving stays only in the shared dialog, after a real input event. The exception covers deny only; it never applies to `task.start` (AI-213) or to any other rule that needs a real input event."; Q7 → APP-164 gains "(owner decision 2026-10-22) until the Image studio exists, its window is titled LILITH — <project>"; Q8 → nothing in the spec (QA-042 already allows Windows Sandbox). Each is marked "(owner decision 2026-10-22)".
  - Spike results as "(plan 4a result, <date>: …)" notes, each citing `docs/spikes/10-app-shell.md` or `11-windows-integration.md`: APP-005, APP-001 and ARCH-114 (the KDDockWidgets version); APP-022 (the windowless sender); APP-135, APP-136, APP-137 and APP-156 (the numbers and the Narrator answers); ARCH-127 (the API, or "can't be held"); QA-088 (the route); ARCH-126 (P4a-R15's VRAM reading).
  - (2026-10-23) Task 1's further results: APP-022's and APP-146's "no progress toasts" (P4a-R36), the CLSID in APP-022 and Appendix G (P4a-R37), and CORE-160's fullscreen readings. The owner decisions of rounds 12 and 13: QA-042's host exception (P4a-R39), QA-036's one-click merges, with every `lil dev merge` authenticated, and QA-088's prompt placement (P4a-R38), ARCH-127's "Windows reports only", and the review's plan-4a items in CORE-117, CORE-160, CORE-161, APP-146 and Appendix F's `app/confirmations.list` (P4a-R40). Each is marked "(owner decision 2026-10-23)" or "(plan 4a result, 2026-10-23: …)". (2026-10-24) Round 14's "no live progress toasts" in APP-022, APP-146 and Appendix G (P4a-R36), marked "(owner decision 2026-10-24)"; the restart hold in ARCH-022, CORE-160, Appendix F and Appendix G (P4a-R41); and the external facts of Step 7 in Appendix B.3.
- [ ] **Step 4: Edit CORE-146's table** (table rows, not rule lines): `projects.list`, `workspaces.list`, `desktop.get`, `engines.environment`, `telemetry.energy`, `diagnostics.recent` (read, M1); `lilref.write`, `notify.claude_code` (write, M1). APP-043's map gains them too: `projects.list` and `workspaces.list` in the Session row, `telemetry.energy` in Hardware, `diagnostics.recent` and `engines.environment` in Diagnostics, `lilref.write` and `notify.claude_code` in Interchange.
- [ ] **Step 5: Edit Appendix F's `app/confirmation.answer` row** to `{confirmation_id, answer, cap?, toast_token?}` with a pointer to Appendix G's "Deny from a toast". **Edit Appendix D:** protocol 1.2 and the capabilities of Global Constraints; the methods table's `app/*` row names the four plan-4a methods and their capabilities; the error-code row `job.not_queued` | `data_conflict` | `never` | plan 4a; the `lil` command rows `lil confirm-window [<id>]` and `lil hook notification`.
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
  | Pause reason | A named cause that holds queued jobs, such as your own pause or the fullscreen auto-pause, each with the classes it covers. | pause type, pause flag | CORE-160 |
  | Restart hold | Keeping a queued job that never reached an engine waiting after a core restart, instead of failing it, until you release it. | hold (alone: a document hold is APP-103's), pause | ARCH-022, CORE-160 |
  | Power request | What the core holds so the PC doesn't sleep while a GPU job runs. | wake lock, keep-awake lock | ARCH-127 |
  | Cost and time chip | The estimate shown before an action that spends GPU time or money. | price tag, quote | APP-140, CORE-161 |
  | Deploy folder | The trimmed `windeployqt` folder that runs with Qt and MSVC off PATH. | install folder (Velopack's) | ARCH-114, OPS-094 |
  | Agent edit | A change an AI agent made to a candidate or sealed file, recorded as AI work and never as a human edit. | human edit, hand edit | roadmap row 4b (attribution) |
  | Claim | Your hold on an asset or document you opened to edit: the director doesn't write to it, and it lasts until you hand it back. | lock, lease (a checkout lease is another thing), hold (a document hold is APP-103's) | roadmap N31 |
  | Hand back | Your action that ends a claim, so the director may work on that asset again. | release, unlock | roadmap N31 |
  | Owner stop | A point in a director task where it waits for your decision; a game has one per feature, where you play what was built and approve the next. | gate (alone), approval gate, QA gate (a technical check) | roadmap N32 |
  | Game engine | A program that builds and runs games, such as Unity or Godot, which the suite exports to. | engine (alone: the suite's engines are ComfyUI and the like) | APP-162, roadmap N38 |
  | Engine target | One game engine and version, with its settings, that a project exports to. | platform, build target | APP-162, roadmap N38 |
  | Engine adapter | The suite's code and capability table for one game engine and version, which writes and checks its exports. | plugin, bridge, exporter (alone) | roadmap N38 |

- [ ] **Step 9: Roadmap:** row 4a cites the two spike documents; row 4b gains the carries this plan sends it ("Not in this plan"); the carry-forward table gains a row for the standalone hook-fix PR (P4a-R22). (2026-10-23) Also: the Backlog's record of the freeze exception and of the declined generate-and-review plan; backlog notes N30 to N54, and the amended N2, N4, N5, N10, N13, N16, N21 and N22; the local-backup row 6c, with the windowless launcher's carries and the first-usable-build line; a frozen Goal 2 row; the review's plan-writer notes on rows 4a, 4b, 5, 6a and 6b; row 4a's progress toasts and merge flow; and row 7+'s director models.
- [ ] **Step 10: Set `EXPECTED_RULE_COUNT` to 1080** (protected) and verify.

Run: `grep -cE '^- \*\*[A-Z]+-[0-9]{3}\*\* \[' docs/superpowers/specs/2026-10-12-lilith-design.md` and `cargo run -q -p lil-cli -- dev spec-report`
Expected: 1080 from both; 0 dangling citations; 0 unrecorded rulings. Then `cargo nextest run -p lil-cli --retries 0 -E 'test(/spec_report/)'`: PASS.

- [ ] **Step 11: Commit.** The PR body has a "Needs your OK" section naming P4a-R41's exception to the ARCH-022 invariant, and the merge card's owner summary repeats it (2026-10-24).

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
  - `jobs_queue_4a.rs` (removed by Task 6): `job_move_next_and_last_stay_within_the_class` (CORE-160: three queued `agent` jobs behind a gated running job; `job.move {to: "next"}` on the third makes `jobs.list` order it first among `agent`, never ahead of a queued `user` job; `last` puts it last); `job_move_of_a_running_job_is_not_queued` (the code is `job.not_queued`); `job_cancel_by_ids_batch_and_all_mine` (`{ids}` cancels exactly those; `{batch_id}` cancels the batch of `test.job {batch: 3}`; `{all_mine: true}` from `cli` leaves the `ui:test` app's jobs alone; two selectors → `params.invalid`); `queue_pause_holds_every_queued_job_and_never_interrupts_the_running_prompt` (CORE-117, CORE-160: after `app/queue.pause {}` the running gated job keeps running, every queued job shows `queue_paused`, `jobs.list` `lanes[0]` is `{lane: "gpu-heavy", paused: true, reasons: ["manual"]}`; after `app/queue.resume {}` they run in order); `queue_pause_withdraws_a_pending_prompt_and_reposts_it_once` (CORE-160, CORE-120: with an outside prompt running in the fake ComfyUI and a suite prompt `queued_in_engine` behind it, pausing posts exactly one `/api/jobs/{id}/cancel`, the job returns to `queue_paused`, and after resume exactly one new `POST /prompt` carries a different prompt id); `queue_pause_is_only_for_verified_apps` (APP-040: `cli` gets `caller.not_allowed`; a `ui:test` session without `queue.v1` gets `caller.not_allowed`); `job_records_carry_batch_origin_phase_and_status_seq` (CORE-117: a `test.job {phases: ["loading_models", "sampling"]}` shows `batch_id` `b_` plus 16 hex, `index` 0, `origin` `{surface: "cli"}`, `phase` ending `sampling`, `subtasks` names in order, and `status_seq` starting `queued`); `a_silent_job_is_unresponsive_and_nothing_is_cancelled` (ARCH-072, with `LIL_TEST_STALL_SECS=2`: `test.job {silent_secs: 4}` gets `unresponsive: true` and `job:<id>` `{kind: "unresponsive", on: true}`, then succeeds); and `all_cached_run_says_nothing_changed`, which moves to Task 6 with the fake it needs (owner ruling at Task 3's review);
  - `estimates.rs` (Task 7): `job_estimate_is_none_without_history` (CORE-161: `source: "none"`, numbers `null`); `job_estimate_uses_measured_history_after_three_runs` (three `test.job {estimate_key: "k"}` runs with `test.hw_tick` → `source: "measured_history"`, `samples: 3`, `wall_s_p50` a decimal string, `cost` `"0"`); `job_estimate_validates_args_like_a_call` (`params.invalid` for an unknown argument, `tool.unknown` for an unknown tool); `telemetry_energy_sums_a_range` (`telemetry.energy` over ticked samples equals the sum of `telemetry.series` `gpu.energy` points); `ui_connections_are_tracked_in_telemetry` (P4a-R27: `Snapshot.processes` holds the test app's pid with role `app` while connected, not after);
  - `comfy_profile.rs` (Task 8): `owned_comfyui_starts_with_the_launch_profile` (ARCH-126: the fake ComfyUI's `system_stats.system.argv` holds `--reserve-vram 1.5` and `--preview-method taesd` and no Manager flag; `engines.status` shows `launch_profile.flags` equal to them); `enable_assets_in_extra_args_stops_the_start` (`engine.not_configured` naming it); `out_of_memory_retries_twice_then_fails_with_advice` (CORE-163: `oom_next(3)` → three `POST /prompt`, then `comfyui.out_of_memory` with the hint of Appendix G); `models_report_names_orphan_duplicate_and_unknown_family` (CORE-164: a fixture root with a safetensors file carrying `modelspec.architecture`, a copy of it, a file with no metadata and a registry entry whose file is absent → those four states, nothing deleted); `engine_environment_reports_a_changed_node` (CORE-130: two captures that differ in one node → one `changes` row `{what: "node", change: "changed"}`);
  - `notifications.rs` (Task 10): `toasts_follow_categories_and_defaults` (APP-146: with `test.desktop {foreground: "other"}`, a finished batch posts one `job_finished` toast; `desktop.get` has no `job_progress` key (P4a-R36); after `app/desktop.set {notifications: {job_finished: false}}` none is posted); `no_toast_while_a_suite_window_is_in_front` (P4a-R7); `quiet_while_fullscreen_posts_silently_and_never_drops` (`fullscreen: "d3d"` → `silent: true`, still posted); `no_progress_toast_for_a_long_job` (P4a-R36: a job of `expected_secs` 60 → no toast op for it until its `job_finished` `post`); `with_the_notification_path_a_call_waits_without_an_app` (ARCH-055, QA-016: no app connected, path ready → `test.destructive` from `cli` gets `pending_confirmation` and a `confirmation_waiting` toast with launch `lil-test-<id>:confirm/<id>` and two actions, "Open" with that URL and "Deny" with `lil-test-<id>:deny/<id>/<32 hex>`); `a_toast_deny_answers_once_with_its_token` (P4a-R9: `TestApp` sends `app/confirmation.answer {answer: "deny", toast_token}` with the token from that URL → denied, and the retry gets `confirm.denied`; the same token again → `confirm.unknown` (the token died when it was used); a wrong token → `confirm.unknown`; after the confirmation is answered in the window, its toast's token → `confirm.unknown`; `toast_token` with a `cap` → `params.invalid`; `answer: "approve"` with a token → `params.invalid`); `turning_off_confirmation_toasts_makes_agent_calls_unavailable` (`confirm.unavailable`); `toasts_off_in_windows_make_the_path_unreachable` (after a call that waits through the toast, the contrast: with the path ready and no app, an interactive `cli` `test.destructive` gets `pending_confirmation` and a `confirmation_waiting` toast; then `test.desktop {toasts: "disabled_for_app"}` → `desktop.get {fresh: true}` shows `toasts: "unavailable"` and `toasts_reason: "disabled_for_app"`, and a `cli` `test.destructive` with no app gets `confirm.unavailable` at once, with no new confirmation toast); `non_interactive_callers_never_wait` (after the same waiting call: `lil --non-interactive call test.destructive` → exit 1, `confirm.unavailable`, no new confirmation toast); `toast_text_is_escaped_and_normalised` (a message holding `<`, `&`, U+202E and U+0007 shows `&lt;`, `&amp;`, `⟨U+202E⟩` and no control character); `notify_claude_code_posts_agent_activity_and_drops_duplicates` (ARCH-103); `agent_open_posts_agent_activity` (APP-065: an `mcp` caller's `ui.show` posts an `agent_activity` toast whose launch is an `open/` URL);
  - `desktop.rs` (Task 9): `desktop_settings_are_set_only_by_a_verified_app` (`app/desktop.set` from `cli` → `caller.not_allowed`; from `ui:test` it writes `suite.toml` and `desktop.get` reads it back); `autopause_is_off_by_default`; `fullscreen_autopause_pauses_and_resumes` (CORE-160, with `LIL_TEST_DESKTOP_POLL_MS=50`: after `app/desktop.set {fullscreen_autopause: true}` and `fullscreen: "d3d"`, `reasons` is `["fullscreen"]`; after `"no"` for two polls, the lane runs); `fullscreen_reason_never_clears_a_manual_pause`; `keep_awake_is_held_only_while_a_gpu_job_runs` (ARCH-127: `power_held` true while a gated `gpu-heavy` job runs, false after; false while only `cpu` jobs run; false with `keep_awake_gpu: false`);
  - `interchange_4a.rs` (Task 11): `open_in_launch_args_carry_base64url_targets` (APP-020, N6: `launch_command.args` ends `["--intent", "open", "lil-test-<id>:open/<b64>"]` and the base64url decodes to the target); `lilref_write_makes_a_file_that_parses_back` (APP-021); `lilref_write_refuses_a_candidate_ref` (`ref.invalid`); `app_set_context_sets_the_session_project` (ARCH-088); `projects_and_workspaces_are_listed` (APP-006); `mcp_launches_a_stopped_app_without_focus` (ARCH-015: under the fake `claude`, `ui.show` → `lil-fake-image` recorded `--no-activate` before the URL; the result text has "Started image in the background.");
  - `diagnostics.rs` (Task 12): `diagnostics_bundle_redacts_secrets_labels_and_emails` (ARCH-121, QA-084: a `suite.toml` holding a `pass_env`-style `API_KEY`, a Claude Code profile label and an e-mail address → the zip's `suite.toml` and logs hold none of them, `redactions` ≥ 3, `manifest.json` `bundle` is 1, no workspace path is in `files`); `diagnostics_recent_groups_errors_by_code` (QA-083); `every_process_logs_json_lines` (ARCH-117: after a `lil status`, `<local>\logs\lil.<date>.log` holds a JSON line);
  - `doctor_4a.rs` (Task 13): `doctor_checks_the_aumid_shortcut_and_repairs_after_yes` (APP-022: `problem:` naming the shortcut; `n` changes nothing; `y` → the test root's `Programs` holds `LILITH (Test-<id>).lnk` and a second run prints `ok:`); `doctor_registers_the_handlers_after_yes` (QA-060, under the test classes root); `doctor_reports_engine_changes_and_the_asset_catalogue` (CORE-130, ARCH-065); `confirm_window_starts_the_app_in_confirm_mode` (APP-049: `lil confirm-window cf_0123456789abcdef` with `lil-fake-image` → recorded args `["--confirm", "cf_0123456789abcdef"]`, exit 0; `--non-interactive` → exit 2);
  - `lil_url.rs` (Task 4): `every_form_round_trips` (APP-020: `confirm`, `deny`, `task`, `open` and a raw ref each parse to their variant, and `open_url` round-trips 1,000 generated targets); `refuses_everything_else` (another channel's verb, an unknown verb, a bad id, a `deny/` with no token or a 31-hex token, padding `=`, a decoded control character, `%00`, a 8,193-character argument → refused, with `shown` at most 80 characters); `raw_refs_are_percent_decoded_once` (`%2520` stays `%20` after one decode and is refused as a ref).
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
pub enum LilUrl { Confirm(String), Deny { id: String, token: String }, Task(String), Open(String /* decoded target */), Ref(String /* after one percent-decode */) }
pub struct UrlRefusal { pub shown: String /* shown_text(arg, 80) */, pub reason: &'static str }
pub fn strip_argument(raw: &str) -> &str;                       // surrounding spaces, one pair of quotes
pub fn parse_lil_url(arg: &str, channel_scheme: &str) -> Result<LilUrl, UrlRefusal>;
pub fn open_url(channel_scheme: &str, target: &str) -> String;  // "<scheme>:open/<base64url>"
pub fn confirm_url(channel_scheme: &str, id: &str) -> String;
pub fn deny_url(channel_scheme: &str, id: &str, token: &str) -> String;
pub fn task_url(channel_scheme: &str, job_id: &str) -> String;
pub fn shown_text(s: &str, max_chars: usize) -> String;         // P4a-R8
pub enum Family { Known(String), Unknown }
pub enum HeaderError { Io(std::io::Error), Malformed(&'static str) }
pub fn safetensors_family(r: &mut dyn std::io::Read) -> Result<Family, HeaderError>;
pub fn gguf_family(r: &mut dyn std::io::Read) -> Result<Family, HeaderError>;
```

- [ ] **Step 1: Write the failing tests:**
  - `lil_url.rs`: `confirm_deny_task_open_and_ref_parse` (each form of Appendix G to its variant, scheme in any case); `deny_needs_an_id_and_a_32_hex_token` (no token, 31 or 33 hex, upper-case hex and a third segment are refused); `other_channel_verbs_are_refused` (`lil:confirm/…` under `lil-dev` → refused; `lil://…` under `lil-dev` → `Ref`); `bad_ids_and_padding_are_refused` (`cf_` + 15 hex, upper-case hex, `open/` with `=`, with `?x`, with `#y`); `decoded_targets_hold_no_control_characters`; `one_trailing_slash_is_dropped_two_are_refused`; `raw_refs_are_percent_decoded_once` (`%20` → space, then the ref grammar decides; `%2520` stays `%20` and is refused; `%00`, `%7F` and invalid UTF-8 refused); `long_arguments_are_refused` (8,193 characters; `shown` at most 80 characters); `strip_argument_removes_one_pair_of_quotes` (`"\"x\""` → `x`, `"\"\"x\"\""` → `"x"`); property tests `open_url_round_trips` (any UTF-8 target of 1–4,096 bytes with no control character) and `any_argument_is_classified_or_refused_without_panic` (any string up to 10,000 characters), with the `[proptest] cases` of `test-params.toml`;
  - `text.rs`: `controls_are_removed` (U+0000–U+001F except none kept, U+007F–U+009F); `bidi_and_zero_width_are_shown` (each listed code point becomes `⟨U+XXXX⟩`); `emoji_sequences_and_rtl_text_survive` (a ZWJ family emoji, woman technologist with a skin tone, heart on fire, the rainbow flag and Hebrew text unchanged: a U+200D inside a UTS #51 emoji ZWJ sequence is kept, as Appendix G's Toast XML bullet defines it); `other_joiners_are_shown` (a U+200D after a letter and a U+200C in Persian text become `⟨U+200D⟩` and `⟨U+200C⟩`); `cut_at_max_with_an_ellipsis` (201 `char`s → 200, the last `…`, counted over the whole text, markers included);
  - `model_header.rs`: `safetensors_reads_modelspec_architecture` then `ss_base_model_version`; `safetensors_without_metadata_is_unknown`; `safetensors_header_over_100_mib_is_unknown_without_reading_it` (an 8-byte prefix announcing 100 MiB + 1 and a reader that fails after 8 bytes → `Unknown`, no read past 8); `gguf_reads_general_architecture` (versions 2 and 3); `gguf_with_more_than_1024_keys_first_is_unknown`; `gguf_string_over_64_kib_is_unknown`; property test `truncated_or_random_headers_never_panic`.
- [ ] **Step 2: Run them.** Run: `cargo nextest run -p lil-parse --retries 0`. Expected: FAIL (`todo!`).
- [ ] **Step 3: Implement.** `Extended_Pictographic` comes from a range table in `text.rs`, generated once from Unicode 16.0's `emoji-data.txt` and committed with its source line (no new dependency); `base64`'s URL-safe no-pad engine; the parser never allocates more than the input's length plus 4 KiB; the GGUF reader skips values by their declared type and length, with every length checked before it is read.
- [ ] **Step 4: Run them, then the gate** (`-p lil-parse -p lil-testkit -E 'binary(dependency_boundaries)'`). Remove the `lil_url.rs` entries. Expected: PASS, flake guard clean.
- [ ] **Step 5: Commit.**

```bash
git commit -m "lil-parse: lil: URL grammar with base64url targets, shown-text normaliser, safetensors and GGUF family readers"
```

---

### Task 5: Platform: toasts, the AUMID shortcut, handler registration, fullscreen state, the power request, foreground, launches and logs (APP-013, APP-014, APP-022, ARCH-015, ARCH-095, ARCH-117, ARCH-127, CORE-160, QA-038; P4a-R2, P4a-R20, P4a-R26, P4a-R29, P4a-R36, P4a-R37) — Qt-free, parallel

**Depends on:** Tasks 1 and 3 merged.

**Files:**
- Create: `crates/lil-platform/src/{desktop.rs, toast.rs, shortcut.rs, classes.rs, power.rs, logs.rs, desktop_fake.rs}`.
- Modify: `crates/lil-platform/src/{env.rs, process.rs, session_end.rs, guard.rs, lib.rs}`, `crates/lil-platform/Cargo.toml` (Task 1's `windows` features, each with a comment naming its use).
- Test: `crates/lil-platform/tests/{desktop.rs, logs.rs, env_names.rs}`.

**Interfaces:**
- Consumes: Task 1's feature list and power-request API. (S1.1's 29 features in `spikes/winshell/Cargo.toml`; `PowerSetRequest(PowerRequestSystemRequired)` on a `PowerCreateRequest` object, S1.7.)
- Produces:

```rust
// lil_platform::env::Env
pub fn activation_scheme(&self) -> String;            // "lil" | "lil-dev" | "lil-test-<id>"
pub fn classes_root(&self) -> String;                 // Appendix G
pub fn start_menu_shortcut(&self) -> PathBuf;         // <Programs>\LILITH[ (<Channel>)].lnk
pub fn toast_activator_clsid(&self) -> String;        // "{…}", fixed per channel (P4a-R37)
pub fn app_state_dir(&self, app: &str) -> PathBuf;    // local_dir\apps\<app>
pub fn app_pipe_name(&self, app: &str, confirm: bool) -> String;
pub fn diagnostics_dir(&self) -> PathBuf;             // local_dir\diagnostics
pub fn lilref_dir(&self) -> PathBuf;                  // local_dir\cache\lilref
// lil_platform::desktop
pub enum Fullscreen { No, D3d, Busy, Presentation, Unknown }
pub struct Toast { pub tag: String, pub group: String, pub xml: String, pub silent: bool }
// no ToastProgress, UpdateResult or Desktop::update: no progress toasts in this plan (P4a-R36)
pub enum ToastsState { Ready, NotYetShown, Unavailable(&'static str /* disabled_for_app | disabled_for_user | disabled_by_policy | elevated | no_platform */) }   // NotYetShown: Setting() answers ERROR_NOT_FOUND, the AUMID never showed a toast (plan 4a ruling 2026-10-24, Appendix G "Ready")
impl ToastsState { pub fn can_post(self) -> bool; }   // Ready or NotYetShown: the posting rule and Reachable gate on can_post() together with shortcut: ok
pub const NOT_SHOWN_GRACE: Duration;                  // 5 s
pub fn toasts_state_after_post(state: ToastsState, since_first_ok_post: Option<Duration>) -> ToastsState;   // NotYetShown NOT_SHOWN_GRACE after an Ok post → Unavailable("no_platform")
pub struct NotShownEnd; impl NotShownEnd { pub fn posted_ok(&mut self, now: Duration); pub fn state(&mut self, read: ToastsState, now: Duration) -> ToastsState; }   // the real desktop's timer: any reading but NotYetShown clears it
pub enum ShortcutState { Ok, Missing, WrongTarget, WrongAumid, WrongClsid }   // WrongClsid: the CLSID or its registration is missing or differs (P4a-R37)
pub enum HandlerState { Ok, Missing, OtherChannel(PathBuf) }
pub trait Desktop: Send + Sync {
    fn fullscreen(&self) -> Fullscreen;
    fn foreground_pid(&self) -> Option<u32>;
    fn toasts_state(&self) -> ToastsState;    // Ready only when Setting() == Enabled and not elevated; NotYetShown before the AUMID's first toast, ending as no_platform 5 s after an Ok post that never showed
    fn post(&self, t: &Toast) -> Result<(), String>;
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
pub fn require_desktop_group(host_ok: bool);          // panics unless LIL_SANDBOX=1, or host_ok and LIL_DESKTOP_HOST=1 (P4a-R29)
// lil_platform::desktop_fake
pub struct FakeDesktop; // impl Desktop; set_fullscreen, set_foreground, set_shortcut, set_handlers, set_toasts_state; records() -> Vec<ToastRecord>; power_held() -> bool; polls() -> u32
```

- [ ] **Step 1: Write the failing tests:**
  - `env_names.rs`: `names_per_channel` (stable `lil`, `HKCU\Software\Classes`, `<Programs>\LILITH.lnk`; dev `lil-dev`, `LILITH (Dev).lnk`; test `lil-test-abc`, `HKCU\Software\LILITH-test-abc\Classes`, a shortcut under the test root; `app_pipe_name("image", true)` = `\\.\pipe\lilith-app-image-confirm-<sid>-<channel>`; `toast_activator_clsid()` is fixed per channel and differs between channels, P4a-R37); the existing QA-040 guard rejects a test channel whose shortcut or classes root resolves to the real ones;
  - `desktop.rs`: `split_first_argument_keeps_the_rest_untouched` (`"C:\x y\lil-image.exe" "lil-dev:confirm/cf_0123456789abcdef"` → the exe and `"lil-dev:confirm/cf_0123456789abcdef"` with its quotes; an unquoted exe; tabs); `fake_desktop_records_posts_and_removes`; `require_desktop_group_panics_outside_the_sandbox` (with `LIL_SANDBOX` unset in a child process, and with `CI=true` alone; a Sandbox-only test also panics with `LIL_DESKTOP_HOST=1` alone, and a host-allowed test runs with it, P4a-R29); `toasts_state_maps_each_notifier_setting` (a fake `Setting()` for each value, and an elevated token, map to Appendix G's reasons);
  - desktop group (`#[ignore = "desktop"]`, each starting with `require_desktop_group(host_ok)`, `host_ok` true only for the toast tests, P4a-R39): `desktop_toast_posts_under_the_shortcut_aumid` and `desktop_toasts_state_is_ready_on_this_pc` (host run); `desktop_shortcut_and_handlers_round_trip_under_a_test_scheme` (write, read back `Ok` with the AUMID and the CLSID on the shortcut and the CLSID's `LocalServer32` under the test classes root, P4a-R37; remove the CLSID registration alone → `WrongClsid`; remove all) and `desktop_power_request_sets_and_clears` (Sandbox runner);
  - `logs.rs`: `one_file_a_day_and_14_kept` (with an injected date); `a_day_stops_at_the_cap_with_one_capped_line` (`init_at` with a 4 KiB cap: the file ends with exactly one `log_capped` line and stays under 4 KiB plus that line); `panic_hook_logs_the_location` (a child process panics; its log holds `"panic"` and a `file:line`).
- [ ] **Step 2: Run them.** Run: `cargo nextest run -p lil-platform --retries 0 -E 'binary(env_names) | binary(desktop) | binary(logs)'`. Expected: FAIL.
- [ ] **Step 3: Implement**, with Task 1's recipes: the shortcut through `IShellLinkW` and `IPropertyStore` (`PKEY_AppUserModel_ID`); registration with `RegCreateKeyExW` under `classes_root`; `HandlerState::OtherChannel` when the command names another exe; the power request with `PowerSetRequest(PowerRequestSystemRequired)` on a `PowerCreateRequest` object with a reason string (S1.7; the fallback isn't needed); `PKEY_AppUserModel_ToastActivatorCLSID` on the shortcut and `<classes root>\CLSID\{<CLSID>}\LocalServer32 = "<exe>"`, written with the shortcut by `write_shortcut` and checked by `shortcut_state` (P4a-R37).
- [ ] **Step 4: Run them, then the gate** (`-p lil-platform`). The desktop group isn't run here (QA-042). Expected: PASS.
- [ ] **Step 5: Commit.**

```bash
git commit -m "platform: toasts, AUMID shortcut, per-user handlers, fullscreen state, power request, foreground and no-activate launches, shared logs"
```

---

### Task 6: Jobs: record fields, queue order, `job.move`, batch cancels and Pause GPU work (CORE-117, CORE-120, CORE-160, ARCH-022, ARCH-072, APP-039, APP-040, APP-043; P4a-R5, P4a-R12, P4a-R13, P4a-R40, P4a-R41) — Qt-free, adversarial review — **protected: `crates/lil-spec-tests/tests/core_session.rs`**

**Depends on:** Task 3 merged.

**Files:**
- Create: `crates/lil-core/src/jobs/pause.rs`; `crates/lil-spec-tests/tests/jobs_cache_4a.rs` (the spec test `all_cached_run_says_nothing_changed`, a free addition under Global Constraints' one exception for spec tests, coordinator ruling 2026-10-24).
- Modify: `crates/lil-core/src/jobs/{mod.rs, queue.rs, runner.rs, record.rs}`; `crates/lil-core/src/engines/comfyui/{handler.rs, ws.rs}` (`execution_cached`, phases, `last_progress_at`, withdrawal); `crates/lil-core/src/tools/{job_tools.rs, test_tools.rs}`; `crates/lil-core/src/rpc.rs` (the two `app/queue.*` methods); `crates/lil-core/src/state/migrations.rs` (append `JOBS_V2`: suite migration 9, workspace migration 8); `crates/lil-schema/src/suite.rs` (`SUITE_STATE_SCHEMA = 9`, `WORKSPACE_STATE_SCHEMA = 8`); `crates/lil-core/src/store/failpoints.rs` (`jobs.after_withdraw`); `crates/lil-protocol/src/{session.rs, app.rs, codes.rs}` (1.2, `queue.v1`, `QueueParams`, `QueueState`, `job.not_queued`); `crates/lil-testkit/src/fake_comfy.rs` (`cached_nodes`); `crates/lil-core/tests/hello.rs`; protected `crates/lil-spec-tests/tests/core_session.rs` (`"1.2"`).
- Modify (shared, recompute when rebasing): `crates/lil-core/src/tools/tests.rs` (the registry pins), `crates/lil-core/tests/hello.rs` (its capability assertion becomes "the core's list ends with `queue.v1`", not a count).
- Test: `crates/lil-core/tests/{jobs.rs, comfy_client.rs, state.rs}` additions; `crates/lil-protocol` unit tests.

**Interfaces:**
- Produces:

```rust
pub enum PauseReason { Manual, Fullscreen }                     // wire "manual", "fullscreen"; both PauseScope::All
pub enum PauseScope { All, AgentOnly }                          // fn PauseReason::scope(); fn PauseScope::covers(Priority) (P4a-R5)
pub struct StartedBy { pub surface: Surface, pub id: Option<String>, pub step: Option<String> }  // JobSummary.origin, JSON in jobs.origin; named apart from jobs::JobOrigin, the stored tool call job.retry uses
pub enum Surface { Panel, Mcp, AgentSession, Cli, Core, Other(String) }   // wire snake_case; an open set, so the director plan's `director_task` reads as Other until then (P4a-R12)
pub enum MoveTo { Next, Last }
pub enum CancelSelector { One(JobId), Ids(Vec<JobId>), Batch(String), AllMine }
impl Jobs {
    pub async fn pause(&self, lane: Lane, why: PauseReason) -> QueueState;
    pub async fn resume(&self, lane: Lane, why: PauseReason) -> QueueState;   // removes only `why`, and releases restart-held jobs on `Manual` (P4a-R41)
    pub async fn release_restart_held(&self, lane: Lane) -> QueueState;       // app/queue.resume {held_only: true}: removes no reason (P4a-R41)
    pub fn queue_state(&self, lane: Lane) -> QueueState;
    pub async fn move_job(&self, caller: &JobCaller, id: &JobId, to: MoveTo) -> Result<JobSummary, ErrorBody>;
    pub async fn cancel_selected(&self, caller: &JobCaller, sel: CancelSelector) -> Result<Vec<JobSummary>, ErrorBody>;
    pub async fn retry_batch(&self, caller: &JobCaller, batch_id: &str) -> Result<RetryBatch, ErrorBody>;  // job.retry {batch_id} (P4a-R41)
}
// The restart hold (P4a-R41): recovery keeps each never-started job queued and records it in an in-memory
//   `restart_held: HashSet<JobId>` (its `pause_reasons` read ["restart"]); `resume(lane, Manual)` also releases the lane's.
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
  - `jobs.rs`: `move_next_and_last_stay_within_the_class` (CORE-160); `move_refuses_engine_running_and_finished_jobs` (`job.not_queued` for `queued_in_engine`, `running`, `succeeded`); `moved_jobs_never_count_as_overtaken_by_groups`; `cancel_selectors_are_exclusive` (`params.invalid` for none or two); `all_mine_is_scoped_by_caller_kind_and_app` (a `ui:test` job survives a `cli` `all_mine`); `cancel_by_batch_and_by_ids`; `pause_holds_every_class_and_caller_as_queue_paused`; `pause_never_interrupts_the_executing_prompt`; `resume_removes_only_its_reason` (paused with `manual` and `fullscreen`; `resume(manual)` leaves it paused with `["fullscreen"]`); `queue_paused_jobs_list_their_reasons` (`pause_reasons` is `["manual", "fullscreen"]`, then `["fullscreen"]`); `a_scope_covers_only_its_classes` (unit test: `All` covers every class, `AgentOnly` only `agent`); `a_restart_ends_a_pause` (P4a-R5); **`a_restart_holds_never_started_jobs`** (P4a-R41, ARCH-022: a batch of 3 waiting jobs and one `queued_in_engine` job with an `external_id`; after the restart the 3 are `queued` with `pause_reasons: ["restart"]` and the other is `interrupted`; a job submitted after the restart runs); `resume_releases_restart_held_jobs` (and they still wait while `fullscreen` is set); `a_second_restart_holds_them_again`; `retry_by_batch_retries_only_ended_jobs` (only the newest attempt at each index; the retries keep `batch_id` and `index`; the result is `{retried, refused}`; an `interrupted_unknown` job is refused with `job.not_retryable`); `retry_by_batch_twice_retries_nothing_new` (the second call creates no job, and lists the first call's originals in `refused` as "already retried" and their still-running retries with their status; `job.retry {job_id}` on an already retried job is `job.not_retryable`); `a_director_task_is_never_held` (an `assistant_task` waiting at the restart becomes `interrupted`, ARCH-022); `a_held_job_keeps_its_reservation`; `queue_state_counts_restart_held` (`held` and `restart_held`); **`held_only_resume_keeps_your_pause`** (P4a-R41: paused with `manual` and 3 restart-held jobs, `app/queue.resume {held_only: true}` → `reasons` still `["manual"]`, `restart_held: 0`, and nothing runs until a plain `app/queue.resume`). The existing `startup_marks_unfinished_jobs_interrupted_and_runs_nothing` (`crates/lil-core/tests/jobs.rs`, not protected) changes with the hold: its queued `cpu` job is now held with `pause_reasons: ["restart"]` instead of `interrupted`, an intended change, not a regression, while its running job and its external job stay `interrupted` (the protected `jobs_queue.rs::core_restart_interrupts_unfinished_jobs_and_never_reruns` uses a running job, so it still holds). `every_job_has_a_batch_and_an_index` (P4a-R13); `origin_follows_the_caller` (P4a-R12: `ui:test` → `{surface: "panel", id: "test"}`, `cli` → `{surface: "cli"}`, `mcp` → `{surface: "mcp"}`, a core-submitted inventory job → `{surface: "core"}`); `jobs_log_lines_carry_origin`; `an_unknown_surface_round_trips` (a stored `{surface: "later_thing"}` reads back unchanged); `status_seq_keeps_the_first_and_newest_63`; `unresponsive_flips_after_30_s_and_back` (`FakeClock`; `job:<id>` gets `on: true` then `on: false` after a `progress`); `old_rows_read_with_derived_batch_and_origin` (`state.rs`: a suite `state.db` at migration 8 holding a job row migrates to 9, and the row reads back with the derived values);
  - `comfy_client.rs`: `phase_follows_the_executing_node_class` (the Appendix G table, including `ingesting` after `execution_success`); `subtasks_and_indeterminate` (a loader node with no `progress` → `indeterminate: true`; a sampler with `progress` → `subtasks` last entry has `value` and `max`); `all_cached_run_says_nothing_changed`; **`withdraw_only_while_pending_and_repost_with_a_new_id_once`** (Review Focus 4: an outside prompt running, a suite prompt held pending; pause → one `/api/jobs/{id}/cancel`; resume → the core reads `/api/jobs/{old}` and `/history/{old}` first, then one `POST /prompt` with a new id; a prompt the fake starts between the `/queue` read and the cancel is left running, `AlreadyStarted`); **`crash_after_withdraw_never_posts_twice`** (fail point `jobs.after_withdraw`, core killed; after the restart the job is `interrupted`, ARCH-022, and the fake saw exactly one `POST /prompt` for it); `external_side_effects_jobs_are_never_withdrawn`;
  - `lil-protocol`: `protocol_is_1_2_and_offers_queue_v1` (`PROTOCOL.to_string() == "1.2"`, `CAPABILITIES` holds `queue.v1` after the five of 1.1, by content, so Tasks 9 and 11 can append; a 1.1 client's shared set lacks it); `job_not_queued_is_in_the_table`;
  - `dispatch.rs`: `queue_methods_need_a_verified_app_and_the_capability` (`cli` → `caller.not_allowed`; `ui:test` without `queue.v1` → `caller.not_allowed`; with it → `QueueState`);
  - spec test `crates/lil-spec-tests/tests/jobs_cache_4a.rs` (written first, from Appendix G only, with `FakeComfy::cached_nodes` beside it; Global Constraints' one exception for spec tests): `all_cached_run_says_nothing_changed` (CORE-117: with every node of the prompt cached, the job succeeds with `all_cached: true`, `cache_hit_nodes` naming them and the summary text "Nothing changed: ComfyUI served every node from its cache."), entered in `expected-fail.toml` under Task 6 with its rule.
- [ ] **Step 2: Run them.** Run: `cargo nextest run -p lil-core -p lil-protocol --retries 0 -E 'binary(jobs) | binary(comfy_client) | binary(state) | binary(dispatch) | binary(hello) | package(lil-protocol)'`, then `cargo run -q -p lil-cli -- dev spec-tests`. Expected: FAIL, and `jobs_cache_4a::all_cached_run_says_nothing_changed` listed as expected-fail.
- [ ] **Step 3: Implement.** The queue's pick orders by class rank, then `order_key`; the lane's pause is checked in admission, before VRAM. The withdrawal reads `/queue`, then cancels; the job row changes only after the cancel's answer (fail point between). `test.job` gains `phases`, `batch` and `silent_secs`, and a test-channel core honours `LIL_TEST_STALL_SECS`. Change `hello_major_mismatch_is_refused`'s value to `"1.2"` (protected).
- [ ] **Step 4: Run them, then the gate** (`-p lil-core -p lil-protocol -p lil-schema -p lil-testkit`). Remove the `jobs_queue_4a.rs` and `jobs_cache_4a.rs` entries. Expected: PASS, flake guard clean, `dev spec-tests` green with the edited `core_session.rs`.
- [ ] **Step 5: Commit.**

```bash
git commit -m "jobs: batches, origin, phases and stall flag; class order and job.move; batch cancels; Pause GPU work with safe withdrawal; protocol 1.2"
```

---

### Task 7: Telemetry and estimates: `job_stats`, `job.estimate`, `telemetry.energy`, GPU users and app tracking (CORE-161, ARCH-072, APP-127, APP-131, APP-140; P4a-R14, P4a-R27, P4a-R40) — Qt-free; merges before Task 8, which merges before Task 9

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
// crates/lil-protocol/src/estimate.rs (pure, so lil-qt uses it too): Estimate's parts, and
pub fn sum_part(parts: &[Option<&Spread>]) -> SpreadSum;   // Appendix G "Adding estimates": p50s add, p90 by root-sum-square, `of` and `not_estimated`
```

- [ ] **Step 1: Write the failing tests:**
  - `telemetry.rs`: `version_1_migrates_to_2_keeping_rows`; `job_stats_rows_round_trip`; `energy_over_a_range_matches_the_job_energy_formula` (ticked samples; `system_mj` equals `(gpu + cpu + rest) / psu` as `JobEnergy` computes); `energy_picks_the_finest_table_that_holds_the_range`; `gpu_users_lists_the_top_8_by_vram_tracked_or_not`; `a_ui_connection_is_tracked_as_app_until_it_closes` (P4a-R27);
  - `jobs.rs`: `estimate_has_no_local_or_wall_part_below_three_samples` (both listed in `unknown` with `no_history`; a tool with no key lists `no_key`); `estimate_uses_nearest_rank_over_the_newest_20` (wall seconds 1–25 recorded → the newest 20 are 6–25, p50 15.0, p90 23.0); `estimate_key_ignores_seeds_and_prefixes` (two graphs differing only in `seed`, `noise_seed` and `filename_prefix` → one key; a changed `steps` → another); `estimate_is_taken_at_submit` (`JobSummary.estimate` equals `job.estimate` at the time); `job_estimate_validates_args_like_a_call`; `a_local_job_has_no_money_part_and_an_energy_cost_when_a_rate_is_set`; `estimate_is_never_changed_after_submit` (more history later leaves `JobSummary.estimate` as it was); `sum_part_adds_p50s_and_never_p90s` (p50s 10 and 20, p90s 14 and 23 → p50 30, p90 35.0; one job without the part → `of: 3, not_estimated: 1`);
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

### Task 8: ComfyUI: the launch profile, the out-of-memory advice, the model inventory report and stored environment captures (ARCH-065, ARCH-126, CORE-130, CORE-163, CORE-164; P4a-R15, P4a-R28) — Qt-free; after Task 7, before Task 9

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

### Task 9: Desktop state: the fullscreen poll, auto-pause, keep-awake and desktop settings (ARCH-127, CORE-160, APP-039, APP-146; P4a-R5, P4a-R6, P4a-R24; owner decision 2026-10-22) — Qt-free, adversarial review

**Depends on:** Tasks 5, 6 and 8 merged (after Task 8, for `crates/lil-schema/src/suite.rs`).

**Files:**
- Create: `crates/lil-core/src/desktop.rs` (infrastructure, P4a-R24).
- Modify: `crates/lil-core/src/server/mod.rs` (`real_desktop` on stable and dev, `FakeDesktop` on test channels; `LIL_TEST_DESKTOP_POLL_MS`), `crates/lil-core/src/rpc.rs` (`app/desktop.set`), `crates/lil-core/src/tools/{mod.rs, test_tools.rs}` (`desktop.get`, `test.desktop`), `crates/lil-core/src/config.rs`, `crates/lil-schema/src/{suite.rs, suite_edit.rs}` (`[notifications]`, `[jobs] fullscreen_autopause`, `[power] keep_awake_gpu`, targeted edits), `crates/lil-protocol/src/{session.rs, app.rs}` (`desktop.v1`, `DesktopSetParams`, `DesktopSettings`), `crates/lil-core/tests/module_deps.rs`.
- Modify (shared): `crates/lil-core/src/tools/tests.rs` (pins), `crates/lil-core/tests/hello.rs` (its list ends with `desktop.v1`).
- Test: `crates/lil-core/tests/desktop.rs` (new), `crates/lil-core/tests/dispatch.rs` additions.

**Interfaces:**
- Consumes: `lil_platform::desktop::{Desktop, Fullscreen}`, `session_end::PowerEvent` (Task 5); `Jobs::{pause, resume}` (Task 6).
- Produces (test contract, Global Constraints' test-channel tools): `test.desktop` records each posted toast with its `xml` (the `Toast.xml` handed to `Desktop::post`), and `foreground: "suite"` makes a connected verified `ui:*` app's pid the foreground pid (`"other"`, another pid).
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

### Task 10: Notifications: the notifier, toasts and the notification path for confirmations (ARCH-005, ARCH-055, CORE-113, APP-022, APP-051, APP-146, QA-016; P4a-R7, P4a-R8, P4a-R9, P4a-R36, P4a-R40, P4a-R41) — Qt-free, adversarial review

**Depends on:** Tasks 4, 7 and 9 merged.

**Files:**
- Create: `crates/lil-core/src/interchange/{notify.rs, toast_xml.rs}`.
- Modify: `crates/lil-core/src/interchange/{mod.rs, tools.rs}` (`notify.claude_code`), `crates/lil-core/src/confirm/mod.rs` (`Notifier` as the `NotificationPath`; `UiPresence` keeps the pids; the toast tokens and `toast_token` in `app/confirmation.answer`), `crates/lil-protocol/src/app.rs` (`toast_token`), `crates/lil-core/src/server/mod.rs` (wiring), `crates/lil-core/Cargo.toml` (`lil-parse`, if Task 8 hasn't added it).
- Test: `crates/lil-core/tests/notify.rs` (new), `crates/lil-core/tests/confirm.rs` additions.

**Interfaces:**
- Consumes: `DesktopWatch`, `DesktopSettingsStore` (Task 9); `lil_parse::{shown_text, confirm_url, task_url, open_url}` (Task 4); `JobSummary.origin` (Task 6).
- Produces (test contracts that Task 3's spec tests rely on):
  - `desktop.get {fresh: true}` recomputes the one cached reachability that `NotificationPath::reachable` reads (a single 10 s cache, P4a-R33), so a confirmation call made right after it sees the new state;
  - Task 10 wires an agent caller's `open_in` and `ui.show` to `Notifier::agent_opened`, so `notifications::agent_open_posts_agent_activity` stays in Task 10's group (Task 11 consumes the same call for `lil-mcp`'s launches).
- Produces:

```rust
pub enum ToastActions { None, OpenAndDeny { open_url: String, deny_url: String } }
pub struct ToastText { pub title: String, pub body: String, pub launch: String, pub actions: ToastActions }  // no progress binding (P4a-R36)
pub fn toast_xml(t: &ToastText) -> String;                                       // Appendix G
pub enum NotifyOutcome { Posted, CategoryOff, Foreground, Duplicate, Unavailable }
pub struct Notifier;
impl Notifier {
    pub fn start(desktop: Arc<dyn Desktop>, watch: Arc<DesktopWatch>, settings: watch::Receiver<DesktopSettings>,
                 events: Arc<EventBus>, presence: Arc<UiPresence>, env: Env, clock: Arc<dyn Clock>) -> Arc<Notifier>;
    pub async fn claude_code(&self, message: &str, title: Option<&str>) -> NotifyOutcome;
    pub async fn agent_opened(&self, target_title: &str, target: &str) -> NotifyOutcome;
    pub async fn restart_held(&self, held: usize, first: &JobId) -> NotifyOutcome;   // Appendix G "Restart hold" (P4a-R41)
}
impl confirm::NotificationPath for Notifier { fn reachable(&self) -> bool; }    // Appendix G "Reachable"
```

- [ ] **Step 1: Write the failing tests:**
  - `notify.rs`: `toast_xml_escapes_and_normalises` (`<`, `>`, `&`, `"` escaped; U+202E shown; the XML parses); `confirmation_toast_xml_has_open_and_deny` (two protocol actions, "Open" with the confirm URL and "Deny" with the deny URL; no other toast has actions); `categories_and_their_defaults`; `one_toast_per_batch_when_its_last_job_ends` (a batch of 3 → one "Batch finished (3 jobs)"; one failed → "Job failed"); `cancelled_and_core_jobs_never_toast`; `a_suite_window_in_front_suppresses_and_nothing_is_queued` (P4a-R7: moving the foreground away later posts nothing old); `quiet_while_fullscreen_sets_silent_and_drops_nothing`; `no_progress_toast_is_ever_posted` (P4a-R36: an old `job_progress = true` in `suite.toml`, kept and ignored, and a job estimated at 60 s → no toast op for it until `job_finished`); `a_restart_hold_posts_one_toast` (P4a-R41: a start that held 3 jobs posts one `job_failed`-category "Jobs held after a restart" toast launching `task/<the first held job>`; a start that held none posts nothing); `fyi_toasts_coalesce_per_origin` (two Claude Code batches → the second's toast replaces the first by Tag `o_<16 hex>`, each pops once when its batch ends; a `panel` batch keeps its own Tag; a confirmation toast is never replaced by another's); `an_unknown_category_key_is_kept_and_ignored`; **`confirmation_toast_is_removed_when_answered_or_expired`** (Review Focus 3); **`a_toast_deny_needs_its_token_and_works_once`** (P4a-R9: each toast gets a fresh 128-bit token; the right token denies once; a reused, wrong or other confirmation's token is `confirm.unknown`, checked before the confirmation's state; after the confirmation is answered in the window, or expires, or is cancelled, its token is `confirm.unknown` at once; a window answer without a token for an already answered id is `confirm.already_answered`; after a core restart an old token is `confirm.unknown`; `approve` with a token, or a token with a `cap`, is `params.invalid`); `a_toast_token_never_reaches_a_log_line` (after a deny and a refused deny, no core log line holds the 32-hex token, and the logged URL ends `deny/<id>/…`); `toast_tokens_come_from_the_os_random_source` (the notifier takes its tokens from the injected id source, which is the platform's random source in production); `cap_alert_posts_once_per_threshold`; `claude_code_duplicates_within_10_s_are_dropped`; `agent_open_posts_agent_activity_with_an_open_url`;
  - `confirm.rs`: `reachable_needs_toasts_shortcut_handlers_and_the_category` (each part missing → `false`); **`reachable_is_false_when_windows_turns_the_apps_toasts_off`** (Review Focus 6: each `ToastsState::Unavailable` reason → `false`, and `desktop.get` names it); **`an_elevated_core_is_never_reachable`**; `an_unreachable_path_answers_unavailable_at_once_without_an_app` (no 30-minute wait: the call returns `confirm.unavailable`); `a_call_waits_without_an_app_when_reachable` (ARCH-055: `cli` `test.destructive`, no app → `pending_confirmation` and a `confirmation_waiting` toast whose launch is `<scheme>:confirm/<id>`; an app connected later answers it and the retry runs once); `non_interactive_callers_still_get_unavailable` (QA-016); `turning_the_category_off_makes_calls_unavailable_without_an_app`.
- [ ] **Step 2: Run them.** Run: `cargo nextest run -p lil-core --retries 0 -E 'binary(notify) | binary(confirm)'`. Expected: FAIL.
- [ ] **Step 3: Implement.** The notifier subscribes in-process to `jobs`, `confirmations` and `caps`; it keeps the tags it posted, so it can remove them; reachability is cached for 10 s.
- [ ] **Step 4: Run them, then the gate** (`-p lil-core`). Remove the `notifications.rs` entries. Expected: PASS.
- [ ] **Step 5: Commit.**

```bash
git commit -m "notifications: toasts by category with quiet fullscreen and per-origin coalescing, and the notification path so confirmations wait with no app open"
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
- Produces (test contracts that Task 3's spec tests rely on):
  - `lil-fake-image` as Global Constraints' "Test fakes" bullet says: `#![windows_subsystem = "windows"]`, built with the test binaries as `target\testbins\debug\lil-fake-image.exe`, and one JSON line `{"args": [<argument>, …]}` a run in `%LIL_TEST_ROOT%\fake-image-args.jsonl`;
  - a copied install works on a test channel: copies of `lil-core.exe`, `lil-mcp.exe` and `lil-image.exe` in one folder, with `apps\image.toml` beside the copied core, started with `CoreFixture::start_exe`, trust each other there as an installed `bin\` does (`interchange_4a::mcp_launches_a_stopped_app_without_focus`), and a copied `lil.exe` beside `lil-image.exe` runs `lil confirm-window` (`doctor_4a::confirm_window_starts_the_app_in_confirm_mode`). If they don't, the fix goes in the test kit or the test channel's trust rule, never in the spec tests.
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
- Reads (a format Task 3's spec test relies on): `Redactor.labels` are the `label`s of `suite.toml`'s `[[claude_code.profiles]] {id, label, config_dir}`, AI-088's fields; the Claude Code profiles plan keeps that key or migrates it.
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
  - `diagnostics.rs`: `bundle_holds_the_listed_entries_and_no_workspace_file`; `redaction_covers_scrub_words_labels_emails_and_targets`; `profile_folder_and_machine_name_are_rewritten` (owner decision 2026-10-22: the profile folder in its long form, its 8.3 short form (as `GetShortPathNameW` gives it), with forward slashes, with a `\\?\` prefix, in another letter case, and JSON-escaped with doubled backslashes in a log line, each becomes `%USERPROFILE%`; the computer name becomes `<machine>`); `bundle_writes_only_under_the_diagnostics_folder`; `the_redactor_cuts_deny_tokens` (a log line holding `lil-dev:deny/cf_0123456789abcdef/<32 hex>` becomes `lil-dev:deny/cf_0123456789abcdef/<redacted>`); `bundle_with_a_job_adds_its_summary_and_untrusted_args`; `recent_groups_by_code_newest_first_over_the_last_1000`; `each_error_is_published_on_the_diagnostics_topic`; `nothing_is_sent_anywhere` (a bundle run with the test network guard makes no outbound connection, ARCH-122);
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
- Modify: `crates/lil-cli/src/user/{doctor.rs, mod.rs}`; `crates/lil-testkit/src/env.rs` (a new cleanup for leftover test registry keys; Task 3's review M9). A doctor test that is killed (nextest's `terminate-after`) leaves its `HKCU\Software\LILITH-test-<id>` key behind: harmless, outside the shell's view, but accumulating. The test kit has no sweep for it yet, so this task adds one:
  - the key's name already carries the test channel id (`<id>`, from `TestEnv`'s `Channel::Test(<id>)`); `TestEnv` also puts that id in its temporary folder's name, `lil-testenv-<id>-<random>` (and `lil-testenv-<label>-<id>-<random>` for `with_label`), so a key can be matched to its root;
  - once per test process, on the first `TestEnv::new`, the sweep lists the subkeys of `HKCU\Software` named `LILITH-test-<8 lowercase hex>` and deletes each one for which no folder `lil-testenv-*<id>-*` exists in the system temporary folder; a key whose root still exists (a test running in another process) is kept;
  - it touches nothing else: never `HKCU\Software\LILITH`, `HKCU\Software\Classes` or a key of another shape.
- Test: `crates/lil-cli/tests/{doctor_desktop.rs, confirm_window.rs}` (new); `crates/lil-testkit/tests/cleanup.rs` addition (`leftover_test_registry_keys_are_swept`: a `LILITH-test-<id>` key with no root is removed, one whose root exists is kept, and a key of any other name is never touched).

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
- Create: `/CMakeLists.txt` (Corrosion imports `lil-core`, `lil-cli`, `lil-mcp` and `lil-qt`; `add_subdirectory(apps/…)`; every exe and DLL lands in one `bin/`), `/CMakePresets.json` (configure `dev` and `ci`: Ninja, `RelWithDebInfo`, `build/<preset>`, `CMAKE_PREFIX_PATH` from `LIL_QT_DIR`; build presets of the same names; test presets `qml`, `grabs`, `deploy`, `bench`, and `ci-qml` and `ci-deploy`, each by CTest label, `qml` and `grabs` with `QT_QPA_PLATFORM=offscreen`).
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
  Hardware, Usage & spend, Diagnostics and Settings tabbed at the bottom (ARCH-010's panels of this plan, and Settings).
Global keys (plan 4a): Ctrl+1 Home, Ctrl+2 Jobs, Ctrl+3 Confirmations, Ctrl+4 Hardware, Ctrl+5 Usage & spend,
  Ctrl+6 Diagnostics, Ctrl+, Settings, Ctrl+Shift+P Pause or resume GPU work, F6 / Shift+F6 next / previous panel.
  All are in the global scope; the browser, Review and canvas scopes (APP-143) may reuse none of them.
  Scopes BrowserReview and Canvas are declared empty (plans 4b and 5).
Theme tokens (dark, APP-012): bg, surface, surfaceRaised, text, textMuted, accent, danger, warning, ok, focus.
A11yWalk: visits every item of a panel; each visible interactive item must have a non-empty Accessible.name,
  an Accessible.role other than NoRole, and be in the tab chain.
Window title: "LILITH — <project>" until the Image studio exists (P4a-R25; owner decision 2026-10-22).
```

- [ ] **Step 1: Write the failing tests:**
  - Qt-free: **`layout_clamps_windows_to_the_available_area`** (Review Focus 5: a window on an unplugged screen moves onto the primary's available area; a layout saved at 150 % restored on a 100 % screen keeps the window inside the area; one larger than its area shrinks to it; a maximized window restores onto its own screen); `layout_file_round_trips`; `a_bad_layout_file_is_set_aside`; `the_plan_4a_map_has_no_conflicts` (Ctrl+, included); `a_scope_key_equal_to_a_global_key_is_a_conflict`; `app_state_round_trips_last_seen_per_workspace`;
  - QML: `tst_layout::test_restore_runs_in_order` (the shim's hook logs: windows created → factory registered → restore → clamp); **`test_bad_layout_falls_back_to_default`** (a damaged file, an empty file, and a KDDockWidgets blob its restore refuses, as a newer version's would; each is set aside and the Default layout opens); `test_float_and_redock_a_placeholder_keeps_its_state` (ARCH-116; each real panel adds its own, and Task 29 runs the full set); `tst_a11y::test_every_control_has_a_role_a_name_and_focus` (A11yWalk over a page of every control); `tst_layout::test_window_title_names_the_project` (with the fixture project `sketchbook` the title is "LILITH — sketchbook"); `tst_theme::test_text_contrast_is_at_least_4_5_to_1` (`text` and `textMuted` on `bg`, `surface` and `surfaceRaised`; `focus` at least 3:1); `tst_controls::test_keyboard_operates_every_control` (Space and Enter press buttons, toggles flip, Escape closes a banner); `grab_shell` (the Default layout).
- [ ] **Step 2: Run them.** Run: `cargo nextest run -p lil-qt --retries 0 -E 'test(/layout|shortcut|app_state/)'` and `ctest --preset qml -R "tst_layout|tst_a11y|tst_theme|tst_controls"`; `ctest --preset grabs -R grab_shell`. Expected: FAIL.
- [ ] **Step 2b: The accessibility smoke check** (automated, no owner action; owner direction 2026-10-23 and round 12): Task 0's S0.7 list on the real shell, read through UI Automation on your PC as spike 10 read it (names exposed, the announcement arriving, chart tiles named "chart"), not by a Narrator walk-through, recorded in the PR.
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
- Test: `crates/lil-qt/tests/instance.rs` (Qt-free, two processes through a probe binary); `apps/lil-ui-kit/tests/tst_open_card.qml`; `crates/lil-qt/tests/desktop_app.rs` (default features; `desktop_second_launch_comes_to_front`, which starts the deployed app from `LIL_DEPLOY_DIR`).

**Interfaces:**
- Consumes: `lil_parse::{strip_argument, parse_lil_url}`, `lil_schema::parse_lilref`; `raw_command_line`, `split_first_argument`, `allow_set_foreground` (Task 5).
- Produces:

```rust
pub enum Mode { Shell, Confirm { id: Option<String> } }
pub enum Intent { Open, Edit, UseAsInput }                    // lil-qt's own; wire as in APP-066
pub enum Opened { Confirm(String), Deny { id: String, token: String }, Task(String), Target(String), Ref(String), LilRef { reference: String, title: Option<String> } }
pub struct BootPlan { pub mode: Mode, pub intent: Intent, pub no_activate: bool, pub opened: Option<Opened>, pub refused: Option<String>, pub forwarded: bool, pub org: String, pub app: String }
pub fn plan_boot(raw_cmdline: &str, env: &Env) -> BootPlan;
pub struct InstanceServer;
impl InstanceServer { pub fn claim(env: &Env, app: &str, confirm: bool) -> std::io::Result<Option<InstanceServer>>; pub fn next(&self) -> Option<Forwarded>; }
pub struct Forwarded { pub args: Vec<String> }
pub fn forward(env: &Env, app: &str, confirm: bool, args: &[String]) -> Result<u32 /* pid */, ForwardError>;
```

- [ ] **Step 1: Write the failing tests:**
  - Qt-free `args.rs` tests: `args_parse_every_form` (`--intent`, `--no-activate`, `--confirm` with and without an id, a URL, a path, a `.lilref`); **`args_refuse_a_large_lilref_and_a_bad_hash`** (Review Focus 2: a 1 GB sparse `.lilref` is refused after reading at most 4 KiB; a `.lilref` naming a ref the core answers with `ref.hash_mismatch` shows that message); `args_refuse_a_network_lilref` (a `\\server\share\x.lilref` path is refused unread: "LILITH opens .lilref files from this PC only"); `a_refused_argument_never_answers_or_opens`;
  - `instance.rs`: `a_second_launch_forwards_and_exits` (the probe's second process exits 0 within 2 s and the first receives its arguments); **`two_quick_second_launches_reach_one_window`** (Review Focus 3: two launches started 10 ms apart both forward to the one running instance, which gets both arguments in order, and no second window appears); `forward_checks_the_server_image_path` (a server run from another exe → `ForwardError::Untrusted`, nothing written); `forward_gives_up_after_2_s`; `a_confirm_url_goes_to_the_running_full_app_first` (P4a-R11); `the_confirm_window_has_its_own_pipe`; **`deny_url_opens_no_window_and_answers_once`** (a `deny/` URL with the app running is forwarded, and the app sends one `app/confirmation.answer {answer: "deny", toast_token}` carrying the URL's token, without showing or activating a window; with no app running, the deny is handled before `QGuiApplication` starts: the process connects with `StartPolicy::NoStart`, sends the deny and exits 0, so a stale toast's Deny after the core stopped starts no core (its token died with that core); an unknown or dead token shows nothing and writes one log line with the token cut);
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
- Test: `crates/lil-qt` unit tests; `apps/lil-ui-kit/tests/{tst_confirmations.qml, tst_cap_prompt.qml, tst_confirm_window.qml}`; `crates/lil-qt/tests/desktop_app.rs` gains `desktop_toast_url_brings_the_confirm_window_to_front` (default features, the deployed app from `LIL_DEPLOY_DIR`); grabs.

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
  - `tst_confirm_window.qml`: **`test_answered_elsewhere_shows_state`** (Review Focus 3: pending → answered by another app → "Answered in the app at <time>" and no buttons); `test_expired_and_unknown_ids_say_so`; **`test_reconnect_keeps_the_dialog_and_its_state`** (Review Focus 1: `Core.connection` goes `reconnecting` while the dialog is open; its buttons disable, the banner shows, and after `ready` it re-reads the confirmation and shows its live state); `test_without_an_id_lists_every_pending_confirmation`; `test_a_second_url_adds_to_the_open_window` (P4a-R11); `test_a_deny_url_reaching_the_open_window_denies_without_changing_focus` (the toast's Deny arrives on the confirm-window process's pipe while its window is open: the card shows "Denied from the notification" and the window doesn't take focus);
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

### Task 22: The Jobs panel and the cost and time chip (APP-114, APP-139, APP-140, CORE-160, CORE-161; P4a-R40, P4a-R41) — needs Qt

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
pub fn queued_eta_text(running_left: Option<f64>, ahead: &[Option<&Spread>], own: Option<&Spread>) -> String;  // Appendix G: "about 3 min", "after 2 jobs with no estimate" (P4a-R40)
pub fn elapsed_text(s: f64) -> String;
```

- [ ] **Step 1: Write the failing tests:** Qt-free `chip_text_local`, `chip_text_money_with_its_cap_share`, `chip_text_none`, `eta_never_goes_below_zero`, `queued_eta_adds_the_work_ahead_and_counts_unestimated_jobs`, `chip_text_reads_the_local_part` (the chip's text is unchanged from a flat estimate); `tst_jobs_panel.qml`: `test_rows_update_in_place_at_most_10_hz` (FakeCore sends 50 progress events in a second: the row's value is the newest and at most 11 updates applied); `test_preview_url_changes_with_each_frame`; `test_lane_header_shows_waiting_for_vram_and_outside_jobs` (a queued job in `waiting_for_vram` and `outside_jobs {running: 1, queued: 2}` show "Waiting for GPU memory" and "ComfyUI: 1 outside job running, 2 queued"); `test_run_next_and_last_call_job_move`; `test_cancel_batch_and_all_mine`; `test_pause_and_resume_call_app_queue`; `test_paused_header_names_manual_and_fullscreen` (`reasons: ["manual"]` shows "Paused by you", `["fullscreen"]` "Paused while fullscreen", both "Paused by you and while fullscreen"); `test_a_held_row_names_its_reasons` (`pause_reasons`; `["restart"]` reads "Held after a restart"); `test_restart_held_jobs_show_a_resume_control_when_not_paused` (P4a-R41: `QueueState {paused: false, held: 3, restart_held: 3}` shows "Resume held jobs (3)", which sends `app/queue.resume {lane, held_only: true}`; with `restart_held: 0` the control is gone); **`test_resume_held_jobs_keeps_your_pause`** (P4a-R41: `QueueState {paused: true, reasons: ["manual"], held: 5, restart_held: 3}` shows both "Resume" and "Resume held jobs (3)"; the held control sends `{held_only: true}` and nothing else, the fake core then reports `reasons: ["manual"]` and `restart_held: 0`, the header still says "Paused by you", and no job starts until "Resume" is pressed); `test_unknown_kind_origin_and_reason_show_their_raw_names` (P4a-R40); `test_retry_shows_the_cost_chip_from_the_jobs_estimate`; `test_float_and_redock_keeps_jobs_state` (ARCH-116); A11yWalk (job rows set role and name); `tst_cost_chip.qml`: `test_debounced_estimate_call` (one `job.estimate` 300 ms after the last change); grabs: running, paused, empty.
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

- [ ] **Step 1: Write the failing tests:** Qt-free `fifteen_minutes_use_1s_and_24_hours_use_1m`, `downsampling_keeps_min_and_max_per_pixel`, `energy_periods_follow_the_local_calendar` (DST day included); `tst_hardware.qml`: `test_live_values_from_the_snapshot` (a fixture `Snapshot` with GPU 63 %, 9,728 of 16,384 MiB, 71 °C, 285 W, CPU 18 % on 16 cores and 64 W, RAM 41 of 64 GiB shows "63 %", "9.5 / 16 GiB", "71 °C", "285 W", 16 core bars, "64 W" and "41 / 64 GiB"; disks, network, clocks, fan and throttle reasons each show their fixture value); `test_cpu_temperature_is_never_shown` (APP-128); `test_opens_on_15_minutes_and_zooms_to_24_hours` (APP-129); `test_energy_table_calls_telemetry_energy_for_each_period` (APP-131, with cost when set); `test_alerts_show_and_clear` (APP-132); `test_gpu_users_list_names_the_app_and_engines` (a `gpu_users` fixture with roles `app` and `engine` and one untracked game lists "LILITH (this app)", "ComfyUI" and the game's name, each with its MiB); `test_estimated_and_missing_parts_are_labelled`; `test_float_and_redock_keeps_hardware_state` (ARCH-116); A11yWalk (chart tiles have role and name); grabs.
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

- [ ] **Step 1: Write the failing tests:** `tst_diagnostics.qml`: `test_versions_and_engine_health` (from `core/hello`'s `build_id` and `engines.status`: "Core <build>", "ComfyUI <version> · owned · ready", and "attached" for an attached engine); `test_launch_profile_flags_and_catalogue_are_shown` (ARCH-126, ARCH-065); `test_recent_errors_are_grouped_by_code`; `test_event_log_keeps_the_newest_1000`; `test_export_calls_diagnostics_bundle_and_shows_the_path`; `test_models_report_states_are_listed`; `test_backup_status_line_and_the_backups_panel_empty_state` (APP-133, APP-134); `test_notification_path_problems_are_named` (Review Focus 6: each `notification_path` problem shows its sentence: "Notifications are off for LILITH in Windows Settings", "Notifications are off in Windows Settings", "A policy blocks notifications", "The core runs as administrator, so Windows won't show its notifications", "The Start-menu shortcut is missing", "Another LILITH build handles lil: links"; each ends with what to do, `lil doctor` or the Settings panel's "Set up"); `test_keep_awake_unavailable_is_said` (ARCH-127); `test_start_times_are_shown` (APP-136); `test_float_and_redock_keeps_diagnostics_state` (ARCH-116); A11yWalk; grabs.
- [ ] **Step 2: Run them.** Run: `ctest --preset qml -R tst_diagnostics`. Expected: FAIL.
- [ ] **Step 3: Implement.** Cards read their tools once on open and follow their topics; the event log is Task 17's `EventLogModel`; the start times are `Core.startTimes`.
- [ ] **Step 4: Run them, then the gate.** Expected: PASS.
- [ ] **Step 5: Commit.**

```bash
git commit -m "diagnostics panel: versions, engines and launch profile, errors by code, event log, model inventory, bundle export; backups empty state"
```

---

### Task 26: Home (minimal) with "While you were away" (APP-111, APP-016, APP-163; P4a-R16, P4a-R21, P4a-R41) — needs Qt

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

- [ ] **Step 1: Write the failing tests:** Qt-free `away_selects_finished_failed_unknown_and_pending_since_last_seen`, `away_is_empty_without_a_last_seen_time` (first run shows no card), `away_orders_newest_first`, `away_lists_jobs_held_by_a_restart` (P4a-R41: a queued job with `pause_reasons: ["restart"]` is listed whatever its age); `tst_home.qml`: `test_projects_and_status_counts` (APP-111: a fixture with projects `sketchbook` (12 assets) and `lantern` (3) and `assets_by_status` of 4 candidate, 2 in review, 1 stale, 0 blocked shows both projects with their counts and "Takes waiting 4 · In review 2 · Stale 1 · Blocked 0"); `test_status_copy_says_takes` (APP-163: the `candidate` count reads "Takes waiting"); `test_open_jobs_list`; `test_away_card_lists_items_and_last_seen_is_written_after_3_s_visible`; `test_away_card_dismiss_clears_it`; `test_no_workspace_shows_recent_loose_files_instead` (APP-016; empty until plan 5); `test_opened_card_sits_in_home` (P4a-R21); `test_float_and_redock_keeps_home_state` (ARCH-116); A11yWalk; grabs.
- [ ] **Step 2: Run them.** Run: `cargo nextest run -p lil-qt --retries 0 -E 'test(/away_/)'` and `ctest --preset qml -R tst_home`. Expected: FAIL.
- [ ] **Step 3: Implement.** Home reads its cards on open and after each `jobs` or `confirmations` event; `last_seen_at` is written through `AppState` after 3 s of visibility.
- [ ] **Step 4: Run them, then the gate.** Expected: PASS.
- [ ] **Step 5: Commit.**

```bash
git commit -m "home: projects, recent assets, status counts, open jobs and the While you were away card"
```

---

### Task 27: The Settings panel: notifications, quiet mode, auto-pause, keep-awake and notification setup (APP-022, APP-146, ARCH-127, CORE-160; P4a-R26, P4a-R36; owner decisions 2026-10-22 and 2026-10-24) — needs Qt

**Depends on:** Tasks 5, 9, 10, 18 and 25 merged.

**Files:**
- Create: `apps/lil-ui-kit/qml/panels/settings/{SettingsPanel.qml, NotificationToggles.qml, DesktopToggles.qml, NotificationSetup.qml}`, `apps/lil-ui-kit/qml/shell/FirstStartCard.qml`; `crates/lil-qt/src/setup.rs` (Qt-free: calls `lil_platform::{write_shortcut, register_handlers}` for the channel's `lil-image.exe`, then `desktop.get {fresh: true}` through `lil-client`, and returns the new `notification_path`); `crates/lil-qt/src/qt/setup_bridge.rs` (a thin invokable over it, run on the background runtime).
- Modify: `apps/lil-ui-kit/qml/shell/Panels.js` (Settings replaces its placeholder); `apps/lil-ui-kit/qml/shell/ShellWindow.qml` (the first-start card, owner decision 2026-10-22).
- Test: `apps/lil-ui-kit/tests/{tst_settings.qml, tst_first_start.qml}`; `crates/lil-qt/tests/setup_desktop.rs` (default features, so the Sandbox runner's archive holds it); grabs.

**Interfaces:**
- Consumes: `desktop.get`, `app/desktop.set` and the `desktop` topic (Task 9); `notification_path` and its reasons (Tasks 9, 10); `lil_platform::{write_shortcut, register_handlers}` (Task 5).
- Produces: the Settings panel, with one toggle per notification category (six: the seventh category of owner decision 2026-10-22 is in, and "Job progress" is withdrawn, owner decision 2026-10-24, P4a-R36), labelled "Job finished", "Job failed" (which also covers the "Jobs held after a restart" toast, P4a-R41), "Confirmation waiting", "Spending alerts", "Director-task decisions" (shown disabled until the director plan) and "Claude Code and agent activity"; "Quiet while fullscreen"; "Pause GPU work while a game or video is fullscreen" (fullscreen auto-pause), "Keep this PC awake while GPU work runs" (hidden when `keep_awake_available` is false, ARCH-127), and a "Notifications on this PC" card showing `notification_path` with a "Set up" button while the shortcut or the handlers are missing (P4a-R26). On the app's first start (no `state.json` yet) a card offers the same "Set up" button while they are missing; "Not now" dismisses it, and later starts don't show it again (Settings still does).

- [ ] **Step 1: Write the failing tests:** `tst_settings.qml`: `test_toggles_show_desktop_get` (a fixture `DesktopSettings` with `cap_alert` off and the rest on shows exactly that, and no "Job progress" toggle exists, P4a-R36); `test_each_toggle_calls_app_desktop_set_with_one_key` (flipping "Job failed" sends `app/desktop.set {notifications: {job_failed: false}}` and nothing else; the auto-pause and keep-awake toggles send `{fullscreen_autopause: true}` and `{keep_awake_gpu: false}`); `test_keep_awake_toggle_hidden_when_unavailable`; `test_settings_event_updates_the_toggles` (a `desktop` `{kind: "settings"}` event from another app changes the switches); `test_a_refused_set_restores_the_toggle_and_says_why` (a `caller.not_allowed` or `params.invalid` answer puts the switch back and shows the error); **`test_unreachable_path_shows_the_reason_and_a_fix`** (Review Focus 6: `toasts_reason: "disabled_for_app"` shows "Notifications are off for LILITH in Windows Settings" and an "Open Windows notification settings" link, `ms-settings:notifications`); `test_set_up_button_writes_then_rereads` (the button calls the setup bridge once and shows the `notification_path` it returns, from `desktop.get {fresh: true}`, so the card leaves at once when `shortcut` and `handlers` are `ok`); `tst_first_start.qml`: `test_card_shows_on_the_first_start_while_setup_is_missing`, `test_card_never_shows_once_state_json_exists`, `test_not_now_dismisses_it`; `setup_desktop.rs`: `desktop_set_up_writes_the_shortcut_and_handlers` (`#[ignore = "desktop"]`, Sandbox runner) and `setup_rereads_fresh_after_writing` (with `FakeDesktop` and a test core: one `desktop.get {fresh: true}` after the writes); `test_float_and_redock_keeps_settings_state` (ARCH-116); A11yWalk over the panel; grabs: settings, unreachable, first start.
- [ ] **Step 2: Run them.** Run: `ctest --preset qml -R "tst_settings|tst_first_start"`. Expected: FAIL.
- [ ] **Step 3: Implement.** Each toggle sends one key and shows the answer the core returns (never its own guess); the setup bridge runs on the background runtime, never on the GUI thread (QA-026).
- [ ] **Step 4: Run them, then the gate.** The real write is the desktop-group test `desktop_set_up_writes_the_shortcut_and_handlers`, run through the Sandbox runner (Task 30). Run also `cargo nextest run -p lil-qt --retries 0 -E 'binary(setup_desktop)'`. Expected: PASS.
- [ ] **Step 5: Commit.**

```bash
git commit -m "settings panel: notification categories, quiet while fullscreen, fullscreen auto-pause, keep-awake, notification setup and the first-start card"
```

---

### Task 28: Windows Hello for `lil dev approve` (QA-088, QA-020, QA-036; P4a-R23, P4a-R38; owner decisions 2026-10-23) — Qt-free, adversarial review — **protected: `crates/lil-cli/src/dev/reviewer.rs`, `crates/lil-cli/src/dev/mod.rs`**

**Depends on:** Tasks 1 and 5 merged.

**Files:**
- Create: `crates/lil-platform/src/hello.rs`.
- Modify: `crates/lil-platform/{Cargo.toml, src/lib.rs}`; protected `crates/lil-cli/src/dev/{reviewer.rs, mod.rs}` (`lil dev reviewer-hello enroll | status | remove`; `approve` tries Windows Hello first when enrolled), protected `crates/lil-cli/src/dev/merge_flow.rs` (new: `lil dev merge-flow`, P4a-R38) and the `lil dev merge` source (every merge asks for your authentication); the suite repo's Claude Code settings and their protected hook-pattern tests (QA-061), changed only as far as the merge flow needs.
- Test: `crates/lil-cli/tests/reviewer_hello.rs` (new; not a protected path).

**Interfaces:**
- Consumes: no route: Task 1's S1.8 left it open (spike 11 §7–8). Step 0 settles it (P4a-R38), and only the route it proves is built, with the passphrase as the fallback either way.
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
// lil_platform::hello, from Step 0: the window-owned calls (bindings from newer metadata, P4a-R38)
pub fn kek_enroll_for_window(hwnd: isize, credential: &str) -> std::io::Result<()>;
pub fn kek_derive_for_window(hwnd: isize, credential: &str, challenge: &[u8; 32]) -> std::io::Result<Zeroizing<[u8; 32]>>;
// lil-cli dev: lil dev merge-flow <PR> --summary <file> --audit <file>  (P4a-R38)
pub trait Authenticator { fn authenticate(&self, summary: &str, audit: &str) -> Result<Zeroizing<Vec<u8>>, KeyError>; }  // Hello first, then the passphrase; a fake in tests
```

- [ ] **Step 0: the route step** (on your PC with the hands-off pop-up, so no input reaches a call; each probe cancels its prompt once the placement is read, and the one real create and sign is the single short owner check, under 2 minutes, that round 12 allows). Generate the window-owned bindings (`windows-bindgen` from newer metadata, or a raw COM binding) and run a probe that creates, signs with and deletes a throwaway credential owned by a real window that is in front, with another application in front first and then a fullscreen one, recording `GetForegroundWindow` every 150 ms. Pass: the prompt is the foreground window within 2 s each time, with no input since the call. Then the same for `RequestVerificationForWindowAsync` owned by that window. Route: KEK when the first passes, else DPAPI with the Windows Hello gate when the second passes, else passphrase only, which goes to the owner. Record it in the PR body and in `docs/spikes/11-windows-integration.md` §8.
- [ ] **Step 1: Write the failing tests** (no real Windows Hello: an injected `Unlocker`):
  - `enroll_needs_the_existing_passphrase_key` (enrolling decrypts `app-key.age` with your passphrase first, then writes the Hello file; the passphrase file stays);
  - `approve_uses_hello_when_enrolled` and `approve_falls_back_to_the_passphrase_when_hello_is_cancelled_or_fails`;
  - `no_network_call_before_the_key_is_unlocked` (the existing invariant, through Hello too);
  - `status_names_the_route_and_whether_the_key_is_hardware_held` (never "hardware-bound" without a TPM, QA-088);
  - `remove_deletes_the_hello_files_and_the_credential_and_keeps_the_passphrase_file`;
  - `a_damaged_hello_file_never_blocks_the_passphrase_path`.
  - `merge_flow_shows_the_summary_and_the_audit_before_authenticating` (a fake `Authenticator` sees both texts first);
  - `merge_flow_merges_nothing_without_authentication` (cancelled or failed: no review is posted and `lil dev merge` isn't run);
  - `dev_merge_asks_for_authentication_on_every_merge` (protected files or not).
- [ ] **Step 2: Run them.** Run: `cargo nextest run -p lil-cli --retries 0 -E 'binary(reviewer_hello)'`. Expected: FAIL.
- [ ] **Step 3: Implement.** The decrypted PEM, the KEK and the signature stay in `Zeroizing` buffers and are never logged.
- [ ] **Step 4: Run them, then the gate** (`-p lil-cli -p lil-platform`; automated). Then your one-time setup and the merge flow's one owner check (round 12: at most one per feature, under 2 minutes): `lil dev reviewer-hello enroll` (the setup), then one `lil dev merge-flow` on a test PR that touches a protected file (the check), recorded in the PR body. Expected: PASS; the flow shows the summary and the audit, the Hello prompt comes to the front by itself, and the GitHub review posts and the merge runs only after it. The fallbacks (Hello cancelled → the passphrase prompt; no authentication → no review and no merge) are Step 1's tests, with an injected `Unlocker` and `Authenticator`, not hand checks.
- [ ] **Step 5: Commit.**

```bash
git commit -m "dev: Windows Hello (route proven in Step 0) unlocks the reviewer key and gates the one-click merge flow; the passphrase file stays the fallback"
```

---

### Task 29: Close-out: the desktop group, the layout and responsiveness checks, and the milestone checks (APP-001, APP-003, APP-135, APP-136, APP-139, ARCH-116, QA-030, QA-042, QA-064, QA-071; P4a-R31) — **protected: `test-params.toml`, `.github/workflows/ci-pr-ready.yml` (pushed by the owner)**

**Depends on:** Tasks 4–28 and 30 merged.

**Files:**
- Modify (protected): `test-params.toml` (`[budgets]` `gui_stall_p99_ms = 16`, `event_to_ui_ms = 100`, `cold_start_ms = 2000`, `deploy_folder_mib = <Task 0's trimmed size + 10 %>`); `.github/workflows/ci-pr-ready.yml` (the `bench` and `deploy` presets read the budgets; no desktop group in CI, P4a-R31; the owner pushes it, P4a-R32).
- Create: `apps/lil-ui-kit/bench/{bench_gui_stalls.qml, bench_event_to_ui.qml}`, CTest `bench_cold_start` (label `bench`) `apps/lil-ui-kit/tests/tst_layout_full.qml`.
- Modify: `apps/lil-ui-kit/CMakeLists.txt` (CMake reads `test-params.toml`'s `[budgets]` lines with `file(STRINGS)` and passes them to the benchmarks as `LIL_BUDGET_<NAME>` environment variables); `README.md` (a short "Build and run the app" section: the developer shell, `cmake --preset dev`, the deploy target, and the Settings panel or `lil doctor` for the notification setup).

**Interfaces:**
- Consumes: every earlier task.

- [ ] **Step 1: Write the checks:** `tst_layout_full::test_default_layout_has_every_panel_of_this_plan` and `test_float_and_redock_every_panel_keeps_its_state` (APP-001, ARCH-116: the seven real panels, now that they exist); `bench_gui_stalls` (the shell with Jobs at 8 × 10 Hz previews, Hardware live and Home, under the fake feed; p99 heartbeat lateness and frame interval < `LIL_BUDGET_GUI_STALL_P99_MS`); `bench_event_to_ui` (fake events stamped at receipt; job progress p99 < `LIL_BUDGET_EVENT_TO_UI_MS`; a confirmation shows in the frame after it arrives, APP-139); `bench_cold_start` (the deploy folder, 10 warm starts with `--no-activate`, `interactive` < `LIL_BUDGET_COLD_START_MS`).
- [ ] **Step 2: Run them** (automated). Run: `ctest --preset qml -R tst_layout_full` and `ctest --preset bench` (about 6 minutes; ask first). Expected: PASS.
- [ ] **Step 3: the desktop group** (automated, no owner action; ask first, since together the runs take about 13 minutes). In the Sandbox runner (about 8 minutes), `cargo run -q -p lil-cli -- dev desktop-tests` (Task 30, P4a-R31): `desktop_shortcut_and_handlers_round_trip_under_a_test_scheme`, `desktop_power_request_sets_and_clears` and `desktop_set_up_writes_the_shortcut_and_handlers`; expected: each passes, the results folder holds the JUnit files and the Sandbox has closed. On your PC (the host run, P4a-R39; about 5 minutes with the hands-off pop-up), `cargo run -q -p lil-cli -- dev desktop-tests --host`: `desktop_toast_posts_under_the_shortcut_aumid`, `desktop_toasts_state_is_ready_on_this_pc`, `desktop_second_launch_comes_to_front` and `desktop_toast_url_brings_the_confirm_window_to_front`; expected: each passes, and the cleanup check finds nothing left.
- [ ] **Step 4: under load and from cold** (automated, no owner action, under the owner's round 12 answer; ask first, about 10 minutes): `bench_gui_stalls` again while `spikes/app-shell/comfy-load.ps1` saturates the GPU (APP-135), and the first start of a fresh copy of the deploy folder after a Defender scan of it (APP-136), both run unattended as spike 10's S0.6 and S0.5 (c) were. The first start after a reboot isn't re-measured: Task 0's S0.5 (b) covers it, `interactive` at 452 ms (`docs/spikes/10-app-shell.md`). Results go in the PR body.
- [ ] **Step 5: The final checks** (automated). Run: `cargo run -q -p lil-cli -- dev spec-tests` (no plan-4a entry left in `expected-fail.toml`), `cargo run -q -p lil-cli -- dev spec-report` (1080 rules, 0 dangling), `cargo run -q -p lil-cli -- dev privacy-scan --tree HEAD`. Expected: all exit 0. Then the owner's check 1 below (notifications, under 2 minutes, the only owner step of this task), recorded in the PR body.
- [ ] **Step 6: Commit.**

```bash
git commit -m "close-out: full layout checks, desktop group in the Sandbox runner and the host run, GUI stall, event-to-UI and cold-start benchmarks, build notes"
```

---

### Task 30: The Windows Sandbox runner: `lil dev desktop-tests` (QA-042, QA-073; P4a-R29, P4a-R31, P4a-R35, P4a-R39; owner decisions 2026-10-22 and 2026-10-23) — Qt-free, may run beside Tasks 6–14 — **protected: `crates/lil-cli/src/dev/desktop_tests.rs`, `crates/lil-cli/src/dev/mod.rs`**

**Depends on:** Tasks 1 and 5 merged (Task 1's S1.9: the Qt on-screen grab, protocol, power, Rust and lifecycle items passed 3 runs of 3 at 132 s a run with the Store Sandbox and its `wsb` CLI; the toast and foreground items failed only because the logon command runs elevated and neither `schtasks /RL LIMITED` nor `runas /trustlevel:0x20000` gave a non-elevated token, so those tests use the host run, P4a-R39; `wsb exec … -r ExistingLogin` is untried). The desktop-group tests of later tasks (19, 21, 27) run through this runner at their PRs once it has merged; Task 5's run at this task's PR. The `--suite qml|grabs` mode is used only if Task 0's S0.9 failed (P4a-R35).

**Files:**
- Create (protected): `crates/lil-cli/src/dev/desktop_tests.rs` (with its unit tests); modify (protected) `crates/lil-cli/src/dev/mod.rs` (the `desktop-tests` command and its help row).
- Create: `tools/sandbox/lilith-desktop.wsb.in` (the template), `tools/sandbox/run-inside.ps1` (runs inside the Sandbox), `tools/sandbox/nextest-sandbox.toml`, the one home of the `desktop` profile (moved here from Task 29 so the run inside reads it; `--config-file` replaces `.config/nextest.toml` there): `[profile.desktop]` with `test-threads = 1` (the group is serialised, QA-042), `retries = 0`, `slow-timeout = { period = "60s", terminate-after = 5 }` and `[profile.desktop.junit] path = "junit.xml"`, and no setup scripts, because there is no cargo in the Sandbox.
- Modify: `crates/lil-testkit/src/bins.rs` (the `LIL_TESTBINS_DIR` override: when it is set, the test binaries are taken from that folder before the compile-time `target\testbins\debug` path, and nothing is ever built; when it is set and a binary is missing, or when `LIL_SANDBOX=1` and it isn't set, the test fails with "test binaries not found in <dir>; the Sandbox runner bundles target\testbins\debug" instead of calling `cargo build`); `crates/lil-testkit/tests/bins_override.rs` (new).
- Test: the unit tests in `desktop_tests.rs`, through a fake `SandboxLauncher`; `crates/lil-testkit/tests/bins_override.rs`.

**Interfaces:**
- Consumes: Task 1's S1.9 results; `cargo-nextest` 0.9.148's archive interface, checked with `cargo nextest archive --help` and `cargo nextest run --help`: `archive --archive-file`, whose `-E` takes binary-level filtersets only (no test predicates and no `--run-ignored`), and on the run side `--archive-file`, `--workspace-remap`, `--extract-to`, `--config-file`, `--run-ignored only` and `-E`.
- Produces:

```rust
// crates/lil-cli/src/dev/desktop_tests.rs
pub enum Availability { Cli /* wsb.exe answers `wsb list` */, Inbox /* WindowsSandbox.exe present */, Missing }
pub trait SandboxLauncher {
    fn availability(&self) -> Availability;
    fn running(&self) -> bool;                          // `wsb list`, else a process with one of the names S1.9 recorded; the run's own lingering `wsb connect` client never counts
    fn start(&self, wsb: &Path) -> io::Result<SandboxHandle>;   // `wsb start --config "<the .wsb's XML text>"`, else `WindowsSandbox.exe <file>`
    fn stop(&self, h: &SandboxHandle) -> io::Result<()>;       // `wsb stop --id`, else end the started processes; then close the run's own `wsb connect` client (`WindowsSandboxRemoteSession`), pinned when first seen, which stays open after the VM stops (S1.9)
}
pub struct RealLauncher; impl SandboxLauncher for RealLauncher { /* … */ }
pub enum Suite { Desktop, Qml, Grabs, Host }   // Host: the toast and foreground tests on your PC, no Sandbox (P4a-R39)
pub struct RunConfig { pub suite: Suite, pub timeout: Duration, pub start_timeout: Duration, pub keep_open: bool }
pub fn run(repo: &Path, cfg: &RunConfig, launcher: &dyn SandboxLauncher, clock: &dyn Clock) -> Result<Summary, RunError>;
```

```text
lil dev desktop-tests [--suite desktop|qml|grabs] [--host] [--timeout-mins <n> (default 20)] [--keep-open]
  --host (P4a-R39): no Sandbox. Refuses when elevated (exit 2); builds the desktop tests; shows the hands-off pop-up
     (topmost, click-through, never takes the focus, at least 10 s of warning and a countdown); runs
     `cargo nextest run -p lil-platform -p lil-qt --run-ignored only --profile desktop -E '<the host set>'` with
     LIL_DESKTOP_HOST=1 and LIL_DEPLOY_DIR set; then checks that no test shortcut, registry key, CLSID or toast is left,
     and the pop-up says "done". The host set is P4a-R39's four tests.
  (a hidden --timeout-secs exists for tests)
  1. availability() is Missing → exit 2, naming the owner's one-time step (turn on Windows Sandbox, then restart);
     running() → exit 2, "Only one Windows Sandbox can run at a time; close the open one".
  2. builds the bundle in build\sandbox\in\ (about 10 minutes the first time, 1–2 after):
     desktop: `cargo nextest archive -p lil-platform -p lil-qt --archive-file build\sandbox\in\desktop.tar.zst`
       (default features: the desktop tests drive the deployed app and need no `qt` feature), the host's
       cargo-nextest.exe (the same binary builds the archive and runs it inside; its version goes in done.json, and
       the flags were checked on 0.9.148), a copy of the source tree (tracked files only, `git ls-files`) in in\src for
       --workspace-remap, the test kit's binaries (`target\testbins\debug\*`, built on the host by the same
       `cargo build --bins -p lil-cli -p lil-core -p lil-mcp -p lil-testkit --target-dir target/testbins` the nextest
       setup script runs) in in\testbins, nextest-sandbox.toml, the three VC++ runtime DLLs, and run-inside.ps1;
       the deploy folder (Task 15's `cmake --build --preset dev --target deploy`) is mapped as well;
     qml / grabs: `ctest --preset <suite> --show-only=json-v1` written to in\ctest.json, without `lil_qt_e2e`
       (it runs `cargo test`, and it isn't a window test); the CMake build tree and
       the Qt pair's folder (LIL_QT_DIR) are mapped read-only at their host paths, so the commands run unchanged.
  3. empties build\sandbox\out\ (it must exist before the start) and writes build\sandbox\lilith-desktop.wsb from the
     template: MappedFolder build\sandbox\in at C:\lil\in (ReadOnly true); the deploy folder at C:\lil\deploy
     (ReadOnly true); for qml / grabs, the build tree and LIL_QT_DIR at their host paths (ReadOnly true);
     build\sandbox\out at C:\lil\out (ReadOnly false); <Networking>Disable</Networking>,
     <ClipboardRedirection>Disable</ClipboardRedirection>; a LogonCommand running run-inside.ps1 (as is: S1.9 found no
     de-elevated launch that works, so the Sandbox runs no host-set test). The .wsb holds absolute local paths, so it lives under
     build\ and is never committed. Host folders must grant SYSTEM read (shares run as SYSTEM), else exit 2 naming them.
  4. start(); waits for out\started.json within 120 s (else stop() and exit 2, "Windows Sandbox didn't start"), then
     for out\done.json until the timeout.
  5. reads the JUnit files and done.json, prints one line per test and a summary, copies them to build\sandbox\results\,
     and calls stop() unless --keep-open (run-inside.ps1 also shuts the Sandbox down when done).
  exit 0 all passed, 1 a test failed, 2 couldn't run.
run-inside.ps1 (desktop; Windows PowerShell 5.1, started by the LogonCommand `powershell.exe -ExecutionPolicy Bypass
  -File C:\lil\in\run-inside.ps1`): writes started.json; sets LIL_SANDBOX=1, LIL_DEPLOY_DIR=C:\lil\deploy and
  LIL_TESTBINS_DIR=C:\lil\in\testbins; puts the VC++ DLLs on PATH; copies C:\lil\in\src to C:\lil\work\src (the remapped
  workspace must be writable for nextest's store) and creates C:\lil\work\bin (--extract-to fails on a missing folder);
  runs `C:\lil\in\cargo-nextest.exe nextest run --archive-file C:\lil\in\desktop.tar.zst --workspace-remap C:\lil\work\src
   --extract-to C:\lil\work\bin --config-file C:\lil\in\nextest-sandbox.toml --profile desktop --run-ignored only
   -E 'test(/^desktop_/) - <the host set>'`; copies C:\lil\work\src\target\nextest\desktop\junit.xml to C:\lil\out; writes done.json
  {exit, tests, passed, failed, nextest_version}; `shutdown /s /t 0`.
Which core and app the desktop tests use: tests that need only the platform use no core; tests that need a core and the
  test kit's fakes take them from LIL_TESTBINS_DIR on a test channel; tests that drive the app start
  C:\lil\deploy\lil-core.exe on a test channel (a dev build accepts one) and C:\lil\deploy\lil-image.exe beside it, so
  the core trusts the app as `ui:image` because it runs from the core's own folder (ARCH-046).
run-inside.ps1 (qml / grabs): the same frame, running each ctest.json command with Qt's bin on PATH and without
  QT_QPA_PLATFORM, adding `-o C:\lil\out\<test>.xml,junitxml`, and with LIL_GRAB_DIR=C:\lil\out\grabs, where the grab tests write
  (the build tree is read-only).
Desktop tests read fixtures only through `std::env::var("CARGO_MANIFEST_DIR")` at run time (nextest sets it to the
  remapped path), never `env!("CARGO_MANIFEST_DIR")`.
During a run (about 8 minutes): keep the PC unlocked (S1.9 ran unlocked throughout, and Microsoft's guidance asks for an
  unlocked host for real input and screen capture).
```

- [ ] **Step 1: Write the failing tests** (unit tests in `desktop_tests.rs`, with a fake `SandboxLauncher` and `FakeClock`; no real Sandbox, no real time): `wsb_maps_the_bundle_read_only_and_the_results_writable` (the written `.wsb` parses as XML; `C:\lil\in` and `C:\lil\deploy` are `ReadOnly` true, `C:\lil\out` false; networking and clipboard are off; the logon command names `run-inside.ps1`); `qml_suite_maps_the_build_tree_and_qt_at_their_host_paths`; `the_inside_command_runs_ignored_desktop_tests_from_the_archive` (the `run-inside.ps1` the bundle carries holds `--archive-file`, `--workspace-remap C:\lil\work\src`, `--config-file`, `--profile desktop`, `--run-ignored only` and `-E 'test(/^desktop_/)'`, creates `C:\lil\work\bin` first, and sets `LIL_TESTBINS_DIR`); `the_desktop_profile_is_serialised` (the bundled `nextest-sandbox.toml` parses and its `[profile.desktop]` has `test-threads = 1`); `the_bundle_holds_the_testbins_and_the_vc_runtime`; `qml_suite_leaves_out_lil_qt_e2e_and_sets_the_grab_dir`; `a_host_folder_system_cannot_read_exits_2` (a fake ACL check refusing one folder → exit 2 naming it); `the_archive_command_has_no_test_predicate` (the host's archive command line has no `test(` and no `--run-ignored`); `the_wsb_is_written_under_build_and_never_tracked`; `the_out_folder_is_made_and_emptied_before_the_start`; `refuses_when_the_sandbox_is_missing` (exit 2 and the owner's step); `refuses_while_another_sandbox_runs`; `gives_up_when_the_sandbox_never_starts` (no `started.json` within 120 s of fake time → `stop()` called, exit 2); `collects_results_and_reports_a_failure` (the fake writes `started.json`, two JUnit files, one with a failure, and `done.json` → exit 1, both tests listed, files copied, `stop()` called); `times_out_and_stops_the_sandbox` (no `done.json` before the fake clock passes `--timeout-secs 5` → exit 2 and `stop()` called); `keep_open_never_calls_stop`; `uses_wsb_when_the_cli_is_there` (the real launcher's command lines, checked without running them: `Cli` passes the `.wsb`'s XML text to `wsb start --config`, `Inbox` passes the file path to `WindowsSandbox.exe`); `bins_override.rs`: `testbins_dir_override_is_used_before_the_compile_time_path`, `a_missing_binary_under_the_override_fails_without_building`, `in_the_sandbox_without_the_override_it_fails_with_the_message`; `host_mode_refuses_when_elevated`; `host_mode_shows_the_pop_up_before_running_only_the_host_set`; `host_mode_checks_its_cleanup`; `the_sandbox_filter_leaves_out_the_host_set`; `stop_closes_the_runs_own_viewer_client` (and `running()` ignores it).
- [ ] **Step 2: Run them.** Run: `cargo nextest run -p lil-cli -p lil-testkit --retries 0 -E 'test(/desktop_tests::/) | binary(bins_override)'`. Expected: FAIL.
- [ ] **Step 3: Implement.** The command maps nothing but the folders above; nothing in the Sandbox can write to the source tree or the build tree.
- [ ] **Step 4: Run them, then the gate** (`-p lil-cli`; the flake guard takes about a minute, since the tests use fake time). Then one real run, which opens the Sandbox's window on your desktop (ask first; about 8 minutes: about 1 to start the Sandbox, about 5 for the tests, the rest to collect and close, plus about 10 the first time for the bundle): `cargo run -q -p lil-cli -- dev desktop-tests`. Expected: Task 5's Sandbox-set `desktop_*` tests pass inside the Sandbox, and the Sandbox and its viewer have closed. Then one host run (ask first; about 5 minutes with the pop-up): `cargo run -q -p lil-cli -- dev desktop-tests --host`, which runs Task 5's toast tests and finds nothing left behind.
- [ ] **Step 5: Commit.**

```bash
git commit -m "dev: lil dev desktop-tests runs the desktop group (and, if needed, the QML suites) in Windows Sandbox"
```

---

## Owner manual checks

(2026-10-24, under the owner's round 12 answer) Every check that something automatic can do is automated. Where nothing automatic works, a feature keeps at most one owner check, under 2 minutes, recorded in the PR body or the milestone report (QA-064). These are the only ones:
1. **Notifications** (one check, under 2 minutes; Task 29 Step 5). With the app closed, run `lil call resource.delete` three times:
   1. On throwaway resource A: click the "Confirmation waiting" toast's body. The confirm window comes to the front, and you deny there with a click.
   2. On throwaway resource B: click the toast's "Deny". No window opens.
   3. On throwaway resource C, with a game or a fullscreen video in front: no banner pops, and the toast is in the notification centre afterwards.

   This is by hand for two reasons. No synthetic click on a toast works, and UI Automation sees no toast on the test PC (P4a-R39). Nothing on the test PC can see whether a toast popped, and spike 11 once saw a `SuppressPopup` toast pop, possibly the control toast posted just before it (P4a-R36).
2. **Windows Hello and the merge flow** (Task 28). These are two features, each with one check of under 2 minutes:
   - Windows Hello's route (QA-088): Step 0's one real create and sign.
   - The one-click merge flow (QA-036): Step 4's one `lil dev merge-flow` on a test PR.

   Your enrolment is setup, not a check.

Automated instead of the earlier manual checks:
- the "Set up" button and `lil doctor`'s `y` path: Task 27's `desktop_set_up_writes_the_shortcut_and_handlers` (Sandbox runner) and `test_set_up_button_writes_then_rereads`;
- a toast posted under the AUMID, a toast's URL bringing the confirm window to the front, and a second launch handing over to the running app: the host run (P4a-R39);
- the "Job finished" toast, a Deny from a toast, and a retry with its retry token: Task 10's and Task 3's tests on the fake desktop;
- the confirm window's initial focus on Deny, with Enter not approving: Task 21's `test_initial_focus_is_deny_and_enter_does_not_approve` (P4a-R10);
- the fullscreen auto-pause, its reason in Jobs, and your own pause staying: Task 9's `autopause_adds_and_removes_the_fullscreen_reason` and `fullscreen_reason_never_clears_a_manual_pause`, and Task 22's `test_paused_header_names_manual_and_fullscreen`;
- Narrator (APP-156): Task 18's Step 2b reads the S0.7 list through UI Automation, as spike 10 did (owner direction 2026-10-23); what Narrator speaks and the tab order stay untested;
- the deploy folder off PATH: Task 15's `deploy_runs_off_path`;
- cold start after a Defender scan: Task 29's Step 4. After a reboot it is covered by Task 0's S0.5 (b), 452 ms (`docs/spikes/10-app-shell.md`);
- keep-awake: the Sandbox runner checks the power request (Task 30), and you see keep-awake with plan 6a's Generate panel, since this plan has no product GPU tool. There are no progress toasts (P4a-R36).

## Owner decisions (2026-10-22)

The owner answered the plan's eight questions on 2026-10-22. Each is now a decision, marked "(owner decision 2026-10-22)" where it applies, and Task 2 records Q1, Q2 and Q4–Q7 in the spec.

1. **Q1. Who changes the notification, quiet-mode, auto-pause and keep-awake settings:** only you, from the Settings panel (Task 27), through `app/desktop.set`, which only a verified app may call; APP-039's list gains it.
2. **Q2. Claude Code's notices:** a seventh category, "Claude Code and agent activity", on by default. As Q2 proposed, it also carries an agent's `open_in` and `ui.show` (APP-065).
3. **Q3. Toasts while a LILITH window is in front:** not posted; the app shows the same news (P4a-R7).
4. **Q4. The diagnostics bundle:** also replaces the user-profile folder with `%USERPROFILE%` and the computer name with `<machine>`.
5. **Q5. Notification setup:** one "Set up" button, on a card on the app's first start while setup is missing and in the Settings panel; it writes only after your click; `lil doctor` keeps its `y` path (P4a-R26).
6. **Q6. Buttons on confirmation toasts:** "Open" and "Deny". Deny answers straight from the toast; approving opens the window and needs a real click there (P4a-R9).
7. **Q7. The window title:** "LILITH — <project>" until the Image studio exists, then "ATHANOR — <project>" (P4a-R25).
8. **Q8. Where the desktop group runs:** in a Windows Sandbox runner built in this plan (Task 30, P4a-R31); QA-042 stays as written.
   Superseded in part on 2026-10-23: the toast and foreground tests run on your PC (below, 1).

## Owner decisions (2026-10-23 and 2026-10-24)

The owner's answers in rounds 12, 13 and 14 that bind this plan, with the coordinator's rulings. Task 2 records them in the spec and the roadmap.

1. **Where the desktop tests run** (round 12): tests may run on your PC at any time. With Task 1's S1.9 result, the toast and foreground tests run there with the hands-off pop-up, and the Sandbox runner keeps the rest (P4a-R29, P4a-R31, P4a-R39; QA-042 gains the exception).
2. **Windows Hello** (round 12): kept; the app brings the prompt to the front of every application, with the passphrase as the fallback (P4a-R38, QA-088).
3. **Merges** (round 12): one click. You approve a card; the merge flow then runs, shows a summary and an independent audit, and merges only after your Windows Hello or passphrase authentication (P4a-R38, QA-036). You stay the one who authenticates: every `lil dev merge` asks for it, protected files or not. Until Task 28's flow is merged, you run `lil dev approve` and `lil dev merge`.
4. **Keep-awake** (round 12): checked through Windows' own reports only, so S1.7's wait past the sleep timeout isn't run (ARCH-127).
5. **The review's reservations** (round 13): all of them, as one recorded exception to the spec freeze. This plan's four land with Task 2 (P4a-R40); the others go in a docs PR after it.
6. Round 13's other answers change only the roadmap: second-drive backups in plan 6; a claim lasts until you press Hand back; one combined stop per feature; only approved work goes on while a stop waits; asset-only director stops are chosen in the Start dialog; any model that passes the suite's tool test may direct; no separate generate-and-review plan.
7. **Owner checks** (round 12): at most one short owner check per feature, under 2 minutes, and only where nothing automatic works. "Owner manual checks" above follows it (reworked 2026-10-24).
8. **Progress toasts** (round 14, 2026-10-24): none, in this plan or later. Progress shows in the Jobs panel, and toasts go out only for finished work or for what needs you (P4a-R36).
9. **Your hands on claimed work** (round 14, 2026-10-24) changes only the roadmap: TETHER and your own Claude Code act as your hands on items you claimed, and their edits are marked "AI, done for you"; work you secured (signed off as a version, or locked) is never changed by the director unless you ask, and a director change to it is a new take for you to accept (backlog N31 and row 4b's attribution note).

The rulings of 2026-10-24 that change this plan: every `lil dev merge` asks for your authentication (item 3; P4a-R38), Task 5 builds the CLSID form directly (P4a-R37), and the restart hold (P4a-R41), which changes ARCH-022, an invariant, and so needs your OK through Task 2's merge.
