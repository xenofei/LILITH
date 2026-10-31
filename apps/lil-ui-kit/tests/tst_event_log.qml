// EventLogModel (Diagnostics' event log): the newest 1,000 events of every topic the app
// follows, fed from the core object's coreEvent without a subscription of its own, bounded in
// memory, with no image bytes.
import QtQuick
import QtTest
import Lilith.Core

TestCase {
    id: tc
    name: "EventLog"

    Component {
        id: fakeComponent
        FakeCore {}
    }

    Component {
        id: logComponent
        EventLogModel {}
    }

    function test_keeps_the_newest_1000() {
        const core = createTemporaryObject(fakeComponent, tc)
        const log = createTemporaryObject(logComponent, tc, {core: core})
        compare(log.rowCount(), 0)
        for (let i = 0; i < 1003; ++i)
            core.emit("jobs", "event", {kind: "job", job: {job_id: "j" + i, status: "queued"}})
        core.emit("job:j1", "event", {kind: "retrying", attempt: 2, of: 3})
        core.emit("confirmations", "resync_required", null)
        compare(log.rowCount(), 1000)
        // Oldest first: the first 5 of 1,005 are gone.
        compare(log.get(0).seq, 6)
        compare(log.get(0).topic, "jobs")
        verify(log.get(0).text.indexOf('"job_id":"j5"') >= 0, log.get(0).text)
        compare(log.get(998).topic, "job:j1")
        compare(log.get(998).kind, "event")
        compare(log.get(999).topic, "confirmations")
        compare(log.get(999).kind, "resync_required")
        verify(log.get(999).receivedMs > 0)
        compare(core.subscribed, [], "no subscription of its own")

        // No image bytes: a preview event shows its format and size.
        const payload = "iVBORw0KGgoAAAANSUhEUgAAAAIAAAABCAIAAAB7QOjdAAAAD0lEQVR4nGP4z8Dwn4EBAAj+Af9IxWaQAAAAAElFTkSuQmCC"
        core.emit("things", "event", {format: "png", data_b64: payload})
        const shown = log.get(999).text
        verify(shown.indexOf(payload) < 0, shown)
        verify(shown.indexOf("data_b64") < 0, shown)
        verify(shown.indexOf('"format":"png"') >= 0, shown)
        verify(shown.indexOf('"bytes":72') >= 0, shown)

        // At any depth (review M-8).
        core.emit("things", "event", {frames: [{format: "png", data_b64: payload}], at: {data_b64: payload}})
        const nested = log.get(999).text
        verify(nested.indexOf(payload) < 0, nested)
        verify(nested.indexOf("data_b64") < 0, nested)
        compare(nested.split('"bytes":72').length, 3, nested)

        // Bounded: a long event is cut.
        core.emit("things", "event", {text: "x".repeat(100000)})
        verify(log.get(999).text.length <= 2000, log.get(999).text.length)
        compare(log.rowCount(), 1000)
    }

    // A job's `status` is the same summary as its `jobs` event (the core publishes one value on both):
    // an ordinary batch of about 100 images must not clear what came before it. Each job here is what
    // a real one sends: four changes on both topics and a queue event, and its progress.
    function test_a_batch_of_111_keeps_the_early_error() { // spec: APP-134
        const core = createTemporaryObject(fakeComponent, tc)
        const log = createTemporaryObject(logComponent, tc, {core: core})
        core.emit("diagnostics", "event", {kind: "error", code: "store.io", tool: "asset.get", at: "2026-10-30T10:00:00.000Z"})
        core.emit("engines", "event", {kind: "status", engine: {engine: "comfyui", state: "ready"}})
        for (let i = 0; i < 111; ++i) {
            const id = "j_" + i
            for (const status of ["queued", "running", "running", "succeeded"]) {
                const job = {job_id: id, status: status}
                core.emit("jobs", "event", {kind: "job", job: job})
                core.emit("job:" + id, "event", {kind: "status", job: job})
            }
            core.emit("job:" + id, "event", {kind: "progress", value: 1, max: 2})
            core.emit("jobs", "event", {kind: "queue", state: {lane: "gpu-heavy", paused: false, reasons: [], held: 0, restart_held: 0, withdrawn: 0}})
        }
        verify(log.get(0).text.indexOf("store.io") >= 0, "the error is still the first row: " + log.get(0).text)
        compare(log.rowCount(), 2 + 111 * 5, "each job's four `jobs` events and its queue event")
    }

    // What repeats state at a high rate is left out, whatever follows it; everything else stays.
    function test_heartbeats_stay_out() { // spec: APP-134
        const core = createTemporaryObject(fakeComponent, tc)
        const log = createTemporaryObject(logComponent, tc, {core: core})
        core.emit("diagnostics", "event", {kind: "error", code: "store.io", at: "2026-10-30T10:00:00Z"})
        for (let i = 0; i < 1000; ++i) {
            core.emit("telemetry", "event", {kind: "sample", snapshot: {ts: "2026-10-30T10:00:00Z"}})
            core.emit("job:j1", "event", {kind: "progress", value: i, max: 1000})
            core.emit("job:j1", "event", {kind: "status", job: {job_id: "j1", status: "running"}})
            core.emit("preview:j1", "event", {format: "png", data_b64: "AAAA"})
        }
        compare(log.rowCount(), 1, "1,000 of each push nothing out")
        verify(log.get(0).text.indexOf("store.io") >= 0)

        // The same topics' other events, and any topic's session marks.
        const kept = [["telemetry", "event", {kind: "alert", alert: {id: "a"}}],
                      ["telemetry", "event", {kind: "alert_cleared", id: "a"}],
                      ["telemetry", "resync_required", null],
                      ["telemetry", "snapshot", {ts: "2026-10-30T10:00:00Z"}],
                      ["jobs", "event", {kind: "job", job: {job_id: "j1", status: "failed"}}],
                      ["job:j1", "event", {kind: "retrying"}],
                      ["job:j1", "event", {kind: "watchdog"}],
                      ["job:j1", "resync_required", null],
                      ["preview:j1", "resync_required", null],
                      ["engines", "event", {kind: "status", engine: {engine: "comfyui"}}],
                      ["desktop", "event", {kind: "resumed", slept_from: "x", at: "y"}]]
        for (const [topic, kind, data] of kept)
            core.emit(topic, kind, data)
        compare(log.rowCount(), 1 + kept.length)
        // An event whose data doesn't read as a kind stays too.
        core.emit("telemetry", "event", "not an object")
        compare(log.rowCount(), 2 + kept.length)
    }
}
