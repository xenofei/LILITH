# Plan 2a — Core Platform I: Process, Pipe and Dispatcher Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build a per-user `ww-core` process that clients reach over the channel's named pipe:
- it starts on demand, runs one instance per channel, idles out and drains on request or at logoff;
- it verifies who is calling and answers the `core/hello` handshake;
- it runs every tool call through the dispatcher pipeline;
- it returns results in the §6.1 result and error model;
- it keeps durable state in SQLite and streams events.

`ww-client` and the user-facing `ww status | call | events | core stop` commands drive it.

**Architecture:**
- **Crates.** `ww-protocol` holds the wire types: JSON-RPC envelope, results, errors, the handshake, tool descriptors and events. `ww-platform` gains the Windows pieces: pipe instances with an SDDL, pipe peer PIDs, process inspection, detached spawn and the session-end window. `ww-core` is a library plus the `ww-core` binary. Its modules are `server` (transport), `rpc` (method routing and handshake), `identity`, `workspace`, `dispatch` (pipeline, registry and idempotency), `events`, `state` (one actor thread per SQLite file), `lifecycle` and `store::fsops`, the single file-write allowance. `ww-client` is the SDK that finds, starts, verifies and talks to the core, and `ww-cli` wraps it.
- **Later stages.** The pipeline stages that plan 3 fills (settings, estimate, confirm and caps, reserve, meter) are traits, with plan-2 implementations that do nothing or fail closed.
- **Plan 2b** (outlined at the end) adds the workspace data layer: `store`, `index`, `watch`, manifests, versions and trash.

**Tech Stack:**
- Rust 1.99.0 (MSVC), edition 2024, `cargo-nextest`, `cargo-deny`.
- New crates: `tokio` (rt-multi-thread, net, io-util, sync, time, macros), `tokio-util` (codec), `rusqlite` (`bundled`), `schemars` 1.x, `tracing`, `tracing-subscriber` (`json`), `tracing-appender`, `getrandom`.
- `windows` (in `ww-platform` only).

**Spec:** `docs/superpowers/specs/2026-10-12-worldwright-suite-design.md`. Roadmap row 2 (`docs/superpowers/plans/2026-10-12-roadmap.md`), split into 2a and 2b by Task 1.

The rules this plan implements, fully or in the part named:
- §1.1: ARCH-001, ARCH-003, ARCH-004, ARCH-005 (the core shows no UI; it has only the hidden session-end window), ARCH-016 (`status`, `call`, `events --follow`), ARCH-018, ARCH-019, ARCH-020 (clients and in-flight calls; plan 3 registers jobs, confirmations and outside ComfyUI jobs as idle blockers), ARCH-021 (the drain phase, phase order and the session-end window; plan 3 registers the engine phases), ARCH-023;
- §1.2: ARCH-024–027, ARCH-029, ARCH-030 (core side and the `cli` chain), ARCH-031, ARCH-032, ARCH-033 (line cap), ARCH-035, ARCH-036, ARCH-037 (descriptor, schemas and description length; lowering is plan 3), ARCH-038;
- §1.3: ARCH-045–048 (refusal of invalid engine tokens; issuing them is plan 7+), ARCH-050, ARCH-051, ARCH-052 and ARCH-055 (fail-closed error branch; the pending branch is plan 3), ARCH-060 (the untrusted-detail flag);
- §1.5, §1.6: ARCH-090 (reading `suite.toml`), ARCH-094, ARCH-095, ARCH-097 (where `state.db` lives);
- §2.0–2.2: CORE-001, CORE-002 (lint home), CORE-004 and CORE-005 (both `state.db` files), CORE-007 (read), CORE-010, CORE-012, CORE-014 (the `tools`, `state` and `store` modules), CORE-015–017, CORE-022;
- §2.4: CORE-058 (each tool declares its module);
- §6.1: QA-001, QA-002 (result shape), QA-003–005, QA-010 (JSON-RPC errors only for protocol faults), QA-012, QA-016 (error branch);
- process and test rules: QA-017, QA-018, QA-021, QA-024, QA-027–029, QA-031, QA-038, QA-040 (registered-workspace check), QA-041, QA-056 (partial), QA-062 (an older core refuses a newer `state.db`), QA-066 (partial), OPS-108.

Excluded by milestone tag, from §1.1–1.6, §2.0–2.4 and §6.1:
- [M1.5]: ARCH-007, ARCH-066, ARCH-087, ARCH-093, CORE-046;
- [Later]: ARCH-011, ARCH-041, ARCH-067, ARCH-091, ARCH-092, CORE-036, CORE-056.

No [M1b] rule falls in these sections.

In-scope rules owned by other plans:
- **Plan 2b:** ARCH-002 (store), ARCH-016 (`notify`, `doctor`), ARCH-028, ARCH-042, ARCH-044, ARCH-058, ARCH-073–083, ARCH-085, ARCH-088, ARCH-089, ARCH-096; CORE-003, CORE-006, CORE-009, CORE-019, CORE-020, CORE-023, CORE-025–035, CORE-037–042, CORE-047–055, CORE-057; QA-011.
- **Plan 3:** ARCH-012–015, ARCH-022, ARCH-034, ARCH-039, ARCH-040, ARCH-053, ARCH-054, ARCH-056, ARCH-057, ARCH-059, ARCH-061, ARCH-064, ARCH-065, ARCH-068, ARCH-069, ARCH-071, ARCH-072; CORE-008, CORE-011, CORE-013; QA-007–009.
- **Plan 4:** ARCH-009, ARCH-010, ARCH-043; QA-013, QA-015.
- **Plans 5–6:** ARCH-070 [M1a]; CORE-018, CORE-021, CORE-024, CORE-043–045; QA-014.
- **Plan 7+:** ARCH-008, ARCH-017, ARCH-049, ARCH-062, ARCH-063, ARCH-084, ARCH-086; QA-006.

M0 results: `docs/spikes/README.md`. Spike 09 is why the core binary is a Windows-subsystem executable: a console process flashes a window even when it's started hidden.

## Global Constraints

- **Toolchain:** Rust **1.99.0**, `x86_64-pc-windows-msvc`, edition **2024**, as pinned by plan 1.
- **Branch and merge:**
  - Work on `plan-2a-core-platform`, cut from `main` after this plan's docs PR merges. Commit after every task; each message ends with `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`.
  - The branch lands as one PR through `ww dev merge`.
  - **Protected files** (QA-019/020): Tasks 2, 5, 6 and 14 modify them, so the owner must run `ww dev approve <PR>` on the final head. This plan changes no workflow file. Agents can't push those, and none is needed: the E2E tests run inside the existing `cargo nextest` and `ww dev spec-tests` steps.
- **Crates** (ARCH-110): this plan adds exactly one crate, `ww-client`. `ww-core` gains a binary target named `ww-core` (`crates/ww-core/src/main.rs`).
- **Dependency boundaries** (ARCH-111, QA-027):
  - `ww-protocol` may depend only on `serde`, `serde_json`, `schemars` and `sha2`. Never `tokio`, `getrandom` or `uuid`: they pull in `windows-sys`, and the boundary test (`crates/ww-testkit/tests/dependency_boundaries.rs`) must stay green.
  - `ww-schema` adds only `serde` and `toml`.
  - Only `ww-platform` calls Windows APIs. That includes `CommandExt::creation_flags` and the raw security attributes on pipe instances. `ww-core`, `ww-client` and `ww-cli` use its wrappers.
- **New dependencies** (OPS-108: licence compatible with GPL-3.0-or-later; `cargo deny check` must pass). Pin each to its current crates.io release at implementation time (`cargo search <name> --limit 1`), with the floors below, and record the versions in the Task 2 report.

  | Crate | Floor | Licence | Reason |
  |---|---|---|---|
  | `tokio` | 1.53 | MIT | async runtime and named pipes (§1.1: the core is Rust on tokio) |
  | `tokio-util` | 0.7 | MIT | `LinesCodec` with a maximum line length (newline-delimited JSON-RPC, ARCH-025) |
  | `rusqlite` | 0.37 | MIT | `state.db`, bundled SQLite with FTS5 and JSON1 (CORE-004) |
  | `schemars` | 1.0 | MIT | tool input and output schemas pinned to JSON Schema 2020-12 (ARCH-037) |
  | `tracing`, `tracing-subscriber` (`json`), `tracing-appender` | 0.1 / 0.3 / 0.2 | MIT | the windowless core's JSON log file (ARCH-117, the part this plan needs) |
  | `getrandom` | 0.4 | MIT OR Apache-2.0 | session ids, trace ids and idempotency keys (`ww-platform` only) |
- **File writes in `ww-core`** (ARCH-112, QA-024):
  - Every file-mutating call lives in `crates/ww-core/src/store/fsops.rs`, with one `#[allow(clippy::disallowed_methods)]` per function.
  - `crates/ww-core/clippy.toml` also disallows `tokio::fs::{write, rename, remove_file, remove_dir_all, create_dir_all}`, `std::fs::hard_link` and `std::fs::set_permissions`.
  - SQLite creating its own database file is not a `ww-core` file call. Creating the folder for it is, so it goes through `fsops::ensure_dir`.
- **Tests** (QA-031):
  - **In-process tests** use `ww_testkit::FakeClock` and `SeqIds`. Transport timeouts are tested with `#[tokio::test(start_paused = true)]`.
  - **E2E tests** (real `ww`/`ww-core` processes over a real pipe, QA-056) use the wall clock only as a failure deadline, never in an assertion.
  - No test touches the network.
- **Isolation** (QA-038, QA-040, QA-041): as in plan 1, plus:
  - Child processes get `WW_CHANNEL=test-<id>`, `WW_TEST_ROOT=<TestEnv root>` and `WW_TEST_PARENT_PID=<test pid>`.
  - A core on a test channel arms the harness guard itself, refuses a workspace registered in the real `suite.toml`, and exits when the parent test process ends.
- **Names that are contracts:**
  - **Pipe:** `\\.\pipe\worldwright-core-<userSID>-<channel>` (ARCH-024). Test channels use the fixed SID `S-1-5-21-1000-2000-3000-1001` in the name; the pipe's DACL always uses the real user SID.
  - **Process environment:**

    | Variable | Meaning |
    |---|---|
    | `WW_BUILD_CHANNEL` (compile time) | `stable` only in release packaging; default `dev` |
    | `WW_CHANNEL` | a dev build accepts `dev` or `test-<id>`; a stable build refuses any value but `stable` |
    | `WW_TEST_ROOT` | test channels only: `<root>\{roaming,local,profile}` |
    | `WW_TEST_PARENT_PID` | test channels only |
    | `WW_TEST_IDLE_SECS` | test channels only |

  - **Methods** (ARCH-031, amended in Task 1): `core/hello`, `core/drain`, `tools/list`, `tools/call`, `ww/subscribe`, `ww/unsubscribe` and the notification `ww/event`. Every `app/*` method is gated to verified `ui:*` callers.
  - **JSON-RPC protocol-fault codes** (QA-010):

    | Code | Meaning |
    |---|---|
    | -32700 | parse error |
    | -32600 | invalid request; also batches, which aren't supported, and a second `core/hello` |
    | -32601 | method not found |
    | -32602 | invalid params; also an unknown `core/hello` parameter |
    | -32001 | handshake required |
    | -32002 | line too long; the connection is then closed |

    Maximum line length: **4 MiB**.
  - **Exit codes of `ww-core`:** 0 normal exit; 2 startup failure (the cause is in the log); 3 another core already owns the channel's pipe.
  - **Exit codes of the `ww` user commands:** 0 ok; 1 the call returned an `error` result; 2 couldn't run (usage, connection, untrusted core, start failure); 3 `ww status --no-start` found no core; 4 `pending_confirmation`. With `--no-start`, every command except `status` is a silent no-op that exits 0 when no core runs (ARCH-018).
  - **Workspace id:** the 16-hex workspace hash of the canonical path (plan 1 contract), or `"none"`.
  - **`suite.toml`** keys read in this plan: `default_workspace = '<path>'` and `[[workspaces]] path = '<path>'`. Unknown keys are preserved by the type.
  - **Per-workspace `state.db`:** `Env::workspace_local_dir(ws)\state.db`. **Suite `state.db`:** `Env::local_dir()\state.db` (CORE-005).
  - **Test-channel tools**, registered only when the channel is `test-*`, module `"test"`. Spec tests depend on them:

    | Tool | Effect | Arguments | Result (`structured`) |
    |---|---|---|---|
    | `test.echo` | read | `{text}` | `{text}` |
    | `test.write` | write | `{label, fail?: bool}` | `{label, executions}`; `fail: true` returns an error result with code `params.invalid` |
    | `test.block` | write | `{gate}` | blocks until an in-process test opens the gate (in-process tests only) |
    | `test.panic` | read | `{}` | panics |
    | `test.destructive` | destructive | `{}` | `{executed: true}` |
    | `test.emit` | read | `{topic, data}` | publishes `data` to `test:<topic>` |

- **Privacy** (QA-086): no user-profile paths (write `%USERPROFILE%`), no personal names or e-mail addresses in code, fixtures, test names or reports.

## Review Focus

1. **Two clients start the core at the same moment**, e.g. an app and Claude Code's MCP server at login, or eight hook invocations at once.
   - **Expected:** exactly one core runs, and every client connects to it. A spawned core that loses the race exits with code 3 and isn't reported as a failure.
   - **Covered by:** Task 13, `racing_starts_yield_one_core`.
2. **A client disappears mid-call** (Ctrl+C on `ww call`, a crashed app, a killed test).
   - **Expected:** the call either finishes, and a retry with the same idempotency key returns its stored result, or never started. Other clients aren't disturbed. The connection count drops, so idle exit still happens.
   - **Covered by:** Task 7, `client_vanishing_mid_request_frees_the_connection`; Task 10, `client_killed_mid_call_leaves_core_healthy`.
3. **An old or foreign core already owns the channel's pipe:** a core from an earlier build still idling after a rebuild, or a dev core from another checkout or worktree.
   - **Expected:** the client never treats it as trusted, names its executable path and PID in the error, and doesn't start a second core. `ww core stop` can still ask it to drain.
   - **Covered by:** Task 13, `foreign_core_is_refused_and_named`.
