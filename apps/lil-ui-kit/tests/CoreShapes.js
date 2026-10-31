// What the core really answers and publishes, for the top bar's and the banners' tests and grabs,
// each shape checked against the type or code that makes it. A fixture a test needs that isn't
// here would be guessing, and these tests once proved a core that doesn't exist.
.pragma library

// A tool's answer (lil_protocol::outcome::ToolOutput).
function tool(structured) {
    return {status: "ok", structured: structured, artifacts: [], summary_text: ""}
}

// An `app/*` answer: the result itself (runtime.rs, Core.appCall).
function app(result) {
    return Object.assign({status: "ok"}, result)
}

// ErrorBody, with each code's category and retry as lil_protocol::codes has them.
const codes = {
    "workspace.unsupported": ["user_fixable", "after_resolve"],
    "workspace.unknown": ["user_fixable", "after_resolve"],
    "workspace.path_invalid": ["user_fixable", "after_resolve"],
    "project.unknown": ["user_fixable", "after_resolve"],
    "core.unreachable": ["transient", "same_idempotency_key"],
    "store.io": ["transient", "same_idempotency_key"],
    "caller.not_allowed": ["declined", "never"],
}

function errorBody(code, message) {
    const c = codes[code]
    return {code: code, category: c[0], message: message, trace_id: "t_0123456789abcdef", retry: c[1]}
}

function refusal(code, message) {
    return {status: "error", error: errorBody(code, message)}
}

// crates/lil-core/src/jobs/mod.rs JobSummary: its required fields, and the origin a panel's job has.
function job(id, status, over) {
    const iso = new Date(Date.now()).toISOString().replace(/\.\d+Z$/, "Z")
    return Object.assign({job_id: id, kind: "comfy.prompt", lane: "gpu-heavy", priority: "user", status: status,
                          substatus: status === "running" ? "executing" : status === "queued" ? "waiting" : null,
                          created: "2026-10-30T12:00:00Z", updated: iso, attempt: 1, trace_id: "t_" + id,
                          batch_id: "b_" + id, index: 0, origin: {surface: "panel", id: "image"}}, over || {})
}

// lil_protocol::app::QueueState.
function lane(over) {
    return Object.assign({lane: "gpu-heavy", paused: false, reasons: [], held: 0, restart_held: 0, withdrawn: 0},
                         over || {})
}

// `jobs.list`'s structured result (job_tools.rs).
function jobsPage(jobs, lanes, cursor) {
    return tool({jobs: jobs, next_cursor: cursor || null, lanes: lanes || [lane(), lane({lane: "cpu"})]})
}

// telemetry::GpuView.
function gpu(over) {
    return Object.assign({name: "GPU", driver: "1.0", util_pct: 63, power_mw: 120000, temp_c: 71, sm_clock_mhz: 2100,
                          mem_clock_mhz: 7000, throttle: [], vram_total_mib: 16384, vram_used_mib: 9728, measured: true},
                         over || {})
}

// The telemetry topic's sample (telemetry/mod.rs Snapshot): `gpu` is left out when there is none.
function sample(gpuView, ts) {
    const snapshot = {ts: ts || new Date(Date.now()).toISOString(), processes: [],
                      cpu: {util_permille: 100, per_core_permille: [], package_power_mw: 0, measured: false},
                      ram: {total_mib: 32768, used_mib: 8192}, disks: [], sizes: {}, net: [], system_power_mw: 0,
                      system_parts: [], alerts: [], gpu_users: []}
    if (gpuView)
        snapshot.gpu = gpuView
    return {kind: "sample", snapshot: snapshot}
}

// lil_protocol::app::CapRow and CapsView.
function capRow(cap, permille, over) {
    return Object.assign({cap: cap, limit: "100", settled: "1", held: "0", ratio_permille: permille}, over || {})
}

function capsView(rows) {
    return {display_currency: "USD", caps: rows || [], period: "2026-10"}
}

// The `caps` topic (meter/caps.rs) and the `usage` topic (meter/mod.rs).
function capsChanged(rows) {
    return {kind: "changed", caps: capsView(rows)}
}

function capsInvalid(names) {
    return {kind: "invalid", caps: names}
}

function capsAlert() {
    return {kind: "alert", cap: "monthly", threshold_pct: 80, spent_pct: 81, spent: "81", currency: "USD", period: "2026-10"}
}

function usageRecord() {
    return {kind: "record", record: {machine: "m", seq: 1}}
}

// lil_protocol::session::HelloResult, Core.setWorkspace's answer.
function hello(workspace) {
    return app({session_id: "s_1", instance_id: "i_1", protocol: "1.2", build_id: "0.0.0+test",
                schema_hash: "0123456789abcdef", capabilities: [], caller_kind: "ui:image", workspace: workspace,
                core: {pid: 1, channel: "test", pipe: "p"}})
}

// lil_protocol::app::SetContextResult.
function context(workspace, project, effective) {
    return app({workspace: workspace, project: project, effective_project: effective})
}

// The `workspace` topic (workspace_tools.rs).
function defaultChanged(id, path) {
    return {kind: "default_changed", id: id, path: path}
}

// The core's own words for a session whose workspace isn't the default any more (workspace.rs).
function notServed(served, requested) {
    return refusal("workspace.unsupported", "This core serves only the default workspace, \"" + served
                   + "\". To use \"" + requested + "\" instead, make it the default with `lil workspace use \"" + requested + "\"`.")
}

// `projects.list` (Appendix G).
function projectRows() {
    return [{project: "sketchbook", name: "Sketchbook", status: "active", assets: 3},
            {project: "harbor", name: "Harbor", status: "active", assets: 1234}]
}

// `workspaces.list` in M1 (workspace_tools.rs): only the default is `served`.
function workspaceRows() {
    return [{id: "ws1", path: "D:/art/harbor", name: "Harbor", default: true, served: true, missing: false},
            {id: "ws2", path: "D:/art/quay", name: "Quay", default: false, served: false, missing: false},
            {id: "ws3", path: "D:/art/other", name: "Other", default: false, served: false, missing: false},
            {id: "ws4", path: "D:/art/gone", default: false, served: false, missing: true},
            {id: "ws5", path: "D:/art/file.txt", default: false, served: false, missing: false, problem: "not_a_folder"},
            {id: "ws6", path: "D:/art/locked", default: false, served: false, missing: false, problem: "unreadable"}]
}

// Core.unavailable (runtime.rs): a hello refused over its workspace, whose message is the core's
// (workspace.rs), and any other cause.
function refusedHello(code, message) {
    return {reason: message, cause: "refused", code: code, retry: "after_resolve"}
}

function otherCause(cause, reason) {
    return {reason: reason, cause: cause}
}
