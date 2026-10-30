// The cost and time chip (APP-140, CORE-161): one job.estimate 300 ms after the last change of
// its call, its text from the estimate's parts (JobText), "no estimate yet" without one, and the
// cap share from a caps.get view.
import QtQuick
import QtTest
import Lilith.UiKit

TestCase {
    id: tc
    name: "CostChip"

    Component {
        id: fakeComponent
        FakeCore {}
    }

    Component {
        id: chipComponent
        CostChip {}
    }

    readonly property var localEstimate: ({
        local: {source: "measured_history", samples: 4, gpu_s_p50: "8.0", gpu_s_p90: "10.0",
                wh_p50: "2.000", wh_p90: "2.400"},
        wall_s: {source: "measured_history", samples: 4, p50: "9.0", p90: "12.0"},
        unknown: []
    })

    function estimateCalls(core) {
        return core.calls.filter(c => c.name === "job.estimate")
    }

    function test_debounced_estimate_call() { // spec: APP-140, CORE-161
        const core = createTemporaryObject(fakeComponent, tc)
        const chip = createTemporaryObject(chipComponent, tc, {core: core, tool: "test.job",
                                                               args: {steps: 1}})
        compare(chip.text, "no estimate yet", "nothing estimated yet")
        // Three more changes, 100 ms apart: none is asked for while they come.
        for (const steps of [2, 3, 4]) {
            wait(100)
            chip.args = {steps: steps}
            compare(estimateCalls(core).length, 0, "no call while the call changes")
        }
        const last = Date.now()
        wait(200)
        compare(estimateCalls(core).length, 0, "not before 300 ms after the last change")
        tryVerify(() => estimateCalls(core).length === 1, 1000, "one call")
        verify(Date.now() - last >= 290, "300 ms after the last change: " + (Date.now() - last))
        compare(estimateCalls(core)[0].args, {tool: "test.job", args: {steps: 4}}, "for the last call")
        core.respond(estimateCalls(core)[0].id, {status: "ok", structured: localEstimate,
                                                 artifacts: [], summary_text: ""})
        compare(chip.text, "local · ~8 s · ~2 Wh")
        wait(400)
        compare(estimateCalls(core).length, 1, "and only one")

        // A change while a call is out: the old answer is dropped, a new call follows.
        chip.args = {steps: 5}
        tryVerify(() => estimateCalls(core).length === 2, 1000)
        chip.tool = "comfy.prompt"
        verify(core.cancelled.indexOf(estimateCalls(core)[1].id) >= 0, "the stale call is dropped")
        tryVerify(() => estimateCalls(core).length === 3, 1000)
        core.respond(estimateCalls(core)[2].id, {status: "error", error: {code: "tool.unknown",
                     category: "invalid_input", message: "no such tool", trace_id: "t", retry: "never"}})
        compare(chip.text, "no estimate yet", "a failed estimate is none")
    }

    function test_a_given_estimate_needs_no_call() { // spec: APP-140
        const core = createTemporaryObject(fakeComponent, tc)
        const money = {money: {source: "prices_yaml", p50: "0.04", p90: "0.05", upper_bound: "0.06",
                               currency: "USD"}, unknown: []}
        const chip = createTemporaryObject(chipComponent, tc, {core: core, estimate: money})
        compare(chip.text, "≈ $0.04")
        chip.caps = {display_currency: "USD", period: "2026-10",
                     caps: [{cap: "monthly", limit: "1.33", settled: "0", held: "0", ratio_permille: 0}]}
        compare(chip.text, "≈ $0.04 · 3 % of cap", "the share of the monthly cap")
        chip.estimate = null
        compare(chip.text, "no estimate yet")
        wait(400)
        compare(estimateCalls(core).length, 0, "no call without a tool")
        compare(chip.Accessible.name, "Estimate: no estimate yet")
    }
}