4. **A malformed, oversized or half-written request line**, from a script piping bad JSON or a client that dies mid-write.
   - **Expected:** that connection gets a JSON-RPC protocol error, or is closed when the line is too long. The core and every other session keep working.
   - **Covered by:** Task 7, `garbage_line_gets_parse_error_and_connection_survives`, `oversized_line_closes_only_that_connection` and `truncated_line_at_disconnect_is_ignored`.
5. **Data and profile paths with spaces and non-ASCII characters**, e.g. a user name with an accent, or redirected folders.
   - **Expected:** the core spawns, finds its folders, opens `state.db` and serves the pipe.
   - **Covered by:** Task 13, `non_ascii_and_spaced_roots_work`.

---

### Task 1: Spec changes first (QA-029)

**Files:**
- Modify: `docs/superpowers/specs/2026-10-12-worldwright-suite-design.md`. No new IDs and no renumbering; the ID check stays `925 925 []`.
- Modify: `docs/superpowers/plans/2026-10-12-roadmap.md`.

**Interfaces:**
- Produces: the spec text Tasks 2, 8 and 12 implement.

- [ ] **Step 1: Apply the two edits for genuine contradictions.** Mark each "(plan 2 clarification)".
  - **ARCH-031 and ARCH-023:** ARCH-029 makes `core/hello` the mandatory first call and refuses a major mismatch, yet ARCH-023 needs a client that sees a major mismatch to ask the core to drain.
    - Add to ARCH-031's list: "`core/drain`: the 'drain and exit when safe' request of ARCH-023. Every caller kind may call it after `core/hello`, and it is the only call allowed on a connection whose `core/hello` was refused with `protocol.incompatible`. Its name, its parameters (none) and its result never change across protocol majors."
    - Add to ARCH-023: "The request is `core/drain` (ARCH-031)."
  - **ARCH-029:** unknown hello parameters are rejected, so a newer-minor client that sent a new parameter would break the promised minor compatibility. Append: "The set of `core/hello` parameters changes only with a major version. A minor version adds features only as new `capabilities` strings."
- [ ] **Step 2: Update the roadmap.** Split row 2 into **2a** ("process lifecycle, named pipe and caller identity, `core/hello`, dispatcher pipeline, result/error model, `state`, events, `ww-client`, `ww status|call|events|core stop`") and **2b** ("`store` with write order, staging, sealed versions and trash; `index`; `watch`; manifests and refs; pipeline, projects and settings; workspace tools; `ww notify`, `ww doctor` basics"). Each row depends on the one before.
- [ ] **Step 3: Verify the IDs.**

Run: `python -I -c "import re;t=open('docs/superpowers/specs/2026-10-12-worldwright-suite-design.md',encoding='utf-8').read();ids=re.findall(r'(?m)^- \*\*([A-Z]+-\d{3})\*\* \[',t);refs=set(re.findall(r'\b(?:ARCH|CORE|APP|AI|IMG|QA|OPS)-\d{3}\b',t));print(len(ids),len(set(ids)),sorted(refs-set(ids)))"`
Expected: `925 925 []`

- [ ] **Step 4: Commit.**

```bash
git add docs/superpowers/specs/2026-10-12-worldwright-suite-design.md docs/superpowers/plans/2026-10-12-roadmap.md
git commit -m "spec: plan-2 clarifications (core/drain, hello parameters across minors); roadmap 2a/2b"
```

---

### Task 2: `ww-protocol` wire types and the result/error model (ARCH-025, ARCH-029, ARCH-031, ARCH-036, ARCH-038, ARCH-051, CORE-017, QA-001–005) — **protected: root `Cargo.toml`**

**Files:**
- Modify (protected): `Cargo.toml`. Add every new workspace dependency from Global Constraints, in one edit.
- Modify: `crates/ww-protocol/Cargo.toml` (deps: `serde`, `serde_json`, `schemars`, `sha2`).
- Create: `crates/ww-protocol/src/{jsonrpc.rs, outcome.rs, codes.rs, session.rs, tools.rs, events.rs, canonical.rs}`. Modify `lib.rs` to re-export them.

**Interfaces:**

Produces:

```rust
// jsonrpc.rs
pub const MAX_LINE_BYTES: usize = 4 * 1024 * 1024;
pub mod rpc_code { pub const PARSE_ERROR: i64 = -32700; pub const INVALID_REQUEST: i64 = -32600;
  pub const METHOD_NOT_FOUND: i64 = -32601; pub const INVALID_PARAMS: i64 = -32602;
  pub const HANDSHAKE_REQUIRED: i64 = -32001; pub const LINE_TOO_LONG: i64 = -32002; }
pub enum Id { Num(i64), Str(String) }
pub struct Request { pub id: Option<Id>, pub method: String, pub params: Option<serde_json::Value> } // id None = notification
pub struct RpcError { pub code: i64, pub message: String, pub data: Option<serde_json::Value> }
pub enum Response { Result { id: Id, result: serde_json::Value }, Error { id: Option<Id>, error: RpcError } }
pub fn parse_request(line: &str) -> Result<Request, (Option<Id>, RpcError)>;
pub fn parse_server_line(line: &str) -> Result<ServerLine, RpcError>;     // ServerLine::{Response(Response), Notification(Request)}
pub fn encode_request(id: &Id, method: &str, params: &serde_json::Value) -> String;
pub fn encode_notification(method: &str, params: &serde_json::Value) -> String;
pub fn encode_response(r: &Response) -> String;                            // no trailing newline
pub mod method { pub const HELLO: &str = "core/hello"; pub const DRAIN: &str = "core/drain";
  pub const TOOLS_LIST: &str = "tools/list"; pub const TOOLS_CALL: &str = "tools/call";
  pub const SUBSCRIBE: &str = "ww/subscribe"; pub const UNSUBSCRIBE: &str = "ww/unsubscribe";
  pub const EVENT: &str = "ww/event"; pub const APP_PREFIX: &str = "app/"; }
// outcome.rs   (#[serde(tag = "status", rename_all = "snake_case")])
pub enum Outcome<T> { Ok(T), PendingConfirmation(PendingConfirmation), Error { error: ErrorBody } }
pub struct ToolOutput { pub structured: serde_json::Value, pub artifacts: Vec<FileRef>, pub summary_text: String }
pub struct FileRef { pub path: String, pub sha256: Option<String> }
pub struct PendingConfirmation { pub confirmation_id: String, pub retry_token: String, pub expires_at: String, pub covers: Vec<Cover> }
pub enum Cover { Confirm, Cap }
pub struct ErrorBody { pub code: String, pub category: ErrorCategory, pub message: String,
  pub detail: Option<Detail>, pub fix: Option<Fix>, pub trace_id: String, pub retry: RetryPolicy }
pub struct Detail { pub text: String, pub untrusted: bool }               // ARCH-060: raw engine/provider text is untrusted
pub struct Fix { pub ui_action: UiAction, pub agent_hint: Option<AgentHint> }
pub struct UiAction { pub id: String, pub label: String }
pub struct AgentHint { pub tool: String, pub arguments: serde_json::Value }
pub enum ErrorCategory { UserFixable, Transient, ExternalDown, Declined, DataConflict, Cancelled, Bug }
pub enum RetryPolicy { Auto, SameIdempotencyKey, AfterResolve, Never }
// codes.rs
pub struct CodeSpec { pub code: &'static str, pub category: ErrorCategory, pub retry: RetryPolicy }
impl ErrorBody { pub fn new(spec: &CodeSpec, message: impl Into<String>, trace_id: impl Into<String>) -> ErrorBody; }
pub const ALL: &[&CodeSpec];
// session.rs
pub struct ProtocolVersion { pub major: u16, pub minor: u16 }          // FromStr/Display "M.m"
pub const PROTOCOL: ProtocolVersion = ProtocolVersion { major: 1, minor: 0 };
pub const CAPABILITIES: &[&str] = &["events.v1", "tools.v1"];
pub enum Negotiation { Compatible { minor: u16 }, Incompatible }
pub fn negotiate(client: ProtocolVersion, core: ProtocolVersion) -> Negotiation;
pub fn shared_capabilities(client: &[String], core: &[&str]) -> Vec<String>;   // core order
pub fn schema_hash() -> String;                                         // 16 hex
pub enum CallerKind { Ui(String), Assistant, Mcp, Cli }                // "ui:<app>" | "assistant" | "mcp" | "cli"; app = [a-z][a-z0-9-]*
impl CallerKind { pub fn is_agent(&self) -> bool; }                     // assistant | mcp | cli
pub const NO_WORKSPACE: &str = "none";
#[serde(deny_unknown_fields)]
pub struct HelloParams { pub protocol: String, pub build_id: String, pub schema_hash: String, pub capabilities: Vec<String>,
  pub caller_kind: String, pub non_interactive: bool, pub workspace: String,
  pub engine_token: Option<String>, pub profile_id: Option<String> }   // Debug redacts engine_token
pub struct HelloResult { pub session_id: String, pub instance_id: String, pub protocol: String, pub build_id: String,
  pub schema_hash: String, pub capabilities: Vec<String>, pub caller_kind: String,
  pub workspace: Option<WorkspaceInfo>, pub core: CoreInfo }
pub struct WorkspaceInfo { pub id: String, pub path: String }
pub struct CoreInfo { pub pid: u32, pub channel: String, pub pipe: String }
#[serde(deny_unknown_fields)] pub struct DrainParams {}
pub struct DrainResult { pub draining: bool }
// tools.rs
pub enum EffectClass { Read, Write, Destructive, Exec }                 // lowercase on the wire
impl EffectClass { pub fn needs_agent_confirmation(self) -> bool; }    // destructive | exec
pub const MAX_DESCRIPTION_CHARS: usize = 2000;
pub struct ToolDescriptor { pub name: String, pub description: String, pub effect: EffectClass, pub module: String,
  pub input_schema: serde_json::Value, pub output_schema: serde_json::Value,
  pub annotations: std::collections::BTreeMap<String, serde_json::Value> }
#[serde(deny_unknown_fields)]
pub struct ToolCallParams { pub name: String, pub arguments: serde_json::Map<String, serde_json::Value>,
  pub idempotency_key: Option<String>, pub trace_id: Option<String> }
pub struct ToolsListResult { pub tools: Vec<ToolDescriptor> }
// events.rs
pub const HISTORY_SECS: u64 = 600; pub const CLIENT_QUEUE: usize = 1024;
#[serde(deny_unknown_fields)]
pub struct SubscribeParams { pub topics: Vec<String>, #[serde(default)] pub since_seq: BTreeMap<String, u64> }
pub struct SubscribeResult { pub topics: Vec<TopicState> }
pub struct TopicState { pub topic: String, pub seq: u64 }
#[serde(deny_unknown_fields)] pub struct UnsubscribeParams { pub topics: Vec<String> }
pub struct EventParams { pub topic: String, pub seq: u64, pub kind: EventKind, pub data: serde_json::Value }
pub enum EventKind { Event, Snapshot, ResyncRequired }
pub fn is_valid_topic(t: &str) -> bool;                                 // ^[a-z][a-z0-9_]*(:.+)?$
// canonical.rs
pub fn canonical_json(v: &serde_json::Value) -> String;                 // keys sorted recursively, no whitespace
pub fn args_hash(v: &serde_json::Value) -> String;                      // 64 lowercase hex SHA-256 of canonical_json
```

**Codes** (`codes.rs`, one `CodeSpec` constant each):
- **From QA-005:** `confirm.denied`, `confirm.expired`, `confirm.unavailable`, `cap.exceeded`, `doc.held_elsewhere`, `store.hash_conflict`, `workspace.required`, `protocol.incompatible`, `engine.claude_code.untested_version`, `engine.claude_code.not_signed_in`, `comfyui.unreachable`, `model.missing`, `provider.rate_limited`, `provider.spend_limit`, `quota.plan_limit`.
- **Added by plan 2:** `workspace.unknown`, `workspace.unsupported`, `tool.unknown`, `params.invalid`, `idempotency.key_required`, `idempotency.conflict`, `idempotency.in_progress`, `caller.not_allowed`, `caller.engine_token_invalid`, `core.draining`, `core.bug`, `state.unavailable`.
- **Pinned by the spec:**
  - `confirm.unavailable` and `cap.exceeded`: `user_fixable` + `after_resolve` (QA-016);
  - `engine.claude_code.not_signed_in`: `after_resolve` (QA-006);
  - `provider.spend_limit`: `never` (QA-005).
- **Pinned here:**
  - `idempotency.conflict`: `data_conflict` + `never`;
  - `idempotency.in_progress`: `transient` + `same_idempotency_key`;
  - `core.draining`: `transient` + `same_idempotency_key`;
  - `core.bug`: `bug` + `never`;
  - `caller.not_allowed` and `caller.engine_token_invalid`: `declined` + `never`;
  - `params.invalid`, `tool.unknown`, `idempotency.key_required`, `workspace.unknown` and `workspace.unsupported`: `user_fixable` + `after_resolve`.
- The remaining QA-005 codes get a provisional category in a comment, for plan 3 to confirm.

**`schema_hash`** is the first 16 hex characters of SHA-256 over `canonical_json` of the `schemars` schemas of these types, in this order: `HelloParams`, `HelloResult`, `DrainParams`, `ToolCallParams`, `Outcome<ToolOutput>`, `ErrorBody`, `SubscribeParams`, `EventParams`.

- [ ] **Step 1: Write the failing unit tests** in `ww-protocol`:
  - `outcome_wire_shapes` (QA-001, QA-002, ARCH-038). `Ok(ToolOutput)` serialises to `{"status":"ok","structured":…,"artifacts":[],"summary_text":…}`. Pending serialises to `{"status":"pending_confirmation","confirmation_id":…,"retry_token":…,"expires_at":…,"covers":["confirm","cap"]}`. An error serialises to `{"status":"error","error":{"code":…,"category":…,"message":…,"trace_id":…,"retry":…}}`. All three round-trip. `{"status":"maybe"}` fails to parse (QA-001: exactly one of three).
  - `categories_and_retry_policies_are_closed_sets`: exactly the 7 and 4 wire strings of QA-003 and QA-004.
  - `code_specs_follow_the_spec`: the pinned values above. Every code matches `^[a-z_]+(\.[a-z_]+)+$`, and the codes are unique.
  - `hello_params_reject_unknown_fields` and `engine_token_is_redacted_in_debug`.
  - `version_negotiation`: 1.0 vs 1.3 → `Compatible { minor: 0 }`; 2.0 vs 1.0 → `Incompatible`; `"1"` and `"1.x"` don't parse.
  - `shared_capabilities_are_the_intersection_in_core_order`.
  - `caller_kind_wire_strings`: `"ui:image"` ↔ `Ui("image")`; `"ui:"`, `"ui:Image"` and `"robot"` are invalid; `is_agent` is true for exactly `assistant`, `mcp` and `cli`.
  - `effect_class_wire_strings` and `needs_agent_confirmation_is_destructive_or_exec`.
  - `parse_request_classifies_lines`: bad JSON → -32700 with id `None`; a JSON array → -32600; a missing or wrong `"jsonrpc":"2.0"` → -32600 with the id echoed; a request; a notification.
  - `canonical_json_sorts_keys_recursively`: `{"b":1,"a":{"d":1,"c":2}}` → `{"a":{"c":2,"d":1},"b":1}`. `args_hash` is 64 hex characters and is equal for permuted keys.
  - `schema_hash_is_16_hex_and_golden`: a literal recorded in the test. Its doc comment says: when this changes, bump `PROTOCOL` (major for a breaking change, minor otherwise) and update the literal in the same commit.
  - `topic_syntax`.
