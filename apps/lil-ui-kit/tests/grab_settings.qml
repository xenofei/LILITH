// grab_settings: the Settings panel and the first-start card offscreen, saved as
// <grab dir>/settings-<state>.png (LIL_GRAB_DIR, else CMake's build/<preset>/grabs) for the reviewer, on
// FakeCore's scripted answers: the panel alone at the shell's 1280 x 800 with everything in order, with
// the notification path unreachable, with its read failed, the shell's first start, the panel docked in
// the shell's Default layout with everything in order and with Set up to offer. Each grab must have the
// window's size and not be one flat colour.
import QtQuick
import QtTest
import Lilith.Shim
import Lilith.UiKit
import "CoreShapes.js" as Shapes

TestCase {
    id: tc
    name: "GrabSettings"
    when: windowShown
    width: 200
    height: 100

    Component {
        id: fakeComponent
        FakeCore {
            project: "sketchbook"
            workspace: ({id: "ws1", path: "D:/art/harbor"})
        }
    }

    Component {
        id: fakeSetupComponent
        QtObject {
            property bool busy: false
            property bool appMissing: false
            property string appPath
            signal finished(string reportJson)
            function run() { return false }
        }
    }

    Component {
        id: panelComponent
        Window {
            id: host
            required property var core
            required property var setup
            readonly property alias panel: settings
            width: 1280
            height: 800
            visible: true
            color: Theme.bg

            SettingsPanel {
                id: settings
                anchors.fill: parent
                core: host.core
                title: "Settings"
                setup: host.setup
                openExternally: url => false
            }
        }
    }

    Component {
        id: shellComponent
        ShellWindow {}
    }

    property var answered: ({})

    function init() {
        answered = {}
        tryVerify(() => {
            const kddw = JSON.parse(DockLayout.serialize())
            return DockLayout.mainWindows().length === 0
                   && !(Array.isArray(kddw.allDockWidgets) && kddw.allDockWidgets.length > 0)
        }, 5000, "the earlier shell's docks are gone")
    }

    function answerRead(core, outcome) {
        let call = null
        tryVerify(() => (call = core.calls.find(c => c.name === "desktop.get" && !answered[c.id]) || null) !== null, 5000)
        answered[call.id] = true
        verify(core.respond(call.id, outcome))
    }

    function distinctColours(img) {
        const seen = {}
        let n = 0
        for (let y = 0; y < img.height; y += 7) {
            for (let x = 0; x < img.width; x += 7) {
                const c = String(img.pixel(x, y))
                if (!seen[c]) {
                    seen[c] = true
                    ++n
                }
            }
        }
        return n
    }

    function save(win, state) {
        wait(300)
        compare(win.width, 1280)
        const img = grabImage(win.contentItem)
        const dpr = win.devicePixelRatio
        compare(img.width, Math.round(win.width * dpr))
        compare(img.height, Math.round(win.height * dpr))
        verify(distinctColours(img) >= 3, "not one flat colour")
        const dir = TestFiles.env("LIL_GRAB_DIR") || TestFiles.env("LIL_GRAB_DEFAULT_DIR")
        verify(dir !== "", "a grab folder")
        verify(TestFiles.mkpath(dir))
        img.save(dir + "/settings-" + state + ".png")
        verify(TestFiles.list(dir).indexOf("settings-" + state + ".png") >= 0)
    }

    function makeHost(core) {
        return createTemporaryObject(panelComponent, tc, {core: core, setup: fakeSetupComponent.createObject(tc)})
    }

    function test_grab_all_in_order() { // spec: APP-146
        const core = fakeComponent.createObject(tc)
        const host = makeHost(core)
        answerRead(core, Shapes.tool(Shapes.desktopSettings()))
        tryVerify(() => host.panel.fresh, 5000)
        save(host, "ok")
    }

    function test_grab_unreachable() { // spec: APP-146, APP-022
        const core = fakeComponent.createObject(tc)
        const host = makeHost(core)
        answerRead(core, Shapes.tool(Shapes.desktopSettings({notification_path: {
            toasts: "unavailable", toasts_reason: "disabled_for_app", shortcut: "missing", handlers: "other_channel"}})))
        tryVerify(() => host.panel.fresh, 5000)
        save(host, "unreachable")
    }

    function test_grab_read_failed() { // spec: APP-146
        const core = fakeComponent.createObject(tc)
        const host = makeHost(core)
        answerRead(core, Shapes.refusal("store.io", "The settings can't be read now."))
        tryVerify(() => host.panel.readAgainButton.visible, 5000)
        save(host, "read-failed")
    }

    function test_grab_first_start() { // spec: APP-022
        const core = fakeComponent.createObject(tc)
        const shell = createTemporaryObject(shellComponent, tc, {core: core, stateDir: TestFiles.newDir("grab"),
                                                                  setup: fakeSetupComponent.createObject(tc)})
        shell.visible = true
        answerRead(core, Shapes.tool(Shapes.desktopSettings({notification_path: {
            toasts: "not_yet_shown", shortcut: "missing", handlers: "missing"}})))
        tryVerify(() => shell.firstStartCard.visible, 5000)
        tryVerify(() => shell.layoutSettled, 5000)
        save(shell, "first-start")
    }

    function test_grab_docked() { // spec: APP-146
        const core = fakeComponent.createObject(tc)
        const shell = createTemporaryObject(shellComponent, tc, {core: core, setup: fakeSetupComponent.createObject(tc)})
        shell.visible = true
        tryVerify(() => shell.layoutSettled, 5000)
        shell.docks.settings.setAsCurrentTab()
        tryVerify(() => shell.docks.settings.content.shown, 5000)
        answerRead(core, Shapes.tool(Shapes.desktopSettings()))
        tryVerify(() => shell.docks.settings.content.fresh, 5000)
        save(shell, "docked")
    }

    function test_grab_docked_with_set_up() { // spec: APP-146, APP-022
        const core = fakeComponent.createObject(tc)
        const shell = createTemporaryObject(shellComponent, tc, {core: core, setup: fakeSetupComponent.createObject(tc)})
        shell.visible = true
        tryVerify(() => shell.layoutSettled, 5000)
        shell.docks.settings.setAsCurrentTab()
        tryVerify(() => shell.docks.settings.content.shown, 5000)
        answerRead(core, Shapes.tool(Shapes.desktopSettings({notification_path: {
            toasts: "ready", shortcut: "missing", handlers: "ok"}})))
        tryVerify(() => shell.docks.settings.content.fresh, 5000)
        save(shell, "docked-set-up")
    }
}
