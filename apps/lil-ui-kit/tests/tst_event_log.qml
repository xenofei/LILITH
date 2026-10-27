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
        core.emit("job:j1", "event", {kind: "progress", progress: {value: 1, max: 2}})
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
        core.emit("preview:j1", "event", {format: "png", data_b64: payload})
        const shown = log.get(999).text
        verify(shown.indexOf(payload) < 0, shown)
        verify(shown.indexOf("data_b64") < 0, shown)
        verify(shown.indexOf('"format":"png"') >= 0, shown)
        verify(shown.indexOf('"bytes":72') >= 0, shown)

        // Bounded: a long event is cut.
        core.emit("things", "event", {text: "x".repeat(100000)})
        verify(log.get(999).text.length <= 2000, log.get(999).text.length)
        compare(log.rowCount(), 1000)
    }
}
