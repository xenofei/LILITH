// A11yWalk (APP-156) over a page of every base control, and over pages it must fault.
import QtQuick
import QtTest
import Lilith.Shim
import Lilith.UiKit

TestCase {
    id: tc
    name: "A11y"
    when: windowShown
    width: 640
    height: 480

    A11yWalk { id: walk }

    // Each page is a visible window of its own: the TestCase's window isn't shown here, and the
    // walk skips what isn't visible.
    Component {
        id: everyControl
        Window {
            property alias page: column
            width: 480
            height: 480
            visible: true

            Column {
                id: column
                spacing: 4
                Component.onCompleted: banner.show("Saved")
                LBanner { id: banner; width: 400 }
                LButton { text: "Run" }
                LIconButton { glyph: "×"; text: "Remove" }
                LToggle { text: "Keep awake" }
                LTextField { label: "Name" }
                LTextField { placeholderText: "Search" }
                LBadge { text: "3"; name: "3 waiting confirmations" }
                LGauge { label: "Daily cap"; value: 0.41 }
                LCard {
                    title: "Card"
                    width: 300
                    height: 80
                    LButton { text: "Inside a card" }
                }
            }
        }
    }

    Component {
        id: faulty
        Window {
            property alias page: column
            width: 200
            height: 200
            visible: true

            Column {
                id: column
                LButton { objectName: "nameless" }
                Rectangle {
                    width: 40
                    height: 20
                    MouseArea { objectName: "clicky"; anchors.fill: parent }
                }
                // A control the keyboard can't reach.
                LButton { objectName: "nofocus"; text: "No focus"; focusPolicy: Qt.NoFocus }
                // A tile that takes taps through a handler, with nothing accessible.
                Rectangle {
                    objectName: "tile"
                    width: 40
                    height: 20
                    TapHandler {}
                }
                // The accessible-parent pattern, done right: not reported.
                Rectangle {
                    objectName: "goodParent"
                    width: 40
                    height: 20
                    activeFocusOnTab: true
                    Accessible.role: Accessible.Button
                    Accessible.name: "Open"
                    MouseArea { objectName: "goodChild"; anchors.fill: parent }
                }
            }
        }
    }

    function test_every_control_has_a_role_a_name_and_focus() { // spec: APP-156
        const w = createTemporaryObject(everyControl, tc)
        tryVerify(() => w.page.width > 0)
        const p = w.page
        verify(walk.interactiveCount(p) >= 7, "the walk visits the controls: " + walk.interactiveCount(p))
        compare(walk.report(p), [])

        // The non-interactive ones carry a role and a name too.
        const badge = LilShim.accessibleOf(p.children[6])
        compare(badge.role, Accessible.StaticText)
        compare(badge.name, "3 waiting confirmations")
        const gauge = LilShim.accessibleOf(p.children[7])
        compare(gauge.role, Accessible.ProgressBar)
        compare(gauge.name, "Daily cap: 41 %")
        const card = LilShim.accessibleOf(p.children[8])
        compare(card.role, Accessible.Grouping)
        compare(card.name, "Card")
        const banner = LilShim.accessibleOf(p.children[0])
        compare(banner.role, Accessible.AlertMessage)
        compare(banner.name, "Saved")
    }

    function test_the_walk_reports_what_fails() { // spec: APP-156
        const w = createTemporaryObject(faulty, tc)
        tryVerify(() => w.page.width > 0)
        const p = w.page
        const what = walk.report(p)
        verify(what.some(w => w.indexOf("nameless") >= 0 && w.indexOf("no accessible name") >= 0),
               JSON.stringify(what))
        verify(what.some(w => w.indexOf("clicky") >= 0 && w.indexOf("no accessible name") >= 0),
               JSON.stringify(what))
        verify(what.some(w => w.indexOf("clicky") >= 0 && w.indexOf("not in the tab chain") >= 0),
               JSON.stringify(what))
        verify(what.some(w => w.indexOf("nofocus") >= 0 && w.indexOf("lacks Qt.TabFocus") >= 0),
               JSON.stringify(what))
        verify(what.some(w => w.indexOf("tile") >= 0 && w.indexOf("no accessible role") >= 0),
               JSON.stringify(what))
        verify(!what.some(w => w.indexOf("goodParent") >= 0 || w.indexOf("goodChild") >= 0),
               JSON.stringify(what))
    }
}
