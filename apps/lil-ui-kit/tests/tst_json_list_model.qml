// JsonListModel (APP-025): pages a list tool through cursor / next_cursor, upserts a row by its
// id from the topic's events (a nested row of the event kind it accepts, or the event's data
// itself), reloads on a snapshot or resync_required, holding the topic's events until the reload
// is in and applying them through the rows' updated guard, and asks a failed page again (review
// C-1, I-1).
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
            argsJson: JSON.stringify({kind: "comfyui.workflow"})
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

    function job(id, status, updated) {
        return {job_id: id, status: status, updated: updated || "2026-10-27T12:00:00Z"}
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
        // The topic is asked for before the first page.
        tryVerify(() => core.calls.length === 1)
        compare(core.subscribed, ["jobs"])
        compare(core.calls[0].name, "jobs.list")
        compare(core.calls[0].args, {kind: "comfyui.workflow", limit: 2})
        verify(!model.loaded)
        verify(!model.canFetchMore(model.index(-1, 0)), "one page at a time")

        core.respond(core.calls[0].id, page([job("j4", "running"), job("j3", "running")], "c1"))
        verify(model.loaded)
        compare(model.rowCount(), 2)
        compare(model.get(0).job_id, "j4")
        compare(model.get(1).status, "running")

        // A live event brings j2 before the page that holds it.
        core.emit("jobs", "event", {kind: "job", job: job("j2", "queued")})
        compare(model.rowCount(), 3)
        compare(model.get(0).job_id, "j2")

        verify(model.canFetchMore(model.index(-1, 0)))
        model.fetchMore(model.index(-1, 0))
        compare(core.calls.length, 2)
        compare(core.calls[1].args, {kind: "comfyui.workflow", limit: 2, cursor: "c1"})
        model.fetchMore(model.index(-1, 0))
        compare(core.calls.length, 2, "never two pages at once")
        core.respond(core.calls[1].id, page([job("j2", "running"), job("j1", "running")], "c2"))
        compare(model.rowCount(), 4, "the row the page holds already is updated, not added twice")
        compare(model.get(0).job_id, "j2")
        compare(model.get(0).status, "running")
        compare(model.get(3).job_id, "j1")

        model.fetchMore(model.index(-1, 0))
        core.respond(core.calls[2].id, page([job("j0", "running")], null))
        compare(model.rowCount(), 5)
        verify(!model.canFetchMore(model.index(-1, 0)), "next_cursor null: the last page")

        // The roles a delegate reads.
        const view = createTemporaryObject(viewComponent, tc, {model: model})
        compare(view.count, 5)
        compare(view.objectAt(0).job_id, "j2")
        compare(view.objectAt(1).job_id, "j4")
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

        // Another core: the old one's topic is left, and the model loads from the new one.
        const other = createTemporaryObject(fakeComponent, tc)
        model.core = other
        compare(core.subscriberCount("jobs"), 0)
        tryVerify(() => other.calls.length === 1)
        compare(other.subscribed, ["jobs"])
        other.respond(other.calls[0].id, page([job("j9", "queued")]))
        compare(model.rowCount(), 1)
        compare(model.get(0).job_id, "j9")
        core.emit("jobs", "event", {kind: "job", job: job("j8", "queued")})
        compare(model.rowCount(), 1, "the old core's events are not the model's")
        other.emit("jobs", "event", {kind: "job", job: job("j8", "queued")})
        compare(model.rowCount(), 2)
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
        compare(core.calls[2].args, {kind: "comfyui.workflow", limit: 2}, "from the first page again")
        compare(model.rowCount(), 2, "the old rows stay until the reload is in")

        // Until it is in, the topic's events are held, not applied: one newer than the page's
        // row (the job finished after the core read the page), one older.
        core.emit("jobs", "event", {kind: "job", job: job("j5", "succeeded", "2026-10-27T13:00:00Z")})
        core.emit("jobs", "event", {kind: "job", job: job("j4", "queued", "2026-10-27T12:00:00Z")})
        compare(model.get(1).status, "queued")
        compare(model.rowCount(), 2)
        // The given-up page's answer is nobody's.
        core.respond(nextPage, page([job("j0", "queued")], null))
        compare(model.rowCount(), 2)

        core.respond(core.calls[2].id, page([job("j5", "running", "2026-10-27T12:30:00Z"),
                                             job("j4", "running", "2026-10-27T12:30:00Z")], null))
        verify(model.loaded)
        compare(reset.count, 1)
        compare(model.rowCount(), 2)
        compare(model.get(0).job_id, "j5")
        compare(model.get(0).status, "succeeded", "the newer event survives the page")
        compare(model.get(1).status, "running", "the older one doesn't")
        verify(!model.canFetchMore(model.index(-1, 0)))
        compare(core.subscriberCount("jobs"), 1, "still followed once")

        core.emit("jobs", "event", {kind: "job", job: job("j5", "failed", "2026-10-27T13:00:01Z")})
        compare(model.get(0).status, "failed", "applied at once when loaded")
        return {core: core, model: model}
    }

    function test_snapshot_reloads() {
        reloadsOn("snapshot")
    }

    function test_resync_required_reloads() {
        const s = reloadsOn("resync_required")
        const core = s.core, model = s.model
        // Past 1,024 events held during a load, they are dropped and the model loads once more.
        model.reload()
        const calls = core.calls.length
        for (let i = 0; i < 1025; ++i)
            core.emit("jobs", "event", {kind: "job", job: job("x" + i, "queued")})
        compare(core.calls.length, calls + 1)
        core.respond(core.calls[calls].id, page([job("j5", "failed")]))
        compare(model.rowCount(), 1, "the dropped events are not applied")
    }

    // Review C-1: a failed page is asked again, after 1 s, then longer; a page that comes in
    // clears the error.
    function test_a_failed_page_is_asked_again() {
        const core = createTemporaryObject(fakeComponent, tc)
        const model = createTemporaryObject(jobsComponent, tc, {core: core})
        tryVerify(() => core.calls.length === 1)
        verify(!model.error)
        core.respond(core.calls[0].id, {status: "error", error: {code: "core.unreachable", message: "no core"}})
        verify(!model.loaded)
        compare(model.error.code, "core.unreachable")
        // Asked again after about a second, with the same arguments.
        wait(500)
        compare(core.calls.length, 1, "not at once")
        tryVerify(() => core.calls.length === 2, 3000)
        compare(core.calls[1].args, core.calls[0].args)
        // A second failure waits longer (2 s).
        core.respond(core.calls[1].id, {status: "error", error: {code: "core.busy", message: "busy"}})
        compare(model.error.code, "core.busy")
        wait(1300)
        compare(core.calls.length, 2, "the wait doubles")
        tryVerify(() => core.calls.length === 3, 3000)
        core.respond(core.calls[2].id, page([job("j1", "running")], "c1"))
        verify(model.loaded)
        verify(!model.error)
        compare(model.rowCount(), 1)

        // A later page that fails is asked again with its cursor.
        model.fetchMore(model.index(-1, 0))
        core.respond(core.calls[3].id, {status: "error", error: {code: "core.unreachable", message: "no core"}})
        compare(model.error.code, "core.unreachable")
        tryVerify(() => core.calls.length === 5, 3000)
        compare(core.calls[4].args.cursor, "c1")
        core.respond(core.calls[4].id, page([job("j0", "running")], null))
        compare(model.rowCount(), 2)
        verify(!model.error)
    }

    // Review C-1: a model whose page failed while the session was away loads once the
    // connection is ready, at once; once loaded, `ready` reloads nothing.
    function test_a_model_that_failed_while_away_loads_once_ready() {
        const core = createTemporaryObject(fakeComponent, tc, {connection: "reconnecting"})
        const model = createTemporaryObject(jobsComponent, tc, {core: core})
        tryVerify(() => core.calls.length === 1)
        core.respond(core.calls[0].id, {status: "error", error: {code: "core.unreachable", message: "away"}})
        verify(!!model.error)
        core.connection = "ready"
        compare(core.calls.length, 2, "loaded at once")
        core.respond(core.calls[1].id, page([job("j1", "running")]))
        verify(model.loaded)
        verify(!model.error)
        // The reload and the page started the backoff over: no retry is left waiting.
        wait(1300)
        compare(core.calls.length, 2)
        core.connection = "reconnecting"
        core.connection = "ready"
        compare(core.calls.length, 2, "a loaded model doesn't reload on ready (ARCH-100)")
    }

    // Review C-1, M-3: bound to the Core singleton itself (no session in the test runner, so
    // every call fails core.unreachable), the model reports its error and keeps asking. This
    // also runs the link's QString path and Core's typed signals.
    function test_a_model_on_core_itself_reports_its_error_and_retries() {
        const model = createTemporaryObject(jobsComponent, tc, {core: Core})
        const errors = spy(model, "errorChanged")
        tryVerify(() => !!model.error, 3000)
        compare(model.error.code, "core.unreachable")
        verify(!model.loaded)
        compare(model.rowCount(), 0)
        tryVerify(() => errors.count >= 2, 3000, "asked again after a second")
    }
}
