// The child process of tst_diagnostics' test_closing_waits_for_an_export_under_way: it starts a large
// copy through the real shell's Diagnostics panel and returns, so the event loop ends with the copy
// under way, as lil-image's does when you close it. The parent then reads the folder (LIL_CLOSE_DIR).
import QtQuick
import QtTest
import Lilith.UiKit

TestCase {
    id: tc
    name: "ExportAtClose"
    when: windowShown
    width: 200
    height: 100

    Component {
        id: fakeComponent
        FakeCore { project: "sketchbook" }
    }

    Component {
        id: shellComponent
        ShellWindow {}
    }

    function test_start_a_copy_and_leave() {
        const core = fakeComponent.createObject(tc)
        const shell = shellComponent.createObject(tc, {core: core})
        shell.visible = true
        tryVerify(() => shell.layoutSettled, 5000)
        const panel = shell.docks.diagnostics.content
        const dir = TestFiles.env("LIL_CLOSE_DIR")
        verify(dir !== "", "a folder from the parent")
        verify(panel.exporter.save("file:///" + dir + "/kept.zip", dir + "/source.bin"))
        verify(panel.exporter.busy)
    }
}
