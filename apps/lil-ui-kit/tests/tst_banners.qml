// The shell's connection and no-workspace banners (APP-016, APP-034, APP-036, ARCH-100).
import QtQuick
import QtTest
import Lilith.Shim
import Lilith.UiKit

TestCase {
    id: tc
    name: "Banners"
    when: windowShown
    width: 200
    height: 100

    readonly property var workspaceOne: ({id: "ws1", path: "D:/art/harbor"})

    Component {
        id: fakeComponent
        FakeCore {}
    }

    Component {
        id: shellComponent
        ShellWindow {}
    }

    A11yWalk { id: walk }

    property var answered: ({})
    // The shells go before their FakeCore, so their subscriptions end on a live core.
    property var windows: []

    function init() {
        answered = {}
        tryVerify(() => {
            const kddw = JSON.parse(DockLayout.serialize())
            return DockLayout.mainWindows().length === 0
                   && !(Array.isArray(kddw.allDockWidgets) && kddw.allDockWidgets.length > 0)
        }, 5000, "the earlier shell's docks are gone")
    }

    function cleanup() {
        for (const w of windows)
            w.destroy()
        windows = []
        wait(0)
    }

    function ok(structured) {
        return {status: "ok", structured: structured}
    }

    function refusal(code, message) {
        return {status: "error", error: {code: code, category: "user_fixable", message: message,
                                         trace_id: "t_test", retry: "never"}}
    }

    function open(core, test) {
        return core.calls.filter(c => !answered[c.id] && test(c))
    }

    function answer(core, test, outcome, what) {
        tryVerify(() => open(core, test).length > 0, 2000, what || "the call")
        const c = open(core, test)[0]
        answered[c.id] = true
        verify(core.respond(c.id, outcome), "request " + c.id + " is open")
        return c
    }

    function named(name) {
        return c => c.name === name
    }

    // A shell, laid out, with its reads answered, so that its actions are all live.
    function load(workspace) {
        const core = createTemporaryObject(fakeComponent, tc, {workspace: workspace === undefined ? workspaceOne : workspace})
        const shell = createTemporaryObject(shellComponent, tc, {core: core})
        windows.push(shell)
        shell.visible = true
        tryVerify(() => shell.layoutSettled, 5000, "the docks are laid out")
        for (let n = 0; n < 20 && open(core, named("jobs.list")).length > 0; ++n)
            answer(core, named("jobs.list"), ok({jobs: [], next_cursor: null, lanes: []}))
        tryVerify(() => shell.topBar.jobs.loaded)
        return {core: core, shell: shell}
    }

    // Every item under `item` marked `requiresCore: true`.
    function actionsOf(item, out) {
        const list = out || []
        if (item.requiresCore === true)
            list.push(item)
        for (let i = 0; i < item.children.length; ++i)
            actionsOf(item.children[i], list)
        return list
    }

    function test_reconnecting_disables_actions() { // spec: APP-034, ARCH-100
        const t = load()
        const banner = t.shell.connectionBanner
        // What shows now (the no-workspace banner's button doesn't).
        const actions = actionsOf(t.shell.contentItem).filter(a => a.visible)
        verify(actions.length >= 3, "the switchers and the pause: " + actions.length)
        for (const a of actions)
            verify(a.enabled, "live while ready: " + a.text)
        verify(!banner.visible)

        // An open list goes with the connection.
        const sw = t.shell.topBar.workspaceSwitcher
        mouseClick(sw.button)
        tryVerify(() => sw.popup.opened)

        // In the same turn, so within the frame.
        t.core.connection = "reconnecting"
        verify(banner.visible, "the banner shows at once")
        verify(banner.text.startsWith("Reconnecting…"), banner.text)
        compare(banner.kind, "warning")
        for (const a of actions)
            verify(!a.enabled, "disabled at once: " + a.text)
        compare(t.shell.topBar.connectionIndicator.text, "Reconnecting…")
        verify(!banner.closable, "it goes when the state does")
        tryVerify(() => !sw.popup.visible, 2000, "the list closes")
        tryVerify(() => t.shell.dockingArea.y >= banner.y + banner.height, 2000, "the docks move under the banner")
        compare(walk.report(banner), [])

        t.core.connection = "ready"
        verify(!banner.visible, "gone at once")
        for (const a of actions)
            verify(a.enabled, "back at once: " + a.text)
        tryVerify(() => t.shell.dockingArea.y < banner.y + banner.height || !banner.visible)
    }

    function test_incompatible_says_restart_the_core_when_it_is_idle() { // spec: APP-036, ARCH-023
        const t = load()
        const banner = t.shell.connectionBanner
        t.core.coreProtocol = "2.0"
        t.core.connection = "incompatible"
        verify(banner.visible)
        verify(banner.text.indexOf("Restart the core when it's idle") >= 0, banner.text)
        verify(banner.text.indexOf("protocol 2.0") >= 0, banner.text)
        compare(banner.kind, "danger")
        verify(!banner.retryButton.visible, "the session goes on trying by itself")
        for (const a of actionsOf(t.shell.contentItem))
            verify(!a.enabled, "disabled: " + a.text)
        compare(t.shell.topBar.connectionIndicator.text, "Core incompatible")
        compare(walk.report(banner), [])
    }

    function test_unavailable_offers_try_again() { // spec: ARCH-100, APP-034
        const t = load()
        const banner = t.shell.connectionBanner
        t.core.connection = "unavailable"
        verify(banner.visible)
        verify(banner.retryButton.visible)
        compare(banner.retryButton.text, "Try again")
        compare(t.core.retries, 0)
        waitForItemPolished(banner.retryButton.parent)
        wait(50)
        mouseClick(banner.retryButton)
        compare(t.core.retries, 1, "Core.retryNow")
        for (const a of actionsOf(t.shell.contentItem))
            verify(!a.enabled, "disabled: " + a.text)
        compare(walk.report(banner), [])

        t.core.connection = "ready"
        verify(!banner.visible)
    }

    function test_connecting_shows_no_banner() { // spec: APP-033
        const t = load()
        t.core.connection = "connecting"
        verify(!t.shell.connectionBanner.visible, "a start or a switch doesn't flash a banner")
        verify(!t.shell.noWorkspaceBanner.visible, "and no workspace is known yet")
        for (const a of actionsOf(t.shell.contentItem))
            verify(!a.enabled, "disabled: " + a.text)
        compare(t.shell.topBar.connectionIndicator.text, "Connecting…")
    }

    function test_no_workspace_banner_and_the_create_flow() { // spec: APP-016
        const t = load(null)
        const banner = t.shell.noWorkspaceBanner
        verify(banner.visible)
        compare(banner.createButton.text, "Create or choose a workspace")
        verify(banner.createButton.enabled)
        compare(walk.report(banner), [])

        banner.folderChosen("file:///D:/art/new%20place")
        const create = t.core.calls.filter(named("workspace.create"))
        compare(create.length, 1)
        compare(create[0].args, {path: "D:/art/new place"}, "the chosen folder, and nothing about the default")
        verify(banner.visible, "not before the hello")

        answer(t.core, named("workspace.create"), ok({id: "ws9", path: "D:/art/new place", created: true, default: true,
                                                      warnings: ["It is a network folder."]}))
        // Its warning stays for you to read, though the banner is going.
        tryVerify(() => t.shell.topBar.notice.visible)
        compare(t.shell.topBar.notice.text, "It is a network folder.")
        compare(t.shell.topBar.notice.kind, "warning")

        const hello = open(t.core, c => c.kind === "setWorkspace")
        compare(hello.length, 1, "a new hello names the new workspace")
        compare(hello[0].name, "ws9")
        verify(banner.visible, "still, until the hello's workspace is in")
        answered[hello[0].id] = true
        t.core.respond(hello[0].id, {status: "ok", workspace: {id: "ws9", path: "D:/art/new place"}})
        t.core.workspace = {id: "ws9", path: "D:/art/new place"}
        verify(!banner.visible, "the banner leaves")
        verify(t.shell.topBar.projectSwitcher.button.enabled, "projects can be chosen now")
        compare(t.core.calls.filter(named("workspace.set_default")).length, 0)
    }

    function test_no_workspace_refusals_stay_on_the_banner() { // spec: APP-016
        const t = load(null)
        const banner = t.shell.noWorkspaceBanner
        banner.folderChosen("file:///C:/Program%20Files/LILITH")
        answer(t.core, named("workspace.create"), refusal("workspace.path_invalid", "<b>Not</b> inside the install folder."))
        verify(banner.visible)
        verify(banner.text.indexOf("<b>Not</b> inside the install folder.") >= 0, banner.text)
        compare(banner.kind, "danger")
        compare(open(t.core, c => c.kind === "setWorkspace").length, 0)

        // A hello the core refuses too (another workspace is the default).
        banner.folderChosen("file://server/share/art")
        const create = t.core.calls.filter(named("workspace.create")).pop()
        compare(create.args, {path: "//server/share/art"}, "a network folder's path")
        compare(banner.kind, "warning", "the old refusal goes with a new try")
        answer(t.core, named("workspace.create"), ok({id: "ws9", path: "//server/share/art", created: true, default: false,
                                                      warnings: []}))
        const hello = open(t.core, c => c.kind === "setWorkspace")[0]
        answered[hello.id] = true
        t.core.respond(hello.id, refusal("workspace.unsupported", "This core serves another one."))
        verify(banner.visible)
        verify(banner.text.indexOf("This core serves another one.") >= 0, banner.text)
    }

    function test_no_workspace_banner_needs_a_connection() { // spec: APP-016, APP-034
        const t = load(null)
        const banner = t.shell.noWorkspaceBanner
        verify(banner.visible)
        t.core.connection = "reconnecting"
        verify(!banner.visible, "while disconnected, no workspace isn't known")
        verify(!banner.createButton.enabled)
        t.core.connection = "ready"
        verify(banner.visible)
        verify(banner.createButton.enabled)
    }

    function test_banners_stack_above_the_docks() { // spec: APP-001
        const t = load(null)
        const shell = t.shell
        t.core.workspace = null
        verify(shell.noWorkspaceBanner.visible)
        tryVerify(() => shell.dockingArea.y >= shell.noWorkspaceBanner.mapToItem(shell.contentItem, 0, 0).y
                        + shell.noWorkspaceBanner.height, 2000)
        // The shell's own banners stay under them.
        shell.layoutBanner.show("A layout message", false)
        tryVerify(() => shell.layoutBanner.y >= shell.noWorkspaceBanner.mapToItem(shell.contentItem, 0, 0).y
                        + shell.noWorkspaceBanner.height, 2000)
        tryVerify(() => shell.dockingArea.y >= shell.layoutBanner.y + shell.layoutBanner.height, 2000)
    }
}
