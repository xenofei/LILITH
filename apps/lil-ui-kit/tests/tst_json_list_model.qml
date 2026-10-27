// JsonListModel (APP-025): pages a list tool through cursor / next_cursor, upserts a row by its
// id from the topic's events (a nested row of the event kind it accepts, or the event's data
// itself), and reloads on a snapshot or resync_required, dropping the topic's events until the
// reload is in.
import QtQuick
import QtQml.Models
import QtTest
import Lilith.Core

TestCase {
    id: tc
    name: "JsonListModel"

    Component {
        id: fakeComponent
        FakeCore {}
    }

    // The jobs list as Tasks 20-22 use it: `jobs` events are {kind: "job", job} or
    // {kind: "queue", state}.
    Component {
        id: jobsComponent
        JsonListModel {
            tool: "jobs.list"
            argsJson: JSON.stringify({status: "running"})
            listKey: "jobs"
            idKey: "job_id"
            pageSize: 2
            topic: "jobs"
            eventKind: "job"
            rowKey: "job"
            roleNames: ["job_id", "status"]
        }
    }

    // A list whose events are its rows.
    Component {
        id: plainComponent
        JsonListModel {
            tool: "test.things"
            listKey: "things"
            idKey: "id"
            topic: "things"
            roleNames: ["id", "label"]
        }
    }

    Component {
        id: viewComponent
        Instantiator {
            delegate: QtObject {
                required property string job_id
                required property string status
            }
        }
    }

    Component {
        id: spyComponent
        SignalSpy {}
    }

    function spy(target, signalName) {
        return createTemporaryObject(spyComponent, tc, {target: target, signalName: signalName})
    }

    function job(id, status) {
        return {job_id: id, status: status, updated: "2026-10-27T12:00:00Z"}
    }

    function page(rows, next) {
        return {status: "ok", structured: {jobs: rows, next_cursor: next === undefined ? null : next},
                artifacts: [], summary_text: ""}
    }

    // The fake and a jobs model on it, its first page asked for.
    function loadedJobs(rows, next) {
        const core = createTemporaryObject(fakeComponent, tc)
        const model = createTemporaryObject(jobsComponent, tc, {core: core})
        tryVerify(() => core.calls.length === 1)
        core.respond(core.calls[0].id, page(rows, next))
        verify(model.loaded)
        return {core: core, model: model}
    }

    function test_fetch_more_pages_through_the_cursor() {
        const core = createTemporaryObject(fakeComponent, tc)
        const model = createTemporaryObject(jobsComponent, tc, {core: core})
        // The topic is followed before the first page is asked for, so no event is missed.
        tryVerify(() => core.calls.length === 1)
        compare(core.subscribed, ["jobs"])
        compare(core.calls[0].name, "jobs.list")
        compare(core.calls[0].args, {status: "running", limit: 2})
        verify(!model.loaded)
        verify(!model.canFetchMore(model.index(-1, 0)), "one page at a time")

        core.respond(core.calls[0].id, page([job("j4", "running"), job("j3", "running")], "c1"))
        verify(model.loaded)
        compare(model.rowCount(), 2)
        compare(model.get(0).job_id, "j4")
        compare(model.get(1).status, "running")

        verify(model.canFetchMore(model.index(-1, 0)))
        model.fetchMore(model.index(-1, 0))
        compare(core.calls.length, 2)
        compare(core.calls[1].args, {status: "running", limit: 2, cursor: "c1"})
        model.fetchMore(model.index(-1, 0))
        compare(core.calls.length, 2, "never two pages at once")
        core.respond(core.calls[1].id, page([job("j2", "running"), job("j1", "running")], "c2"))
        compare(model.rowCount(), 4)
        compare(model.get(3).job_id, "j1")

        model.fetchMore(model.index(-1, 0))
        core.respond(core.calls[2].id, page([job("j0", "running")], null))
        compare(model.rowCount(), 5)
        verify(!model.canFetchMore(model.index(-1, 0)), "next_cursor null: the last page")

        // The roles a delegate reads.
        const view = createTemporaryObject(viewComponent, tc, {model: model})
        compare(view.count, 5)
        compare(view.objectAt(0).job_id, "j4")
        compare(view.objectAt(4).status, "running")

        // Gone with a page asked for: the request is cancelled, the topic left.
        model.reload()
        tryVerify(() => core.calls.length === 4)
        const open = core.calls[3].id
        model.destroy()
        tryVerify(() => core.cancelled.length === 1)
        compare(core.cancelled[0], open)
        compare(core.subscriberCount("jobs"), 0)
    }

    function test_topic_upsert_updates_one_row() {
        const s = loadedJobs([job("j2", "running"), job("j1", "queued")])
        const core = s.core, model = s.model
        const view = createTemporaryObject(viewComponent, tc, {model: model})
        const changed = spy(model, "dataChanged")
        const inserted = spy(model, "rowsInserted")
        const reset = spy(model, "modelReset")

        core.emit("jobs", "event", {kind: "job", job: {job_id: "j1", status: "running", updated: "2026-10-27T12:00:01Z"}})
        compare(changed.count, 1)
        compare(changed.signalArguments[0][0].row, 1)
        compare(changed.signalArguments[0][1].row, 1, "one row")
        compare(inserted.count + reset.count, 0)
        compare(model.get(1).status, "running")
        compare(view.objectAt(1).status, "running")
        compare(model.get(0).status, "running")

        // The other kind on the topic is skipped; so is an event older than the row.
        core.emit("jobs", "event", {kind: "queue", state: {job_id: "j1", status: "failed"}})
        core.emit("jobs", "event", {kind: "job", job: {job_id: "j1", status: "queued", updated: "2026-10-27T11:59:00Z"}})
        compare(changed.count, 1)
        compare(model.get(1).status, "running")
        // Another topic's events are not this model's.
        core.emit("job:j1", "event", {kind: "status", job: {job_id: "j1", status: "failed"}})
        compare(changed.count, 1)

        // A new job goes first.
        core.emit("jobs", "event", {kind: "job", job: job("j3", "queued")})
        compare(inserted.count, 1)
        compare(model.rowCount(), 3)
        compare(model.get(0).job_id, "j3")
        compare(view.objectAt(0).job_id, "j3")

        // With no event kind or row key, the event's data is the row.
        const plain = createTemporaryObject(plainComponent, tc, {core: core})
        tryVerify(() => core.calls.length === 2)
        core.respond(core.calls[1].id, {status: "ok", structured: {things: [{id: "a", label: "A"}], next_cursor: null},
                                        artifacts: [], summary_text: ""})
        core.emit("things", "event", {id: "a", label: "A2"})
        compare(plain.get(0).label, "A2")
        compare(plain.rowCount(), 1)
        compare(model.rowCount(), 3, "the jobs model is untouched")
    }

    function reloadsOn(kind) {
        const s = loadedJobs([job("j2", "running"), job("j1", "queued")], "c1")
        const core = s.core, model = s.model
        const reset = spy(model, "modelReset")
        // A next page asked for: the reload gives it up.
        model.fetchMore(model.index(-1, 0))
        compare(core.calls.length, 2)
        const nextPage = core.calls[1].id

        core.emit("jobs", kind, kind === "snapshot" ? {jobs: []} : null)
        verify(!model.loaded, "writes wait for the reload")
        verify(core.cancelled.indexOf(nextPage) >= 0, "the old page is given up")
        compare(core.calls.length, 3)
        compare(core.calls[2].args, {status: "running", limit: 2}, "from the first page again")
        compare(model.rowCount(), 2, "the old rows stay until the reload is in")

        // Until it is in, the topic's events are dropped.
        core.emit("jobs", "event", {kind: "job", job: {job_id: "j1", status: "failed", updated: "2026-10-27T13:00:00Z"}})
        compare(model.get(1).status, "queued")
        // The given-up page's answer is nobody's.
        core.respond(nextPage, page([job("j0", "queued")], null))
        compare(model.rowCount(), 2)

        core.respond(core.calls[2].id, page([job("j5", "succeeded")], null))
        verify(model.loaded)
        compare(reset.count, 1)
        compare(model.rowCount(), 1)
        compare(model.get(0).job_id, "j5")
        verify(!model.canFetchMore(model.index(-1, 0)))
        compare(core.subscriberCount("jobs"), 1, "still followed once")

        core.emit("jobs", "event", {kind: "job", job: {job_id: "j5", status: "failed", updated: "2026-10-27T13:00:00Z"}})
        compare(model.get(0).status, "failed", "applied again once loaded")
    }

    function test_snapshot_reloads() {
        reloadsOn("snapshot")
    }

    function test_resync_required_reloads() {
        reloadsOn("resync_required")
    }
}