- [ ] **Step 2: Run them.**

Run: `cargo nextest run -p ww-protocol`
Expected: FAIL. Nothing is implemented.

- [ ] **Step 3: Implement the types**, add the workspace dependencies to the root `Cargo.toml`, and add `ww-protocol`'s own dependencies.
- [ ] **Step 4: Run the tests and the boundary and licence checks.**

Run: `cargo nextest run -p ww-protocol && cargo nextest run -p ww-testkit --test dependency_boundaries && cargo deny check && cargo clippy -p ww-protocol --all-targets -- -D warnings`
Expected: PASS. The report lists the pinned versions.

- [ ] **Step 5: Commit.**

```bash
git add Cargo.toml Cargo.lock crates/ww-protocol
git commit -m "protocol: JSON-RPC envelope, result/error model, handshake and event types (QA-001-005)"
```

---

### Task 3: `ww-platform`: process environment, clock, ids and the registered-workspace guard (ARCH-094, ARCH-095, CORE-012, QA-031, QA-038, QA-040)

**Files:**
- Modify: `crates/ww-platform/Cargo.toml` (add `getrandom`, `serde`, `toml`).
- Create: `crates/ww-platform/src/{process_env.rs, clock.rs, ids.rs}`.
- Modify: `crates/ww-platform/src/{env.rs, guard.rs, lib.rs}`, `crates/ww-platform/tests/guard.rs`, `crates/ww-testkit/src/env.rs` (use `TEST_PIPE_SID` from `ww-platform`).
- Test: unit tests in the new files. Create `crates/ww-platform/tests/process_env.rs`; it arms the guard, so it is its own binary.

**Interfaces:**

Produces:

```rust
// process_env.rs
pub const ENV_CHANNEL: &str = "WW_CHANNEL"; pub const ENV_TEST_ROOT: &str = "WW_TEST_ROOT";
pub const ENV_TEST_PARENT_PID: &str = "WW_TEST_PARENT_PID"; pub const ENV_TEST_IDLE_SECS: &str = "WW_TEST_IDLE_SECS";
pub const BUILD_CHANNEL: &str;                      // option_env!("WW_BUILD_CHANNEL") or "dev"
pub const TEST_PIPE_SID: &str = "S-1-5-21-1000-2000-3000-1001";
pub struct TestOverrides { pub parent_pid: Option<u32>, pub idle_secs: Option<u64> }
// env.rs
impl Channel { /* FromStr: "stable" | "dev" | "test-<id>" */ }
impl Env {
    pub fn from_process() -> Result<Env, EnvError>;                       // from_vars(BUILD_CHANNEL, std::env::var)
    pub fn from_vars(build_channel: &str, var: &dyn Fn(&str) -> Option<String>) -> Result<Env, EnvError>;
    pub fn test_overrides(&self, var: &dyn Fn(&str) -> Option<String>) -> TestOverrides; // all None unless test channel
    pub fn install_dir(&self) -> Option<PathBuf>;                         // stable: <local root>\WorldwrightSuite\current
    pub fn suite_toml(&self) -> PathBuf;                                  // data_dir\suite.toml
    pub fn suite_state_db(&self) -> PathBuf;                              // local_dir\state.db
}
// new EnvError variants: ChannelNotAllowed { build: String, requested: String }, TestRootMissing, TestRootInvalid(PathBuf)
// clock.rs
pub trait Clock: Send + Sync + std::fmt::Debug { fn monotonic(&self) -> Duration; fn utc_now(&self) -> SystemTime; }
pub struct SystemClock;                                                   // monotonic = Instant since construction
// ids.rs
pub trait IdSource: Send + Sync + std::fmt::Debug { fn id(&self, prefix: &str) -> String; }  // "<prefix>_<16 lowercase hex>"
pub struct RandomIds;                                                     // getrandom
// guard.rs
pub struct RealLocations { /* existing fields */ pub workspaces: Vec<PathBuf> }
pub fn parse_registered_workspaces(suite_toml: &str) -> Vec<PathBuf>;     // [[workspaces]] path and default_workspace; malformed → empty
pub fn check_workspace(path: &Path) -> PathBuf;                           // panics when armed and path is, or is under, a real registered workspace
```

**`from_vars` rules:**
- **Build channel `stable`:** `WW_CHANNEL` unset or `stable` → `from_system(Stable)`; anything else → `ChannelNotAllowed`.
- **Build channel `dev`:** `WW_CHANNEL` unset or `dev` → `from_system(Dev)`. `test-<id>` → `WW_TEST_ROOT` must be an absolute, existing directory. Roots are `<root>\{roaming,local,profile}`, the SID is `TEST_PIPE_SID`, and the guard is armed with `RealLocations::from_system()` **before** the `Env` is built (QA-040).

`RealLocations::from_system()` reads the real `%APPDATA%\Worldwright\suite.toml` **read-only**, if it exists, to fill `workspaces`. This is the only read of a real Worldwright file, and QA-040 requires it.

