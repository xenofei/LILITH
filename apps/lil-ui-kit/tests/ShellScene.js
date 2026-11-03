// The whole shell's reads, answered as the real core answers them (each shape from CoreShapes.js or
// the panel test that checked it against the core's code), for the tests and benches that load every
// panel at once: tst_layout_full, bench_gui_stalls and bench_event_to_ui.
//
//   const scene = ShellScene.scene({jobs: [...], pending: [...]})
//   ShellScene.answerOpen(core, answered, scene)   // answers every call not yet answered; the count
//
// `answered` is the caller's {id: true} map, so a call is answered once. A call the scene has no
// answer for stays open (a write, an on-demand read), and the caller sees it in `core.calls`.
.pragma library
.import "CoreShapes.js" as Shapes

// telemetry::Snapshot with a measured GPU (tst_hardware's), `over` replacing its fields.
function hardwareSnapshot(over) {
    const cores = []
    for (let i = 0; i < 16; ++i)
        cores.push(100 + 10 * i)
    return Object.assign({
        ts: new Date(Date.now()).toISOString(),
        gpu: {name: "Test GPU", driver: "999.99", util_pct: 63, power_mw: 285000, temp_c: 71,
              sm_clock_mhz: 1905, mem_clock_mhz: 10501, fan_pct: 45, throttle: [],
              vram_total_mib: 16384, vram_used_mib: 9728, measured: true},
        processes: [],
        cpu: {util_permille: 180, per_core_permille: cores, package_power_mw: 64000, measured: true},
        ram: {total_mib: 65536, used_mib: 41984},
        disks: [{volume: "C:\\", free_mib: 122880}],
        sizes: {},
        net: [{adapter: "Ethernet", rx_bps: 1200000, tx_bps: 40000}],
        system_power_mw: 412000,
        system_parts: [{part: "gpu", mw: 285000, measured: "measured"},
                       {part: "cpu", mw: 64000, measured: "measured"},
                       {part: "rest", mw: 63000, measured: "estimated"}],
        alerts: [],
        gpu_users: []
    }, over || {})
}

// `engines.status`'s ComfyUI row (tst_diagnostics' comfy()).
function comfy(over) {
    return Object.assign({
        engine: "comfyui", class: "owned", state: "ready", url: "http://127.0.0.1:8188", pid: 4242,
        version: "0.42.1", listen: "loopback", outside_jobs: {running: 0, queued: 0},
        restart_needed: false, models_root_set: true, inventory: {models: 1234, refreshed: "2026-10-30T10:00:00Z"},
        launch_profile: {reserve_vram_gb: "1.5", preview_method: "taesd", manager: false, dynamic_vram: true,
                         flags: ["--reserve-vram", "1.5", "--preview-method", "taesd"]},
        asset_catalogue: "off"
    }, over || {})
}

// `usage.summary`'s result (meter's UsageSummary, tst_usage's summary()).
function usageSummary(rows) {
    return {
        bases: {
            money: {records: 0, estimated: 0, reconciled: 0, by_currency: {}, units: {}},
            subscription_quota: {records: 0, units: {}},
            local: {records: 0, energy_cost: {}}
        },
        by_currency: {}, display_currency: "USD", display_total: "0", fixed_costs: [],
        unattributed: {records: 0, estimated: 0, reconciled: 0, by_currency: {}, units: {}},
        rows: rows || []
    }
}

// A money row of `usage.summary` for `period`, grouped by `fields`.
function usageRow(period, fields) {
    return Object.assign({period: period, basis: "money", records: 1, estimated: 0, reconciled: 0,
                          by_currency: {USD: "1"}, units: {}}, fields)
}

// What the scene answers: every panel's first reads, `over` replacing a part.
//   jobs, lanes        the live jobs and the lanes `jobs.list` answers over (FakeCore.jobsListAnswer)
//   pending, history   `app/confirmations.list` by state
//   desktop            `desktop.get`'s DesktopSettings
//   usageRows          (args) => rows for a `usage.summary` call
//   snapshot           `telemetry.snapshot`'s answer
function scene(over) {
    return Object.assign({
        workspace: {id: "ws1", path: "D:/art/harbor"},
        projects: [{project: "sketchbook", name: "sketchbook", status: "active", assets: 12},
                   {project: "lantern", name: "lantern", status: "active", assets: 3}],
        status: Shapes.projectStatus("sketchbook", {candidate: 4, in_review: 2, stale: 1, approved: 5}),
        assets: [Shapes.asset("sketchbook", "chr_crane", "candidate", "2026-10-30T13:00:05Z"),
                 Shapes.asset("lantern", "bg_harbor", "approved", "2026-10-29T09:00:00Z")],
        jobs: [],
        lanes: [Shapes.lane(), Shapes.lane({lane: "cpu"})],
        pending: [],
        history: [],
        desktop: Shapes.desktopSettings(),
        usageRows: () => [],
        snapshot: hardwareSnapshot(),
        engines: [comfy()]
    }, over || {})
}

// The outcome the scene gives call `c`, or null for a call it leaves open.
function outcome(core, c, s) {
    const tool = Shapes.tool
    switch (c.name) {
    case "jobs.list":
        return core.jobsListAnswer(c.args, s.jobs, s.lanes)
    case "app/confirmations.list":
        return Shapes.confirmations(c.args.state === "history" ? s.history : s.pending)
    case "app/set_context":
        return Shapes.context(s.workspace, c.args.project || "sketchbook", c.args.project || "sketchbook")
    case "caps.get":
        return tool(Shapes.capsView([]))
    case "desktop.get":
        return tool(s.desktop)
    case "projects.list":
        return tool({projects: s.projects})
    case "project.status":
        return s.status
    case "assets.query":
        return Shapes.assetsPage(s.assets)
    case "workspaces.list":
        return tool({workspaces: Shapes.workspaceRows()})
    case "usage.summary":
        return tool(usageSummary(s.usageRows(c.args)))
    case "telemetry.snapshot":
        return tool(s.snapshot)
    case "telemetry.series":
        return tool({series: (c.args.metrics || []).map(m => ({metric: m, points: []}))})
    case "telemetry.energy":
        return tool({from: c.args.from, to: c.args.to, gpu_mj: 1, cpu_mj: 1, system_mj: 2,
                     measured: {gpu: true, cpu: true}})
    case "telemetry.jobs":
        return tool({jobs: []})
    case "engines.status":
        return tool({engines: s.engines})
    case "engines.environment":
        return tool({captures: [], changes: []})
    case "diagnostics.recent":
        return tool({errors: []})
    case "models.report":
        return tool({root: "D:\\comfy", refreshed: "2026-10-30T10:00:00Z", registry: "ok", models: [], missing: []})
    default:
        return null
    }
}

// Answers every open call of `core` the scene knows; how many it answered.
function answerOpen(core, answered, s) {
    let n = 0
    for (const c of core.calls) {
        if (answered[c.id])
            continue
        const out = outcome(core, c, s)
        if (out === null)
            continue
        answered[c.id] = true
        core.respond(c.id, out)
        ++n
    }
    return n
}
