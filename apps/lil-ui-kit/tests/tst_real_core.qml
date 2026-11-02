// The shell against a real test-channel core (ledger TC-01: Task 20's review findings C1 and I3), through
// the UI's own paths, never FakeCore. crates/lil-qt/tests/qml_real_core.rs runs it: this runner as
// lil-image.exe beside a test channel's lil-core.exe, with LIL_QML_REAL_CORE set, so the Core singleton
// connects as lil-image's does (serve_core: `ui:image`, suite.toml's default workspace, a core started
// when none runs). Two workspaces are registered (LIL_QML_WS_A, the default at the start, and
// LIL_QML_WS_B); LIL_QML_LIL is the test channel's `lil`.
//
// - C1: "Make this the default" on the other workspace's row moves the app with it: the core then
//   refuses the old session's every call, so only the new hello (`Core.setWorkspace` with the answer's
//   id) gets the app on, and the switcher's list reads again.
// - I3: `lil workspace use` elsewhere, then a lost connection (`lil core stop`; the session starts a
//   new core): the hello names the old workspace and is refused. The banner says so in the core's words
//   and offers "Use the default workspace", which gets the app back.
// Each test works from whichever workspace is current, so their order doesn't matter.
import QtQuick
import QtTest
import Lilith.Core
import Lilith.UiKit

TestCase {
    id: tc
    name: "RealCore"
    when: windowShown
    width: 200
    height: 100

    readonly property string lil: TestFiles.env("LIL_QML_LIL")
    readonly property var paths: [TestFiles.env("LIL_QML_WS_A"), TestFiles.env("LIL_QML_WS_B")]

    Component {
        id: shellComponent
        ShellWindow {
            core: Core
            shim: QtObject {
                function activate(window, takeFocus) {}
            }
        }
    }

    property var shell: null

    function initTestCase() {
        verify(TestFiles.env("LIL_QML_REAL_CORE") !== "", "run by qml_real_core.rs, which sets LIL_QML_REAL_CORE")
        verify(lil !== "" && paths[0] !== "" && paths[1] !== "", "the launcher's paths")
        shell = shellComponent.createObject(tc)
        verify(shell, "ShellWindow created")
        shell.visible = true
        tryVerify(() => shell.layoutSettled, 10000, "the docks are laid out")
    }

    function cleanupTestCase() {
        if (shell)
            shell.destroy()
    }

    // A path as the core may write it: no \\?\ prefix, backslashes, any case.
    function samePath(a, b) {
        const norm = p => String(p || "").replace(/^\\\\\?\\/, "").replace(/\//g, "\\").replace(/\\+$/, "").toLowerCase()
        return norm(a) === norm(b)
    }

    function connected() {
        tryVerify(() => Core.connection === "ready" && !!Core.workspace, 30000,
                  "the app is connected on a workspace: " + Core.connection)
    }

    // The switcher's list, read through the switcher itself; closed again.
    function listed() {
        const sw = shell.topBar.workspaceSwitcher
        sw.workspaces = []
        sw.open()
        tryVerify(() => sw.workspaces.length >= 2, 15000, "the switcher lists the workspaces: " + sw.popup.message)
        const rows = sw.workspaces
        sw.popup.close()
        return rows
    }

    // The registered workspace that isn't current: {row, path}.
    function other(rows) {
        const current = String(Core.workspace.id)
        const row = rows.find(w => w.id !== current && (samePath(w.path, paths[0]) || samePath(w.path, paths[1])))
        verify(row, "the other workspace's row: " + JSON.stringify(rows))
        return row
    }

    function find(item, pred) {
        if (pred(item))
            return item
        for (let n = 0; n < item.children.length; ++n) {
            const found = find(item.children[n], pred)
            if (found)
                return found
        }
        return null
    }

    function runLil(args) {
        const r = TestFiles.run(lil, args)
        compare(r.code, 0, "lil " + args.join(" ") + ": " + r.out)
        return r.out
    }

    function test_make_this_the_default_moves_the_app_with_it() { // spec: APP-006, ARCH-030, ARCH-090
        connected()
        const target = other(listed())
        const sw = shell.topBar.workspaceSwitcher
        sw.open()
        tryVerify(() => sw.workspaces.length >= 2 && sw.popup.view.count >= 2, 15000, "the list is open")
        const index = sw.workspaces.findIndex(w => w.id === target.id)
        sw.popup.view.positionViewAtIndex(index, ListView.Contain)
        let row = null
        tryVerify(() => (row = sw.popup.view.itemAtIndex(index)) !== null, 5000, "the row is made")
        const make = find(row, i => String(i).startsWith("LButton") && i.text === "Make this the default")
        verify(make && make.visible && make.enabled, "its Make this the default button")
        make.clicked()
        // The core has moved its default, and the app is on it: a new hello, not the old session.
        tryVerify(() => Core.connection === "ready" && Core.workspace && String(Core.workspace.id) === target.id, 30000,
                  "the app switched to " + target.id + ": " + JSON.stringify(Core.workspace) + ", " + sw.popup.message)
        tryVerify(() => shell.topBar.notice.visible
                        && shell.topBar.notice.text.indexOf("is now your default workspace, and the app has switched to it") >= 0,
                  5000, "the notice says so: " + shell.topBar.notice.text)
        // The core serves the session again: the list reads, the new one current and the default.
        const rows = listed()
        const now = rows.find(w => w.id === target.id)
        verify(now && now.default && now.served, "the list says it's the default the core serves: " + JSON.stringify(now))
        compare(shell.topBar.movedReason, "", "nothing refused")
        verify(!shell.connectionBanner.visible, "no banner: " + shell.connectionBanner.text)
    }

    function test_a_refused_hello_says_why_and_the_default_gets_out() { // spec: ARCH-100, APP-034, ARCH-090
        connected()
        const target = other(listed())
        const banner = shell.connectionBanner
        // Another client moves the default while the app stays connected.
        runLil(["workspace", "use", target.path])
        tryVerify(() => shell.topBar.movedReason !== "", 15000, "the bar hears the default move")
        // Then the connection is lost: the session starts a new core, and its hello names the old one.
        runLil(["core", "stop"])
        tryVerify(() => Core.connection === "unavailable", 60000, "the hello is refused: " + Core.connection)
        compare(Core.unavailable.cause, "refused")
        compare(Core.unavailable.code, "workspace.unsupported")
        tryVerify(() => banner.visible, 5000, "the banner shows")
        verify(banner.text.indexOf("The core refused this workspace: ") === 0, banner.text)
        verify(banner.text.indexOf("lil workspace use") >= 0, "in the core's own words, which name the fix: " + banner.text)
        verify(banner.text.indexOf("isn't available") < 0, "never that the core isn't available: " + banner.text)
        verify(banner.defaultButton.visible, "Use the default workspace is offered")
        verify(!banner.retryButton.visible, "Try again, which would be refused again, isn't")
        banner.defaultButton.clicked()
        tryVerify(() => Core.connection === "ready" && Core.workspace && String(Core.workspace.id) === target.id, 30000,
                  "the default gets the app back: " + Core.connection + " " + JSON.stringify(Core.workspace))
        tryVerify(() => !banner.visible, 5000, "the banner goes: " + banner.text)
        verify(!Core.unavailable || !Core.unavailable.cause, "the cause is cleared: " + JSON.stringify(Core.unavailable))
        tryVerify(() => shell.topBar.movedReason === "", 15000, "nothing refused now: " + shell.topBar.movedReason)
    }
}