- [ ] **Step 1: Write the failing tests:**
  - **Unit tests:**
    - `dev_build_defaults_to_dev`;
    - `stable_build_refuses_other_channels`;
    - `dev_build_refuses_unknown_channel`;
    - `test_channel_requires_existing_test_root`;
    - `test_overrides_only_on_test_channels`;
    - `install_dir_only_for_stable_and_data_never_inside_it` (ARCH-094: `data_dir` and `local_dir` don't start with `install_dir`);
    - `parse_registered_workspaces_reads_both_keys_and_ignores_garbage`;
    - `random_ids_have_prefix_and_16_hex`;
    - `system_clock_monotonic_never_decreases`.
  - **`tests/process_env.rs`:** `test_channel_env_uses_root_and_fixed_sid` (`pipe_name()` ends with `S-1-5-21-1000-2000-3000-1001-test-<id>`; `data_dir` is under `<root>\roaming`), and `test_channel_env_arms_the_guard` (afterwards an `Env` whose roaming root is the real one panics on `data_dir()`).
  - **`tests/guard.rs`:** `registered_real_workspace_trips_guard`. With `workspaces = [C:\Users\Fake\Ws]`, `check_workspace(C:\Users\Fake\Ws\projects)` panics with "real Worldwright location", and `C:\Users\Fake\Other` passes.
- [ ] **Step 2: Run them.**

Run: `cargo nextest run -p ww-platform`
Expected: FAIL. Nothing is implemented.

- [ ] **Step 3: Implement it.** Move `TEST_SID` out of `ww-testkit` into `TEST_PIPE_SID`.
- [ ] **Step 4: Run them again.**

Run: `cargo nextest run -p ww-platform -p ww-testkit && cargo clippy -p ww-platform --all-targets -- -D warnings`
Expected: PASS.

- [ ] **Step 5: Commit.**

```bash
git add crates/ww-platform crates/ww-testkit Cargo.lock
git commit -m "platform: process environment by channel, clock and ids, registered-workspace guard (QA-038/040)"
```

---

### Task 4: `ww-platform`: pipe instances, pipe peers, process inspection and detached spawn (ARCH-019, ARCH-024, ARCH-026, ARCH-027, ARCH-045)

**Files:**
- Modify: `crates/ww-platform/Cargo.toml`. Add `tokio` (`net`, `rt`, `time`) and the `windows` features `Win32_System_Pipes`, `Win32_System_Diagnostics_ToolHelp` and `Win32_System_SystemServices`.
- Create: `crates/ww-platform/src/{pipe.rs, process.rs}`.
- Test: `crates/ww-platform/tests/pipes.rs`. It uses unique `test-<random>` pipe names.

**Interfaces:**

Produces:

```rust
// pipe.rs
pub fn pipe_sddl(user_sid: &str) -> String;                       // "D:P(A;;GA;;;<sid>)(A;;GA;;;SY)"
pub enum CreateError { AlreadyOwned, Io(io::Error) }
pub fn create_instance(name: &str, first: bool) -> Result<NamedPipeServer, CreateError>;
pub enum ConnectError { NotFound, Busy, Io(io::Error) }
pub fn open_client(name: &str) -> Result<NamedPipeClient, ConnectError>;  // one attempt, no waiting
pub fn client_pid(server: &NamedPipeServer) -> io::Result<u32>;            // GetNamedPipeClientProcessId
pub fn server_pid(client: &NamedPipeClient) -> io::Result<u32>;            // GetNamedPipeServerProcessId
pub fn instance_dacl_sids(server: &NamedPipeServer) -> io::Result<Vec<String>>;
// process.rs
pub struct ProcessInfo { pub pid: u32, pub image: PathBuf, pub user_sid: String, pub parent_pid: Option<u32> }
pub trait ProcessInspector: Send + Sync + std::fmt::Debug { fn inspect(&self, pid: u32) -> io::Result<ProcessInfo>; }
pub struct SystemInspector;
pub struct ProcessHandle { /* owned HANDLE */ }
impl ProcessHandle { pub fn pid(&self) -> u32; pub fn exit_code(&self) -> io::Result<Option<u32>>;
  pub async fn wait(&self) -> io::Result<u32>; pub fn kill(&self) -> io::Result<()>; }
pub fn open(pid: u32) -> io::Result<ProcessHandle>;
pub fn current_user_sid() -> io::Result<String>;                  // re-export of sid.rs with io::Error
pub fn current_exe_dir() -> io::Result<PathBuf>;
pub struct SpawnRequest { pub exe: PathBuf, pub args: Vec<OsString>, pub env: Vec<(OsString, OsString)> }
pub trait RawSpawner { fn spawn(&self, req: &SpawnRequest, flags: u32) -> io::Result<ProcessHandle>; }
pub const DETACHED_FLAGS: u32;                                   // DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP
pub const BREAKAWAY: u32;                                        // CREATE_BREAKAWAY_FROM_JOB
pub fn spawn_detached(req: &SpawnRequest) -> io::Result<ProcessHandle>;
pub fn spawn_detached_with(spawner: &dyn RawSpawner, req: &SpawnRequest) -> io::Result<ProcessHandle>;
```

**Behaviour:**
- **`create_instance`:** builds the SDDL for `current_user_sid()` with `ConvertStringSecurityDescriptorToSecurityDescriptorW`, then passes it through tokio's `ServerOptions::{first_pipe_instance(first), reject_remote_clients(true)}.create_with_security_attributes_raw`. `ERROR_ACCESS_DENIED` on a `first` instance → `AlreadyOwned`.
- **`SystemInspector::inspect`:** opens the process with `PROCESS_QUERY_LIMITED_INFORMATION` immediately (ARCH-027). It reads the image with `QueryFullProcessImageNameW`, the token user with `GetTokenInformation(TokenUser)`, and the parent PID from a Toolhelp snapshot.
- **`spawn_detached_with`:** tries `DETACHED_FLAGS | BREAKAWAY` first. On `ERROR_ACCESS_DENIED` it retries once with `DETACHED_FLAGS` (ARCH-019). Stdio is null and the child inherits no handles.

- [ ] **Step 1: Write the failing tests:**
  - `pipe_sddl_format`;
  - `instance_dacl_is_user_and_system_only`: the DACL SIDs are exactly `{current_user_sid(), "S-1-5-18"}` (ARCH-026);
  - `second_first_instance_is_already_owned` (ARCH-026);
  - `client_and_server_pids_are_this_process`;
  - `open_client_on_missing_pipe_is_not_found`;
  - `inspector_reports_own_image_and_user`: the image equals `std::env::current_exe()`, case-insensitively, and the SID equals `current_user_sid()`;
  - `spawn_retries_without_breakaway_on_access_denied`: a fake `RawSpawner` fails the first call with OS error 5; the recorded flags are `[DETACHED|BREAKAWAY, DETACHED]`;
  - `spawn_detached_runs_child_and_reports_exit_code`: `%ComSpec% /c exit 7` → `wait()` returns 7.
- [ ] **Step 2: Run them.**

Run: `cargo nextest run -p ww-platform --test pipes`
Expected: FAIL. Nothing is implemented.

- [ ] **Step 3: Implement it.** Every `unsafe` block gets a `// SAFETY:` comment.
- [ ] **Step 4: Run them again.**

Run: `cargo nextest run -p ww-platform && cargo clippy -p ww-platform --all-targets -- -D warnings`
Expected: PASS.

- [ ] **Step 5: Commit.**

```bash
git add crates/ww-platform Cargo.lock
git commit -m "platform: user-only pipe instances, pipe peer PIDs, process inspection, detached spawn (ARCH-019/026/027)"
```

---

### Task 5: `ww-testkit`: binaries, child environment, core fixture, raw pipe, fake clock and ids (QA-031, QA-056, QA-066) — **protected: `.config/nextest.toml`, `test-params.toml`**

**Files:**
- Modify: `crates/ww-testkit/Cargo.toml`. Add `ww-protocol`, `serde_json`, `tokio` (`rt`, `net`, `io-util`, `time`) and `rusqlite`.
- Create: `crates/ww-testkit/src/{bins.rs, core.rs, raw_pipe.rs, clock.rs, ids.rs}`. Modify `src/{lib.rs, env.rs}`.
- Modify (protected): `.config/nextest.toml`. Add an override, `filter = 'package(ww-spec-tests) | package(ww-core) | package(ww-client)'`, with `slow-timeout = { period = "60s", terminate-after = 5 }`. The first E2E test in a run builds the binaries.
- Modify (protected): `test-params.toml`. Record `e2e_s = 300` under `[timeouts]`, with a comment pointing to the override.
- Test: unit tests in the new files.

**Interfaces:**

Produces:

```rust
pub mod bins { pub struct Bins { pub dir: PathBuf, pub ww: PathBuf, pub ww_core: PathBuf }
               pub fn ensure_built() -> &'static Bins; }
impl TestEnv {
    pub fn with_label(label: &str) -> TestEnv;                     // label goes into the tempdir name (spaces, non-ASCII allowed)
    pub fn child_env(&self) -> Vec<(String, String)>;              // WW_CHANNEL, WW_TEST_ROOT, WW_TEST_PARENT_PID
    pub fn ww(&self, args: &[&str]) -> WwOutput;                   // runs bins.ww, cwd = self.root(), with child_env
    pub fn ww_with_stdin(&self, args: &[&str], stdin: &str) -> WwOutput;
    pub fn make_workspace(&self, name: &str) -> PathBuf;           // <root>\ws-<name>\worldwright.yaml ("name: <name>")
    pub fn register_workspace(&self, ws: &Path, default: bool);    // writes data_dir\suite.toml
    pub fn write_state_db_version(&self, path: &Path, user_version: i64);
}
pub struct WwOutput { pub code: i32, pub stdout: String, pub stderr: String }
pub struct CoreFixture { /* std::process::Child */ }
impl CoreFixture {
    pub fn start(env: &TestEnv) -> CoreFixture;                    // bins.ww_core; waits ≤ 10 s for the pipe
    pub fn start_exe(env: &TestEnv, exe: &Path) -> CoreFixture;    // e.g. a copy of ww-core.exe in another folder
    pub fn start_with_env(env: &TestEnv, extra: &[(&str, &str)]) -> CoreFixture;  // extra vars override child_env
    pub fn spawn_raw(env: &TestEnv) -> std::process::Child;        // no wait; for exit-code tests
    pub fn pid(&self) -> u32;
    pub fn wait_exit(&mut self, within: Duration) -> Option<i32>;
}                                                                  // Drop: hello + core/drain, wait 5 s, then kill
pub struct RawPipe { /* own current-thread runtime */ }
impl RawPipe {
    pub fn connect(env: &TestEnv) -> RawPipe;                      // retries ≤ 10 s; no server verification
    pub fn try_connect(env: &TestEnv, within: Duration) -> Option<RawPipe>;
    pub fn send_line(&mut self, line: &str);
    pub fn read_line(&mut self, within: Duration) -> Option<String>;
    pub fn call(&mut self, method: &str, params: serde_json::Value) -> serde_json::Value;  // whole response object
    pub fn hello(&mut self, overrides: serde_json::Value) -> serde_json::Value;           // default cli hello, merged
    pub fn tool(&mut self, name: &str, args: serde_json::Value, key: Option<&str>) -> serde_json::Value; // returns `result`
    pub fn next_event(&mut self, within: Duration) -> Option<serde_json::Value>;           // next ww/event params
}
pub struct FakeClock; impl FakeClock { pub fn new() -> Arc<FakeClock>; pub fn advance(&self, d: Duration);
                                       pub fn shift_utc(&self, by: i64 /* seconds, may be negative */); }  // impl Clock
pub struct SeqIds;   // impl IdSource: "<prefix>_" + 16-digit zero-padded counter
```

- **`ensure_built`:** runs `env!("CARGO") build --locked --offline --bins -p ww-cli -p ww-core --manifest-path <workspace>\Cargo.toml --target-dir <workspace>\target\testbins` once per process (`OnceLock`), with `CARGO_TARGET_DIR` removed from the child's environment. It returns `<workspace>\target\testbins\debug`.
  - A separate target directory means a running test core never locks a binary the main build wants to relink, and feature unification differences never rebuild one under a running test.
  - A missing `ww-core.exe` (before Task 7) isn't an error here; `CoreFixture::start` panics naming it.
- **The default hello in `RawPipe::hello`:** `{protocol: "1.0", build_id: "testkit", schema_hash: ww_protocol::schema_hash(), capabilities: CAPABILITIES, caller_kind: "cli", non_interactive: false, workspace: "none"}`. An override value of `null` removes the key.

- [ ] **Step 1: Write the failing unit tests:**
  - `child_env_names_test_channel_root_and_parent`;
  - `with_label_keeps_spaces_and_non_ascii` (label `"Zoë Ünï space"`; `root()` contains it);
  - `make_and_register_workspace_writes_suite_toml` (parsed back with `ww_platform::parse_registered_workspaces`);
  - `write_state_db_version_sets_user_version`;
  - `fake_clock_advances_only_when_told`;
  - `seq_ids_are_deterministic`;
  - `ensure_built_puts_ww_in_testbins` (`bins.ww` exists and is under `target\testbins\debug`);
  - `raw_pipe_hello_merge_removes_null_keys` (pure helper `merge_hello(overrides) -> Value`).
- [ ] **Step 2: Run them.**

Run: `cargo nextest run -p ww-testkit`
Expected: FAIL. Nothing is implemented.

- [ ] **Step 3: Implement it**, and make the two protected edits.
- [ ] **Step 4: Run them again.**

Run: `cargo nextest run -p ww-testkit && cargo clippy -p ww-testkit --all-targets -- -D warnings`
Expected: PASS.

- [ ] **Step 5: Commit.**

```bash
git add crates/ww-testkit .config/nextest.toml test-params.toml Cargo.lock
git commit -m "testkit: test binaries, child env, core fixture, raw pipe, fake clock and ids (QA-066)"
```

---

### Task 6: Protected spec tests for plan 2a, expected to fail (QA-017, QA-018, QA-021) — **protected: `crates/ww-spec-tests/Cargo.toml`**

A test author who doesn't read the implementation writes these from the spec, Global Constraints and the Interfaces blocks above only. They drive only stable surfaces (QA-018): `RawPipe` tool and RPC calls, the `ww` CLI through `TestEnv::ww`, `ww-protocol` types, and `state.db` files written with `TestEnv::write_state_db_version`. Each test cites its rules with `// spec:`.

**Files:**
- Modify (protected): `crates/ww-spec-tests/Cargo.toml`. Add `[dev-dependencies]` on `ww-testkit`, `ww-protocol` and `serde_json`.
- Create (free additions, R26): `crates/ww-spec-tests/tests/{core_session.rs, core_dispatch.rs, core_events.rs, core_state.rs, cli_basics.rs}`.
- Modify: `crates/ww-spec-tests/expected-fail.toml`. Add one entry per new test (allowed: the same change adds the files).

**Interfaces:**
- Consumes: Task 5's testkit API and Task 2's types.
- Produces: the scenario tests that Tasks 7–14 turn green. **Every later task removes the entries its change makes pass, so `ww dev spec-tests` exits 0 at every task's end.**

- [ ] **Step 1: Write the tests.**

  **`core_session.rs`:**

  | Test | Rules | Assertion |
  |---|---|---|
  | `single_core_per_channel` | ARCH-024, ARCH-026 | a second `CoreFixture::spawn_raw` on the same `TestEnv` exits with code 3 within 10 s; the first still answers hello |
  | `hello_is_mandatory_first_call` | ARCH-029 | `tools/list` before hello → `error.code == -32001` |
  | `hello_major_mismatch_is_refused` | ARCH-029, QA-005 | protocol `"2.0"` → `result.status == "error"`, `result.error.code == "protocol.incompatible"` |
  | `hello_minor_mismatch_uses_shared_capabilities` | ARCH-029 | `"1.7"` with `["events.v1","future.x"]` → ok with capabilities `["events.v1"]` |
  | `hello_rejects_unknown_parameters` | ARCH-029 | `{"color":"red"}` → `error.code == -32602` |
  | `drain_is_the_only_call_after_incompatible_hello` | ARCH-023, ARCH-031 | after the refused hello, `tools/list` → -32001; `core/drain` → ok; the core exits within 10 s |
  | `forged_engine_token_is_refused_not_downgraded` | ARCH-048, ARCH-050 | caller `assistant` with `engine_token: "forged"` → error `caller.engine_token_invalid` |
  | `declared_ui_from_unverified_process_is_cli` | ARCH-045, ARCH-046 | caller `ui:image` from the test process → ok with `caller_kind == "cli"` |
  | `hello_reports_pipe_contract` | ARCH-024 | `core.pipe == "\\\\.\\pipe\\worldwright-core-S-1-5-21-1000-2000-3000-1001-" + channel` |

  **`core_dispatch.rs`:**

  | Test | Rules | Assertion |
  |---|---|---|
  | `results_are_exactly_ok_pending_or_error` | QA-001, ARCH-036, CORE-017 | `test.echo` → `status == "ok"` with `structured.text`, `artifacts`, `summary_text`; `no.such` → `status == "error"` |
  | `errors_carry_code_category_trace_and_retry` | QA-003, QA-004 | the error has non-empty `code`, `message` and `trace_id`, a category from the 7 and a retry from the 4 |
  | `unknown_tool_arguments_are_rejected` | CORE-015 | `test.echo {"text":"x","bogus":1}` → `params.invalid` |
  | `write_without_idempotency_key_is_refused` | ARCH-035 | → `idempotency.key_required` |
  | `same_key_same_args_returns_same_result` | CORE-016 | `test.write {label:"a"}` key `k` twice → identical `result` JSON, `executions == 1` |
  | `same_key_different_args_is_rejected` | CORE-016 | → `idempotency.conflict` |
  | `agent_destructive_call_without_answer_path_is_unavailable` | QA-016, ARCH-052, ARCH-055 | `cli` and `non_interactive: true`, `test.destructive` → `confirm.unavailable`, `user_fixable`, `after_resolve` |
  | `app_methods_refuse_cli_callers` | ARCH-031, APP-040 | `app/set_context` → `result.error.code == "caller.not_allowed"` |
  | `tool_panic_fails_with_bug_and_core_survives` | QA-012 | `test.panic` → `core.bug`, category `bug`; then `test.echo` is ok |

  **`core_events.rs`:**
  - `events_have_per_topic_sequence_and_replay_from_cursor` (ARCH-032). Subscribe to `test:a` with `since_seq {}`. Two `test.emit` calls arrive as `ww/event` with seq 1 and 2. A second connection subscribing with `since_seq {"test:a": 1}` receives only seq 2.

  **`core_state.rs`:**
  - `newer_state_db_is_refused_and_kept` (QA-062, CORE-004). Write the suite `state.db` with `user_version` 999. `CoreFixture::spawn_raw` exits with code 2, and the file's SHA-256 is unchanged.
  - `corrupt_state_db_is_refused_and_kept` (CORE-004). The same, with 4 KiB of `0xAB` bytes.

  **`cli_basics.rs`:**

  | Test | Rules | Assertion |
  |---|---|---|
  | `status_starts_core_and_reports_it` | ARCH-016, ARCH-018 | `ww status --json` → exit 0, `running == true`, `hello.core.pipe` per contract |
  | `status_no_start_reports_not_running` | ARCH-018 | fresh env: `ww --no-start status` → exit 3; `RawPipe::try_connect(env, 1 s)` is `None` afterwards |
  | `no_start_call_is_a_silent_noop` | ARCH-018 | `ww --no-start call test.echo "{\"text\":\"x\"}"` → exit 0, stdout and stderr empty |
  | `call_maps_outcomes_to_exit_codes` | ARCH-016 | `test.echo` → 0 and stdout parses as an ok outcome; `no.such` → 1 |
  | `non_interactive_destructive_is_confirm_unavailable` | QA-016 | `ww --non-interactive call test.destructive` → exit 1, stdout `error.code == "confirm.unavailable"` |
  | `core_stop_drains_and_exits` | ARCH-023 | `ww status` then `ww core stop` → exit 0; `ww --no-start status` → exit 3 |

- [ ] **Step 2: Run them and list the failures.**

Run: `cargo run -q -p ww-cli -- dev spec-tests`
Expected: exit 0, `spec tests: 1 passed, 27 expected-fail`: every new test fails and is listed.

- [ ] **Step 3: Commit.**

```bash
git add crates/ww-spec-tests Cargo.lock
git commit -m "spec-tests: plan-2a core protocol scenarios, expected to fail (QA-017)"
```

---

### Task 7: `ww-core` transport: listener, single instance, framing and the binary (ARCH-004, ARCH-006, ARCH-024–026, ARCH-033, QA-010)

**Files:**
- Modify: `crates/ww-core/Cargo.toml`. Add deps `ww-platform`, `ww-protocol`, `ww-schema`, `tokio`, `tokio-util`, `serde`, `serde_json`, `schemars`, `rusqlite`, `tracing`, `tracing-subscriber` and `tracing-appender`. Add dev-deps `ww-testkit` and `tokio` (`test-util`).
- Modify: `crates/ww-core/clippy.toml` (the entries in Global Constraints).
- Create: `crates/ww-core/src/{config.rs, server/mod.rs, server/conn.rs, main.rs}`. Modify `src/lib.rs`.
- Test: `crates/ww-core/tests/transport.rs`.

**Interfaces:**

Produces:

```rust
pub struct CoreConfig {
    pub env: Env, pub clock: Arc<dyn Clock>, pub ids: Arc<dyn IdSource>, pub inspector: Arc<dyn ProcessInspector>,
    pub idle_after: Duration /* 600 s */, pub hello_timeout: Duration /* 30 s */,
    pub test_tools: bool /* channel is test-* */, pub test_parent_pid: Option<u32>,
}   // Task 8 adds `trusted: TrustedDirs` and `engine_tokens: Arc<dyn EngineTokens>`
impl CoreConfig { pub fn for_env(env: Env) -> io::Result<CoreConfig>; }   // System*/Random*; WW_TEST_* via env.test_overrides
pub enum CoreExit { AlreadyRunning, Startup(String) }
impl CoreExit { pub fn code(&self) -> u8; }                              // 3, 2
pub struct ConnCtx { pub conn_id: u64, pub client_pid: u32, pub outbound: tokio::sync::mpsc::Sender<String> }
pub trait Router: Send + Sync + 'static {
    fn handle<'a>(&'a self, conn: &'a ConnCtx, req: Request) -> Pin<Box<dyn Future<Output = Option<Response>> + Send + 'a>>;
    fn closed(&self, conn: &ConnCtx);
}
pub struct ServerStats { pub connections: AtomicUsize }
pub async fn serve(pipe: &str, router: Arc<dyn Router>, stats: Arc<ServerStats>, stop: CancellationToken) -> Result<(), CoreExit>;
pub async fn start(cfg: CoreConfig) -> Result<RunningCore, CoreExit>;    // the production router (Tasks 8–12)
pub struct RunningCore;
impl RunningCore { pub fn pipe_name(&self) -> &str; pub fn request_drain(&self); pub async fn wait(self); }
    // request_drain cancels `serve` in this task; Task 12 routes it through Lifecycle::begin_drain(Requested)
pub struct CoreThread; impl CoreThread { pub fn core(&self) -> &RunningCore; }
pub fn start_in_thread(cfg: CoreConfig) -> CoreThread;                   // test helper: own multi-thread runtime; Drop stops it
```

`CancellationToken` is `tokio_util::sync::CancellationToken`.

**Transport rules:**
- **Pipe instances.** The first instance is created with `first: true`; `AlreadyOwned` → `CoreExit::AlreadyRunning`. A new instance is always created before a connected one is handed to its task.
- **Per connection:** one reader with `LinesCodec::new_with_max_length(MAX_LINE_BYTES)` and one writer task fed by `outbound`.
  - Each request is handled in its own task, so a slow call never blocks the connection.
  - Lines that fail `parse_request` get the error response.
  - An over-long line gets a -32002 response with id `null`, and then the connection closes.
  - Notifications from clients are ignored.
  - A partial line at EOF is dropped.
  - `Router::closed` runs exactly once per connection.
- **The `ww-core` binary.** `#![cfg_attr(not(test), windows_subsystem = "windows")]`. It builds the `Env` with `Env::from_process()`, builds a multi-thread runtime, runs `start(cfg).await?.wait()`, and maps `CoreExit` to the exit code. In this task the production router answers every method with -32601. The hello timeout (close a connection that hasn't sent `core/hello` within `hello_timeout`) lives in the router, so it lands in Task 8.

- [ ] **Step 1: Write the failing tests** in `tests/transport.rs`. They use `serve` with a fake router (method `echo` echoes params; method `slow` waits on a `Notify` the test releases) and `RawPipe`:
  - `serves_on_the_channel_pipe_name` (ARCH-024);
  - `second_server_on_same_pipe_is_already_running` (ARCH-026);
  - `requests_and_responses_are_one_json_object_per_line` (ARCH-025);
  - `garbage_line_gets_parse_error_and_connection_survives` (Review Focus 4): `{nope` → -32700 with id `null`, then `echo` on the same connection works;
  - `oversized_line_closes_only_that_connection` (Review Focus 4, ARCH-033): 4 MiB + 1 bytes → -32002 then EOF, while a second connection still echoes;
  - `truncated_line_at_disconnect_is_ignored` (Review Focus 4): half a request then disconnect; `closed` is called once and the server still serves;
  - `batch_requests_are_rejected`: `[{…}]` → -32600;
  - `slow_request_does_not_block_others_on_the_connection`;
  - `client_vanishing_mid_request_frees_the_connection` (Review Focus 2): `connections` returns to 0 after the client drops while `slow` is pending;
  - `core_opens_no_network_listener` (ARCH-006). E2E, using `CoreFixture`: every `netstat -ano` row with the core's PID is neither `LISTENING` nor UDP.
  - Remove `single_core_per_channel` from `expected-fail.toml`.
- [ ] **Step 2: Run them.**

Run: `cargo nextest run -p ww-core --test transport`
Expected: FAIL. `serve` isn't defined.

- [ ] **Step 3: Implement it.**
- [ ] **Step 4: Run them, then the spec tests.**

Run: `cargo nextest run -p ww-core && cargo clippy -p ww-core --all-targets -- -D warnings && cargo run -q -p ww-cli -- dev spec-tests`
Expected: PASS; spec tests exit 0.

- [ ] **Step 5: Commit.**

```bash
git add crates/ww-core crates/ww-spec-tests/expected-fail.toml Cargo.lock
git commit -m "core: pipe listener, single instance per channel, NDJSON framing, ww-core binary (ARCH-024-026)"
```

---

### Task 8: `core/hello`, caller identity and workspace resolution (ARCH-029, ARCH-030, ARCH-045–048, ARCH-050, ARCH-090, CORE-007, QA-040)

**Files:**
- Create: `crates/ww-schema/src/suite.rs`. Modify `crates/ww-schema/{Cargo.toml,src/lib.rs}` (deps `serde`, `toml`).
- Create: `crates/ww-core/src/{identity.rs, workspace.rs, session.rs, rpc.rs}`.
- Test: unit tests in `identity.rs` and `workspace.rs`; `crates/ww-core/tests/hello.rs`; `crates/ww-core/tests/workspace_guard.rs`, which arms the guard and so is its own binary.

**Interfaces:**

Consumes:
- Task 2: `HelloParams`, `negotiate` and `CallerKind`.
- Task 3: `Env`, `check_workspace`.
- Task 4: `ProcessInspector`.
- Task 7: `Router` and `CoreConfig`.

Produces:

```rust
// ww-schema
pub struct SuiteToml { pub default_workspace: Option<String>, #[serde(default)] pub workspaces: Vec<WorkspaceEntry>,
                       #[serde(flatten)] pub other: toml::Table }
pub struct WorkspaceEntry { pub path: String, #[serde(flatten)] pub other: toml::Table }
impl SuiteToml { pub fn parse(text: &str) -> Result<SuiteToml, toml::de::Error>; }
// ww-core identity.rs
pub struct TrustedDirs { pub app_dirs: Vec<PathBuf>, pub mcp_dirs: Vec<PathBuf> }
impl TrustedDirs { pub fn for_env(env: &Env, own_exe_dir: &Path) -> TrustedDirs; }
pub const APP_EXES: &[(&str, &str)] = &[("ww-image.exe", "image")];      // (exe file name, app)
pub const MCP_EXE: &str = "ww-mcp.exe";
pub struct EngineSession { pub workspace: String, pub profile_id: Option<String> }
pub trait EngineTokens: Send + Sync { fn validate(&self, token: &str, client_pid: u32) -> Option<EngineSession>; }
pub struct NoEngineTokens;                                                 // plan 2: every token is invalid
pub fn resolve_caller(declared: &CallerKind, engine_token: Option<&str>, client: &ProcessInfo,
                      trusted: &TrustedDirs, tokens: &dyn EngineTokens) -> Result<(CallerKind, Option<EngineSession>), ErrorBody>;
// workspace.rs
pub struct WorkspaceRef { pub id: String, pub path: PathBuf }
pub fn resolve_workspace(env: &Env, requested: &str, trace_id: &str) -> Result<Option<WorkspaceRef>, ErrorBody>;
// session.rs
pub struct Session { pub id: String, pub caller: CallerKind, pub non_interactive: bool, pub workspace: Option<WorkspaceRef>,
                     pub capabilities: Vec<String>, pub client_pid: u32, pub profile_id: Option<String> }
// rpc.rs: the production Router; per connection a state machine
pub enum ConnState { AwaitingHello, Refused /* protocol.incompatible: only core/drain */, Ready(Arc<Session>) }
```

**Rules:**
- **`TrustedDirs::for_env`:**
  - stable: `app_dirs = [install_dir]`, `mcp_dirs = [install_dir, local_dir\bin]`;
  - dev and test: both are `[own_exe_dir]` (ARCH-046, ARCH-047).
- **`resolve_caller`.** Paths compare case-insensitively after `\\?\` stripping.
  - A presented `engine_token` must validate → `Assistant`. Otherwise the result is `Err(caller.engine_token_invalid)`, never a downgrade (ARCH-048, ARCH-050).
  - A token with a declared kind other than `assistant` → `params.invalid`.
  - `ui:<app>` → `Ui(app)` only if the image's file name is that app's exe and its directory is in `app_dirs`; otherwise `Cli`.
  - `mcp` → `Mcp` only for `ww-mcp.exe` in `mcp_dirs`. The `claude.exe` parent or grandparent check is best-effort: if it fails, a warning is logged and the caller stays `Mcp`.
  - Everything else → `Cli`.
- **`resolve_workspace`** (ARCH-030 on the core side, ARCH-090):
  - `"none"` → `None`.
  - Otherwise, read `env.suite_toml()`. An id equal to the hash of `default_workspace`'s canonical path → `Some`, after `check_workspace(path)`.
  - The id of another registered workspace → `workspace.unsupported` ("M1 serves only the default workspace").
  - An unregistered id, or a registered path that no longer exists → `workspace.unknown`.
  - An `assistant` session uses its `EngineSession.workspace` instead (ARCH-030).
- **Hello handling, in order:**
  1. Parse `HelloParams`; failure → -32602.
  2. `negotiate`; `Incompatible` → `protocol.incompatible` and state `Refused`.
  3. Parse `caller_kind`.
  4. `resolve_caller` with `inspector.inspect(client_pid)`.
  5. `resolve_workspace`.
  6. `shared_capabilities`.
  7. Mint `session_id` (`s_…`); `instance_id` (`c_…`) is minted once per core.
  8. Reply with `HelloResult`.
- **Other connection-state rules:**
  - Any method before hello → -32001.
  - A second hello → -32600.
  - A `schema_hash` mismatch is logged, not refused.
  - No hello within `hello_timeout` → the connection is closed.

- [ ] **Step 1: Write the failing tests:**
  - **identity (unit, fake `ProcessInfo`):**
    - `declared_ui_from_app_exe_in_trusted_dir_is_ui`;
    - `declared_ui_from_elsewhere_is_cli`;
    - `declared_ui_with_wrong_exe_is_cli`;
    - `mcp_from_trusted_dir_is_mcp_without_claude_parent`;
    - `mcp_from_untrusted_dir_is_cli`;
    - `any_engine_token_is_refused_in_plan_2`;
    - `token_with_non_assistant_kind_is_invalid_params`;
    - `assistant_without_token_is_cli`;
    - `dir_comparison_ignores_case_and_verbatim_prefix`;
    - `stable_trusts_install_dir_and_bin_for_mcp`;
    - `dev_and_test_trust_own_dir`.
  - **workspace (unit, `TestEnv`):**
    - `none_means_no_workspace`;
    - `default_workspace_id_resolves`;
    - `unregistered_id_is_unknown`;
    - `registered_non_default_is_unsupported_in_m1`;
    - `vanished_workspace_is_unknown`;
    - `suite_toml_round_trips_unknown_keys` (ww-schema).
  - **`tests/hello.rs` (`start_in_thread` + `RawPipe`):**
    - `hello_returns_session_and_shared_capabilities`;
    - `refused_hello_allows_nothing_but_drain` (here `tools/list` → -32001; Task 12 covers drain);
    - `unknown_hello_param_is_invalid_params`;
    - `second_hello_is_invalid_request`;
    - `hello_result_reports_pid_channel_and_pipe`;
    - `silent_connection_is_closed_after_hello_timeout` (`start_paused`).
  - **`tests/workspace_guard.rs`:** `test_core_refuses_real_registered_workspace` (QA-040). Arm the guard with fake real locations whose `workspaces` holds a `TestEnv` workspace that is also registered as that env's default; `resolve_workspace` panics with "real Worldwright location".
  - Remove the `hello_*`, `forged_engine_token_*` and `declared_ui_*` entries from `expected-fail.toml`.
- [ ] **Step 2: Run them.**

Run: `cargo nextest run -p ww-core -p ww-schema`
Expected: FAIL. Nothing is implemented.

- [ ] **Step 3: Implement it.**
- [ ] **Step 4: Run them, then the spec tests.**

Run: `cargo nextest run -p ww-core -p ww-schema && cargo clippy -p ww-core -p ww-schema --all-targets -- -D warnings && cargo run -q -p ww-cli -- dev spec-tests`
Expected: PASS; spec tests exit 0.

- [ ] **Step 5: Commit.**

```bash
git add crates/ww-core crates/ww-schema crates/ww-spec-tests/expected-fail.toml Cargo.lock
git commit -m "core: core/hello handshake, verified caller identity, workspace resolution (ARCH-029/030/045-050)"
```

---

### Task 9: `state` and the store's file-write home (CORE-004, CORE-005, ARCH-097, ARCH-112, QA-012, QA-024, QA-062)

**Files:**
- Delete: `crates/ww-core/src/store.rs`. Create `crates/ww-core/src/store/{mod.rs, fsops.rs}`; `write_file_atomic` moves into `fsops.rs` unchanged.
- Create: `crates/ww-core/src/state/{mod.rs, migrations.rs}`.
- Modify: `crates/ww-testkit/tests/single_write_allowance.rs`. The allowance is permitted only in `src/store/fsops.rs`, and the test fails if that file is missing.
- Modify: `crates/ww-core/src/{lib.rs, rpc.rs}`. At startup the core opens the suite `state.db`; failure → `CoreExit::Startup`, exit 2.
- Test: unit tests in `state/` and `store/fsops.rs`; `crates/ww-core/tests/state.rs`.

**Interfaces:**

Produces:

```rust
// store/fsops.rs: the only file with #[allow(clippy::disallowed_methods)]
pub fn write_file_atomic(path: &Path, bytes: &[u8]) -> io::Result<()>;
pub fn ensure_dir(path: &Path) -> io::Result<()>;
// state
pub enum StateScope { Suite, Workspace }
pub enum StateError { TooNew { found: i64, known: i64 }, Corrupt(String), Io(String), ActorGone }
pub enum IdemBegin { Fresh, Replay(String /* outcome JSON */), InProgress, Conflict }
pub struct StateDb { /* Sender to the actor thread */ }
impl StateDb {
    pub fn open(path: &Path, scope: StateScope, on_fatal: Arc<dyn Fn(&str) + Send + Sync>) -> Result<StateDb, StateError>;
    pub fn schema_version(&self) -> i64;
    pub async fn idem_begin(&self, scope_key: &str, key: &str, args_hash: &str, now_utc_secs: i64) -> Result<IdemBegin, StateError>;
    pub async fn idem_finish(&self, scope_key: &str, key: &str, outcome_json: Option<String>) -> Result<(), StateError>; // None releases
}
pub struct StateRegistry;
impl StateRegistry {
    pub fn open(env: &Env, on_fatal: Arc<dyn Fn(&str) + Send + Sync>) -> Result<StateRegistry, StateError>; // opens the suite db
    pub fn suite(&self) -> &StateDb;
    pub fn for_workspace(&self, ws: &WorkspaceRef) -> Result<Arc<StateDb>, StateError>;              // lazy, cached
}
pub const SUITE_MIGRATIONS: &[&str]; pub const WORKSPACE_MIGRATIONS: &[&str];
```

**Rules:**
- **One actor per database** (CORE-004: one connection each). Each actor runs on its own named thread (`ww-state-suite`, `ww-state-ws-<id>`) and owns the `rusqlite::Connection`.
- **At open:**
  1. `fsops::ensure_dir(parent)`.
  2. Open the connection.
  3. `PRAGMA quick_check`; anything but `ok`, or "file is not a database" → `Corrupt`.
  4. `journal_mode=WAL`, `synchronous=FULL`, `foreign_keys=ON`.
  5. `user_version` > the known count → `TooNew` (QA-062).
  6. Apply the missing migrations, each in `BEGIN IMMEDIATE`.

  The file is **never** deleted, renamed or truncated by `ww-core`.
- **Migration 1, both scopes:** `idempotency(scope TEXT, key TEXT, args_hash TEXT NOT NULL, state TEXT NOT NULL CHECK (state IN ('running','done')), outcome TEXT, created_utc INTEGER NOT NULL, PRIMARY KEY (scope, key))`. On every open, `running` rows (calls that died with an earlier core) are deleted, and `done` rows older than 30 days are pruned.
- **`idem_begin`:**
  - no row → insert `running` → `Fresh`;
  - same hash and `done` → `Replay`;
  - same hash and `running` → `InProgress`;
  - different hash → `Conflict`.
- **Panics.** An actor panic makes every pending and later call return `ActorGone` and calls `on_fatal("state")` once. Task 12 turns that into a clean shutdown (QA-012).

- [ ] **Step 1: Write the failing tests:**
  - `fresh_db_is_migrated_with_wal_and_full_sync`: `synchronous == 2`, `journal_mode == "wal"`, and `user_version == SUITE_MIGRATIONS.len()`;
  - `fts5_and_json1_are_available` (CORE-004): creating an `fts5` virtual table and `SELECT json('{}')` both succeed;
  - `newer_schema_is_refused_and_file_untouched` (QA-062): SHA-256 is equal before and after;
  - `corrupt_file_is_refused_and_kept`;
  - `reopen_at_current_version_is_a_no_op`;
  - `idempotency_lifecycle`: Fresh → finish(Some) → Replay with byte-equal JSON; another hash → Conflict; begin while running → InProgress; finish(None) → Fresh again;
  - `running_rows_are_released_on_reopen`;
  - `actor_panic_reports_fatal_once`, using the `#[cfg(test)]` hook `StateDb::panic_actor_for_test()`;
  - `databases_live_where_core_005_says`: the suite db is at `env.suite_state_db()`, and a workspace db is at `env.workspace_local_dir(ws)\state.db`;
  - `ensure_dir_creates_nested_dirs`;
  - `atomic_write_replaces_contents` (moved);
  - the updated `single_write_allowance` test.
  - Remove the `core_state.rs` entries from `expected-fail.toml`.
- [ ] **Step 2: Run them.**

Run: `cargo nextest run -p ww-core -p ww-testkit`
Expected: FAIL. Nothing is implemented.

- [ ] **Step 3: Implement it.**
- [ ] **Step 4: Run the tests and lint, the spec tests, and a deliberate-violation check.**

Run: `cargo nextest run -p ww-core -p ww-testkit && cargo clippy -p ww-core --all-targets -- -D warnings && cargo run -q -p ww-cli -- dev spec-tests`
Expected: PASS; spec tests exit 0.

Then a deliberate-violation check: in a scratch copy, a `std::fs::create_dir_all` inside `state/mod.rs` must make clippy fail. Record the output, then remove the line.

- [ ] **Step 5: Commit.**

```bash
git add crates/ww-core crates/ww-testkit/tests/single_write_allowance.rs crates/ww-spec-tests/expected-fail.toml Cargo.lock
git commit -m "core: state.db actors with forward-only migrations and idempotency table; store/fsops write home (CORE-004)"
```

---

### Task 10: Dispatcher pipeline, tool registry and idempotency (ARCH-003, ARCH-031, ARCH-035–038, ARCH-051, ARCH-052, ARCH-055, CORE-015–017, CORE-022, CORE-058, QA-001, QA-012, QA-016)

**Files:**
- Create: `crates/ww-core/src/dispatch/{mod.rs, registry.rs, stages.rs, test_tools.rs}`.
- Modify: `crates/ww-core/src/rpc.rs`. Route `tools/list`, `tools/call` and the `app/*` gate.
- Test: unit tests in `dispatch/`; `crates/ww-core/tests/dispatch.rs`.

**Interfaces:**

Consumes:
- Task 8: `Session`.
- Task 9: `StateRegistry`, `IdemBegin`.

Produces:

```rust
pub type ToolFuture<'a> = Pin<Box<dyn Future<Output = Result<ToolOutput, ErrorBody>> + Send + 'a>>;
pub struct CallCtx { pub session: Arc<Session>, pub trace_id: String, pub settings: Settings }   // Task 11 adds `events: Arc<EventBus>`
pub trait Tool: Send + Sync + 'static {
    fn descriptor(&self) -> &ToolDescriptor;
    fn validate(&self, args: &serde_json::Value) -> Result<(), String>;      // stage 2; Err = params.invalid message
    fn run<'a>(&'a self, ctx: &'a CallCtx, args: serde_json::Value) -> ToolFuture<'a>;
}
pub fn typed_tool<P, R, F>(name: &'static str, description: &'static str, effect: EffectClass, module: &'static str, f: F) -> Arc<dyn Tool>
where P: DeserializeOwned + JsonSchema + Send + 'static, R: Serialize + JsonSchema,
      F: for<'a> Fn(&'a CallCtx, P) -> Pin<Box<dyn Future<Output = Result<(R, String /* summary */), ErrorBody>> + Send + 'a>> + Send + Sync + 'static;
pub struct Registry;
impl Registry { pub fn new() -> Registry; pub fn register(&mut self, t: Arc<dyn Tool>);   // panics on duplicates or descriptions > 2000 chars
                pub fn get(&self, name: &str) -> Option<Arc<dyn Tool>>; pub fn list_for(&self, caller: &CallerKind) -> Vec<ToolDescriptor>; }
// stages.rs: one trait per stage plan 3 replaces; plan-2 impls in brackets
pub struct Settings;                                   // empty in plan 2 (2b: cascade)
pub enum Estimate { Zero }                             // plan 3 adds external estimates
pub enum ConfirmDecision { Proceed, Pending(PendingConfirmation), Unavailable }
pub trait SettingsStage: Send + Sync { fn resolve(&self, s: &Session) -> Settings; }                        // [empty]
pub trait EstimateStage: Send + Sync { fn estimate(&self, d: &ToolDescriptor) -> Estimate; }               // [Zero]
pub trait ConfirmStage: Send + Sync { fn check(&self, s: &Session, d: &ToolDescriptor, e: &Estimate) -> ConfirmDecision; }
    // [agent caller && effect.needs_agent_confirmation() → Unavailable, else Proceed]
pub trait ReserveStage: Send + Sync { fn reserve(&self, s: &Session, e: &Estimate) -> Result<(), ErrorBody>; } // [Ok]
pub trait MeterStage: Send + Sync { fn settle(&self, s: &Session, out: &Outcome<ToolOutput>); }            // [no-op]
pub trait RecordStage: Send + Sync { fn record(&self, s: &Session, d: &ToolDescriptor, out: &mut Outcome<ToolOutput>); } // [no-op; 2b]
pub struct PipelineHooks { pub settings: Arc<dyn SettingsStage>, pub estimate: Arc<dyn EstimateStage>, pub confirm: Arc<dyn ConfirmStage>,
                           pub reserve: Arc<dyn ReserveStage>, pub meter: Arc<dyn MeterStage>, pub record: Arc<dyn RecordStage> }
impl PipelineHooks { pub fn plan2() -> PipelineHooks; }
pub struct Dispatcher;
impl Dispatcher {
    pub fn new(reg: Registry, hooks: PipelineHooks, states: Arc<StateRegistry>, ids: Arc<dyn IdSource>, clock: Arc<dyn Clock>) -> Dispatcher;
    pub async fn call(&self, session: Arc<Session>, p: ToolCallParams) -> Outcome<ToolOutput>;
}
pub struct TestGates; impl TestGates { pub fn open(&self, gate: &str); }
impl RunningCore { pub fn test_gates(&self) -> &TestGates; }                  // addition to Task 7's type
```

**`Dispatcher::call`** runs the CORE-015 stages in order. Any `Err` becomes `Outcome::Error`.
1. **Resolve:**
   - `trace_id` is the caller's (`[A-Za-z0-9_-]{1,64}`) or `ids.id("t")`.
   - The tool must exist and be visible to the caller, else `tool.unknown`.
   - Effect ≠ `read` with no `idempotency_key` → `idempotency.key_required`.
   - `scope_key = "<caller kind>|<workspace id or none>"` (CORE-016).
2. **Validate:**
   - `tool.validate(args)`, else `params.invalid`.
   - For effect ≠ `read`: `idem_begin` on the session's workspace `state.db`, or on the suite one when there is no workspace. `Replay` returns the stored outcome immediately; `Conflict` → `idempotency.conflict`; `InProgress` → `idempotency.in_progress`.
3. **Settings.**
4. **Estimate.**
5. **Confirm and caps:** `Unavailable` → `confirm.unavailable`; `Pending` → `Outcome::PendingConfirmation` (QA-016, CORE-017).
6. **Reserve.**
7. **Execute:** run the tool in `tokio::spawn`. A `JoinError` panic → `core.bug`, category `bug`, with the panic message as untrusted detail (QA-012).
8. **Meter.**
9. **Record.**

Then `idem_finish`: `Ok` and `PendingConfirmation` are stored; an `Error` releases the key. `state.db` errors → `state.unavailable`.

**RPC routing:**
- `tools/list` → `ToolsListResult` from `list_for`.
- `tools/call` → `Dispatcher::call`, wrapped in a JSON-RPC result.
- `app/*` (ARCH-031, APP-040): from a non-`Ui` caller → `Outcome::Error(caller.not_allowed)`. From a `Ui` caller → -32601, because plan 2 defines no `app/*` method and no read-only `cli` subset yet.

**Test tools.** Registered when `cfg.test_tools` is set, per Global Constraints. `test.emit` lands in Task 11.

- [ ] **Step 1: Write the failing tests:**
  - **Unit tests (`Dispatcher` with recording hooks and `SeqIds`):**
    - `stages_run_in_core_015_order`: `["resolve","validate","settings","estimate","confirm","reserve","execute","meter","record"]`;
    - `unknown_tool_is_tool_unknown`;
    - `invalid_arguments_stop_after_validate`: only `resolve` and `validate` are recorded;
    - `write_without_key_is_refused` (ARCH-035);
    - `same_key_same_args_replays_without_executing` (CORE-016): `executions` stays 1 and the outcomes are byte-equal;
    - `same_key_different_args_conflicts`;
    - `key_scope_is_caller_kind_and_workspace`: the same key from a `Cli` and an `Mcp` session executes twice;
    - `error_outcomes_release_the_key`: `fail: true`, then a retry with the same key executes;
    - `agent_destructive_is_confirm_unavailable` (ARCH-052, QA-016; also for a non-interactive session; the tool doesn't run);
    - `ui_destructive_proceeds` (CORE-109: your own actions never ask);
    - `tool_panic_is_core_bug` (QA-012, ARCH-060): the panic message is in `detail` with `untrusted: true`;
    - `tool_failures_are_results_not_rpc_errors` (QA-010): `tool.unknown` and `params.invalid` arrive as `Outcome::Error` inside a JSON-RPC `result`;
    - `tools_list_is_the_registry_filtered_per_caller` (ARCH-003, ARCH-031);
    - `descriptors_use_json_schema_2020_12_and_fit_2000_chars` (ARCH-037): every registered input and output schema has `$schema == "https://json-schema.org/draft/2020-12/schema"`;
    - `every_tool_declares_a_module` (CORE-058);
    - `test_tools_exist_only_on_test_channels`;
    - `tool_results_are_json_text_with_file_paths` (CORE-022, ARCH-038): `ToolOutput` carries `artifacts[].path` as strings.
  - **`tests/dispatch.rs` (`start_in_thread` + `RawPipe`):**
    - `app_namespace_refuses_non_ui`;
    - `concurrent_same_key_gets_in_progress` (`test.block`);
    - `client_killed_mid_call_leaves_core_healthy` (Review Focus 2). Connection A calls `test.block {gate:"g"}` with key `k`, then A is dropped. Connection B's `test.echo` is ok. Connection C, before the gate opens → `idempotency.in_progress`. Open the gate. Connection C, with the same key and arguments → ok with `executions == 1`, the stored result of A's call.
  - Remove the `core_dispatch.rs` entries from `expected-fail.toml`.
- [ ] **Step 2: Run them.**

Run: `cargo nextest run -p ww-core`
Expected: FAIL. Nothing is implemented.

- [ ] **Step 3: Implement it.**
- [ ] **Step 4: Run them, then the spec tests.**

Run: `cargo nextest run -p ww-core && cargo clippy -p ww-core --all-targets -- -D warnings && cargo run -q -p ww-cli -- dev spec-tests`
Expected: PASS; spec tests exit 0.

- [ ] **Step 5: Commit.**

```bash
git add crates/ww-core crates/ww-spec-tests/expected-fail.toml
git commit -m "core: dispatcher pipeline, tool registry, idempotency, fail-closed agent confirmations (CORE-015-017)"
```

---

### Task 11: Events: `ww/subscribe`, per-topic sequences, history and resync (ARCH-032, CORE-012)

**Files:**
- Create: `crates/ww-core/src/events.rs`.
- Modify: `crates/ww-core/src/{rpc.rs, dispatch/test_tools.rs}` (`test.emit`).
- Test: unit tests in `events.rs`; `crates/ww-core/tests/events.rs`.

**Interfaces:**

Produces:

```rust
pub type ClientId = u64;                                   // = ConnCtx::conn_id
pub trait SnapshotProvider: Send + Sync { fn snapshot(&self, topic: &str) -> serde_json::Value; }
pub struct EventBus;
impl EventBus {
    pub fn new(clock: Arc<dyn Clock>) -> EventBus;
    pub fn publish(&self, topic: &str, data: serde_json::Value) -> u64;              // returns the new seq
    pub fn subscribe(&self, client: ClientId, topics: &[String], since: &BTreeMap<String, u64>,
                     sink: tokio::sync::mpsc::Sender<String>) -> Vec<TopicState>;     // sink = ConnCtx::outbound
    pub fn unsubscribe(&self, client: ClientId, topics: &[String]);
    pub fn drop_client(&self, client: ClientId);                                       // from Router::closed
    pub fn register_snapshot(&self, topic_prefix: &str, p: Arc<dyn SnapshotProvider>);
}
// addition to Task 10: CallCtx gains `pub events: Arc<EventBus>`, so tools (test.emit; 2b's store) can publish
```

**Rules** (ARCH-032):
- **Sequences.** Each topic's `seq` starts at 1 and increments per publish.
- **History.** Kept per topic for `HISTORY_SECS` (600) of **monotonic** time, pruned on publish and on subscribe (CORE-012).
- **Subscribe with a cursor `s` for a topic:**
  - `s == current` → nothing is replayed;
  - the retained history covers `s+1..=current` → those events are replayed in order;
  - otherwise (older than retained, or `s > current`, e.g. a cursor from an earlier core instance) → one `Snapshot` event from the provider whose prefix matches, else one `ResyncRequired`.
  - No cursor → live only.
- **Delivery.** Events go out as `ww/event` notifications through a per-client queue of `CLIENT_QUEUE` (1024). When it would overflow, that client's queued events are dropped and one `ResyncRequired` per subscribed topic is delivered instead.
- **Validation.** Invalid topic names → -32602.

- [ ] **Step 1: Write the failing tests:**
  - **Unit (`FakeClock`):**
    - `seq_is_per_topic_and_monotonic`;
    - `replay_from_cursor`;
    - `cursor_older_than_ten_minutes_gets_snapshot` (advance 601 s, provider registered);
    - `cursor_older_without_provider_gets_resync_required`;
    - `cursor_from_the_future_gets_resync_required`;
    - `overflow_sends_resync_required_once_per_topic`;
    - `unsubscribe_stops_delivery`;
    - `dropped_client_is_forgotten`;
    - `utc_jump_does_not_expire_history` (`shift_utc(-3600)` and `shift_utc(+7200)` keep events at 599 s of monotonic time).
  - **`tests/events.rs`:** `events_arrive_as_ww_event_notifications` (`RawPipe` + `test.emit`; the params match `EventParams`) and `invalid_topic_is_invalid_params`.
  - Remove the `core_events.rs` entry from `expected-fail.toml`.
- [ ] **Step 2: Run them.**

Run: `cargo nextest run -p ww-core`
Expected: FAIL. Nothing is implemented.

- [ ] **Step 3: Implement it.**
- [ ] **Step 4: Run them, then the spec tests.**

Run: `cargo nextest run -p ww-core && cargo clippy -p ww-core --all-targets -- -D warnings && cargo run -q -p ww-cli -- dev spec-tests`
Expected: PASS; spec tests exit 0.

- [ ] **Step 5: Commit.**

```bash
git add crates/ww-core crates/ww-spec-tests/expected-fail.toml
git commit -m "core: event bus with per-topic sequences, 10-minute history, snapshots and resync (ARCH-032)"
```

---

### Task 12: Lifecycle: idle exit, drain, shutdown order, session end, logs (ARCH-005, ARCH-020, ARCH-021, ARCH-023, CORE-012, QA-012)

**Files:**
- Create: `crates/ww-core/src/lifecycle.rs`, `crates/ww-platform/src/session_end.rs`.
- Modify: `crates/ww-platform/Cargo.toml`. Add the `windows` features `Win32_UI_WindowsAndMessaging`, `Win32_System_LibraryLoader` and `Win32_Graphics_Gdi`.
- Modify: `crates/ww-core/src/{rpc.rs, dispatch/mod.rs, main.rs, lib.rs}`.
- Test: unit tests in `lifecycle.rs`; `crates/ww-core/tests/lifecycle.rs`; `crates/ww-platform/tests/session_end.rs`.

**Interfaces:**

Produces:

```rust
// ww-core
pub const IDLE_EXIT_AFTER: Duration = Duration::from_secs(600);
pub const IDLE_TICK: Duration = Duration::from_secs(15);
pub const SESSION_END_BUDGET: Duration = Duration::from_millis(4500);
pub const FATAL_BUDGET: Duration = Duration::from_secs(5);
pub trait IdleBlocker: Send + Sync { fn name(&self) -> &'static str; fn busy(&self) -> bool; }
pub enum ShutdownPhase { Drain, CancelEnginePrompts, PersistInterrupted, StopEngines }       // ARCH-021 order
pub trait ShutdownStep: Send + Sync { fn phase(&self) -> ShutdownPhase; fn name(&self) -> &'static str;
                                     fn run(&self) -> Pin<Box<dyn Future<Output = ()> + Send + '_>>; }
pub enum DrainReason { Idle, Requested, SessionEnd, Fatal(String), TestParentGone }
pub struct CallGuard;                                                                       // in-flight call; Drop decrements
pub struct Lifecycle;
impl Lifecycle {
    pub fn new(clock: Arc<dyn Clock>, idle_after: Duration) -> Arc<Lifecycle>;
    pub fn connection_opened(&self); pub fn connection_closed(&self); pub fn call_started(&self) -> CallGuard;
    pub fn add_blocker(&self, b: Arc<dyn IdleBlocker>); pub fn add_shutdown_step(&self, s: Arc<dyn ShutdownStep>);
    pub fn tick(&self) -> Option<DrainReason>;                                              // Some(Idle) when idle long enough
    pub fn begin_drain(&self, r: DrainReason); pub fn is_draining(&self) -> bool;
    pub async fn run_shutdown(&self) -> ShutdownReport;
}
pub struct ShutdownReport { pub reason: DrainReason, pub steps: Vec<&'static str>, pub abandoned_calls: usize }
// ww-platform
pub enum SessionEnd { QueryEnd, End }
pub struct SessionEndWatcher;
impl SessionEndWatcher {
    pub fn start() -> io::Result<(SessionEndWatcher, tokio::sync::mpsc::UnboundedReceiver<SessionEnd>)>;
    pub fn shutdown_complete(&self);
    pub fn simulate_query_end_session(&self) -> bool;          // SendMessageW to its own window; for tests
}
```

**Rules:**
- **Idle** (ARCH-020). The core is idle when connections == 0, in-flight calls == 0 and no blocker is busy. The idle period restarts on any change, measured with `Clock::monotonic`. `tick` returns `Idle` once it reaches `idle_after`.
  - The binary ticks every `IDLE_TICK`. A test channel uses `WW_TEST_IDLE_SECS` when it's set.
  - Plan 3 registers jobs, pending confirmations and outside ComfyUI jobs as blockers.
- **Draining** (ARCH-021 step 1). New `tools/call`s get `core.draining`, while `core/hello`, `core/drain` and `ww/*` still work. Shutdown waits:
  - **`Requested`** (`core/drain`, "exit when safe", ARCH-023): until in-flight calls == 0 and no blocker is busy, with no timeout; connected clients don't hold it up.
  - **`SessionEnd`:** at most `SESSION_END_BUDGET`.
  - **`Fatal` and `TestParentGone`:** at most `FATAL_BUDGET`.
  - **`Idle`:** no wait.

  Then the steps run grouped by phase, in `ShutdownPhase` order, whatever order they were registered in. Then the pipe closes and the process exits 0.
- **`core/drain`** replies `DrainResult { draining: true }` before it begins draining. It is accepted in `Ready` and `Refused`, and refused with -32001 in `AwaitingHello`.
- **The session-end window** (ARCH-021, ARCH-005: hidden, never shown). A hidden top-level window (class `WorldwrightCoreSessionEnd`) on its own thread with a message loop.
  - `WM_QUERYENDSESSION` → send `QueryEnd`, return TRUE.
  - `WM_ENDSESSION` with wParam TRUE → send `End`, then block until `shutdown_complete()` or `SESSION_END_BUDGET`.
  - The core begins `DrainReason::SessionEnd` on either message.
- **Fatal.** `StateRegistry`'s `on_fatal` → `begin_drain(Fatal("state"))` (QA-012).
- **Test parent.** On a test channel with `WW_TEST_PARENT_PID`, a task waits for that process to exit, or finds it already gone, → `TestParentGone`.
- **Logs** (ARCH-117/119, the part this plan needs). `tracing-subscriber` JSON to `tracing_appender::rolling` (daily, prefix `core`, suffix `log`, 14 files) in `env.logs_dir()`. A panic hook logs the message and location before the default hook. Plan 4 adds the size cap and the shared log setup.

- [ ] **Step 1: Write the failing tests:**
  - **Unit (`FakeClock`):**
    - `idle_exit_after_ten_minutes_with_nothing_running` (599 s → `None`, 600 s → `Some(Idle)`);
    - `connection_call_or_blocker_restarts_idle`;
    - `utc_jump_does_not_trigger_idle_exit` (CORE-012);
    - `shutdown_runs_phases_in_arch_021_order` (steps registered in reverse order run Drain → CancelEnginePrompts → PersistInterrupted → StopEngines);
    - `requested_drain_waits_for_in_flight_calls_and_blockers`;
    - `session_end_drain_is_time_boxed` (`start_paused`).
  - **`tests/lifecycle.rs` (`start_in_thread` + `RawPipe`):**
    - `draining_core_refuses_new_calls_with_core_draining`;
    - `drain_after_incompatible_hello_stops_core` (ARCH-023);
    - `drain_before_hello_is_handshake_required`;
    - `state_actor_death_stops_core` (QA-012);
    - `test_core_exits_when_parent_is_gone` (E2E: `CoreFixture::start_with_env` with `WW_TEST_PARENT_PID` set to an exited `%ComSpec% /c exit 0` child; the core exits 0 within 10 s).
  - **`ww-platform/tests/session_end.rs`:**
    - `query_end_session_is_reported`: `simulate_query_end_session()` returns TRUE and the receiver gets `QueryEnd`;
    - `session_end_window_is_never_visible` (ARCH-005): `IsWindowVisible` is false.
  - Remove `drain_is_the_only_call_after_incompatible_hello` from `expected-fail.toml`.
- [ ] **Step 2: Run them.**

Run: `cargo nextest run -p ww-core -p ww-platform`
Expected: FAIL. Nothing is implemented.

- [ ] **Step 3: Implement it.**
- [ ] **Step 4: Run them, then the spec tests.**

Run: `cargo nextest run -p ww-core -p ww-platform && cargo clippy -p ww-core -p ww-platform --all-targets -- -D warnings && cargo run -q -p ww-cli -- dev spec-tests`
Expected: PASS; spec tests exit 0.

- [ ] **Step 5: Commit.**

```bash
git add crates/ww-core crates/ww-platform crates/ww-spec-tests/expected-fail.toml Cargo.lock
git commit -m "core: idle exit, drain-when-safe, ARCH-021 shutdown order, session-end window, JSON logs"
```

---

### Task 13: `ww-client`: find, start, verify, handshake, call, subscribe (ARCH-018, ARCH-019, ARCH-023, ARCH-027, ARCH-030, ARCH-110)

**Files:**
- Create: `crates/ww-client/{Cargo.toml, src/lib.rs, src/connect.rs, src/conn.rs, src/workspace.rs, src/blocking.rs}`.
  - Deps: `ww-platform`, `ww-protocol`, `ww-schema`, `tokio`, `serde_json`.
  - Dev-deps: `ww-testkit`, `tempfile`.
- Test: unit tests in `workspace.rs`; `crates/ww-client/tests/client.rs`, which uses the real `ww-core` from `ww_testkit::bins`.

**Interfaces:**

Consumes:
- Task 4: `open_client`, `server_pid`, `SystemInspector`, `spawn_detached`, `ProcessHandle`.
- Task 2: the protocol types.

Produces:

```rust
pub enum StartPolicy { StartIfNeeded, NoStart }
pub struct ClientConfig {
    pub env: Env, pub caller_kind: CallerKind, pub non_interactive: bool, pub workspace: String /* id | "none" */,
    pub start: StartPolicy, pub core_exe: PathBuf, pub server_dirs: Vec<PathBuf>, pub protocol: ProtocolVersion,
    pub start_timeout: Duration /* 10 s */, pub build_id: String,
    pub spawn_env: Vec<(OsString, OsString)>,   // added to the spawned core's inherited env; empty in production, TestEnv::child_env() in tests
}
impl ClientConfig { pub fn for_env(env: Env, caller_kind: CallerKind) -> io::Result<ClientConfig>; }
    // core_exe = <own exe dir>\ww-core.exe; server_dirs = stable ? [install_dir] : [own exe dir]; protocol = PROTOCOL
pub enum ClientError {
    NotRunning, Untrusted { pid: u32, image: PathBuf, reason: String }, Incompatible { core_protocol: String },
    StartFailed { exit_code: Option<u32>, logs_dir: PathBuf }, Timeout, Refused(ErrorBody), Protocol(RpcError), Io(io::Error), Closed,
}
impl std::fmt::Display for ClientError;   // plain language; Untrusted names image and pid; StartFailed names the logs folder
pub struct Client;
impl Client {
    pub async fn connect(cfg: &ClientConfig) -> Result<Client, ClientError>;
    pub fn hello(&self) -> &HelloResult;
    pub async fn call_tool(&self, name: &str, args: serde_json::Map<String, serde_json::Value>, key: Option<String>) -> Result<Outcome<ToolOutput>, ClientError>;
    pub async fn list_tools(&self) -> Result<Vec<ToolDescriptor>, ClientError>;
    pub async fn subscribe(&self, topics: Vec<String>, since: BTreeMap<String, u64>) -> Result<EventStream, ClientError>;
}
pub struct EventStream; impl EventStream { pub async fn next(&mut self) -> Option<EventParams>; }  // None = core went away
pub enum DrainOutcome { NotRunning, Stopped }
pub async fn drain_and_wait(cfg: &ClientConfig, within: Duration) -> Result<DrainOutcome, ClientError>;
pub fn resolve_workspace_id(env: &Env, explicit: Option<&Path>, cwd: &Path) -> Result<String, ClientError>;
pub mod blocking { pub struct BlockingClient; /* same methods, synchronous; owns a current-thread runtime */ }
```

**`Client::connect`:**
1. Loop until `start_timeout`, with backoff from 25 ms doubling to 250 ms:
   - `open_client`: `Busy` → retry.
   - `NotFound` + `NoStart` → `NotRunning`.
   - `NotFound` otherwise → spawn the core once with `spawn_detached` (no args; the env is inherited, which carries `ww`'s own `WW_*` variables, plus `spawn_env`), then retry.
   - If the spawned core has exited: code 3 means it lost the race, so keep connecting; any other code → `StartFailed { exit_code, logs_dir: env.logs_dir() }`.
2. Verify the server (ARCH-027): `server_pid` → `inspect` immediately. The user SID must equal `current_user_sid()`, and the image's directory must be in `server_dirs`; otherwise `Untrusted`.
3. Send `core/hello`. On `protocol.incompatible` (ARCH-023): send `core/drain` on that connection, wait up to 60 s for the pipe to disappear, start a new core (unless `NoStart`) and handshake again once. If it's still incompatible → `Incompatible`.

**Other operations:**
- **`call_tool`** mints `ik_<16 hex>` when `key` is `None` (ARCH-110: `ww-client` owns idempotency keys).
- **`drain_and_wait`** sends `core/drain` after a hello that may be refused. It also does this for an `Untrusted` server owned by the same user: drain is harmless and lets `ww core stop` clear a foreign core. Then it polls `open_client` until `NotFound`.
- **`resolve_workspace_id`** (ARCH-030 `cli` chain):
  1. the explicit path (it must contain `worldwright.yaml`) → its hash;
  2. otherwise the nearest ancestor of `cwd` that has `worldwright.yaml` → its hash;
  3. otherwise `suite.toml`'s `default_workspace` → its hash;
  4. otherwise `"none"`.
- **Reconnecting and resubscribing** after a core restart belong to plan 4 (APP-034/035). `HelloResult.instance_id` already lets a client tell that a core restarted.

- [ ] **Step 1: Write the failing tests:**
  - **Unit:** `workspace_chain_explicit_then_cwd_then_default_then_none` and `explicit_path_without_worldwright_yaml_is_an_error`.
  - **`tests/client.rs`.** `server_dirs = [bins.dir]`, `core_exe = bins.ww_core` and `spawn_env = TestEnv::child_env()`.
    - `starts_core_when_missing_and_connects` (ARCH-004, ARCH-018, ARCH-019: an on-demand detached process, not a service);
    - `no_start_reports_not_running_without_spawning`;
    - `racing_starts_yield_one_core` (Review Focus 1): 8 threads connect at once on a fresh channel, and every `hello().core.pid` is equal;
    - `foreign_core_is_refused_and_named` (Review Focus 3, ARCH-027): `CoreFixture::start_exe` with a copy of `ww-core.exe` in `<root>\other bin\`. `connect` → `Untrusted`, whose `image` is that copy and whose `pid` is the fixture's. Its `Display` names both. No second core was spawned: `drain_and_wait` returns `Stopped` and the fixture exits;
    - `incompatible_core_is_drained_and_replaced` (ARCH-023): with `cfg.protocol = 2.0`, the first core exits, a second one is started, and the result is `Incompatible`;
    - `non_ascii_and_spaced_roots_work` (Review Focus 5): `TestEnv::with_label("Zoë Ünï space")`. Connect succeeds, and `env.suite_state_db()` exists;
    - `start_failure_reports_exit_code_and_logs_dir`: a corrupt suite `state.db` → `StartFailed { exit_code: Some(2), .. }`;
    - `call_mints_an_idempotency_key`: two `test.write` calls without keys both execute;
    - `blocking_client_round_trip`.
- [ ] **Step 2: Run them.**

Run: `cargo nextest run -p ww-client`
Expected: FAIL. Nothing is implemented.

- [ ] **Step 3: Implement it.**
- [ ] **Step 4: Run them again.**

Run: `cargo nextest run -p ww-client && cargo clippy -p ww-client --all-targets -- -D warnings && cargo nextest run -p ww-testkit --test dependency_boundaries`
Expected: PASS.

- [ ] **Step 5: Commit.**

```bash
git add crates/ww-client Cargo.lock
git commit -m "client: find, start, verify and handshake with the core; startup races; drain-and-replace (ARCH-018/023/027)"
```

---

### Task 14: `ww status | call | events | core stop` and the plan-2a close-out (ARCH-016, ARCH-018, ARCH-030, ARCH-055, QA-016, QA-028) — **protected: `crates/ww-cli/Cargo.toml`, `crates/ww-cli/src/main.rs`**

**Files:**
- Modify (protected): `crates/ww-cli/Cargo.toml` (add `ww-client`, `ww-protocol`; dev-dep `ww-testkit`).
- Modify (protected): `crates/ww-cli/src/main.rs`. Add `mod user;`, a `#[command(flatten)] global: user::GlobalArgs` on `Cli`, and a `#[command(flatten)] User(user::UserCommand)` variant dispatched to `user::run`. This is the only change to these files: later user commands (2b and on) go in `src/user/**`, which isn't protected.
- Create: `crates/ww-cli/src/user/{mod.rs, status.rs, call.rs, events.rs, core.rs}`.
- Test: CLI parse tests in `user/mod.rs`; `crates/ww-cli/tests/user_cli.rs`.
- Modify: `dev/rule-classification.toml`.

**Interfaces:**

Consumes:
- Task 13: `ClientConfig`, `blocking::BlockingClient`, `drain_and_wait`, `resolve_workspace_id`.

Produces:

```rust
#[derive(clap::Args)]
pub struct GlobalArgs {
    #[arg(long, global = true, value_name = "PATH")] pub workspace: Option<PathBuf>,
    #[arg(long, global = true)] pub non_interactive: bool,
    #[arg(long, global = true)] pub no_start: bool,
}
#[derive(clap::Subcommand)]
pub enum UserCommand {
    /// Show whether the core is running, and its pipe, protocol, build and workspace.
    Status { #[arg(long)] json: bool },
    /// Call a core tool: ww call <tool> [<JSON object> | -]. Prints the result as JSON.
    Call { tool: String, args: Option<String>, #[arg(long)] idempotency_key: Option<String> },
    /// Print events as JSON lines until the core goes away.
    Events { #[arg(long, required = true)] follow: bool, #[arg(long = "topic", required = true)] topics: Vec<String>,
             #[arg(long = "since", value_name = "TOPIC=SEQ")] since: Vec<String> },
    /// Manage the core process.
    #[command(subcommand)] Core(CoreCommand),
}
#[derive(clap::Subcommand)]
pub enum CoreCommand { /// Ask the core to finish its work and exit, then wait for it.
                       Stop { #[arg(long, default_value_t = 60)] timeout_secs: u64 } }
pub fn run(cmd: UserCommand, global: &GlobalArgs) -> i32;
```

**Behaviour:**
- Every command builds `ClientConfig::for_env(Env::from_process()?, CallerKind::Cli)` with:
  - `non_interactive` from the flag;
  - `start` = `NoStart` when `--no-start` is set, and always for `core stop`;
  - `workspace = resolve_workspace_id(env, --workspace, cwd)`.
- **Exit codes** follow Global Constraints.
- **`status`.** The human form prints these lines:
  - `core: running (pid <pid>, channel <channel>)`
  - `pipe: <pipe>`
  - `protocol: <protocol> (build <build_id>)`
  - `workspace: <path | none>`
  - `caller: <caller_kind>`

  `--json` prints `{"running":true,"hello":<HelloResult>}` or `{"running":false}`.
- **`call`** prints the `Outcome` as pretty JSON on stdout. The argument is `{}` by default, and `-` reads it from stdin; anything that isn't a JSON object → exit 2.
- **`events`** prints one `EventParams` JSON per line and flushes after each line.
- **`core stop`** prints `core stopped` or `core not running`.
- `ClientError`s go to stderr in plain language (exit 2). Under `--no-start`, `NotRunning` is silent (exit 0) for every command except `status`.

- [ ] **Step 1: Write the failing tests:**
  - **Parse tests:**
    - `user_commands_parse`;
    - `events_needs_follow_and_a_topic`;
    - `global_flags_work_before_and_after_the_subcommand`;
    - `dev_commands_still_parse` (`ww dev merge 3`).
  - **`tests/user_cli.rs` (`TestEnv::ww`):**
    - `call_reads_args_from_stdin_dash`;
    - `call_rejects_non_object_args_with_exit_2`;
    - `events_follow_prints_ndjson`: follow `test:a`, `ww call test.emit` from a second process, and the first stdout line parses as `EventParams` with seq 1;
    - `workspace_flag_selects_registered_default` (`status --json` reports the workspace path);
    - `untrusted_core_message_names_the_exe` (a `CoreFixture::start_exe` copy; `ww status` exits 2 and stderr contains the copy's path).
  - Remove the `cli_basics.rs` entries from `expected-fail.toml`. The list then holds no plan-2a test.
- [ ] **Step 2: Run them.**

Run: `cargo nextest run -p ww-cli user`
Expected: FAIL. Nothing is implemented.

- [ ] **Step 3: Implement it**, with the two protected edits.
- [ ] **Step 4: Classify the rules** (QA-028) in `dev/rule-classification.toml`:
  - **`test`:** every rule cited by a plan-2a test.
  - **`lint`:** ARCH-112, QA-024, QA-027.
  - **`n/a`:** ARCH-001, CORE-001, CORE-010.
- [ ] **Step 5: Run the full gate.**

Run: `cargo fmt --check && cargo clippy --workspace --all-targets -- -D warnings && cargo deny check && cargo nextest run --workspace && cargo run -q -p ww-cli -- dev spec-tests && cargo run -q -p ww-cli -- dev spec-report`
Expected:
- everything passes;
- `ww dev spec-tests` prints `28 passed, 0 expected-fail`;
- `spec-report` shows 0 dangling citations and no `test`-classified plan-2a rule without a test.

- [ ] **Step 6: Commit.**

```bash
git add crates/ww-cli dev/rule-classification.toml crates/ww-spec-tests/expected-fail.toml Cargo.lock
git commit -m "cli: ww status, call, events --follow and core stop over ww-client (ARCH-016/018)"
```

---

## Plan 2b outline: Core Platform I, workspace data (written in full after 2a merges)

Plan 2b depends on 2a's `Dispatcher`, its `RecordStage` and `SettingsStage` hooks, `EventBus`, `StateRegistry`, `store::fsops` and `ww-client`. It covers the rules listed under "Plan 2b" in the Spec section above. Its tasks, in order:

1. **Workspace-format types in `ww-schema`:**
   - lossless manifest round-trip with `preserve_order` and an extras map, using the real lantern manifest as the golden (ARCH-076, ARCH-077, CORE-148);
   - `ww://` refs that carry content hashes (ARCH-042, CORE-031, CORE-047);
   - the status machine (CORE-048);
   - comment-preserving targeted YAML edits for `worldwright.yaml` and `project.yaml`. Choose a maintained, compatible crate, or targeted line edits; the choice is recorded with tests.
2. **`store` writes:**
   - the hash check, temp file, flush, then `ReplaceFileW`/`MoveFileExW` with write-through, retrying sharing violations (ARCH-074, ARCH-078);
   - the 180-character path budget and reserved names, with long-path handling (CORE-040);
   - `ww_platform::fs` wrappers added to `clippy.toml`'s disallowed list.
3. **`store` order:** the write order output → manifest → jobs log line → index, and the orphan listing (ARCH-075, CORE-038). Locked per-machine-per-month append logs, with `seq`, torn-line repair and dedupe (ARCH-081, CORE-009).
4. **`store` areas:** staging handles on the workspace volume (CORE-003, ARCH-080, ARCH-033) and the `core.lock` read-only fallback (ARCH-028).
5. **Sealed versions:** version allocation and sign-off sealing under a per-asset lock, with read-only sealed files (CORE-026, CORE-027, CORE-028). Trash with restore records and a 30-day purge (ARCH-058, CORE-037, CORE-110). Rejected-candidate retention (CORE-032) and `legacy` folders (CORE-033).
6. **`index`:** `index.db` (WAL, schema-versioned, rebuilt on mismatch or a failed `quick_check`), reconciled by path, size, mtime, FileId and ChangeTime, fed by store events (ARCH-089, CORE-010, CORE-041). Plus the rebuild benchmark gate (CORE-042).
7. **`watch`:**
   - `ReadDirectoryChangesW`, with a full rescan on overflow;
   - staging excluded, and `work/` excluded from human-edit tracking;
   - human edits recorded, never overwritten; invalid manifests marked "needs repair" (ARCH-079, CORE-034);
   - writes paused for outside git activity, up to 60 s (ARCH-083);
   - LFS pointers marked `not_fetched` (ARCH-085);
   - the watcher fence in `ww-testkit`.

   Also `ww notify <path>`.
8. **Pipeline, projects and settings:** the settings cascade (CORE-057), the projects tools (CORE-055), `progress.md` entries (CORE-039), `has_working_doc` (CORE-049) and the active-project hint (ARCH-088).
9. **Workspace tools and the Record stage:** `assets.query`, `asset.get`, `asset.signoff`, `asset.request_signoff`, `asset.import` (copy or link-external, CORE-029), `asset.pin_input`, `project.create`, `project.set_status`, `project.status` and `project.context`. Also the Record branches with hand-off blocks (CORE-019, CORE-020, CORE-051–054), `asset:<ref>` and `project:<slug>` topics with snapshots (ARCH-044), and spec tests written first for `store` (QA-017).
10. **`ww doctor` basics and crash recovery:**
    - `ww doctor` basics: `core.longpaths`, `lfs.setlockablereadonly=false`, the `.gitignore` entries, idempotent (CORE-040, ARCH-085);
    - the crash and recovery fast subset (QA-055): fail points at every write-order step, held handles and disk full;
    - the outside-writers reconciler, both the simulated and the real-NTFS variants (QA-046).

**2b Review Focus candidates**, to be pinned when 2b is written:
- a crash between output and manifest;
- a target held open by Defender, Explorer's preview pane or an image viewer;
- a relative path near 180 characters under a deep workspace, and an absolute path past 260;
- a workspace in a OneDrive-synced or network folder, where FileId or ChangeTime is unstable or rename isn't atomic;
- `git checkout` of thousands of files overflowing the watcher buffer.

**2b protected-file edits:** new spec-test files (free), and `crates/ww-spec-tests/Cargo.toml` only if a new dev-dependency is needed. 2b needs no root `Cargo.toml` change if 2a's workspace dependencies already cover it; otherwise it adds one, and that needs approval.
