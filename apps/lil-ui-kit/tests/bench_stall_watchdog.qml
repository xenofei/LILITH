// The GUI-thread stall watchdog (APP-135; spike 10 §8): a 300 ms block of the GUI thread is at
// least one gui_stall (over 50 ms) and one gui_stall_stack (past 250 ms). Label qml (a check of
// behaviour, not a budget; the PR tier runs it), offscreen. The stall count is "at least one":
// a shared runner's own hiccup of over 50 ms in the window must not fail a PR; the stack, past
// 250 ms, is exactly one.
import QtQuick
import QtTest
import Lilith.Shim

TestCase {
    name: "StallWatchdog"

    function test_a_300_ms_block_is_at_least_one_stall_and_one_stack() { // spec: APP-135
        LilShim.startStallWatchdog()
        wait(300)
        const before = LilShim.stallCounts()
        const start = Date.now()
        while (Date.now() - start < 300) {
            // Blocks the GUI thread.
        }
        wait(300)
        const after = LilShim.stallCounts()
        verify(after.stalls - before.stalls >= 1, "at least one gui_stall")
        compare(after.stacks - before.stacks, 1, "one gui_stall_stack")
    }
}
