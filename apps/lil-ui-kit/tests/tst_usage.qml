// The Usage & spend panel (APP-116 to APP-124, ARCH-116) on FakeCore's scripted answers: the
// month and day breakdowns by each dimension, the billing bases apart, fixed costs, currencies,
// the cap gauges and their alerts, the accounts, the Claude Code section, the CSV export (into
// this process's temporary folder only), floating and re-docking, and the accessibility walk.
import QtQuick
import QtTest
import Lilith.UiKit

TestCase {
    id: tc
    name: "Usage"
    when: windowShown
    width: 200
    height: 100

    Component {
        id: fakeComponent
        FakeCore { project: "sketchbook" }
    }

    // The panel alone in a window of the shell's size (a TestCase itself isn't visible).
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

    A11yWalk { id: walk }

    readonly property string monthKey: "2026-10"
    readonly property string dayKey: "2026-10-28"
    property var answered: ({})

    // The shell of the test before deletes its docks later (KDDockWidgets' names are
    // process-wide): let that run first.
    function init() {
        answered = {}
        wait(100)
    }

    // A FakeCore that outlives the panel made after it (never destroyed before the process ends).
    function makeCore() {
        return fakeComponent.createObject(tc)
    }

    function makePanel(core) {
        const host = createTemporaryObject(panelComponent, tc, {core: core})
        verify(host && host.panel, "UsagePanel created")
        return host.panel
    }

    function ok(structured) {
        return {status: "ok", structured: structured, artifacts: [], summary_text: ""}
    }

    // The call matching `pred` that isn't answered yet, waited for.
    function nextCall(core, pred) {
        let call = null
        tryVerify(() => (call = core.calls.find(c => pred(c) && !answered[c.id]) || null) !== null, 5000)
        return call
    }

    function answer(core, pred, outcome) {
        const call = nextCall(core, pred)
        answered[call.id] = true
        verify(core.respond(call.id, outcome), "answered " + call.name)
        return call
    }

    function isSummary(period, dims) {
        return c => c.name === "usage.summary" && c.args.period === period
                    && JSON.stringify(c.args.group_by) === JSON.stringify(dims)
    }

    function isCaps(c) {
        return c.name === "caps.get"
    }

    function summary(rows, over) {
        return Object.assign({
            bases: {
                money: {records: 0, estimated: 0, reconciled: 0, by_currency: {}},
                subscription_quota: {records: 0, units: {}},
                local: {records: 0, energy_cost: {}}
            },
            by_currency: {}, display_currency: "USD", display_total: "0", fixed_costs: [],
            unattributed: {records: 0, estimated: 0, reconciled: 0, by_currency: {}},
            rows: rows || []
        }, over || {})
    }

    function row(period, basis, fields, over) {
        const base = basis === "money" || basis === "unattributed"
                   ? {records: 1, estimated: 0, reconciled: 0, by_currency: {USD: "1"}}
                   : basis === "local" ? {records: 1, energy_cost: {}} : {records: 1, units: {}}
        return Object.assign({period: period, basis: basis}, base, fields, over || {})
    }

    function capsView(caps) {
        return {display_currency: "USD", period: monthKey, caps: caps || []}
    }

    // Answers the panel's first reads: the month and day breakdowns by `dim`, the accounts and
    // the caps.
    function load(core, dim, month, day, accounts, caps) {
        answer(core, isSummary("month", [dim]), ok(month || summary()))
        answer(core, isSummary("day", [dim]), ok(day || summary()))
        answer(core, isSummary("month", ["provider", "account"]), ok(accounts || summary()))
        answer(core, isCaps, ok(caps || capsView()))
    }

    // The visible texts under `item`.
    function texts(item) {
        const out = []
        const walkItem = i => {
            if (!i.visible)
                return
            // A Text, or a component built on one (lineCount is Text's own).
            if (typeof i.text === "string" && typeof i.lineCount === "number")
                out.push(i.text)
            for (let n = 0; n < i.children.length; ++n)
                walkItem(i.children[n])
        }
        walkItem(item)
        return out
    }

    function shows(item, s) {
        return texts(item).some(t => t.indexOf(s) >= 0)
    }

    function section(panel, period, name) {
        const s = findChild(period === "day" ? panel.day : panel.month, "section_" + name)
        verify(s, "section " + name)
        return s
    }

    function summaryCalls(core) {
        return core.calls.filter(c => c.name === "usage.summary").length
    }

    function test_month_and_day_totals_by_each_dimension() { // spec: APP-116
        const core = makeCore()
        const panel = makePanel(core)
        compare(panel.dimension, "provider", "by provider at first")
        answer(core, isSummary("month", ["provider", "account"]), ok(summary()))
        answer(core, isCaps, ok(capsView()))
        const values = {
            provider: [["openrouter", "openrouter"], ["anthropic", "anthropic"]],
            model: [["gpt-image-1", "gpt-image-1"], ["flux-dev", "flux-dev"]],
            project: [["sketchbook", "sketchbook"], ["covers", "covers"]],
            workspace: [["w_main", "w_main"], ["loose", "loose"]],
            caller_kind: [["ui:image", "You"], ["assistant", "Assistant"], ["mcp", "Claude Code"], ["cli", "lil"]],
            account: [["k_3f9a", "k_3f9a"], ["k_77c0", "k_77c0"]],
            tool: [["image.generate", "image.generate"], ["comfy.run", "comfy.run"]],
        }
        for (const dim of ["provider", "model", "project", "workspace", "caller_kind", "account", "tool"]) {
            // Provider's reads are the panel's first, beside the accounts' (counted as the one
            // before); each other dimension reads both tabs again.
            const before = dim === "provider" ? 1 : summaryCalls(core)
            panel.setDimension(dim)
            compare(panel.dimension, dim)
            const monthRows = values[dim].map(([v], i) => row(monthKey, "money", {[dim]: v}, {by_currency: {USD: String(i + 2)}}))
            const dayRows = values[dim].slice(0, 1).map(([v]) => row(dayKey, "money", {[dim]: v}))
            answer(core, isSummary("month", [dim]), ok(summary(monthRows, {display_total: "5"})))
            answer(core, isSummary("day", [dim]), ok(summary(dayRows, {display_total: "1"})))
            compare(summaryCalls(core) - before, 2, dim + ": one usage.summary per tab")
            tryVerify(() => panel.month.loaded && panel.month.shown("money").length === values[dim].length, 5000,
                      dim + ": one month row each")
            compare(panel.month.shown("money").map(r => r.label), values[dim].map(([, label]) => label))
            tryVerify(() => panel.day.shown("money").length === 1, 5000, dim + ": one day row")
            compare(panel.day.shown("money")[0].label, values[dim][0][1])
            const money = section(panel, "month", "money")
            for (const [, label] of values[dim])
                verify(shows(money, label), dim + ": " + label + " is shown")
            // The caller kinds read as people and tools, the raw kind beside.
            if (dim === "caller_kind")
                verify(shows(money, "ui:image"), "the raw caller kind beside You")
        }
        // The month tab and the day tab.
        compare(panel.tab, "month")
        verify(panel.month.visible && !panel.day.visible)
        verify(shows(panel.month, "5 USD"), "the month's total")
        panel.setTab("day")
        verify(panel.day.visible && !panel.month.visible)
        verify(shows(panel.day, "1 USD"), "the day's total")
    }

    function test_tokens_images_and_seconds_by_model() { // spec: APP-117
        const core = makeCore()
        const panel = makePanel(core)
        load(core, "provider")
        panel.setDimension("model")
        answer(core, isSummary("month", ["model"]), ok(summary([
            row(monthKey, "subscription_quota", {model: "claude-sonnet"},
                {records: 3, units: {input_tokens: 1200, output_tokens: 300, requests: 3}}),
            row(monthKey, "subscription_quota", {model: "gpt-image-1"},
                {records: 2, units: {images: 4, seconds: "12.5"}}),
            row(monthKey, "local", {model: "flux-dev"},
                {records: 5, gpu_seconds: "81.5", energy_wh_gross: "9.25", energy_cost: {USD: "0.002"}}),
        ])))
        answer(core, isSummary("day", ["model"]), ok(summary()))
        tryVerify(() => panel.month.shown("subscription_quota").length === 2, 5000)
        const quota = section(panel, "month", "subscription_quota")
        for (const s of ["claude-sonnet", "1200 input tokens", "300 output tokens", "3 requests",
                         "gpt-image-1", "4 images", "12.5 s"])
            verify(shows(quota, s), s)
        const local = section(panel, "month", "local")
        for (const s of ["flux-dev", "81.5 GPU-seconds", "9.25 Wh", "0.002 USD"])
            verify(shows(local, s), s)
    }

    function test_estimated_and_reconciled_are_marked() { // spec: APP-117
        const core = makeCore()
        const panel = makePanel(core)
        load(core, "provider", summary([
            row(monthKey, "money", {provider: "openrouter"}, {records: 3, estimated: 2, reconciled: 0}),
            row(monthKey, "money", {provider: "anthropic"}, {records: 4, estimated: 0, reconciled: 1}),
            row(monthKey, "money", {provider: "fal"}, {records: 1}),
        ], {bases: {money: {records: 8, estimated: 2, reconciled: 1, by_currency: {USD: "3"}},
                    subscription_quota: {records: 0, units: {}}, local: {records: 0, energy_cost: {}}}}))
        tryVerify(() => panel.month.shown("money").length === 3, 5000)
        const rows = panel.month.shown("money")
        compare(rows[0].marks, ["2 of 3 estimated"])
        compare(rows[1].marks, ["1 reconciled"])
        compare(rows[2].marks, [])
        const money = section(panel, "month", "money")
        verify(shows(money, "2 of 3 estimated"))
        verify(shows(money, "1 reconciled"))
        verify(shows(money, "8 records: 2 estimated, 1 reconciled"), "the basis total marks them too")
    }

    function test_unattributed_spend_is_separate() { // spec: APP-116, CORE-082
        const core = makeCore()
        const panel = makePanel(core)
        load(core, "provider", summary([
            row(monthKey, "money", {provider: "openrouter"}, {by_currency: {USD: "2"}}),
            row(monthKey, "unattributed", {provider: "comfy-org"}, {by_currency: {USD: "3"}}),
        ], {by_currency: {USD: "2"}, display_total: "2",
            unattributed: {records: 1, estimated: 0, reconciled: 0, by_currency: {USD: "3"}}}))
        tryVerify(() => panel.month.loaded, 5000)
        compare(panel.month.shown("money").map(r => r.label), ["openrouter"])
        compare(panel.month.shown("unattributed").map(r => r.label), ["comfy-org"])
        const outside = section(panel, "month", "unattributed")
        verify(shows(outside, "comfy-org"))
        verify(shows(outside, "3 USD"))
        verify(!shows(section(panel, "month", "money"), "comfy-org"), "not in the money section")
        verify(shows(section(panel, "month", "totals"), "2 USD"), "the total is the core's, without it")
        verify(!shows(section(panel, "month", "totals"), "5 USD"))
    }

    function test_billing_bases_are_never_added_together() { // spec: APP-118, CORE-081
        const core = makeCore()
        const panel = makePanel(core)
        load(core, "provider", summary([
            row(monthKey, "money", {provider: "openrouter"}, {by_currency: {USD: "4"}}),
            row(monthKey, "subscription_quota", {provider: "anthropic"}, {records: 9, units: {requests: 9}}),
            row(monthKey, "local", {provider: "local"}, {records: 2, gpu_seconds: "30"}),
        ], {by_currency: {USD: "4"}, display_total: "4",
            bases: {money: {records: 1, estimated: 0, reconciled: 0, by_currency: {USD: "4"}},
                    subscription_quota: {records: 9, units: {requests: 9}},
                    local: {records: 2, gpu_seconds: "30", energy_cost: {}}}}))
        tryVerify(() => panel.month.loaded, 5000)
        const money = section(panel, "month", "money")
        const quota = section(panel, "month", "subscription_quota")
        const local = section(panel, "month", "local")
        verify(money !== quota && quota !== local)
        verify(shows(money, "openrouter") && !shows(money, "anthropic") && !shows(money, "30 GPU-seconds"))
        verify(shows(quota, "anthropic") && shows(quota, "9 requests"))
        verify(!shows(quota, "USD"), "subscription quota is in its own units, never money")
        verify(shows(quota, "not money, and not in the spend total"))
        verify(shows(local, "30 GPU-seconds") && !shows(local, "openrouter"))
        verify(shows(section(panel, "month", "totals"), "4 USD"))
    }

    function test_fixed_costs_are_in_the_month_total() { // spec: APP-119, CORE-083
        const core = makeCore()
        const panel = makePanel(core)
        load(core, "provider",
             summary([], {display_total: "6", by_currency: {USD: "1"}, fixed_costs: [
                 {name: "storage", amount: "5", currency: "USD", display_amount: "5"},
                 {name: "plan", amount: "18", currency: "GBP", display_amount: null}]}),
             summary([], {display_total: "1", by_currency: {USD: "1"}}))
        tryVerify(() => panel.month.loaded && panel.day.loaded, 5000)
        const fixed = section(panel, "month", "fixed_costs")
        verify(fixed.visible)
        verify(shows(fixed, "storage") && shows(fixed, "5 USD"))
        verify(shows(fixed, "plan") && shows(fixed, "18 GBP"))
        verify(shows(fixed, "no rate"), "a cost that couldn't be converted says so")
        verify(shows(fixed, "Caps don't apply"))
        verify(shows(section(panel, "month", "totals"), "6 USD"))
        verify(shows(section(panel, "month", "totals"), "fixed costs included"))
        panel.setTab("day")
        verify(!section(panel, "day", "fixed_costs").visible, "a day has no fixed costs")
        verify(!shows(section(panel, "day", "totals"), "fixed costs included"))
    }

    function test_per_currency_and_display_totals() { // spec: APP-120, CORE-081
        const core = makeCore()
        const panel = makePanel(core)
        load(core, "provider",
             summary([row(monthKey, "money", {provider: "p-eur"}, {records: 2, by_currency: {EUR: "3"}})],
                     {by_currency: {EUR: "3", USD: "1"}, display_total: "9.4", unconverted: ["GBP"]}))
        tryVerify(() => panel.month.loaded, 5000)
        const totals = section(panel, "month", "totals")
        verify(shows(totals, "9.4 USD"), "the converted total, as the core gives it")
        verify(shows(totals, "3 EUR") && shows(totals, "1 USD"), "each currency's own total")
        verify(shows(totals, "GBP"), "a currency with no rate is named")
        verify(shows(section(panel, "month", "money"), "3 EUR"))
        // The display currency is the caps' own, as caps.get gives it.
        compare(panel.gauges.currency, "USD")
    }

    function test_cap_gauges_and_the_50_80_100_alerts() { // spec: APP-121, CORE-089
        const core = makeCore()
        const panel = makePanel(core)
        load(core, "provider", null, null, null, capsView([
            {cap: "monthly", limit: "10", settled: "4", held: "1", ratio_permille: 500},
            {cap: "provider:openrouter", limit: "5", settled: "0", held: "0", ratio_permille: 0, invalid: true},
        ]))
        tryVerify(() => panel.gauges.view && panel.gauges.view.caps.length === 2, 5000)
        const gauge = findChild(panel.gauges, "cap_monthly")
        verify(gauge, "the monthly gauge")
        compare(gauge.value, 0.5)
        verify(gauge.label.indexOf("4") >= 0 && gauge.label.indexOf("10 USD") >= 0, gauge.label)
        verify(!findChild(panel.gauges, "cap_provider:openrouter"), "an invalid cap has no gauge")
        const invalid = findChild(panel.gauges, "cap_provider:openrouter_invalid")
        verify(invalid && invalid.visible)
        verify(invalid.text.indexOf("invalid") >= 0, invalid.text)

        // The alerts: one line each, never stacked; a repeat adds none.
        for (const pct of [50, 80, 100])
            core.emit("caps", "event", {kind: "alert", cap: "monthly", threshold_pct: pct, spent_pct: pct,
                                        spent: String(pct / 10), currency: "USD", period: monthKey})
        core.emit("caps", "event", {kind: "alert", cap: "monthly", threshold_pct: 80, spent_pct: 85,
                                    spent: "8.5", currency: "USD", period: monthKey})
        tryVerify(() => panel.gauges.alerts.length === 3, 5000)
        const lines = findChild(panel.gauges, "alerts")
        const shown = []
        for (let i = 0; i < lines.children.length; ++i) {
            if (lines.children[i].objectName === "alert" && lines.children[i].visible)
                shown.push(lines.children[i])
        }
        compare(shown.length, 3)
        tryVerify(() => shown[0].y + shown[0].height <= shown[1].y && shown[1].y + shown[1].height <= shown[2].y,
                  2000, "one per line")
        verify(shown[0].text.indexOf("100 %") >= 0, "the newest first: " + shown[0].text)
        verify(shown[2].text.indexOf("50 %") >= 0, shown[2].text)
        verify(shown[1].text.indexOf("Monthly") >= 0, shown[1].text)
    }

    function test_caps_are_read_again_at_most_once_a_second() { // spec: APP-121
        const core = makeCore()
        const panel = makePanel(core)
        load(core, "provider")
        const capsReads = () => core.calls.filter(isCaps).length
        compare(capsReads(), 1)
        wait(1100)
        // A burst: a usage record, a caps change and a finished job.
        core.emit("usage", "event", {kind: "record", record: {provider: "p"}})
        core.emit("caps", "event", {kind: "changed", caps: capsView()})
        core.emit("jobs", "event", {kind: "job", job: {job_id: "j_1", status: "succeeded"}})
        core.emit("usage", "event", {kind: "record", record: {provider: "p"}})
        compare(capsReads(), 2, "the first of a burst reads at once")
        answer(core, isCaps, ok(capsView()))
        wait(300)
        compare(capsReads(), 2, "the rest wait for the second to pass")
        tryVerify(() => capsReads() === 3, 2000, "then one read for the rest")
        answer(core, isCaps, ok(capsView()))
        wait(1300)
        compare(capsReads(), 3, "nothing more")
        // A running job's event isn't a terminal status: no read.
        core.emit("jobs", "event", {kind: "job", job: {job_id: "j_2", status: "running"}})
        wait(100)
        compare(capsReads(), 3)
        // A usage record reads the breakdowns again too, throttled the same way.
        const before = summaryCalls(core)
        core.emit("usage", "event", {kind: "record", record: {provider: "p"}})
        tryVerify(() => summaryCalls(core) === before + 3, 2000, "month, day and accounts")
    }

    function test_resyncs_reload_and_ready_does_not() { // spec: APP-028
        const core = makeCore()
        const panel = makePanel(core)
        load(core, "provider")
        const before = core.calls.length
        core.connection = "reconnecting"
        core.connection = "ready"
        wait(200)
        compare(core.calls.length, before, "nothing reloads on ready")
        const capsReads = () => core.calls.filter(isCaps).length
        core.emit("usage", "resync_required", null)
        tryVerify(() => summaryCalls(core) === 3 + 3, 2000, "a resync reads the summaries again")
        tryVerify(() => capsReads() === 2, 2000, "and the caps, which the usage moves")
        answer(core, isCaps, ok(capsView()))
        wait(1100)
        core.emit("caps", "snapshot", null)
        tryVerify(() => capsReads() === 3, 2000, "a snapshot reads the caps again")
    }

    function test_accounts_say_tracked_only() { // spec: APP-122
        const core = makeCore()
        const panel = makePanel(core)
        load(core, "provider", null, null, summary([
            row(monthKey, "money", {provider: "openrouter", account: "k_3f9a"}),
            row(monthKey, "subscription_quota", {provider: "nanogpt", account: "k_91b2"}),
            row(monthKey, "local", {provider: "local", account: null}),
        ]))
        tryVerify(() => panel.accounts.entries.length === 2, 5000)
        compare(panel.accounts.entries.map(e => e.provider), ["openrouter", "nanogpt"])
        for (const e of panel.accounts.entries)
            compare(e.status, "tracked only")
        verify(shows(panel.accounts, "tracked only"))
        verify(shows(panel.accounts, "openrouter") && shows(panel.accounts, "nanogpt"))
    }

    function test_claude_code_section_is_empty_until_engine_a() { // spec: APP-123
        const core = makeCore()
        const panel = makePanel(core)
        load(core, "provider")
        compare(panel.claudeCode.rowCount, 0)
        verify(panel.claudeCode.visible)
        verify(shows(panel.claudeCode, "Engine A"), texts(panel.claudeCode).join(" | "))
    }

    function test_accounts_show_opaque_ids_when_no_label_exists() { // spec: APP-124
        const core = makeCore()
        const panel = makePanel(core)
        load(core, "provider", null, null, summary([
            row(monthKey, "money", {provider: "anthropic", account: "prof_01J9Z3"}),
        ]))
        tryVerify(() => panel.accounts.entries.length === 1, 5000)
        compare(panel.accounts.entries[0].shown, "prof_01J9Z3")
        verify(shows(panel.accounts, "prof_01J9Z3"))
    }

    function test_csv_export_writes_the_shown_rows() { // spec: APP-117
        const core = makeCore()
        const panel = makePanel(core)
        verify(!panel.exportButton.enabled, "nothing to export before the breakdown is in")
        load(core, "provider", summary([
            row(monthKey, "money", {provider: "openrouter"}, {records: 3, estimated: 2, by_currency: {EUR: "3", USD: "1.5"}}),
            row(monthKey, "money", {provider: "=cmd"}, {records: 1}),
            row(monthKey, "subscription_quota", {provider: "anthropic"}, {records: 2, units: {input_tokens: 900}}),
        ]))
        tryVerify(() => panel.month.loaded && panel.exportButton.enabled, 5000)
        const dir = TestFiles.newDir("usage-csv")
        const file = "file:///" + dir + "/usage.csv"
        verify(panel.exportTo(file))
        tryVerify(() => !panel.exporter.busy && panel.banner.visible, 5000)
        compare(panel.banner.kind, "ok")
        const text = TestFiles.read(dir + "/usage.csv")
        verify(text !== null, "the file is written")
        const lines = text.split("\r\n")
        compare(lines[0], "period,basis,provider,label,records,estimated,reconciled,currency,amount,input_tokens")
        compare(lines[1], "2026-10,money,openrouter,openrouter,3,2,0,EUR,3,")
        compare(lines[2], "2026-10,money,openrouter,openrouter,3,2,0,USD,1.5,")
        compare(lines[3], "2026-10,money,'=cmd,'=cmd,1,0,0,USD,1,", "a formula stays text")
        compare(lines[4], "2026-10,subscription_quota,anthropic,anthropic,2,,,,,900")
        compare(lines[5], "")
        compare(lines.length, 6)

        // A file that can't be written: the panel says so.
        TestFiles.write(dir + "/blocker", "a file, not a folder")
        verify(panel.exportTo("file:///" + dir + "/blocker/usage.csv"))
        tryVerify(() => !panel.exporter.busy && panel.banner.kind === "danger", 5000)
        verify(panel.banner.visible)
        verify(panel.banner.text.indexOf("wasn't saved") >= 0, panel.banner.text)
    }

    function test_float_and_redock_keeps_usage_state() { // spec: ARCH-116
        const core = makeCore()
        const shell = createTemporaryObject(shellComponent, tc, {core: core})
        shell.visible = true
        const dock = shell.docks.usage
        tryVerify(() => shell.docks.home.content && shell.docks.home.content.width > 0, 5000,
                  "the docks are laid out")
        tryVerify(() => dock.content && dock.content.core === core, 5000)
        const panel = dock.content
        wait(50)
        dock.setAsCurrentTab()
        tryVerify(() => panel.visible && panel.width > 0, 5000, "the usage tab is in front")
        load(core, "provider")
        panel.setDimension("model")
        answer(core, isSummary("month", ["model"]), ok(summary([row(monthKey, "money", {model: "flux-dev"})])))
        answer(core, isSummary("day", ["model"]), ok(summary()))
        panel.setTab("day")
        tryVerify(() => panel.month.shown("money").length === 1, 5000)
        const calls = core.calls.length
        dock.isFloating = true
        tryVerify(() => dock.isFloating && panel.Window.window && panel.Window.window !== shell, 5000,
                  "the panel floats in its own window")
        verify(dock.content === panel, "the same panel item")
        compare(panel.dimension, "model")
        compare(panel.tab, "day")
        compare(panel.month.shown("money")[0].label, "flux-dev")
        dock.isFloating = false
        tryVerify(() => panel.Window.window === shell, 5000, "the panel docks back")
        verify(dock.content === panel, "still the same panel item")
        compare(panel.dimension, "model")
        compare(panel.tab, "day")
        compare(panel.month.shown("money")[0].label, "flux-dev")
        compare(core.calls.length, calls, "nothing is read again")
        shell.destroy()
        wait(100)
    }

    function test_a11y_walk_passes() { // spec: APP-156
        const core = makeCore()
        const panel = makePanel(core)
        load(core, "provider", summary([
            row(monthKey, "money", {provider: "openrouter"}, {estimated: 1}),
            row(monthKey, "subscription_quota", {provider: "anthropic"}, {units: {requests: 2}}),
        ], {fixed_costs: [{name: "storage", amount: "5", currency: "USD", display_amount: "5"}]}),
             null, summary([row(monthKey, "money", {provider: "openrouter", account: "k_1"})]),
             capsView([{cap: "monthly", limit: "10", settled: "4", held: "1", ratio_permille: 500},
                       {cap: "per_call_max", limit: "1", settled: "0", held: "0", ratio_permille: 0, invalid: true}]))
        core.emit("caps", "event", {kind: "alert", cap: "monthly", threshold_pct: 50, spent_pct: 50,
                                    spent: "5", currency: "USD", period: monthKey})
        tryVerify(() => panel.month.loaded && panel.gauges.alerts.length === 1, 5000)
        compare(walk.report(panel), [])
        verify(walk.interactiveCount(panel) >= 11, "the tabs, dimensions, export and the figures: "
               + walk.interactiveCount(panel))
    }
}
