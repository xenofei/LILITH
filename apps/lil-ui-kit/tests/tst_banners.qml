// The shell's connection and no-workspace banners (APP-016, APP-034, APP-036, ARCH-100). A state
// goes in as Core applies one (FakeCore.applyState), and what the banners announce is what a screen
// reader would hear.
import QtQuick
import QtTest
import Lilith.Shim
import Lilith.UiKit
import "CoreShapes.js" as Shapes

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

    Component {
        id: spyComponent
        SignalSpy {}
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
    function load(workspace, setup) {
        const core = createTemporaryObject(fakeComponent, tc, Object.assign(
            {workspace: workspace === undefined ? workspaceOne : workspace}, setup || {}))
        const shell = createTemporaryObject(shellComponent, tc, {core: core})
        windows.push(shell)
        shell.visible = true
        tryVerify(() => shell.layoutSettled, 5000, "the docks are laid out")
        if (core.connection === "ready") {
            for (let n = 0; n < 20 && open(core, named("jobs.list")).length > 0; ++n)
                answer(core, named("jobs.list"), Shapes.jobsPage([], []))
            tryVerify(() => shell.topBar.jobs.current)
        }
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
        // What shows now (the no-workspace banner's button doesn't), including an open list's.
        const sw = t.shell.topBar.workspaceSwitcher
        mouseClick(sw.button)
        tryVerify(() => sw.popup.opened)
        answer(t.core, named("workspaces.list"), Shapes.tool({workspaces: Shapes.workspaceRows()}))
        tryVerify(() => sw.popup.view.count === 6 && actionsOf(sw.popup.contentItem).some(a => a.visible), 2000,
                  "the row's own button")
        const inList = actionsOf(sw.popup.contentItem).filter(a => a.visible)
        const inBar = actionsOf(t.shell.contentItem).filter(a => a.visible)
        verify(inBar.length >= 3, "the switchers and the pause: " + inBar.length)
        verify(inList.length >= 1, "Make this the default")
        const actions = inBar.concat(inList)
        for (const a of actions)
            verify(a.enabled, "live while ready: " + a.text)
        verify(!banner.visible)
        const dockTop = t.shell.dockingArea.y

        // In the same turn, so within the frame.
        t.core.applyState({connection: "reconnecting"})
        verify(banner.visible, "the banner shows at once")
        verify(banner.text.startsWith("Reconnecting…"), banner.text)
        compare(banner.kind, "warning")
        for (const a of actions)
            verify(!a.enabled, "disabled at once: " + a.text)
        compare(t.shell.topBar.connectionIndicator.text, "Reconnecting…")
        verify(!banner.closable, "it goes when the state does")
        tryVerify(() => !sw.popup.visible, 2000, "the list closes")
        tryVerify(() => t.shell.dockingArea.y >= banner.mapToItem(t.shell.contentItem, 0, 0).y + banner.height, 2000,
                  "the docks move under the banner")
        compare(walk.report(banner), [])

        t.core.applyState({connection: "ready", workspace: workspaceOne})
        verify(!banner.visible, "gone at once")
        // The switchers are back at once; each pause control also waits for its fresh read.
        for (const a of inBar.filter(a => a.text.indexOf("GPU work") < 0 && a.text !== "Pause"))
            verify(a.enabled, "back at once: " + a.text)
        verify(!t.shell.topBar.pauseButton.enabled, "the pause also waits for its fresh read")
        for (let n = 0; n < 20 && open(t.core, named("jobs.list")).length > 0; ++n)
            answer(t.core, named("jobs.list"), Shapes.jobsPage([], []))
        tryVerify(() => t.shell.topBar.pauseButton.enabled)
        tryCompare(t.shell.dockingArea, "y", dockTop, 2000, "the docks come back")
    }

    function test_incompatible_says_restart_the_core_when_it_is_idle() { // spec: APP-036, ARCH-023
        const t = load()
        const banner = t.shell.connectionBanner
        t.core.applyState({connection: "incompatible", coreProtocol: "2.0"})
        verify(banner.visible)
        verify(banner.text.indexOf("Restart the core when it's idle") >= 0, banner.text)
        verify(banner.text.indexOf("lil core stop") >= 0, "it says how: " + banner.text)
        verify(banner.text.indexOf("reconnects by itself") >= 0, banner.text)
        verify(banner.text.indexOf("protocol 2.0") >= 0, banner.text)
        compare(banner.kind, "danger")
        verify(!banner.retryButton.visible, "the session goes on trying by itself")
        for (const a of actionsOf(t.shell.contentItem))
            verify(!a.enabled, "disabled: " + a.text)
        compare(t.shell.topBar.connectionIndicator.text, "Core incompatible")
        compare(walk.report(banner), [])
    }

    function test_a_screen_reader_hears_only_what_is_true() { // spec: APP-156, APP-033, APP-036
        // A first connection with a workspace announces nothing.
        const t = load(null, {connection: "connecting"})
        const conn = createTemporaryObject(spyComponent, tc, {target: t.shell.connectionBanner, signalName: "announced"})
        const none = createTemporaryObject(spyComponent, tc, {target: t.shell.noWorkspaceBanner, signalName: "announced"})
        t.core.applyState({connection: "ready", coreProtocol: "1.2", workspace: workspaceOne})
        compare(none.count, 0, "no false \"No workspace is open\"")
        compare(conn.count, 0)
        verify(!t.shell.noWorkspaceBanner.visible)

        // A first `incompatible` says the protocol, once.
        const u = load(null, {connection: "connecting", coreProtocol: ""})
        const heard = createTemporaryObject(spyComponent, tc, {target: u.shell.connectionBanner, signalName: "announced"})
        u.core.applyState({connection: "incompatible", coreProtocol: "2.0"})
        compare(heard.count, 1, "once")
        verify(String(heard.signalArguments[0][0]).indexOf("protocol 2.0") >= 0, heard.signalArguments[0][0])

        // A first connection with no workspace does say so, once.
        const v = load(null, {connection: "connecting"})
        const noWorkspace = createTemporaryObject(spyComponent, tc, {target: v.shell.noWorkspaceBanner, signalName: "announced"})
        v.core.applyState({connection: "ready", coreProtocol: "1.2", workspace: null})
        compare(noWorkspace.count, 1)
    }

    function test_unavailable_shows_its_cause_and_offers_try_again() { // spec: ARCH-100, APP-034
        const t = load()
        const banner = t.shell.connectionBanner
        t.core.applyState({connection: "unavailable", unavailable: Shapes.otherCause("start_failed", "The core exited: the store is locked.")})
        verify(banner.visible)
        compare(banner.text, "The core isn't available: The core exited: the store is locked.")
        verify(banner.retryButton.visible)
        verify(!banner.defaultButton.visible)
        compare(banner.retryButton.text, "Try again")
        compare(t.core.retries, 0)
        waitForItemPolished(banner.retryButton.parent)
        wait(50)
        mouseClick(banner.retryButton)
        compare(t.core.retries, 1, "Core.retryNow")
        for (const a of actionsOf(t.shell.contentItem))
            verify(!a.enabled, "disabled: " + a.text)
        compare(walk.report(banner), [])

        t.core.applyState({connection: "ready", workspace: workspaceOne})
        verify(!banner.visible)

        // A refusal marked `never` can't be retried.
        t.core.applyState({connection: "unavailable", unavailable: {reason: "No.", cause: "refused", code: "core.refused", retry: "never"}})
        verify(!banner.retryButton.visible)

        // A failed proof, in `ClientError::Untrusted`'s words: the cut at 300 only costs the path.
        t.core.applyState({connection: "unavailable", unavailable: Shapes.otherCause("untrusted", "The LILITH core pipe names pid 4294967295 as server, but its holder didn't prove that, so it isn't trusted. In another Windows session, use the core's session. An older build can't prove it: its lil core stop works, it ends once nothing is connected to it and it has been idle, or use Task Manager. That pid is C:\\" + "long folder\\".repeat(30) + "lil-core.exe.")})
        verify(banner.text.indexOf("Task Manager.") >= 0, banner.text)
        verify(banner.text.indexOf("lil-core.exe") < 0, banner.text)
    }

    function test_a_refused_workspace_says_so_and_has_a_way_out() { // spec: ARCH-100, ARCH-090
        // The app runs on A, someone runs `lil workspace use B` in a terminal, and the connection
        // drops: the hello names A, the core refuses it, and the session waits.
        const t = load()
        const banner = t.shell.connectionBanner
        const message = "This core serves only the default workspace, \"D:/art/quay\". To use \"D:/art/harbor\" instead, make it the default with `lil workspace use \"D:/art/harbor\"`."
        t.core.applyState({connection: "reconnecting"})
        t.core.applyState({connection: "unavailable", unavailable: Shapes.refusedHello("workspace.unsupported", message)})

        verify(banner.visible)
        verify(banner.text.indexOf("The core isn't available") < 0, "that would be false: " + banner.text)
        verify(banner.text.indexOf("This core serves only the default workspace") >= 0, "the core's words name the fix: " + banner.text)
        verify(!banner.retryButton.visible, "Try again would repeat the refusal")
        verify(banner.defaultButton.visible)
        compare(banner.defaultButton.text, "Use the default workspace")
        compare(walk.report(banner), [])

        // The switchers stay usable, and say the same.
        const top = t.shell.topBar
        verify(top.workspaceSwitcher.button.enabled)
        verify(top.projectSwitcher.button.enabled)
        compare(top.projectSwitcher.button.text, "No project ▾")
        mouseClick(top.workspaceSwitcher.button)
        tryVerify(() => top.workspaceSwitcher.popup.opened)
        const popup = top.workspaceSwitcher.popup
        verify(popup.message.indexOf("This core serves only the default workspace") >= 0, popup.message)
        verify(popup.defaultButton.visible)
        compare(t.core.calls.filter(named("workspaces.list")).filter(c => !answered[c.id]).length, 0, "nothing can be listed")
        compare(walk.report(popup.contentItem), [])

        // The way out, from the list: a new hello on what suite.toml says now.
        wait(50)
        mouseClick(popup.defaultButton)
        const hello = open(t.core, c => c.kind === "useDefaultWorkspace")
        compare(hello.length, 1)
        compare(t.core.calls.filter(named("workspace.set_default")).length, 0, "the default isn't changed by this")
        answered[hello[0].id] = true
        const quay = {id: "ws2", path: "D:/art/quay"}
        t.core.respond(hello[0].id, Shapes.hello(quay))
        t.core.applyState({connection: "ready", coreProtocol: "1.2", workspace: quay})
        verify(!banner.visible)
        tryVerify(() => !popup.visible)
        compare(top.workspaceSwitcher.button.text, "quay ▾")
        verify(top.workspaceSwitcher.button.enabled)
        verify(top.projectSwitcher.button.enabled)
    }

    function test_a_refused_workspace_can_also_leave_from_the_banner() { // spec: ARCH-100
        const t = load()
        const banner = t.shell.connectionBanner
        t.core.applyState({connection: "unavailable", unavailable: Shapes.refusedHello("workspace.unknown", "This workspace's folder is gone.")})
        verify(banner.text.indexOf("This workspace's folder is gone.") >= 0, banner.text)
        verify(banner.defaultButton.visible)
        waitForItemPolished(banner.defaultButton.parent)
        wait(50)
        mouseClick(banner.defaultButton)
        const hello = open(t.core, c => c.kind === "useDefaultWorkspace")
        compare(hello.length, 1)
        // It fails too: that shows, and the session is where it was.
        answered[hello[0].id] = true
        t.core.respond(hello[0].id, Shapes.refusal("core.unreachable", "No core answered."))
        compare(banner.text, "Couldn't use the default workspace: No core answered.")
        verify(banner.defaultButton.visible)
    }

    function test_connecting_shows_no_banner() { // spec: APP-033
        const t = load()
        t.core.applyState({connection: "connecting"})
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
        verify(banner.createButton.requiresCore)
        compare(walk.report(banner), [])
        compare(t.shell.topBar.projectSwitcher.button.text, "No project ▾")

        // The button has the focus as the flow starts, and it must not fall to the window.
        banner.createButton.forceActiveFocus(Qt.TabFocusReason)
        tryVerify(() => banner.createButton.activeFocus)

        banner.folderChosen("file:///D:/art/new%20place")
        const create = t.core.calls.filter(named("workspace.create"))
        compare(create.length, 1)
        compare(create[0].args, {path: "D:/art/new place", default: true}, "the chosen folder, as the default")
        verify(banner.visible, "not before the hello")

        // A second folder while that is out isn't dropped silently.
        banner.folderChosen("file:///D:/art/second")
        compare(t.core.calls.filter(named("workspace.create")).length, 1)
        verify(t.shell.topBar.notice.visible)
        verify(t.shell.topBar.notice.text.indexOf("Try this one again") >= 0, t.shell.topBar.notice.text)

        const made = {id: "ws9", path: "D:/art/new place"}
        answer(t.core, named("workspace.create"), Shapes.tool({id: "ws9", path: made.path, created: true, default: true,
                                                               warnings: ["It is a network folder."]}))
        // It says what happened, and its warning stays for you to read, though the banner is going.
        compare(t.shell.topBar.notice.text, "new place is now your default workspace. It is a network folder.")
        compare(t.shell.topBar.notice.kind, "warning")

        const hello = open(t.core, c => c.kind === "setWorkspace")
        compare(hello.length, 1, "a new hello names the new workspace")
        compare(hello[0].name, "ws9")
        verify(banner.visible, "still, until the hello's workspace is in")
        answered[hello[0].id] = true
        t.core.respond(hello[0].id, Shapes.hello(made))
        t.core.applyState({connection: "ready", workspace: made})
        verify(!banner.visible, "the banner leaves")
        tryVerify(() => t.shell.activeFocusItem && !String(t.shell.activeFocusItem).startsWith("QQuickRootItem"), 2000,
                  "the focus moves on")
        const moved = t.shell.activeFocusItem
        verify(moved.visible && moved.enabled && moved !== banner.createButton, "to a control that can take it: " + moved)
        verify(walk.tabChain(t.shell.contentItem).indexOf(moved) >= 0, "one Tab reaches: " + moved)
        verify(t.shell.topBar.projectSwitcher.button.enabled, "projects can be chosen now")
        compare(t.core.calls.filter(named("workspace.set_default")).length, 0)
    }

    function test_a_first_run_says_the_folder_becomes_the_default() { // spec: APP-016
        const t = load(null)
        const banner = t.shell.noWorkspaceBanner
        verify(!banner.defaultButton.visible)
        answer(t.core, named("workspaces.list"), Shapes.tool({workspaces: []}), "read when the banner shows")
        verify(banner.text.indexOf("The folder you choose becomes your default workspace.") >= 0, banner.text)
        verify(banner.text.indexOf("command line") < 0, "nothing is replaced: " + banner.text)
        verify(!banner.defaultButton.visible)
    }

    function test_a_lost_default_says_what_would_be_replaced_before_the_click() { // spec: APP-016, P4a-R34
        const t = load(null)
        const banner = t.shell.noWorkspaceBanner
        answer(t.core, named("workspaces.list"), Shapes.tool({workspaces: [
            {id: "ws1", path: "E:/art/harbor", name: "Harbor", default: true, served: false, missing: true},
            {id: "ws2", path: "D:/art/quay", default: false, served: false, missing: false}]}))
        verify(banner.text.indexOf("Your default workspace, Harbor (E:/art/harbor), can't be found.") >= 0, banner.text)
        verify(banner.text.indexOf("plug it in") >= 0, banner.text)
        verify(banner.text.indexOf("for the command line and agents too") >= 0, banner.text)
        compare(banner.createButton.text, "Create or choose a workspace", "APP-016's words")
        verify(banner.defaultButton.visible)
        compare(banner.defaultButton.text, "Use the default workspace")
        compare(walk.report(banner), [])
        compare(t.core.calls.filter(named("workspace.create")).length, 0)

        // Its drive still out: nothing changes, and it says so.
        waitForItemPolished(banner.defaultButton.parent)
        wait(50)
        mouseClick(banner.defaultButton)
        let hello = open(t.core, c => c.kind === "useDefaultWorkspace")
        compare(hello.length, 1)
        answered[hello[0].id] = true
        t.core.respond(hello[0].id, Shapes.hello(null))
        verify(banner.visible)
        verify(banner.text.endsWith("It still can't be found."), banner.text)
        verify(banner.text.indexOf("for the command line and agents too") >= 0, "the sentence stays: " + banner.text)

        // Its drive back: the same button picks it up, and no default was changed.
        mouseClick(banner.defaultButton)
        hello = open(t.core, c => c.kind === "useDefaultWorkspace")
        compare(hello.length, 1)
        answered[hello[0].id] = true
        const harbor = {id: "ws1", path: "E:/art/harbor"}
        t.core.respond(hello[0].id, Shapes.hello(harbor))
        t.core.applyState({connection: "ready", workspace: harbor})
        verify(!banner.visible)
        compare(t.core.calls.filter(named("workspace.create")).length, 0)
        compare(t.core.calls.filter(named("workspace.set_default")).length, 0)
    }

    function test_a_good_default_is_named_and_can_be_used() { // spec: APP-016, P4a-R34
        // Started on a first run, then `lil workspace init --default` in a terminal.
        const t = load(null)
        const banner = t.shell.noWorkspaceBanner
        answer(t.core, named("workspaces.list"), Shapes.tool({workspaces: []}))
        verify(!banner.defaultButton.visible)

        const quay = {id: "ws2", path: "D:/art/quay"}
        t.core.emit("workspace", "event", Shapes.defaultChanged(quay.id, quay.path))
        answer(t.core, named("workspaces.list"), Shapes.tool({workspaces: [
            {id: "ws2", path: "D:/art/quay", name: "Quay", default: true, served: true, missing: false}]}), "read again on the event")
        verify(banner.text.indexOf("Your default workspace is Quay (D:/art/quay)") >= 0, banner.text)
        verify(banner.text.indexOf("for the command line and agents too") >= 0, banner.text)
        verify(banner.text.indexOf("can't be found") < 0, banner.text)
        verify(banner.defaultButton.visible)
        compare(walk.report(banner), [])
        waitForItemPolished(banner.defaultButton.parent)
        wait(50)
        mouseClick(banner.defaultButton)
        const hello = open(t.core, c => c.kind === "useDefaultWorkspace")
        compare(hello.length, 1)
        answered[hello[0].id] = true
        t.core.respond(hello[0].id, Shapes.hello(quay))
        t.core.applyState({connection: "ready", workspace: quay})
        verify(!banner.visible)
        compare(t.core.calls.filter(named("workspace.create")).length, 0)
    }

    function test_a_failed_list_read_says_it_in_general_words() { // spec: APP-016
        const t = load(null)
        const banner = t.shell.noWorkspaceBanner
        answer(t.core, named("workspaces.list"), Shapes.refusal("store.io", "suite.toml can't be read."))
        verify(banner.text.indexOf("for the command line and agents too") >= 0, banner.text)
        verify(!banner.defaultButton.visible)
    }

    function test_the_default_moved_while_connected_shows_on_the_banner() { // spec: ARCH-100, ARCH-090
        const t = load()
        const banner = t.shell.connectionBanner
        verify(!banner.visible)
        t.core.emit("workspace", "event", Shapes.defaultChanged("ws2", "D:/art/quay"))
        verify(banner.visible, "without waiting for a list to open")
        verify(banner.text.startsWith("The core refused this workspace: quay became the default workspace"), banner.text)
        verify(!banner.retryButton.visible)
        verify(banner.defaultButton.visible)
        compare(walk.report(banner), [])
        waitForItemPolished(banner.defaultButton.parent)
        wait(50)
        mouseClick(banner.defaultButton)
        const hello = open(t.core, c => c.kind === "useDefaultWorkspace")
        compare(hello.length, 1)
        answered[hello[0].id] = true
        const quay = {id: "ws2", path: "D:/art/quay"}
        t.core.respond(hello[0].id, Shapes.hello(quay))
        t.core.applyState({connection: "ready", workspace: quay})
        verify(!banner.visible, "a new hello ends it")
    }

    // The Task 22 audit's probe: an <img> in every message from outside, and no network request.
    function test_an_img_in_any_outside_message_shows_as_text_and_loads_nothing() { // spec: APP-156, P4a-R8
        const img = "<img src=\"http://localhost:9/x.png\"><b>bad</b>"
        const plain = root => {
            const walkTexts = i => {
                if (i.textFormat !== undefined)
                    compare(i.textFormat, Text.PlainText, "plain text: " + i.text)
                for (let n = 0; n < i.children.length; ++n)
                    walkTexts(i.children[n])
            }
            walkTexts(root)
        }
        const before = TestFiles.networkRequests()

        // The connection banner: an unavailable reason, a refused hello, an incompatible protocol.
        const t = load()
        const top = t.shell.topBar
        const banner = t.shell.connectionBanner
        t.core.applyState({connection: "unavailable", unavailable: Shapes.otherCause("other", img)})
        verify(banner.text.indexOf(img) >= 0, banner.text)
        plain(banner)
        t.core.applyState({connection: "unavailable", unavailable: Shapes.refusedHello("workspace.unknown", img)})
        verify(banner.text.indexOf(img) >= 0, banner.text)
        plain(banner)
        t.core.applyState({connection: "incompatible", coreProtocol: img})
        plain(banner)

        // A switcher's message and its rows.
        t.core.applyState({connection: "ready", workspace: workspaceOne})
        const sw = top.workspaceSwitcher
        mouseClick(sw.button)
        tryVerify(() => sw.popup.opened)
        answer(t.core, named("workspaces.list"), Shapes.refusal("store.io", img))
        verify(sw.popup.message.indexOf(img) >= 0, sw.popup.message)
        plain(sw.popup.contentItem)
        sw.popup.close()
        tryVerify(() => !sw.popup.visible)

        // The moved default, from a folder's name. (The bar's own notice and badge are in
        // tst_top_bar: a lane's reason reaches the Jobs panel's headers in a shell too.)
        t.core.emit("workspace", "event", Shapes.defaultChanged("ws2", "D:/art/" + img))
        plain(banner)

        // The no-workspace banner: a refusal, and a lost default's name and path. A new core's
        // request ids start again.
        answered = {}
        const u = load(null)
        const none = u.shell.noWorkspaceBanner
        answer(u.core, named("workspaces.list"), Shapes.tool({workspaces: [
            {id: "ws1", path: "E:/" + img, name: img, default: true, served: false, missing: true}]}))
        verify(none.text.indexOf(img) >= 0, none.text)
        plain(none)
        none.folderChosen("file:///D:/art/x")
        answer(u.core, named("workspace.create"), Shapes.refusal("workspace.path_invalid", img))
        verify(none.text.indexOf(img) >= 0, none.text)
        plain(none)

        wait(500)
        compare(TestFiles.networkRequests(), before, "nothing loaded the <img>")
    }

    function test_no_workspace_refusals_stay_on_the_banner() { // spec: APP-016
        const t = load(null)
        const banner = t.shell.noWorkspaceBanner
        banner.folderChosen("file:///C:/Program%20Files/LILITH")
        answer(t.core, named("workspace.create"), Shapes.refusal("workspace.path_invalid", "<b>Not</b> inside the install folder."))
        verify(banner.visible)
        verify(banner.text.indexOf("<b>Not</b> inside the install folder.") >= 0, banner.text)
        compare(banner.kind, "danger")
        compare(open(t.core, c => c.kind === "setWorkspace").length, 0)

        // A hello the core refuses too.
        banner.folderChosen("file://server/share/art")
        const create = t.core.calls.filter(named("workspace.create")).pop()
        compare(create.args, {path: "//server/share/art", default: true}, "a network folder's path")
        compare(banner.kind, "warning", "the old refusal goes with a new try")
        answer(t.core, named("workspace.create"), Shapes.tool({id: "ws9", path: "//server/share/art", created: true,
                                                               default: false, warnings: []}))
        verify(!t.shell.topBar.notice.visible, "it didn't become the default, so nothing says it did")
        const hello = open(t.core, c => c.kind === "setWorkspace")[0]
        answered[hello.id] = true
        t.core.respond(hello.id, Shapes.refusal("workspace.unsupported", "This core serves another one."))
        verify(banner.visible)
        verify(banner.text.indexOf("This core serves another one.") >= 0, banner.text)
    }

    function test_no_workspace_banner_needs_a_connection() { // spec: APP-016, APP-034
        const t = load(null)
        const banner = t.shell.noWorkspaceBanner
        verify(banner.visible)
        t.core.applyState({connection: "reconnecting"})
        verify(!banner.visible, "while disconnected, no workspace isn't known")
        verify(!banner.createButton.enabled)
        t.core.applyState({connection: "ready", workspace: null})
        verify(banner.visible)
        verify(banner.createButton.enabled)
    }

    function test_the_list_is_read_each_time_the_banner_shows() { // spec: APP-016
        // A session that loses its workspace: the banner wasn't showing when the shell was made.
        const t = load()
        const reads = () => t.core.calls.filter(named("workspaces.list")).length
        const before = reads()
        const banner = t.shell.noWorkspaceBanner
        verify(!banner.visible)
        t.core.applyState({connection: "ready", workspace: null})
        verify(banner.visible)
        compare(reads(), before + 1)
        answer(t.core, named("workspaces.list"), Shapes.tool({workspaces: [
            {id: "ws1", path: "E:/art/harbor", name: "Harbor", default: true, served: false, missing: true}]}))
        verify(banner.defaultButton.visible)
    }

    function test_banners_stack_above_the_docks() { // spec: APP-001
        const t = load(null)
        const shell = t.shell
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
