// Home (APP-111, APP-016, APP-163; P4a-R16, P4a-R21, P4a-R41): its cards, "While you were away" and
// when the time it counts from moves, what each card says when a read fails or the core goes away,
// and a late answer after a workspace switch. Everything the fake core answers has the real core's
// shape (CoreShapes.js, FakeCore.jobsListAnswer); the app state is a temporary folder of this process.
import QtQuick
import QtTest
import Lilith.Core
import Lilith.UiKit
import "CoreShapes.js" as Shapes

TestCase {
    id: tc
    name: "Home"
    when: windowShown
    width: 200
    height: 100

    readonly property var wsOne: ({id: "ws1", path: "D:/art/harbor"})
    readonly property var wsTwo: ({id: "ws2", path: "D:/art/quay"})
    readonly property string seen: "2026-10-30T12:00:00Z"

    Component {
        id: fakeComponent
        FakeCore { project: "sketchbook" }
    }

    Component {
        id: hostComponent
        Window {
            id: host
            required property var core
            property string stateDir
            readonly property alias panel: home
            width: 1280
            height: 800
            visible: true

            HomePanel {
                id: home
                anchors.fill: parent
                core: host.core
                title: "Home"
                stateDir: host.stateDir
            }
        }
    }

    Component {
        id: shellComponent
        ShellWindow {
            shim: QtObject {
                function activate(window, takeFocus) {}
            }
        }
    }

    A11yWalk { id: walk }

    property var answered: ({})

    function init() {
        // The shell of the test before deletes its docks later (KDDockWidgets' names are process-wide).
        wait(100)
        answered = {}
    }

    function makeCore(workspace) {
        return createTemporaryObject(fakeComponent, tc, {workspace: workspace === undefined ? wsOne : workspace})
    }

    function makeHome(core, stateDir) {
        const host = createTemporaryObject(hostComponent, tc, {core: core, stateDir: stateDir || ""})
        verify(host && host.panel, "HomePanel created")
        tryVerify(() => host.panel.onScreen, 5000, "Home is on screen")
        return host.panel
    }

    function makeShell(core, stateDir) {
        const shell = createTemporaryObject(shellComponent, tc, {core: core, stateDir: stateDir || ""})
        verify(shell, "ShellWindow created")
        shell.visible = true
        tryVerify(() => shell.docks.home.content && shell.docks.home.content.onScreen, 5000, "the docks are laid out")
        return shell
    }

    function stateWith(ws, at) {
        const dir = TestFiles.newDir("home")
        if (at)
            compare(ShellState.setLastSeen(dir, ws, at), "")
        return dir
    }

    function keptTime(dir, ws) {
        const text = TestFiles.read(dir + "/state.json")
        if (text === null || text === undefined)
            return null
        const row = JSON.parse(text).workspaces[ws]
        return row ? row.last_seen_at : null
    }

    function scene(over) {
        return Object.assign({
            projects: [{project: "sketchbook", name: "sketchbook", status: "active", assets: 12},
                       {project: "lantern", name: "lantern", status: "active", assets: 3}],
            // The core lists only the statuses that have assets: no `blocked` here.
            status: Shapes.projectStatus("sketchbook", {candidate: 4, in_review: 2, stale: 1, approved: 5}),
            assets: [Shapes.asset("sketchbook", "chr_crane", "candidate", "2026-10-30T13:00:05Z"),
                     Shapes.asset("lantern", "bg_harbor", "approved", "2026-10-29T09:00:00Z")],
            jobs: [],
            pending: [],
            fail: ({})
        }, over || {})
    }

    // Answers every call Home asked that isn't answered yet (the first read's pages lead to more).
    function answerAll(core, s) {
        for (const c of core.calls) {
            if (answered[c.id])
                continue
            let outcome = null
            if (s.fail[c.name])
                outcome = s.fail[c.name]
            else if (c.name === "projects.list")
                outcome = Shapes.tool({projects: s.projects})
            else if (c.name === "project.status")
                outcome = s.status
            else if (c.name === "assets.query")
                outcome = Shapes.assetsPage(s.assets)
            else if (c.name === "jobs.list")
                outcome = core.jobsListAnswer(c.args, s.jobs, [Shapes.lane(), Shapes.lane({lane: "cpu"})])
            else if (c.name === "app/confirmations.list")
                outcome = Shapes.confirmations(s.pending)
            if (outcome) {
                answered[c.id] = true
                core.respond(c.id, outcome)
            }
        }
    }

    // Answers until the panel has no read out and nothing is due.
    function settle(core, panel, s) {
        tryVerify(() => {
            answerAll(core, s)
            return panel._open === 0 && !coalesceDue(panel)
        }, 5000, "the reads are in")
    }

    function coalesceDue(panel) {
        return !!(panel._due.jobs || panel._due.confirmations || panel._due.cards || panel._due.status)
    }

    // Keeps answering until the kept time isn't `from`: it is written once one more jobs read has
    // landed.
    function keptMoved(core, s, dir, ws, from, ms, what) {
        tryVerify(() => {
            answerAll(core, s)
            return keptTime(dir, ws) !== from
        }, ms, what || "the kept time moves")
    }

    function isoAgo(ms) {
        return new Date(Date.now() - ms).toISOString().replace(/\.\d+Z$/, "Z")
    }

    // Keeps answering from `s` for `ms`.
    function answerFor(core, s, ms) {
        const from = Date.now()
        while (Date.now() - from < ms) {
            answerAll(core, s)
            wait(50)
        }
    }

    function isInside(item, ancestor) {
        for (let p = item; p; p = p.parent) {
            if (p === ancestor)
                return true
        }
        return false
    }

    function callsOf(core, name) {
        return core.calls.filter(c => c.name === name)
    }

    function lineTexts(card) {
        const out = []
        for (let i = 0; i < card.lineRepeater.count; ++i)
            out.push(card.lineRepeater.itemAt(i).text)
        return out
    }

    function rowTexts(repeater) {
        const out = []
        for (let i = 0; i < repeater.count; ++i)
            out.push(repeater.itemAt(i).text)
        return out
    }

    function stamp(at) {
        return Qt.formatDateTime(new Date(Date.parse(at)), "yyyy-MM-dd HH:mm")
    }

    // A card's own Texts, wherever they sit.

    function ownTexts(item, out) {
        out = out || []
        if (item instanceof Text)
            out.push(item)
        for (const child of item.children || [])
            ownTexts(child, out)
        if (item.contentItem && item.contentItem !== item)
            ownTexts(item.contentItem, out)
        return out
    }

    // --- The cards. ---

    function test_projects_and_status_counts() { // spec: APP-111
        const core = makeCore()
        const panel = makeHome(core)
        settle(core, panel, scene())
        compare(rowTexts(panel.projectsCard.rows), ["sketchbook · 12 assets", "lantern · 3 assets"])
        compare(panel.statusCounts.countsLabel.text, "Takes waiting 4 · In review 2 · Stale 1 · Blocked 0")
        compare(panel.statusCounts.title, "Assets in sketchbook")
        compare(callsOf(core, "project.status")[0].args, {}, "naming one would move the session's project and the hint")
        compare(panel.projectsCard.workspaceText, "Workspace: harbor")
        const query = callsOf(core, "assets.query")[0].args
        compare(query, {sort: {field: "updated", dir: "desc"}, limit: 12})
        compare(rowTexts(panel.recentAssets.rows),
                ["chr_crane · sketchbook · Take waiting · " + stamp("2026-10-30T13:00:05Z"),
                 "bg_harbor · lantern · Approved · " + stamp("2026-10-29T09:00:00Z")])
        compare(panel.recentAssets.title, "Recent assets")
        compare(panel.statusCounts.note, "")
        compare(panel.projectsCard.note, "")
    }

    function test_status_copy_says_takes() { // spec: APP-163
        const core = makeCore()
        const panel = makeHome(core)
        settle(core, panel, scene({status: Shapes.projectStatus("sketchbook", {candidate: 1234})}))
        compare(panel.statusCounts.countsLabel.text, "Takes waiting 1,234 · In review 0 · Stale 0 · Blocked 0")
        for (const t of ownTexts(panel))
            verify(!/candidate/i.test(t.text), "copy says takes: " + t.text)
    }

    function test_counts_have_their_separators_and_one_asset_is_one() { // spec: APP-111
        const core = makeCore()
        const panel = makeHome(core)
        settle(core, panel, scene({projects: [{project: "a", name: "A", status: "active", assets: 1234},
                                              {project: "b", name: "B", status: "paused", assets: 1}]}))
        compare(rowTexts(panel.projectsCard.rows), ["A · 1,234 assets", "B · 1 asset · paused"])
    }

    function test_a_workspace_with_no_project_says_so() { // spec: APP-111
        const core = makeCore()
        core.project = ""
        const panel = makeHome(core)
        const unknown = Shapes.refusal("project.unknown", "Project \"sketchbook\" doesn't exist.")
        settle(core, panel, scene({projects: [], assets: [], status: unknown}))
        compare(callsOf(core, "project.status")[0].args, {})
        compare(panel.projectsCard.note, "No projects in this workspace yet.")
        compare(panel.statusCounts.note, "No project to count yet.")
        compare(panel.statusCounts.countsLabel.visible, false)
        compare(panel.recentAssets.note, "No assets yet.")
        // Projects, but the one the core falls back on isn't among them.
        core.emit("jobs", "resync_required", {})
        settle(core, panel, scene({assets: [], status: unknown}))
        compare(panel.statusCounts.note, "Choose a project in the top bar to see its counts.")
    }

    function test_open_jobs_list() { // spec: APP-111
        const core = makeCore()
        const panel = makeHome(core)
        verify(panel.openJobs.note === "Reading…", panel.openJobs.note)
        settle(core, panel, scene({jobs: [
            Shapes.job("j_0000000000000001", "running", {project: "sketchbook"}),
            Shapes.job("j_0000000000000002", "queued", {project: "lantern", queue_position: 0}),
            Shapes.job("j_0000000000000003", "queued", {substatus: "queue_paused", pause_reasons: ["restart"]}),
            Shapes.job("j_0000000000000004", "queued", {kind: "future.kind", project: "gone", queue_position: 1}),
            // Ended: not an open job.
            Shapes.job("j_0000000000000005", "succeeded"),
        ]}))
        const view = panel.openJobs.view
        compare(panel.openJobs.count, 4)
        tryVerify(() => view.count === 4 && !!view.itemAtIndex(3), 5000, "the rows are made")
        compare(view.itemAtIndex(0).text, "ComfyUI prompt · sketchbook · Running")
        const texts = [1, 2, 3].map(i => view.itemAtIndex(i).text)
        verify(texts.indexOf("ComfyUI prompt · lantern · Queued") >= 0, texts)
        verify(texts.indexOf("future.kind · gone · Queued") >= 0, "a kind and a project it doesn't know: " + texts)
        verify(texts.indexOf("ComfyUI prompt · no project · Held after a restart") >= 0, "no project: " + texts)
        compare(panel.openJobs.note, "")
        settle(core, panel, scene())
    }

    function test_open_jobs_has_a_line_when_there_are_none_and_counts_the_rest() { // spec: APP-111
        const core = makeCore()
        const panel = makeHome(core)
        settle(core, panel, scene())
        compare(panel.openJobs.note, "Nothing is running or queued.")
        const jobs = []
        for (let i = 0; i < 11; ++i)
            jobs.push(Shapes.job("j_000000000000001" + i.toString(16), "queued", {queue_position: i}))
        core.emit("jobs", "event", {kind: "job", job: jobs[0]})
        tryVerify(() => {
            answerAll(core, scene({jobs: jobs}))
            return panel.openJobs.count === 11 && panel._open === 0
        }, 5000)
        compare(panel.openJobs.moreLabel.text, "and 3 more. Jobs lists them.")
        compare(panel.openJobs.note, "")
    }

    function test_no_workspace_shows_recent_loose_files_instead() { // spec: APP-016
        const core = makeCore(null)
        const panel = makeHome(core, stateWith("ws1", seen))
        settle(core, panel, scene({jobs: [Shapes.job("j_0000000000000001", "running")]}))
        compare(panel.recentAssets.title, "Recent loose files")
        compare(panel.recentAssets.rows.count, 0)
        compare(panel.recentAssets.note, "This version doesn't list loose files yet.")
        compare(panel.projectsCard.note, "No workspace is open.")
        verify(!panel.statusCounts.visible, "no counts without a project")
        verify(!panel.awayCard.visible, "no card: the time is per workspace")
        compare(panel.openJobs.count, 1, "the open jobs still show")
        for (const name of ["projects.list", "project.status", "assets.query", "app/confirmations.list"])
            compare(callsOf(core, name), [], "nothing that needs a workspace: " + name)
        verify(callsOf(core, "jobs.list").every(c => c.args.since === undefined), "no time to read since")
        // A workspace arrives: the cards read it.
        core.applyState({connection: "ready", workspace: wsOne})
        settle(core, panel, scene())
        compare(panel.recentAssets.title, "Recent assets")
        compare(panel.recentAssets.rows.count, 2)
        compare(panel.projectsCard.rows.count, 2)
    }

    // --- While you were away. ---

    function awayScene(over) {
        const old = "2026-10-30T11:00:00Z"
        return scene(Object.assign({
            jobs: [Shapes.job("j_00000000000000a1", "succeeded", {updated: "2026-10-30T13:00:05Z", project: "sketchbook"}),
                   Shapes.job("j_00000000000000a2", "failed", {updated: "2026-10-30T13:00:04Z"}),
                   Shapes.job("j_00000000000000a3", "succeeded", {updated: old}),
                   Shapes.job("j_00000000000000a4", "queued", {updated: old, substatus: "queue_paused",
                                                               pause_reasons: ["restart"]})],
            pending: [Shapes.confirmation("cf_00000000000000b1", "comfy.run", "2026-10-30T13:00:01Z")]
        }, over || {}))
    }

    function test_away_card_lists_items_and_last_seen_is_written_after_3_s_visible() { // spec: APP-111, P4a-R16, P4a-R41
        const dir = stateWith("ws1", seen)
        const core = makeCore()
        const panel = makeHome(core, dir)
        const shownAt = Date.now()
        settle(core, panel, awayScene())
        const since = callsOf(core, "jobs.list").filter(c => c.args.since !== undefined)
        verify(since.length > 0 && since.every(c => c.args.since === seen), "it reads since the kept time")
        verify(panel.awayCard.visible, "the card shows")
        compare(lineTexts(panel.awayCard), [
            "Finished · ComfyUI prompt · sketchbook · " + stamp("2026-10-30T13:00:05Z"),
            "Failed · ComfyUI prompt · no project · " + stamp("2026-10-30T13:00:04Z"),
            "Confirmation waiting · Claude Code wants to run comfy.run · " + stamp("2026-10-30T13:00:01Z"),
            "Needs you · ComfyUI prompt · no project · Held after a restart · " + stamp("2026-10-30T11:00:00Z"),
        ])
        compare(walk.problems(panel.awayCard), [])
        wait(Math.max(0, 2000 - (Date.now() - shownAt)))
        compare(keptTime(dir, "ws1"), seen, "not yet")
        keptMoved(core, awayScene(), dir, "ws1", seen, 2500, "written after 3 s")
        const at = Date.parse(keptTime(dir, "ws1"))
        verify(at >= shownAt + 2500 && at <= Date.now(), "it is when one more read began, at the 3 s: " + keptTime(dir, "ws1"))
        verify(Date.now() - shownAt >= 2900, "and not before 3 s")
        // The lines stay: they don't vanish as you read them, nor on the next read.
        verify(panel.awayCard.visible)
        compare(panel.awayCard.lineRepeater.count, 4)
        core.emit("confirmations", "event", Shapes.confirmationEvent("pending", Shapes.confirmation("cf_00000000000000b1", "comfy.run", "2026-10-30T13:00:01Z")))
        tryVerify(() => {
            answerAll(core, awayScene())
            return panel._open === 0 && !coalesceDue(panel) && callsOf(core, "app/confirmations.list").length >= 2
        }, 5000)
        compare(panel.awayCard.lineRepeater.count, 4)
    }

    function test_first_run_shows_no_card_and_starts_the_time() { // spec: APP-111
        const dir = stateWith("ws1")
        const core = makeCore()
        const panel = makeHome(core, dir)
        settle(core, panel, awayScene())
        verify(!panel.awayCard.visible, "the first run shows no card")
        verify(callsOf(core, "jobs.list").every(c => c.args.since === undefined), "and reads no `since`")
        compare(callsOf(core, "app/confirmations.list"), [])
        keptMoved(core, awayScene(), dir, "ws1", null, 4000, "the time starts")
    }

    function test_a_time_that_does_not_read_is_as_a_first_run() { // spec: P4a-R16
        const dir = stateWith("ws1", "yesterday")
        const core = makeCore()
        const panel = makeHome(core, dir)
        settle(core, panel, awayScene())
        compare(panel.lastSeen, "")
        verify(callsOf(core, "jobs.list").every(c => c.args.since === undefined), "never sent to the core")
    }

    function test_nothing_new_shows_no_card() { // spec: APP-111
        const dir = stateWith("ws1", "2026-10-30T14:00:00Z")
        const core = makeCore()
        const panel = makeHome(core, dir)
        settle(core, panel, awayScene({jobs: [Shapes.job("j_00000000000000a1", "succeeded", {updated: "2026-10-30T13:00:05Z"})], pending: []}))
        verify(!panel.awayCard.visible)
    }

    function test_away_card_dismiss_clears_it() { // spec: APP-111
        const dir = stateWith("ws1", seen)
        const core = makeCore()
        const panel = makeHome(core, dir)
        const s = awayScene()
        settle(core, panel, s)
        verify(panel.awayCard.dismissButton.visible)
        // A real click: once the card has been laid out.
        tryVerify(() => panel.awayCard.dismissButton.y > panel.awayCard.lineRepeater.itemAt(3).y)
        wait(50)
        mouseClick(panel.awayCard.dismissButton)
        verify(!panel.awayCard.visible, "the card goes")
        verify(keptTime(dir, "ws1") !== seen, "and the time moves at once")
        compare(panel.lastSeen, keptTime(dir, "ws1"))
        // The jobs a restart holds and the pending confirmation stay out on the next read.
        core.emit("jobs", "resync_required", {})
        settle(core, panel, s)
        verify(!panel.awayCard.visible, "still clear")
        const since = callsOf(core, "jobs.list").filter(c => c.args.since !== undefined)
        compare(since[since.length - 1].args.since, panel.lastSeen, "reads count from then")
        // What comes after it does show.
        const newer = new Date(Date.now() + 5000).toISOString().replace(/\.\d+Z$/, "Z")
        const fresh = Shapes.job("j_00000000000000c1", "failed", {updated: newer})
        core.emit("jobs", "event", {kind: "job", job: fresh})
        tryVerify(() => {
            answerAll(core, awayScene({jobs: s.jobs.concat([fresh])}))
            return panel.awayCard.visible
        }, 5000, "a new line shows the card again")
        compare(panel.awayCard.lineRepeater.count, 1)
        // A dismissed job that changes is a new line.
        const again = Shapes.confirmation("cf_00000000000000b2", "comfy.run", newer)
        core.emit("confirmations", "event", Shapes.confirmationEvent("pending", again))
        tryVerify(() => {
            answerAll(core, awayScene({jobs: s.jobs.concat([fresh]), pending: s.pending.concat([again])}))
            return panel.awayCard.lineRepeater.count === 2
        }, 5000)
    }

    function test_a_read_out_as_you_dismiss_does_not_bring_the_lines_back() { // spec: APP-111
        const dir = stateWith("ws1", seen)
        const core = makeCore()
        const panel = makeHome(core, dir)
        const s = awayScene()
        settle(core, panel, s)
        core.emit("jobs", "resync_required", {})
        verify(panel._open > 0, "a read is out")
        panel.dismissAway()
        settle(core, panel, s)
        verify(!panel.awayCard.visible)
    }

    function test_a_new_line_holds_the_time_back_again() { // spec: APP-111
        const dir = stateWith("ws1", seen)
        const core = makeCore()
        const panel = makeHome(core, dir)
        const s = awayScene()
        settle(core, panel, s)
        keptMoved(core, s, dir, "ws1", seen, 4000)
        const first = keptTime(dir, "ws1")
        // A job fails while Home shows: it joins the card, and the time moves past it once it has shown.
        const fresh = Shapes.job("j_00000000000000c1", "failed", {updated: new Date().toISOString().replace(/\.\d+Z$/, "Z")})
        core.emit("jobs", "event", {kind: "job", job: fresh})
        tryVerify(() => {
            answerAll(core, awayScene({jobs: s.jobs.concat([fresh])}))
            return panel.awayCard.lineRepeater.count === 5
        }, 5000, "it joins the card")
        compare(keptTime(dir, "ws1"), first, "not at once")
        keptMoved(core, awayScene({jobs: s.jobs.concat([fresh])}), dir, "ws1", first, 4000, "after it has shown for 3 s")
    }

    function test_last_seen_waits_until_really_on_screen() { // spec: P4a-R16
        const dir = stateWith("ws1", seen)
        const core = makeCore()
        const shell = makeShell(core, dir)
        const panel = shell.docks.home.content
        const s = awayScene()

        // Behind a tab.
        shell.docks.home.addDockWidgetAsTab(shell.docks.jobs)
        wait(100)
        shell.docks.jobs.setAsCurrentTab()
        tryVerify(() => !panel.visible, 5000, "Home is behind the tab")
        answerAll(core, s)
        verify(!panel.onScreen)
        wait(3300)
        verify(!panel.onScreen, "it stayed behind")
        answerAll(core, s)
        compare(keptTime(dir, "ws1"), seen, "not behind a tab")

        // Closed.
        shell.docks.home.setAsCurrentTab()
        shell.docks.home.forceClose()
        tryVerify(() => !shell.docks.home.isOpen, 5000)
        verify(!panel.onScreen, "a closed dock isn't on screen")
        wait(3300)
        answerAll(core, s)
        compare(keptTime(dir, "ws1"), seen, "not in a closed dock")

        // Back, for less than the wait, then minimized: the wait starts over.
        shell.showPanel("home")
        tryVerify(() => panel.onScreen, 5000, "Home is back")
        settle(core, panel, s)
        verify(panel.awayCard.visible)
        wait(1800)
        shell.visibility = Window.Minimized
        tryVerify(() => !panel.onScreen, 5000, "the window is minimized")
        wait(3300)
        compare(keptTime(dir, "ws1"), seen, "not on a minimized window")

        // Back, with the core away.
        core.applyState({connection: "reconnecting"})
        shell.visibility = Window.Windowed
        tryVerify(() => panel.onScreen, 5000, "the window is back")
        wait(3300)
        compare(keptTime(dir, "ws1"), seen, "not with the core away")

        // And once all is well, after a whole wait from then.
        core.applyState({connection: "ready"})
        const from = Date.now()
        settle(core, panel, s)
        keptMoved(core, s, dir, "ws1", seen, 5000, "written")
        verify(Date.now() - from >= 2900, "a whole wait: " + (Date.now() - from))
        shell.destroy()
    }

    function test_last_seen_waits_for_the_card_s_reads() { // spec: P4a-R16
        const dir = stateWith("ws1", seen)
        const core = makeCore()
        makeHome(core, dir)
        // Nothing is answered.
        wait(3400)
        compare(keptTime(dir, "ws1"), seen, "a time isn't moved past what wasn't shown")
    }

    function test_last_seen_keeps_its_time_when_a_read_fails() { // spec: P4a-R16
        const dir = stateWith("ws1", seen)
        const core = makeCore()
        const panel = makeHome(core, dir)
        const s = awayScene({fail: {"jobs.list": Shapes.refusal("store.io", "The state database is busy.")}})
        tryVerify(() => {
            answerAll(core, s)
            return panel.readState.jobs === "failed"
        }, 5000)
        wait(3400)
        answerAll(core, s)
        compare(keptTime(dir, "ws1"), seen)
    }

    function test_the_card_moves_on_after_home_has_been_away() { // spec: APP-111
        const dir = stateWith("ws1", seen)
        const core = makeCore()
        const panel = makeHome(core, dir)
        const s = awayScene()
        const host = panel.Window.window
        settle(core, panel, s)
        keptMoved(core, s, dir, "ws1", seen, 4000)
        const kept = keptTime(dir, "ws1")
        // Off screen for a moment (a float, a quick look away): nothing moves.
        host.visibility = Window.Minimized
        tryVerify(() => !panel.onScreen, 5000)
        wait(300)
        host.visibility = Window.Windowed
        tryVerify(() => panel.onScreen, 5000)
        compare(panel.lastSeen, seen)
        verify(panel.awayCard.visible)
        // Off screen for longer: the card now counts from the kept time, and what you saw has gone.
        host.visibility = Window.Minimized
        tryVerify(() => !panel.onScreen, 5000)
        wait(3300)
        host.visibility = Window.Windowed
        tryVerify(() => panel.onScreen, 5000)
        settle(core, panel, s)
        compare(panel.lastSeen, kept)
        const since = callsOf(core, "jobs.list").filter(c => c.args.since !== undefined)
        compare(since[since.length - 1].args.since, kept)
        compare(lineTexts(panel.awayCard).filter(t => t.startsWith("Finished")), [])
        verify(lineTexts(panel.awayCard).some(t => t.indexOf("Held after a restart") >= 0), "what still needs you stays")
    }

    function test_last_seen_is_per_workspace() { // spec: P4a-R16
        const dir = stateWith("ws1", seen)
        const later = "2026-10-30T12:30:00Z"
        compare(ShellState.setLastSeen(dir, "ws2", later), "")
        const core = makeCore()
        const panel = makeHome(core, dir)
        settle(core, panel, awayScene())
        const before = core.calls.length
        core.applyState({connection: "ready", workspace: wsTwo})
        compare(panel.lastSeen, later)
        settle(core, panel, awayScene())
        const since = core.calls.slice(before).filter(c => c.name === "jobs.list" && c.args.since !== undefined)
        verify(since.length > 0 && since.every(c => c.args.since === later), "never the other workspace's time")
        keptMoved(core, awayScene(), dir, "ws2", later, 4000)
        compare(keptTime(dir, "ws1"), seen, "the other workspace's time stays")
        // A workspace seen for the first time has no card, whatever another's time.
        const third = {id: "ws3", path: "D:/art/third"}
        const at = core.calls.length
        core.applyState({connection: "ready", workspace: third})
        settle(core, panel, awayScene())
        verify(!panel.awayCard.visible)
        verify(core.calls.slice(at).every(c => c.name !== "jobs.list" || c.args.since === undefined))
    }

    function test_a_late_answer_stays_out_after_a_workspace_switch() { // spec: ARCH-090
        const dir = stateWith("ws1", seen)
        const core = makeCore()
        const panel = makeHome(core, dir)
        // The first workspace's reads are out, then the switch.
        tryVerify(() => core.calls.length > 0)
        const old = core.calls.slice()
        core.applyState({connection: "ready", workspace: wsTwo})
        for (const c of old)
            verify(core.cancelled.indexOf(c.id) >= 0, "cancelled: " + c.name)
        // An answer that comes anyway lands nowhere.
        const s = awayScene()
        for (const c of old) {
            answered[c.id] = true
            core.callFinished(c.id, JSON.stringify(c.name === "projects.list"
                ? {structured: {projects: s.projects}, artifacts: [], summary_text: ""}
                : c.name === "assets.query" ? {structured: {assets: s.assets}, artifacts: [], summary_text: ""}
                : {structured: {project: "old", assets_by_status: {candidate: 9}, jobs: s.jobs, next_cursor: null, lanes: []},
                   artifacts: [], summary_text: ""}))
        }
        compare(panel.projects, [])
        compare(panel.assets, [])
        compare(panel.byStatus, null)
        compare(panel.openJobs.count, 0)
        compare(panel.statusCounts.countsLabel.visible, false)
        compare(panel.awayItems, [])
        // The new workspace's own show.
        settle(core, panel, scene({projects: [{project: "quay", name: "Quay", status: "active", assets: 7}]}))
        compare(rowTexts(panel.projectsCard.rows), ["Quay · 7 assets"])
    }

    function test_switching_clears_the_cards_at_once() { // spec: ARCH-090
        const core = makeCore()
        const panel = makeHome(core)
        settle(core, panel, scene({jobs: [Shapes.job("j_0000000000000001", "running")]}))
        compare(panel.projectsCard.rows.count, 2)
        compare(panel.openJobs.count, 1)
        core.applyState({connection: "ready", workspace: wsTwo})
        compare(panel.projectsCard.rows.count, 0)
        compare(panel.recentAssets.rows.count, 0)
        compare(panel.openJobs.count, 0)
        compare(panel.statusCounts.countsLabel.visible, false)
        compare(panel.projectsCard.note, "Reading…")
    }

    function test_a_reconnect_to_the_same_workspace_keeps_the_card_s_time() { // spec: P4a-R16
        const dir = stateWith("ws1", seen)
        const core = makeCore()
        const panel = makeHome(core, dir)
        settle(core, panel, awayScene())
        core.applyState({connection: "reconnecting"})
        core.applyState({connection: "ready", workspace: wsOne})
        settle(core, panel, awayScene())
        compare(panel.lastSeen, seen)
        verify(panel.awayCard.visible)
    }

    // --- Reads. ---

    function test_a_failed_read_says_so_and_recovers() { // spec: APP-111
        const dir = stateWith("ws1", seen)
        const core = makeCore()
        const panel = makeHome(core, dir)
        const good = awayScene({jobs: awayScene().jobs.concat([Shapes.job("j_0000000000000099", "running")])})
        settle(core, panel, good)
        compare(panel.openJobs.count, 2, "the running job and the one a restart holds")
        const refused = Shapes.refusal("store.io", "The index is <b>busy</b>.")
        const bad = Object.assign({}, good, {fail: {"projects.list": refused, "project.status": refused,
                                                    "assets.query": refused, "jobs.list": refused,
                                                    "app/confirmations.list": refused}})
        core.emit("jobs", "resync_required", {})
        settle(core, panel, bad)
        const says = "The index is <b>busy</b>. Trying again…"
        compare(panel.projectsCard.note, "Couldn't read the projects: " + says)
        compare(panel.statusCounts.note, "Couldn't read the counts: " + says)
        compare(panel.recentAssets.note, "Couldn't read the assets: " + says)
        compare(panel.openJobs.note, "Couldn't read the jobs: " + says)
        compare(panel.awayCard.note, "Couldn't read the jobs: " + says)
        compare(panel.projectsCard.noteText.textFormat, Text.PlainText)
        // The figures stay, greyed: none in a normal colour.
        compare(panel.projectsCard.rows.count, 2)
        verify(Qt.colorEqual(panel.projectsCard.rows.itemAt(0).color, Theme.textMuted))
        verify(Qt.colorEqual(panel.statusCounts.countsLabel.color, Theme.textMuted))
        verify(Qt.colorEqual(panel.recentAssets.rows.itemAt(0).color, Theme.textMuted))
        tryVerify(() => !!panel.openJobs.view.itemAtIndex(0))
        verify(Qt.colorEqual(panel.openJobs.view.itemAtIndex(0).color, Theme.textMuted))
        verify(Qt.colorEqual(panel.awayCard.lineRepeater.itemAt(0).color, Theme.textMuted))
        // It asks again by itself, and the words go.
        tryVerify(() => {
            answerAll(core, good)
            return panel.projectsCard.note === "" && panel.openJobs.note === "" && panel.awayCard.note === ""
                   && panel.statusCounts.note === "" && panel.recentAssets.note === ""
        }, 8000, "the retry reads again")
        verify(Qt.colorEqual(panel.projectsCard.rows.itemAt(0).color, Theme.text))
        verify(Qt.colorEqual(panel.statusCounts.countsLabel.color, Theme.text))
        verify(!Qt.colorEqual(panel.awayCard.lineRepeater.itemAt(0).color, Theme.textMuted))
    }

    function test_a_first_read_that_fails_never_says_there_is_nothing() { // spec: APP-111
        const dir = stateWith("ws1", seen)
        const core = makeCore()
        const panel = makeHome(core, dir)
        const refused = Shapes.refusal("store.io", "No.")
        settle(core, panel, scene({fail: {"projects.list": refused, "jobs.list": refused, "assets.query": refused}}))
        verify(panel.projectsCard.note.indexOf("No projects") < 0, panel.projectsCard.note)
        verify(panel.openJobs.note.indexOf("Nothing is running") < 0, panel.openJobs.note)
        verify(panel.recentAssets.note.indexOf("No assets") < 0, panel.recentAssets.note)
        verify(panel.awayCard.visible, "the card says it couldn't read")
        verify(!panel.awayCard.dismissButton.visible, "with nothing to dismiss")
    }

    function test_the_core_going_away_greys_the_cards_and_coming_back_reads_all() { // spec: APP-111, APP-034
        const dir = stateWith("ws1", seen)
        const core = makeCore()
        const panel = makeHome(core, dir)
        const s = awayScene()
        settle(core, panel, s)
        core.applyState({connection: "reconnecting"})
        const away = "Not connected to the core, so this may be out of date."
        compare(panel.projectsCard.note, away)
        compare(panel.statusCounts.note, away)
        compare(panel.recentAssets.note, away)
        compare(panel.openJobs.note, away)
        compare(panel.awayCard.note, away)
        verify(Qt.colorEqual(panel.projectsCard.rows.itemAt(0).color, Theme.textMuted))
        verify(!panel.awayCard.dismissButton.enabled, "Dismiss waits for the core")
        const before = core.calls.length
        core.emit("jobs", "event", {kind: "job", job: Shapes.job("j_0000000000000077", "running")})
        wait(1200)
        compare(core.calls.length, before, "nothing is asked while it's away")
        core.applyState({connection: "ready", workspace: wsOne})
        settle(core, panel, s)
        const names = core.calls.slice(before).map(c => c.name)
        for (const name of ["projects.list", "project.status", "assets.query", "jobs.list", "app/confirmations.list"])
            verify(names.indexOf(name) >= 0, "read again: " + name)
        compare(panel.projectsCard.note, "")
        compare(panel.awayCard.note, "")
        verify(panel.awayCard.dismissButton.enabled)
    }

    function test_a_reload_reads_everything_the_cards_show() { // spec: APP-111
        for (const [topic, kind] of [["jobs", "resync_required"], ["jobs", "snapshot"], ["confirmations", "resync_required"]]) {
            const dir = stateWith("ws1", seen)
            const core = makeCore()
            const panel = makeHome(core, dir)
            answered = {}
            settle(core, panel, awayScene())
            const before = core.calls.length
            core.emit(topic, kind, {})
            const names = core.calls.slice(before).map(c => c.name)
            for (const name of ["projects.list", "project.status", "assets.query", "jobs.list", "app/confirmations.list"])
                verify(names.indexOf(name) >= 0, topic + " " + kind + " reads " + name)
            settle(core, panel, awayScene())
            panel.Window.window.destroy()
        }
    }

    function test_progress_and_queue_events_cause_no_read() { // spec: APP-111
        const core = makeCore()
        const panel = makeHome(core)
        const running = Shapes.job("j_0000000000000001", "running", {progress: {value: 1, max: 20}})
        const s = scene({jobs: [running]})
        settle(core, panel, s)
        tryVerify(() => panel.openJobs.count === 1)
        const before = core.calls.length
        for (let i = 2; i < 12; ++i)
            core.emit("jobs", "event", {kind: "job", job: Object.assign({}, running, {progress: {value: i, max: 20}})})
        core.emit("jobs", "event", {kind: "queue", state: Shapes.lane({paused: true, reasons: ["manual"]})})
        wait(600)
        compare(core.calls.slice(before), [], "no read")
    }

    function test_a_burst_is_one_read_and_one_at_a_time() { // spec: APP-111
        const core = makeCore()
        const panel = makeHome(core)
        const s = scene()
        settle(core, panel, s)
        const before = callsOf(core, "projects.list").length
        // A burst of jobs that end: one read, not one for each.
        for (let i = 0; i < 10; ++i)
            core.emit("jobs", "event", {kind: "job", job: Shapes.job("j_000000000000010" + i, "succeeded")})
        tryVerify(() => callsOf(core, "projects.list").length === before + 1, 3000, "the read goes out")
        // While it is out, nothing more goes out, whatever comes.
        for (let i = 0; i < 5; ++i)
            core.emit("jobs", "event", {kind: "job", job: Shapes.job("j_000000000000020" + i, "failed")})
        wait(400)
        compare(callsOf(core, "projects.list").length, before + 1, "one read at a time")
        // Once it is in, what came meanwhile is read once.
        settle(core, panel, s)
        tryVerify(() => {
            answerAll(core, s)
            return callsOf(core, "projects.list").length === before + 2
        }, 3000, "what came meanwhile is read")
        settle(core, panel, s)
        wait(300)
        compare(callsOf(core, "projects.list").length, before + 2)
    }

    function test_events_read_what_they_change() { // spec: APP-111
        const dir = stateWith("ws1", seen)
        const core = makeCore()
        const panel = makeHome(core, dir)
        const s = awayScene()
        settle(core, panel, s)
        // A confirmation's event: the confirmations.
        let before = core.calls.length
        core.emit("confirmations", "event", Shapes.confirmationEvent("answered", Shapes.confirmation("cf_00000000000000b1", "comfy.run", "2026-10-30T13:00:01Z", {state: "approved"})))
        tryVerify(() => core.calls.length > before, 3000)
        compare(core.calls.slice(before).map(c => c.name), ["app/confirmations.list"])
        const without = awayScene({pending: []})
        settle(core, panel, without)
        compare(lineTexts(panel.awayCard).filter(t => t.startsWith("Confirmation")), [], "answered: off the card")
        // A job that starts: the jobs, not the workspace's cards.
        before = core.calls.length
        core.emit("jobs", "event", {kind: "job", job: Shapes.job("j_0000000000000031", "running")})
        tryVerify(() => core.calls.length > before, 3000)
        verify(core.calls.slice(before).every(c => c.name === "jobs.list"))
        settle(core, panel, without)
        // A job that ends: the cards too.
        before = core.calls.length
        core.emit("jobs", "event", {kind: "job", job: Shapes.job("j_0000000000000031", "succeeded")})
        tryVerify(() => core.calls.length > before, 3000)
        const names = core.calls.slice(before).map(c => c.name)
        verify(names.indexOf("assets.query") >= 0 && names.indexOf("projects.list") >= 0, names)
        settle(core, panel, without)
    }

    function test_a_job_turning_unknown_reads_the_card() { // spec: APP-111
        const dir = stateWith("ws1", seen)
        const core = makeCore()
        const panel = makeHome(core, dir)
        const running = Shapes.job("j_0000000000000031", "running", {updated: "2026-10-30T13:00:00Z"})
        settle(core, panel, awayScene({jobs: [running], pending: []}))
        const unknown = Object.assign({}, running, {substatus: "interrupted_unknown", updated: "2026-10-30T13:01:00Z"})
        core.emit("jobs", "event", {kind: "job", job: unknown})
        tryVerify(() => {
            answerAll(core, awayScene({jobs: [unknown], pending: []}))
            return panel.awayCard.visible
        }, 3000, "it needs you")
        verify(lineTexts(panel.awayCard)[0].indexOf("Needs you · ComfyUI prompt · no project · Outcome unknown") === 0,
               lineTexts(panel.awayCard)[0])
    }

    function test_choosing_a_project_reads_its_counts() { // spec: APP-111
        const core = makeCore()
        const panel = makeHome(core)
        settle(core, panel, scene())
        const before = callsOf(core, "project.status").length
        const others = callsOf(core, "projects.list").length
        core.noteProject("lantern")
        tryVerify(() => callsOf(core, "project.status").length === before + 1, 3000)
        compare(callsOf(core, "project.status")[before].args, {}, "the session's project, never named")
        compare(callsOf(core, "projects.list").length, others, "only the counts depend on it")
        settle(core, panel, scene({status: Shapes.projectStatus("lantern", {stale: 3})}))
        compare(panel.statusCounts.title, "Assets in lantern")
        compare(panel.statusCounts.countsLabel.text, "Takes waiting 0 · In review 0 · Stale 3 · Blocked 0")
    }

    function test_nothing_is_read_while_home_is_off_screen() { // spec: APP-111
        const core = makeCore()
        const panel = makeHome(core)
        const s = scene()
        settle(core, panel, s)
        const host = panel.Window.window
        host.visibility = Window.Minimized
        tryVerify(() => !panel.onScreen, 5000)
        const before = core.calls.length
        core.emit("jobs", "event", {kind: "job", job: Shapes.job("j_0000000000000001", "succeeded")})
        wait(1200)
        compare(core.calls.slice(before), [], "it waits")
        host.visibility = Window.Windowed
        tryVerify(() => core.calls.length > before, 5000, "and reads when it shows")
        settle(core, panel, s)
    }

    function test_many_lines_are_counted() { // spec: APP-111
        const dir = stateWith("ws1", seen)
        const core = makeCore()
        const panel = makeHome(core, dir)
        const jobs = []
        for (let i = 0; i < 25; ++i)
            jobs.push(Shapes.job("j_00000000000" + String(10000 + i), "succeeded", {updated: "2026-10-30T13:00:05Z"}))
        settle(core, panel, scene({jobs: jobs}))
        compare(panel.awayCard.lineRepeater.count, 20)
        compare(panel.awayCard.moreLabel.text, "and 5 more. Jobs lists them.")
    }

    function test_a_since_read_stops_at_its_page_limit_and_says_so() { // spec: APP-111
        const dir = stateWith("ws1", seen)
        const core = makeCore()
        const panel = makeHome(core, dir)
        const jobs = []
        // More pages than a read follows.
        for (let i = 0; i < panel.awayPages * 100 + 1; ++i)
            jobs.push(Shapes.job("j_0000000000" + String(100000 + i), "cancelled", {updated: "2026-10-30T13:00:05Z"}))
        jobs.push(Shapes.job("j_00000000000000a1", "succeeded", {created: "2099-01-01T00:00:00Z", updated: "2026-10-30T13:00:05Z"}))
        settle(core, panel, scene({jobs: jobs}))
        compare(callsOf(core, "jobs.list").filter(c => c.args.since !== undefined).length, panel.awayPages)
        verify(panel.awayCut)
        compare(panel.awayCard.moreLabel.text, "and more. Jobs lists them.")
    }

    function test_outside_text_is_plain() { // spec: AI-222, P4a-R8
        const dir = stateWith("ws1", seen)
        const core = makeCore()
        const panel = makeHome(core, dir)
        const img = '<img src="http://127.0.0.1:9/probe.png">'
        const before = TestFiles.networkRequests()
        const job = Shapes.job("j_00000000000000a1", "failed", {updated: "2026-10-30T13:00:05Z", kind: img + "kind",
                                                                project: "<b>p</b>\u202e"})
        settle(core, panel, scene({
            projects: [{project: "x", name: img + "<b>n</b>", status: img, assets: 1}],
            status: Shapes.projectStatus(img + "proj", {candidate: 1}),
            assets: [Shapes.asset(img, img + "a", img, "2026-10-30T13:00:05Z")],
            jobs: [job, Shapes.job("j_00000000000000a2", "running", {kind: img + "k", project: img, substatus: img})],
            pending: [Shapes.confirmation("cf_00000000000000b1", img + "tool", "2026-10-30T13:00:01Z")],
        }))
        verify(panel.awayCard.visible)
        compare(panel.statusCounts.title.indexOf("<img"), "Assets in ".length, "shown as written")
        verify(lineTexts(panel.awayCard)[0].indexOf("⟨U+202E⟩") >= 0, "a bidi mark is marked: " + lineTexts(panel.awayCard)[0])
        for (const t of ownTexts(panel))
            compare(t.textFormat, Text.PlainText, String(t) + ": " + t.text)
        wait(300)
        compare(TestFiles.networkRequests(), before, "nothing was loaded")
    }

    // --- In the shell. ---

    function test_opened_card_sits_in_home() { // spec: P4a-R21
        const core = makeCore()
        const shell = makeShell(core)
        const home = shell.docks.home.content
        const card = shell.openedCard
        verify(card === home.openedCard, "it is Home's own")
        let p = card
        while (p && p !== home)
            p = p.parent
        verify(p === home, "inside Home")
        const launch = fields => shell.handleLaunchJson(JSON.stringify(Object.assign({intent: "open", focus: true, activate: true}, fields)))
        launch({kind: "target", target: "C:\\art\\crane.png"})
        verify(card.visible, "it shows")
        compare(card.targetText, "C:\\art\\crane.png")
        compare(walk.problems(card), [])
        // Scrolled away, it comes back into view.
        home.scroller.contentHeight = 5000
        home.scroller.contentY = 400
        launch({kind: "target", target: "C:\\art\\b.png"})
        compare(home.scroller.contentY, 0)
        card.dismissButton.clicked()
        verify(!card.visible)

        // Home behind a tab: a launch brings it to the front.
        shell.docks.home.addDockWidgetAsTab(shell.docks.jobs)
        wait(100)
        shell.docks.jobs.setAsCurrentTab()
        tryVerify(() => !home.visible, 5000)
        launch({kind: "target", target: "C:\\art\\c.png", focus: false})
        tryVerify(() => card.visible && home.visible, 5000, "the card shows with Home in front")

        // Home closed: a launch that takes no focus opens it, and the focus stays where it was.
        card.dismissButton.clicked()
        shell.docks.home.forceClose()
        tryVerify(() => !shell.docks.home.isOpen, 5000)
        shell.requestActivate()
        tryVerify(() => shell.active, 2000)
        // A panel Home's tab won't cover.
        shell.docks.hardware.focusPanel()
        tryVerify(() => shell.activeFocusItem !== null, 2000)
        const holder = shell.activeFocusItem
        launch({kind: "refused", message: "This link isn't one LILITH opens: x", focus: false})
        tryVerify(() => shell.docks.home.isOpen && card.visible, 5000, "Home opens on the refusal")
        compare(card.message, "This link isn't one LILITH opens: x")
        verify(shell.activeFocusItem === holder, "focus: false moves no focus")
        // With the focus: it goes into Home, the card's buttons the next tab stops.
        launch({kind: "target", target: "C:\\art\\d.png", focus: true})
        tryVerify(() => shell.docks.home.panelHasFocus, 5000, "the focus goes into Home")
        verify(shell.activeFocusItem.nextItemInFocusChain(true) === card.copyButton, "and on to the card")
        shell.destroy()
    }

    function test_a_launch_before_home_has_shown_is_kept() { // spec: P4a-R21
        const core = makeCore()
        const shell = createTemporaryObject(shellComponent, tc, {core: core})
        verify(!shell.visible, "the window hasn't shown")
        shell.handleLaunchJson(JSON.stringify({kind: "lilref", target: "lil://x", title: "Crane", intent: "open", focus: true,
                                               activate: false}))
        shell.visible = true
        tryVerify(() => shell.docks.home.content && shell.docks.home.content.width > 0, 5000)
        tryVerify(() => shell.openedCard.visible, 5000, "the card shows with Home")
        compare(shell.openedCard.heading, "Crane")
        tryVerify(() => shell.docks.home.isOpen && shell.docks.home.content.onScreen, 5000)
        verify(shell.docks.home.content.Window.window === shell, "Home is docked in the window, not floated")
        shell.destroy()
    }

    function test_float_and_redock_keeps_home_state() { // spec: ARCH-116
        const dir = stateWith("ws1", seen)
        const core = makeCore()
        const shell = makeShell(core, dir)
        const dock = shell.docks.home
        const panel = dock.content
        const s = awayScene()
        settle(core, panel, s)
        shell.handleLaunchJson(JSON.stringify({kind: "target", target: "C:\\art\\crane.png", intent: "open", focus: false, activate: false}))
        verify(panel.awayCard.visible)
        const lines = lineTexts(panel.awayCard)
        const count = n => callsOf(core, n).length
        const reads = [count("projects.list"), count("assets.query"), count("project.status")]
        dock.isFloating = true
        tryVerify(() => dock.isFloating && panel.Window.window && panel.Window.window !== shell, 5000, "Home floats in its own window")
        verify(dock.content === panel, "the same panel item")
        verify(panel.openedCard.visible)
        compare(panel.openedCard.targetText, "C:\\art\\crane.png")
        compare(lineTexts(panel.awayCard), lines)
        compare(panel.projectsCard.rows.count, 2)
        tryVerify(() => panel.onScreen, 5000, "it is on screen in its own window")
        dock.isFloating = false
        tryVerify(() => panel.Window.window === shell, 5000, "Home docks back")
        verify(dock.content === panel, "still the same panel item")
        compare(lineTexts(panel.awayCard), lines)
        compare(panel.openedCard.targetText, "C:\\art\\crane.png")
        compare(panel.lastSeen, seen, "the card's time hasn't moved")
        compare([count("projects.list"), count("assets.query"), count("project.status")], reads, "nothing is read again")
        shell.destroy()
    }

    // --- Fix round 1. ---

    function test_a_retry_survives_a_read_that_does_not_cover_it() { // spec: APP-111
        for (const [name, refusedName] of [["cards", "projects.list"], ["jobs", "jobs.list"], ["counts", "assets.query"]]) {
            const core = makeCore()
            answered = {}
            const panel = makeHome(core, stateWith("ws1", seen))
            const refused = Shapes.refusal("store.io", "The index is busy.")
            settle(core, panel, scene({fail: {[refusedName]: refused}}))
            const card = refusedName === "jobs.list" ? panel.openJobs : refusedName === "assets.query" ? panel.recentAssets : panel.projectsCard
            verify(card.note.indexOf("Trying again") >= 0, card.note)
            // Well inside the retry's wait, an event reads something else.
            core.emit("confirmations", "event", Shapes.confirmationEvent("pending", Shapes.confirmation("cf_00000000000000b1", "comfy.run", "2026-10-30T13:00:01Z")))
            tryVerify(() => {
                answerAll(core, scene())
                return card.note.indexOf("Trying again") < 0 && card.note !== "Reading…"
            }, 5000, name + ": it is asked again, and the words go")
            panel.Window.window.destroy()
        }
    }

    function test_the_time_does_not_pass_a_job_being_read() { // spec: APP-111, P4a-R16
        const dir = stateWith("ws1", seen)
        const core = makeCore()
        const panel = makeHome(core, dir)
        const host = panel.Window.window
        settle(core, panel, scene())
        wait(2300)
        // A job ends now, and its read is slow.
        const job = Shapes.job("j_00000000000000e1", "failed", {updated: isoAgo(0), project: "sketchbook"})
        core.emit("jobs", "event", {kind: "job", job: job})
        tryVerify(() => panel._open > 0, 3000, "the read goes out")
        wait(1500)
        compare(keptTime(dir, "ws1"), seen, "the time waits for the read")
        host.visibility = Window.Minimized
        tryVerify(() => !panel.onScreen, 5000)
        answerAll(core, scene({jobs: [job]}))
        wait(3400)
        host.visibility = Window.Windowed
        tryVerify(() => panel.onScreen, 5000)
        settle(core, panel, scene({jobs: [job]}))
        verify(panel.awayCard.visible, "the job nobody saw is on the card")
        verify(lineTexts(panel.awayCard)[0].startsWith("Failed"), lineTexts(panel.awayCard))
    }

    function test_the_time_is_never_past_the_read_that_was_shown() { // spec: P4a-R16
        // A reload that hasn't landed holds it, whatever the wait.
        const dir = stateWith("ws1", seen)
        const core = makeCore()
        const panel = makeHome(core, dir)
        settle(core, panel, scene())
        core.emit("jobs", "resync_required", {})
        compare(panel._awayLoaded, false, "cleared as the read starts")
        wait(3400)
        compare(keptTime(dir, "ws1"), seen)
        // Once it lands, the wait, then one more read: the time is when that read began.
        const landing = Date.now()
        settle(core, panel, scene())
        keptMoved(core, scene(), dir, "ws1", seen, 4000)
        verify(Date.parse(keptTime(dir, "ws1")) >= landing + 2500, keptTime(dir, "ws1"))
    }

    function test_a_kept_time_never_has_a_fraction_of_exactly_000() { // spec: APP-111
        const core = makeCore()
        const panel = makeHome(core)
        // The core takes .000 as a whole second, and would lose a job later in that second.
        const ms = Date.parse("2026-10-30T12:00:05.000Z")
        compare(panel.seenStamp(ms), "2026-10-30T12:00:04.999Z")
        compare(panel.seenStamp(ms + 250), "2026-10-30T12:00:05.250Z")
    }

    function test_dismiss_while_the_jobs_read_is_failing_keeps_the_time() { // spec: APP-111
        const dir = stateWith("ws1", seen)
        const core = makeCore()
        const panel = makeHome(core, dir)
        const oldJob = Shapes.job("j_00000000000000e2", "succeeded", {updated: "2026-10-30T13:00:05Z"})
        settle(core, panel, scene({jobs: [oldJob]}))
        core.emit("jobs", "resync_required", {})
        settle(core, panel, scene({jobs: [oldJob], fail: {"jobs.list": Shapes.refusal("store.io", "busy")}}))
        const later = Shapes.job("j_00000000000000e3", "failed", {updated: isoAgo(5000)})
        verify(panel.awayCard.dismissButton.visible)
        panel.awayCard.dismissButton.clicked()
        compare(keptTime(dir, "ws1"), seen, "the time holds while the read fails")
        compare(lineTexts(panel.awayCard), [], "the line shown goes")
        tryVerify(() => {
            answerAll(core, scene({jobs: [oldJob, later]}))
            return panel.readState.jobs === "ok"
        }, 5000, "the read recovers")
        compare(lineTexts(panel.awayCard).length, 1, "only the job it never showed")
        verify(lineTexts(panel.awayCard)[0].startsWith("Failed"), lineTexts(panel.awayCard))
    }

    function test_dismiss_with_a_job_end_waiting_keeps_the_job() { // spec: APP-111
        const dir = stateWith("ws1", seen)
        const core = makeCore()
        const panel = makeHome(core, dir)
        const oldJob = Shapes.job("j_00000000000000e2", "succeeded", {updated: "2026-10-30T13:00:05Z"})
        settle(core, panel, scene({jobs: [oldJob]}))
        const later = Shapes.job("j_00000000000000e3", "failed", {updated: isoAgo(2000)})
        core.emit("jobs", "event", {kind: "job", job: later})
        panel.awayCard.dismissButton.clicked()
        tryVerify(() => {
            answerAll(core, scene({jobs: [oldJob, later]}))
            return panel.awayCard.lineRepeater.count === 1
        }, 3000, "the job it never showed comes")
        verify(lineTexts(panel.awayCard)[0].startsWith("Failed"), lineTexts(panel.awayCard))
    }

    function test_coming_back_reads_the_cards() { // spec: APP-111
        const core = makeCore()
        const panel = makeHome(core)
        settle(core, panel, scene())
        const host = panel.Window.window
        // A moment off screen, as a float: nothing.
        host.visibility = Window.Minimized
        tryVerify(() => !panel.onScreen, 5000)
        wait(100)
        let before = core.calls.length
        host.visibility = Window.Windowed
        tryVerify(() => panel.onScreen, 5000)
        wait(400)
        compare(core.calls.slice(before), [], "a brief hide reads nothing")
        // Longer: the cards are read, a moment later, the new ones shown.
        host.visibility = Window.Minimized
        tryVerify(() => !panel.onScreen, 5000)
        wait(1300)
        before = core.calls.length
        host.visibility = Window.Windowed
        tryVerify(() => panel.onScreen, 5000)
        compare(core.calls.slice(before), [], "coalesced, not at once")
        tryVerify(() => core.calls.slice(before).some(c => c.name === "projects.list"), 3000, "the cards are read")
        settle(core, panel, scene({projects: [{project: "new", name: "New", status: "active", assets: 1}]}))
        compare(panel.projectsCard.rows.count, 1)
        verify(core.calls.slice(before).some(c => c.name === "jobs.list" && c.args.status === "running"), "the jobs too")
    }

    function test_figures_read_as_old_until_a_reload_lands() { // spec: APP-111
        const core = makeCore()
        const panel = makeHome(core)
        const s = scene()
        settle(core, panel, s)
        const check = what => {
            verify(panel.isStale("projects"), what)
            verify(Qt.colorEqual(panel.projectsCard.rows.itemAt(0).color, Theme.textMuted), what)
            verify(Qt.colorEqual(panel.statusCounts.countsLabel.color, Theme.textMuted), what)
            verify(Qt.colorEqual(panel.recentAssets.rows.itemAt(0).color, Theme.textMuted), what)
            compare(panel.projectsCard.note, "Reading again, so this may be out of date.", what)
            compare(panel.openJobs.note, "Reading again, so this may be out of date.", what)
        }
        // Back from a loss, before anything has landed.
        core.applyState({connection: "reconnecting"})
        core.applyState({connection: "ready", workspace: wsOne})
        check("back on ready")
        settle(core, panel, s)
        verify(!panel.isStale("projects"))
        verify(Qt.colorEqual(panel.projectsCard.rows.itemAt(0).color, Theme.text))
        compare(panel.projectsCard.note, "")
        // A mark, the same.
        core.emit("jobs", "resync_required", {})
        check("on a resync")
        settle(core, panel, s)
        compare(panel.statusCounts.note, "")
        compare(panel.openJobs.note, "Nothing is running or queued.")
    }

    function test_a_read_from_before_a_mark_does_not_clear_it() { // spec: APP-111
        const core = makeCore()
        const panel = makeHome(core)
        const s = scene()
        settle(core, panel, s)
        core.emit("jobs", "event", {kind: "job", job: Shapes.job("j_0000000000000031", "succeeded")})
        tryVerify(() => callsOf(core, "projects.list").length === 2, 3000, "a read is out")
        core.emit("jobs", "resync_required", {})
        answerAll(core, s)
        verify(panel.isStale("projects"), "what landed was asked before the mark")
        settle(core, panel, s)
        verify(!panel.isStale("projects"))
    }

    function test_project_status_is_limited_after_job_ends() { // spec: APP-111
        const core = makeCore()
        const panel = makeHome(core)
        panel.statusGapMs = 1000
        const s = scene()
        settle(core, panel, s)
        wait(1100)
        const count = () => callsOf(core, "project.status").length
        const before = count()
        // The gap has passed: the first end reads the counts.
        core.emit("jobs", "event", {kind: "job", job: Shapes.job("j_0000000000000031", "succeeded")})
        tryVerify(() => count() === before + 1, 2000, "the first end reads them")
        settle(core, panel, s)
        // More ends within the gap: not one read each.
        const from = Date.now()
        for (let i = 0; i < 4; ++i) {
            core.emit("jobs", "event", {kind: "job", job: Shapes.job("j_000000000000004" + i, "succeeded")})
            answerFor(core, s, 100)
        }
        verify(Date.now() - from < 1000, "the test stayed within the gap")
        compare(count(), before + 1, "limited")
        // And a last one, so the counts settle.
        tryVerify(() => {
            answerAll(core, s)
            return count() === before + 2
        }, 3000, "the final read")
        answerFor(core, s, 1300)
        compare(count(), before + 2, "then nothing more")
    }

    function test_the_default_gap_is_30_s() { // spec: APP-111
        const panel = makeHome(makeCore())
        compare(panel.statusGapMs, 30000)
    }

    function test_away_lines_do_not_wait_for_the_cards() { // spec: APP-111
        const dir = stateWith("ws1", seen)
        const core = makeCore()
        const panel = makeHome(core, dir)
        const s = awayScene()
        // Only the jobs and the confirmations answer: the index is still being built.
        tryVerify(() => {
            for (const c of core.calls) {
                if (answered[c.id])
                    continue
                if (c.name === "jobs.list" || c.name === "app/confirmations.list") {
                    answered[c.id] = true
                    core.respond(c.id, c.name === "jobs.list" ? core.jobsListAnswer(c.args, s.jobs, [])
                                                              : Shapes.confirmations(s.pending))
                }
            }
            return panel.awayCard.visible
        }, 5000, "the card shows with the cards' reads out")
        compare(panel.awayCard.lineRepeater.count, 4)
        compare(panel.projectsCard.note, "Reading…")
        // And a job's end is read meanwhile.
        const fresh = Shapes.job("j_00000000000000c1", "failed", {updated: isoAgo(0)})
        core.emit("jobs", "event", {kind: "job", job: fresh})
        tryVerify(() => {
            for (const c of core.calls) {
                if (!answered[c.id] && c.name === "jobs.list") {
                    answered[c.id] = true
                    core.respond(c.id, core.jobsListAnswer(c.args, s.jobs.concat([fresh]), []))
                }
            }
            return panel.awayCard.lineRepeater.count === 5
        }, 3000, "the slow cards hold nothing up")
    }

    function test_tab_brings_a_control_into_view_with_a_ring_and_a_bar() { // spec: APP-156
        const dir = stateWith("ws1", seen)
        const core = makeCore()
        const panel = makeHome(core, dir)
        const host = panel.Window.window
        host.height = 200
        const jobs = []
        for (let i = 0; i < 25; ++i)
            jobs.push(Shapes.job("j_00000000000" + String(10000 + i), "succeeded", {updated: "2026-10-30T13:00:05Z"}))
        settle(core, panel, scene({jobs: jobs}))
        host.requestActivate()
        tryVerify(() => host.active, 3000)
        wait(100)
        verify(panel.scrollBar.visible, "the bar says there is more")
        panel.scroller.forceActiveFocus()
        verify(panel.focusRing.visible, "the ring shows the scroll area has the focus")
        keyClick(Qt.Key_Tab)
        const b = panel.awayCard.dismissButton
        tryVerify(() => b.activeFocus, 2000, "Tab reaches Dismiss")
        verify(!panel.focusRing.visible, "the ring goes with the focus")
        tryVerify(() => {
            const at = b.mapToItem(panel.scroller, 0, 0)
            return at.y >= 0 && at.y + b.height <= panel.scroller.height
        }, 2000, "the button is in view")
        // Back up: the card's top control comes into view again.
        panel.scroller.contentY = panel.scroller.contentHeight - panel.scroller.height
        panel.scroller.forceActiveFocus()
        compare(walk.problems(panel), [])
    }

    function test_a_kept_time_ahead_of_now_loses_no_line() { // spec: APP-111
        const future = "2099-01-01T00:00:00Z"
        const dir = stateWith("ws1", future)
        const core = makeCore()
        const panel = makeHome(core, dir)
        const job = Shapes.job("j_00000000000000e4", "failed", {updated: isoAgo(60000)})
        const old = Shapes.job("j_00000000000000e5", "failed", {updated: isoAgo(8 * 24 * 3600 * 1000)})
        settle(core, panel, scene({jobs: [job, old]}))
        verify(panel.lastSeen !== future, "never sent")
        verify(callsOf(core, "jobs.list").every(c => c.args.since !== future))
        verify(panel.awayCard.visible, "a job from a minute ago is listed")
        compare(panel.awayCard.lineRepeater.count, 1, "a week back, no further")
        keptMoved(core, scene({jobs: [job, old]}), dir, "ws1", future, 4000, "the kept time comes back to now")
        verify(Date.parse(keptTime(dir, "ws1")) <= Date.now())
        compare(panel.awayCard.lineRepeater.count, 1, "and the line stays")
    }

    function test_a_since_read_past_its_limit_says_so_and_holds_the_time() { // spec: APP-111
        const dir = stateWith("ws1", seen)
        const core = makeCore()
        const panel = makeHome(core, dir)
        const jobs = []
        // Nothing the card lists in the pages it reads.
        for (let i = 0; i < panel.awayPages * 100 + 1; ++i)
            jobs.push(Shapes.job("j_0000000000" + String(100000 + i), "cancelled", {created: "2026-10-30T14:00:00Z", updated: "2026-10-30T14:00:05Z"}))
        jobs.push(Shapes.job("j_00000000000000f1", "failed", {created: "2026-10-30T12:30:00Z", updated: "2026-10-30T13:00:05Z"}))
        settle(core, panel, scene({jobs: jobs}))
        verify(panel.awayCut)
        compare(panel.awayItems, [])
        verify(panel.awayCard.visible, "the card says there is more")
        verify(panel.awayCard.moreLabel.visible && panel.awayCard.moreText.indexOf("than this card reads") >= 0, panel.awayCard.moreText)
        wait(3400)
        compare(keptTime(dir, "ws1"), seen, "the time doesn't pass pages it didn't read")
        verify(panel.awayCard.dismissButton.visible, "you can say you've seen that")
        panel.dismissAway()
        verify(!panel.awayCard.visible)
        verify(keptTime(dir, "ws1") !== seen)
    }

    function test_status_words_are_true_when_the_projects_read_failed() { // spec: APP-111
        const core = makeCore()
        core.project = ""
        const panel = makeHome(core)
        const unknown = Shapes.refusal("project.unknown", "There is no project.")
        settle(core, panel, scene({status: unknown, fail: {"projects.list": Shapes.refusal("store.io", "busy")}}))
        verify(panel.projectsCard.note.indexOf("Couldn't read") === 0, panel.projectsCard.note)
        compare(panel.statusCounts.note, "This workspace has no current project to count.")
    }

    function test_times_that_do_not_read_show_no_time() { // spec: APP-111
        const core = makeCore()
        const panel = makeHome(core)
        settle(core, panel, scene())
        for (const v of ["", "garbage", "12", "1", "2026", "2026-13-45T00:00:00Z", "2026-02-31T00:00:00Z", "2026-10-30",
                         "2026-10-30T13:00:05", "10/30/2026", "Sat Oct 31 2026", null, undefined, 5])
            compare(panel.localStamp(v), "", "no time for " + v)
        compare(panel.localStamp("2026-10-30T13:00:05.250Z"), stamp("2026-10-30T13:00:05Z"))
        compare(panel.localStamp("2026-10-30T15:00:05+02:00"), stamp("2026-10-30T13:00:05Z"))
        core.emit("jobs", "resync_required", {})
        settle(core, panel, scene({assets: [Shapes.asset("sketchbook", "a", "approved", null), Shapes.asset("sketchbook", "b", "approved", "12")]}))
        compare(rowTexts(panel.recentAssets.rows), ["a · sketchbook · Approved", "b · sketchbook · Approved"])
    }

    function test_a_kept_time_the_core_would_refuse_is_never_sent() { // spec: P4a-R16
        for (const bad of ["2026-02-31T00:00:00Z", "2026-10-30T12:00:60Z", "2026-10-30T12:00:00z"]) {
            const dir = stateWith("ws1", bad)
            const core = makeCore()
            answered = {}
            const panel = makeHome(core, dir)
            settle(core, panel, awayScene())
            compare(panel.lastSeen, "", bad)
            verify(callsOf(core, "jobs.list").every(c => c.args.since === undefined), bad)
            panel.Window.window.destroy()
        }
    }

    function test_focus_after_dismiss_stays_in_home() { // spec: APP-156
        const dir = stateWith("ws1", seen)
        const core = makeCore()
        const panel = makeHome(core, dir)
        const host = panel.Window.window
        settle(core, panel, awayScene())
        host.requestActivate()
        tryVerify(() => host.active, 3000)
        panel.openedCard.show({target: "C:\\art\\x.png", intent: "open"})
        wait(100)
        panel.awayCard.dismissButton.forceActiveFocus()
        keyClick(Qt.Key_Space)
        verify(!panel.awayCard.visible)
        tryVerify(() => panel.scroller.activeFocus, 2000, "the focus goes to the scroll area: " + host.activeFocusItem)
        panel.openedCard.dismissButton.forceActiveFocus()
        keyClick(Qt.Key_Space)
        verify(!panel.openedCard.visible)
        tryVerify(() => panel.scroller.activeFocus, 2000, "and from the Opened card: " + host.activeFocusItem)
    }

    function test_a_focus_launch_while_the_default_layout_waits_gets_the_focus() { // spec: P4a-R21
        const core = makeCore()
        const shell = createTemporaryObject(shellComponent, tc, {core: core})
        shell.handleLaunchJson(JSON.stringify({kind: "target", target: "C:\\art\\x.png", intent: "open", focus: true, activate: false}))
        shell.visible = true
        tryVerify(() => shell.docks.home.content && shell.docks.home.content.onScreen, 5000)
        shell.requestActivate()
        tryVerify(() => shell.docks.home.panelHasFocus, 5000, "Home takes the focus the launch asked for")
        shell.destroy()
    }

    function test_a_job_with_an_unknown_outcome_does_not_say_running() { // spec: APP-111
        const core = makeCore()
        const panel = makeHome(core)
        settle(core, panel, scene({jobs: [Shapes.job("j_0000000000000001", "running", {substatus: "interrupted_unknown", project: "sketchbook"})]}))
        const view = panel.openJobs.view
        tryVerify(() => !!view.itemAtIndex(0), 5000)
        compare(view.itemAtIndex(0).text, "ComfyUI prompt · sketchbook · Outcome unknown")
    }

    // --- Fix round 2. ---

    function test_a_job_watched_ending_does_not_come_back_after_you_leave() { // spec: APP-111
        const dir = stateWith("ws1", seen)
        const core = makeCore()
        const panel = makeHome(core, dir)
        const host = panel.Window.window
        const empty = scene()
        settle(core, panel, empty)
        keptMoved(core, empty, dir, "ws1", seen, 4000)
        const first = keptTime(dir, "ws1")
        // A job ends while Home shows; its line shows for its 3 s.
        const job = Shapes.job("j_00000000000000e1", "failed", {updated: isoAgo(0), project: "sketchbook"})
        const s = scene({jobs: [job]})
        core.emit("jobs", "event", {kind: "job", job: job})
        tryVerify(() => {
            answerAll(core, s)
            return panel.awayCard.lineRepeater.count === 1
        }, 3000, "it joins the card")
        keptMoved(core, s, dir, "ws1", first, 5000)
        // The core's `since` lists the second before a fractional time and on: past the job's.
        const keptSecond = Math.floor(Date.parse(keptTime(dir, "ws1")) / 1000)
        verify(keptSecond - 1 >= Date.parse(job.updated) / 1000, keptTime(dir, "ws1") + " against " + job.updated)
        host.visibility = Window.Minimized
        tryVerify(() => !panel.onScreen, 5000)
        wait(3400)
        host.visibility = Window.Windowed
        tryVerify(() => panel.onScreen, 5000)
        settle(core, panel, s)
        compare(lineTexts(panel.awayCard), [], "it doesn't come back")
        verify(!panel.awayCard.visible)
    }

    function test_the_time_waits_for_one_more_read() { // spec: APP-111, P4a-R16
        const dir = stateWith("ws1", seen)
        const core = makeCore()
        const panel = makeHome(core, dir)
        const s = scene()
        settle(core, panel, s)
        const before = callsOf(core, "jobs.list").length
        tryVerify(() => callsOf(core, "jobs.list").length > before, 3500, "one more jobs read at the 3 s")
        const startedAt = Date.now()
        wait(800)
        compare(keptTime(dir, "ws1"), seen, "nothing is written until it lands")
        answerAll(core, s)
        tryVerify(() => {
            answerAll(core, s)
            return keptTime(dir, "ws1") !== seen
        }, 1000, "it lands, and the time is written at once")
        const at = Date.parse(keptTime(dir, "ws1"))
        verify(at >= startedAt - 100 && at <= startedAt + 100, "when that read began: " + keptTime(dir, "ws1"))
    }

    function test_one_more_read_with_a_new_line_waits_again() { // spec: APP-111
        const dir = stateWith("ws1", seen)
        const core = makeCore()
        const panel = makeHome(core, dir)
        settle(core, panel, scene())
        const before = callsOf(core, "jobs.list").length
        tryVerify(() => callsOf(core, "jobs.list").length > before, 3500, "one more jobs read")
        // It brings a line nobody has been shown.
        const job = Shapes.job("j_00000000000000e1", "failed", {updated: isoAgo(0)})
        const s = scene({jobs: [job]})
        answerAll(core, s)
        tryVerify(() => panel._open === 0 || panel.awayCard.lineRepeater.count === 1, 2000)
        answerFor(core, s, 300)
        compare(panel.awayCard.lineRepeater.count, 1)
        compare(keptTime(dir, "ws1"), seen, "not written")
        answerFor(core, s, 1800)
        compare(keptTime(dir, "ws1"), seen, "the wait starts again")
        keptMoved(core, s, dir, "ws1", seen, 3000, "then it is")
    }

    function test_one_more_read_that_fails_moves_nothing() { // spec: P4a-R16
        const dir = stateWith("ws1", seen)
        const core = makeCore()
        const panel = makeHome(core, dir)
        const s = scene()
        settle(core, panel, s)
        const before = callsOf(core, "jobs.list").length
        tryVerify(() => callsOf(core, "jobs.list").length > before, 3500, "one more jobs read")
        const bad = scene({fail: {"jobs.list": Shapes.refusal("store.io", "busy")}})
        answerAll(core, bad)
        compare(panel.readState.jobs, "failed")
        compare(keptTime(dir, "ws1"), seen)
        answerFor(core, bad, 500)
        compare(keptTime(dir, "ws1"), seen, "while it fails")
        keptMoved(core, s, dir, "ws1", seen, 6000, "and once it reads again, after the wait")
    }

    function test_one_more_read_landing_off_screen_moves_nothing() { // spec: P4a-R16
        const dir = stateWith("ws1", seen)
        const core = makeCore()
        const panel = makeHome(core, dir)
        const host = panel.Window.window
        const s = scene()
        settle(core, panel, s)
        const before = callsOf(core, "jobs.list").length
        tryVerify(() => callsOf(core, "jobs.list").length > before, 3500, "one more jobs read")
        host.visibility = Window.Minimized
        tryVerify(() => !panel.onScreen, 5000)
        answerAll(core, s)
        answerFor(core, s, 300)
        compare(keptTime(dir, "ws1"), seen, "nobody was there")
        host.visibility = Window.Windowed
        tryVerify(() => panel.onScreen, 5000)
        keptMoved(core, s, dir, "ws1", seen, 5000, "back, after the wait")
    }

    function test_dismiss_also_ends_past_the_second_of_the_read() { // spec: APP-111
        const dir = stateWith("ws1", seen)
        const core = makeCore()
        const panel = makeHome(core, dir)
        const job = Shapes.job("j_00000000000000e1", "failed", {updated: isoAgo(0)})
        const s = scene({jobs: [job]})
        settle(core, panel, s)
        panel.dismissAway()
        const dismissed = keptTime(dir, "ws1")
        verify(dismissed !== seen, "it moves at once")
        // A later start has forgotten what was dismissed: the time goes past the job's second too.
        keptMoved(core, s, dir, "ws1", dismissed, 5000, "and on, after the wait")
        const keptSecond = Math.floor(Date.parse(keptTime(dir, "ws1")) / 1000)
        verify(keptSecond - 1 >= Date.parse(job.updated) / 1000, keptTime(dir, "ws1") + " against " + job.updated)
    }

    function test_counts_are_read_again_once_the_switcher_has_set_the_project() { // spec: APP-111, ARCH-088
        const core = makeCore()
        const shell = makeShell(core)
        const panel = shell.docks.home.content
        const sketch = Shapes.projectStatus("sketchbook", {candidate: 1})
        settle(core, panel, scene({status: sketch}))
        compare(panel.statusCounts.title, "Assets in sketchbook")
        core.applyState({connection: "reconnecting"})
        core.applyState({connection: "ready", workspace: wsOne})
        // The new session: Home's read is answered first, by the hint another client has moved.
        settle(core, panel, scene({status: Shapes.projectStatus("lantern", {stale: 3})}))
        compare(panel.statusCounts.title, "Assets in lantern")
        const before = callsOf(core, "project.status").length
        const set = core.calls.filter(c => c.name === "app/set_context" && c.args.project === "sketchbook" && !answered[c.id]).pop()
        verify(set, "the switcher sends its project again")
        answered[set.id] = true
        verify(core.respond(set.id, Shapes.context("ws1", "sketchbook", "sketchbook")))
        tryVerify(() => callsOf(core, "project.status").length === before + 1, 3000, "the counts are read again")
        compare(callsOf(core, "project.status")[before].args, {}, "never naming it")
        settle(core, panel, scene({status: sketch}))
        compare(panel.statusCounts.title, "Assets in sketchbook")
        shell.destroy()
    }

    function test_a_failing_status_read_stays_on_the_backoff() { // spec: APP-111
        const core = makeCore()
        const panel = makeHome(core, stateWith("ws1", seen))
        const bad = scene({fail: {"project.status": Shapes.refusal("store.io", "The walk failed.")}})
        settle(core, panel, bad)
        verify(panel.statusCounts.note.indexOf("Trying again") >= 0, panel.statusCounts.note)
        const count = () => callsOf(core, "project.status").length
        const n = count()
        // Reads of anything else go on, and don't walk the folder again.
        const others = callsOf(core, "app/confirmations.list").length
        for (let i = 0; i < 3; ++i) {
            core.emit("confirmations", "event", Shapes.confirmationEvent("pending", Shapes.confirmation("cf_00000000000000b1", "comfy.run", "2026-10-30T13:00:01Z")))
            answerFor(core, bad, 200)
        }
        verify(callsOf(core, "app/confirmations.list").length > others, "the confirmations are read")
        compare(count(), n, "the counts wait for their backoff")
        tryVerify(() => {
            answerAll(core, bad)
            return count() === n + 1
        }, 2000, "and then are asked again")
        // The next wait is longer.
        const again = Date.now()
        tryVerify(() => {
            answerAll(core, bad)
            return count() === n + 2
        }, 4000, "after a longer wait")
        verify(Date.now() - again >= 1500, String(Date.now() - again))
    }

    function test_lines_that_hide_rows_say_where_they_are() { // spec: APP-111
        const dir = stateWith("ws1", seen)
        const core = makeCore()
        const panel = makeHome(core, dir)
        const jobs = []
        for (let i = 0; i < 25; ++i)
            jobs.push(Shapes.job("j_00000000000" + String(10000 + i), "succeeded", {updated: "2026-10-30T13:00:05Z"}))
        // An older confirmation sorts below them, so it is one of the rows hidden.
        const pending = [Shapes.confirmation("cf_00000000000000b1", "comfy.run", "2026-10-30T12:30:00Z")]
        settle(core, panel, scene({jobs: jobs, pending: pending}))
        compare(panel.awayCard.moreLabel.text, "and 6 more. Jobs and Confirmations list them.")
    }

    function test_a11y_walk_passes() { // spec: APP-156
        const dir = stateWith("ws1", seen)
        const core = makeCore()
        const panel = makeHome(core, dir)
        settle(core, panel, awayScene({jobs: awayScene().jobs.concat([Shapes.job("j_0000000000000099", "running")])}))
        panel.openedCard.show({target: "C:\\art\\crane.png", intent: "open"})
        verify(panel.awayCard.visible && panel.openedCard.visible)
        compare(walk.problems(panel), [])
        // And with no workspace.
        const bare = makeCore(null)
        answered = {}
        const empty = makeHome(bare)
        settle(bare, empty, scene())
        compare(walk.problems(empty), [])
    }
}
