// A11yWalk (APP-156) over a page of every base control, and over pages it must fault.
import QtQuick
import QtQuick.Controls.Basic as QQC
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

    // The keyboard-navigable list: the view is the one tab stop, its rows are named ListItems
    // that take clicks (a MouseArea, a TapHandler, a press action). Nothing to report.
    Component {
        id: navigableList
        Window {
            property alias page: column
            width: 300
            height: 300
            visible: true

            Column {
                id: column
                ListView {
                    objectName: "jobsList"
                    width: 280
                    height: 200
                    activeFocusOnTab: true
                    keyNavigationEnabled: true
                    Accessible.role: Accessible.List
                    Accessible.name: "Jobs"
                    // A header isn't a row: its button is checked as a button.
                    header: LButton { objectName: "refresh"; text: "Refresh" }
                    model: 4
                    delegate: Rectangle {
                        id: row
                        required property int index
                        width: 280
                        height: 30
                        Accessible.role: Accessible.ListItem
                        Accessible.name: "Job " + index
                        Accessible.onPressAction: ListView.view.currentIndex = index
                        MouseArea { anchors.fill: parent; onClicked: row.ListView.view.currentIndex = row.index }
                        Rectangle {
                            width: 20
                            height: 20
                            TapHandler {}
                        }
                    }
                }
            }
        }
    }

    // A navigable list with a nameless row, and a list whose rows take clicks but which isn't a
    // tab stop: both reported.
    Component {
        id: faultyLists
        Window {
            property alias page: column
            width: 300
            height: 600
            visible: true

            Column {
                id: column
                ListView {
                    width: 280
                    height: 100
                    activeFocusOnTab: true
                    Accessible.role: Accessible.List
                    Accessible.name: "Backups"
                    model: 2
                    delegate: Rectangle {
                        objectName: "namelessRow"
                        width: 280
                        height: 30
                        Accessible.role: Accessible.ListItem
                        MouseArea { anchors.fill: parent }
                    }
                }
                ListView {
                    width: 280
                    height: 100
                    activeFocusOnTab: true
                    Accessible.role: Accessible.List
                    Accessible.name: "Logs"
                    header: LButton { objectName: "namelessHeader" }
                    model: 1
                    delegate: Rectangle {
                        width: 280
                        height: 30
                        Accessible.role: Accessible.ListItem
                        Accessible.name: "Log"
                    }
                }
                ListView {
                    width: 280
                    height: 100
                    activeFocusOnTab: true
                    keyNavigationEnabled: false
                    Accessible.role: Accessible.List
                    Accessible.name: "Events"
                    model: 2
                    delegate: Rectangle {
                        required property int index
                        width: 280
                        height: 30
                        Accessible.role: Accessible.ListItem
                        Accessible.name: "Event " + index
                        MouseArea { objectName: "noKeysClick"; anchors.fill: parent }
                    }
                }
                ListView {
                    width: 280
                    height: 100
                    Accessible.role: Accessible.List
                    Accessible.name: "Diagnostics"
                    model: 2
                    delegate: Rectangle {
                        required property int index
                        objectName: "unreachableRow"
                        width: 280
                        height: 30
                        Accessible.role: Accessible.ListItem
                        Accessible.name: "Event " + index
                        MouseArea { objectName: "unreachableClick"; anchors.fill: parent }
                    }
                }
            }
        }
    }

    // Static controls and a named list with its scroll bar: nothing to report.
    Component {
        id: staticControls
        Window {
            property alias page: column
            width: 480
            height: 600
            visible: true

            Column {
                id: column
                spacing: 4
                QQC.Label { objectName: "label"; text: "Hello" }
                QQC.ProgressBar { objectName: "progress"; value: 0.4 }
                QQC.Pane { objectName: "pane"; width: 100; height: 30 }
                QQC.Frame { objectName: "frame"; width: 100; height: 30 }
                QQC.ToolSeparator { objectName: "separator" }
                QQC.ScrollView {
                    objectName: "scrollView"
                    width: 100
                    height: 40
                    Text { text: "inside" }
                }
                ListView {
                    objectName: "list"
                    width: 200
                    height: 80
                    activeFocusOnTab: true
                    Accessible.name: "Recent jobs"
                    model: 20
                    delegate: Text { text: "Job " + index }
                    QQC.ScrollBar.vertical: QQC.ScrollBar { objectName: "scrollBar" }
                }
                Text { text: "plain" }
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
                // A nameless tile under a named, focusable Pane: the Pane is a container, so the
                // tile is reported.
                Rectangle {
                    width: 60
                    height: 30
                    activeFocusOnTab: true
                    Accessible.role: Accessible.Pane
                    Accessible.name: "Hardware"
                    Rectangle {
                        objectName: "paneTile"
                        width: 40
                        height: 20
                        TapHandler {}
                    }
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
        verify(what.some(w => w.indexOf("paneTile") >= 0 && w.indexOf("no accessible name") >= 0),
               JSON.stringify(what))
    }

    function test_a_navigable_list_with_named_rows_passes() { // spec: APP-156
        const w = createTemporaryObject(navigableList, tc)
        tryVerify(() => w.page.width > 0)
        wait(50)
        compare(walk.report(w.page), [])
        verify(walk.interactiveCount(w.page) >= 5, "the list and its rows' clicks are checked")
    }

    function test_lists_with_nameless_or_unreachable_rows_are_reported() { // spec: APP-156
        const w = createTemporaryObject(faultyLists, tc)
        tryVerify(() => w.page.width > 0)
        wait(50)
        const what = walk.report(w.page)
        verify(what.some(x => x.indexOf("namelessRow") >= 0 && x.indexOf("no accessible name") >= 0),
               JSON.stringify(what))
        verify(what.some(x => x.indexOf("unreachableClick") >= 0 && x.indexOf("not in the tab chain") >= 0),
               JSON.stringify(what))
        // S-1: a broken header button is reported as a button, not as a row.
        verify(what.some(x => x.indexOf("namelessHeader") >= 0 && x.indexOf("no accessible name") >= 0
                              && x.indexOf("row role") < 0), JSON.stringify(what))
        verify(!what.some(x => x.indexOf("namelessHeader") >= 0 && x.indexOf("row role") >= 0),
               JSON.stringify(what))
        // S-2: rows that take clicks in a list without key navigation aren't reachable.
        verify(what.some(x => x.indexOf("noKeysClick") >= 0 && x.indexOf("not in the tab chain") >= 0),
               JSON.stringify(what))
        // R-2: a plain item is told to become focusable, not to change a focus policy.
        verify(!what.some(x => x.indexOf("unreachableClick") >= 0 && x.indexOf("focusPolicy") >= 0),
               JSON.stringify(what))
    }

    function test_static_controls_and_scroll_bars_pass() { // spec: APP-156
        const w = createTemporaryObject(staticControls, tc)
        tryVerify(() => w.page.width > 0)
        compare(walk.report(w.page), [])
        verify(walk.interactiveCount(w.page) >= 1, "the list is checked")
    }
}
