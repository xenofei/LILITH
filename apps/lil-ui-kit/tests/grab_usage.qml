// grab_usage: the Usage & spend panel offscreen, saved as <grab dir>/usage-<state>.png
// (LIL_GRAB_DIR, else CMake's build/<preset>/grabs) for the reviewer, on FakeCore's scripted
// answers: the panel alone at the shell's 1280 x 800 by provider this month and by caller today,
// and the shell's Default layout with the panel's tab in front. Each grab must have the window's
// size and not be one flat colour.
import QtQuick
import QtTest
import Lilith.UiKit

TestCase {
    id: tc
    name: "GrabUsage"
    when: windowShown
    width: 200
    height: 100

    Component {
        id: fakeComponent
        FakeCore { project: "sketchbook" }
    }

    Component {
        id: panelComponent
        Window {
            id: host
            required property var core
            readonly property alias panel: usagePanel
            width: 1280
            height: 800
            visible: true
            color: Theme.bg

            UsagePanel {
                id: usagePanel
                anchors.fill: parent
                core: host.core
                title: "Usage & spend"
            }
        }
    }

    Component {
        id: shellComponent
        ShellWindow {}
    }

    property var answered: ({})

    function init() {
        answered = {}
        wait(100)
    }

    function ok(structured) {
        return {status: "ok", structured: structured, artifacts: [], summary_text: ""}
    }

    function answer(core, pred, outcome) {
        let call = null
        tryVerify(() => (call = core.calls.find(c => pred(c) && !answered[c.id]) || null) !== null, 5000)
        answered[call.id] = true
        verify(core.respond(call.id, outcome))
    }

    function isSummary(period, dims) {
        return c => c.name === "usage.summary" && c.args.period === period
                    && JSON.stringify(c.args.group_by) === JSON.stringify(dims)
    }

    function money(period, fields, records, byCurrency, over) {
        return Object.assign({period: period, basis: "money", records: records, estimated: 0, reconciled: 0,
                              by_currency: byCurrency}, fields, over || {})
    }

    function month(dim) {
        const p = "2026-10"
        const rows = dim === "caller_kind" ? [
            money(p, {caller_kind: "ui:image"}, 14, {USD: "3.42"}),
            money(p, {caller_kind: "assistant"}, 6, {USD: "1.10"}, {estimated: 2}),
            money(p, {caller_kind: "mcp"}, 9, {USD: "0.86", EUR: "1.20"}),
            money(p, {caller_kind: "cli"}, 2, {USD: "0.04"}),
        ] : [
            money(p, {provider: "openrouter"}, 18, {USD: "2.96"}, {estimated: 3}),
            money(p, {provider: "fal"}, 11, {USD: "2.46", EUR: "1.20"}, {reconciled: 4}),
            {period: p, basis: "subscription_quota", provider: "nanogpt", records: 40,
             units: {input_tokens: 182000, output_tokens: 31500, requests: 40}},
            {period: p, basis: "local", provider: "local", records: 23, gpu_seconds: "1840.5",
             energy_wh_gross: "212.4", energy_cost: {USD: "0.06"}},
            {period: p, basis: "unattributed", provider: "comfy-org", records: 2, estimated: 0, reconciled: 0,
             by_currency: {USD: "0.80"}},
        ]
        return {
            bases: {money: {records: 29, estimated: 3, reconciled: 4, by_currency: {EUR: "1.20", USD: "5.42"}},
                    subscription_quota: {records: 40, units: {input_tokens: 182000, output_tokens: 31500, requests: 40}},
                    local: {records: 23, gpu_seconds: "1840.5", energy_wh_gross: "212.4", energy_cost: {USD: "0.06"}}},
            by_currency: {EUR: "1.20", USD: "5.42"}, display_currency: "USD", display_total: "16.71",
            fixed_costs: [{name: "Backup storage", amount: "4", currency: "USD", display_amount: "4"},
                          {name: "Claude plan", amount: "18", currency: "GBP", display_amount: null}],
            unconverted: ["GBP"],
            unattributed: {records: 2, estimated: 0, reconciled: 0, by_currency: {USD: "0.80"}},
            rows: rows
        }
    }

    function day(dim) {
        const p = "2026-10-28"
        const rows = dim === "caller_kind"
            ? [money(p, {caller_kind: "ui:image"}, 3, {USD: "0.62"}), money(p, {caller_kind: "mcp"}, 1, {USD: "0.08"})]
            : [money(p, {provider: "openrouter"}, 4, {USD: "0.70"})]
        return {
            bases: {money: {records: 4, estimated: 0, reconciled: 0, by_currency: {USD: "0.70"}},
                    subscription_quota: {records: 0, units: {}}, local: {records: 0, energy_cost: {}}},
            by_currency: {USD: "0.70"}, display_currency: "USD", display_total: "0.70", fixed_costs: [],
            unattributed: {records: 0, estimated: 0, reconciled: 0, by_currency: {}}, rows: rows
        }
    }

    function accounts() {
        const p = "2026-10"
        return {rows: [money(p, {provider: "openrouter", account: "k_3f9a1c"}, 18, {USD: "2.96"}),
                       money(p, {provider: "fal", account: "k_77c02e"}, 11, {USD: "2.46"}),
                       {period: p, basis: "subscription_quota", provider: "nanogpt", account: "k_91b2d4", records: 40, units: {}}]}
    }

    function caps() {
        return {display_currency: "USD", period: "2026-10", caps: [
            {cap: "monthly", limit: "20", settled: "16.71", held: "0.40", ratio_permille: 855},
            {cap: "provider:openrouter", limit: "5", settled: "2.96", held: "0", ratio_permille: 592},
            {cap: "per_call_max", limit: "x", settled: "0", held: "0", ratio_permille: 0, invalid: true}]}
    }

    function load(core, dim) {
        answer(core, isSummary("month", [dim]), ok(month(dim)))
        answer(core, isSummary("day", [dim]), ok(day(dim)))
        answer(core, isSummary("month", ["provider", "account"]), ok(accounts()))
        answer(core, c => c.name === "caps.get", ok(caps()))
        for (const pct of [50, 80])
            core.emit("caps", "event", {kind: "alert", cap: "monthly", threshold_pct: pct, spent_pct: pct + 3,
                                        spent: String((pct + 3) / 5), currency: "USD", period: "2026-10"})
    }

    function distinctColours(img) {
        const seen = {}
        let n = 0
        for (let gy = 0; gy < 32; ++gy) {
            for (let gx = 0; gx < 32; ++gx) {
                const key = String(img.pixel(Math.floor((gx + 0.5) * img.width / 32),
                                             Math.floor((gy + 0.5) * img.height / 32)))
                if (!seen[key]) {
                    seen[key] = true
                    ++n
                }
            }
        }
        return n
    }

    function save(win, item, state) {
        wait(300)
        compare(win.width, 1280)
        const img = grabImage(item)
        const dpr = win.devicePixelRatio
        compare(img.width, Math.round(win.width * dpr))
        compare(img.height, Math.round(win.height * dpr))
        verify(distinctColours(img) >= 4, "not one flat colour")
        const dir = TestFiles.env("LIL_GRAB_DIR") || TestFiles.env("LIL_GRAB_DEFAULT_DIR")
        verify(dir !== "", "a grab folder")
        verify(TestFiles.mkpath(dir))
        img.save(dir + "/usage-" + state + ".png")
        verify(TestFiles.list(dir).indexOf("usage-" + state + ".png") >= 0)
    }

    function test_grab_usage_month() { // spec: APP-116, APP-118, APP-119, APP-120, APP-121, APP-122, APP-123
        const core = fakeComponent.createObject(tc)
        const host = createTemporaryObject(panelComponent, tc, {core: core})
        load(core, "provider")
        tryVerify(() => host.panel.month.loaded && host.panel.gauges.alerts.length === 2, 5000)
        save(host, host.contentItem, "month")
    }

    function test_grab_usage_day_by_caller() { // spec: APP-116
        const core = fakeComponent.createObject(tc)
        const host = createTemporaryObject(panelComponent, tc, {core: core})
        load(core, "provider")
        host.panel.setDimension("caller_kind")
        answer(core, isSummary("month", ["caller_kind"]), ok(month("caller_kind")))
        answer(core, isSummary("day", ["caller_kind"]), ok(day("caller_kind")))
        host.panel.setTab("day")
        tryVerify(() => host.panel.day.loaded && host.panel.day.shown("money").length === 2, 5000)
        save(host, host.contentItem, "day-by-caller")
    }

    function test_grab_usage_docked() { // spec: APP-004, APP-116
        const core = fakeComponent.createObject(tc)
        const shell = createTemporaryObject(shellComponent, tc, {core: core})
        shell.visible = true
        tryVerify(() => shell.docks.usage.content && shell.docks.home.content && shell.docks.home.content.width > 0, 5000)
        wait(50)
        shell.docks.usage.setAsCurrentTab()
        load(core, "provider")
        tryVerify(() => shell.docks.usage.content.visible && shell.docks.usage.content.month.loaded, 5000)
        save(shell, shell.contentItem, "docked")
        shell.destroy()
        wait(100)
    }
}
