// The Usage & spend panel (APP-116 to APP-124): this month's and today's spend by a dimension you
// pick (one `usage.summary` per tab, grouped by it), the billing bases kept apart, fixed costs,
// currencies, the cap gauges and their alerts, the accounts, the Claude Code section and a CSV
// export of the rows shown.
//
// Reads (Ruling Q-D): the breakdowns and the accounts at open, then again on `usage` events;
// the caps at open, then again on `caps` and `usage` events and when a job ends (`jobs`). Each
// kind of read happens at most once a second: the first of a burst at once, the rest as one
// read when the second has passed. A `resync_required` or a snapshot on a topic reads what it
// moves again; the connection's own state never does. Nothing here adds money up: the totals
// are the core's.
//
// The export writes the rows of the tab shown (UsageText.exportTable) to the file you pick,
// through the CsvExport bridge, off the GUI thread; a failed write says why in the banner, and
// what became of the file that was there. Every Text of the panel's own is plain text, and every
// string from the usage logs or suite.toml goes through `core.shownText` (P4a-R8).
pragma ComponentBehavior: Bound
import QtQuick
import QtQuick.Dialogs
import Lilith.Core
import Lilith.UiKit
import "UsageText.js" as UsageText

Rectangle {
    id: root

    required property var core
    required property string title

    // "month" or "day".
    property string tab: "month"
    // The dimension's `group_by` field (UsageText.dimensions).
    property string dimension: "provider"

    readonly property alias month: monthBreakdown
    readonly property alias day: dayBreakdown
    readonly property alias gauges: capGauges
    readonly property alias accounts: accountsList
    readonly property alias claudeCode: claudeCodeSection
    readonly property alias banner: panelBanner
    readonly property alias exporter: csvExport
    readonly property alias exportButton: exportButton
    readonly property alias scroller: scroller
    readonly property var current: tab === "day" ? dayBreakdown : monthBreakdown

    color: Theme.surface

    Accessible.role: Accessible.Pane
    Accessible.name: title

    function setTab(t) {
        tab = t === "day" ? "day" : "month"
    }

    function setDimension(d) {
        if (d === dimension || !UsageText.dimensions.some(x => x.id === d))
            return
        dimension = d
        monthBreakdown.reload()
        dayBreakdown.reload()
    }

    // The CSV of the tab shown, written to `file` (a file: URL); false when it can't start.
    function exportTo(file) {
        const table = UsageText.exportTable(current.summary, current.shownDimension)
        if (csvExport.save(file, JSON.stringify(table.rows), JSON.stringify(table.columns)))
            return true
        panelBanner.kind = "danger"
        panelBanner.show(csvExport.busy ? "An export is still being written." : "The CSV wasn't saved: pick a file on this PC.")
        return false
    }

    function chooseFile() {
        if (dialogLoader.item)
            (dialogLoader.item as FileDialog).open()
        else
            dialogLoader.active = true
    }

    function readSummaries() {
        monthBreakdown.reload()
        dayBreakdown.reload()
        accountsList.read()
    }

    // At most one run a second: the first request at once, the rest of a burst as one run once
    // the second has passed.
    component Gate: Timer {
        property bool due: false
        signal run()

        function request() {
            if (running) {
                due = true
                return
            }
            run()
            start()
        }

        interval: 1000
        onTriggered: {
            if (!due)
                return
            due = false
            run()
            start()
        }
    }

    Gate {
        id: summariesGate
        onRun: root.readSummaries()
    }

    Gate {
        id: capsGate
        onRun: capGauges.read()
    }

    Component.onCompleted: {
        CoreCall.subscribe(core, "usage", root, (kind, seq, data) => {
            summariesGate.request()
            capsGate.request()
        })
        CoreCall.subscribe(core, "caps", root, (kind, seq, data) => {
            if (kind === "event" && data && data.kind === "alert")
                capGauges.addAlert(data)
            capsGate.request()
        })
        CoreCall.subscribe(core, "jobs", root, (kind, seq, data) => {
            const status = data && data.job ? data.job.status : ""
            if (kind !== "event" || ["succeeded", "failed", "cancelled"].indexOf(status) >= 0)
                capsGate.request()
        })
        summariesGate.request()
        capsGate.request()
    }

    CsvExport {
        id: csvExport
        onSaved: path => {
            panelBanner.kind = "ok"
            panelBanner.show("Saved the CSV as " + UsageText.shown(root.core, path, 260) + ".")
        }
        onFailed: (path, why) => {
            panelBanner.kind = "danger"
            panelBanner.show("The CSV wasn't saved: " + UsageText.shown(root.core, why, 400))
        }
    }

    Loader {
        id: dialogLoader
        active: false
        sourceComponent: FileDialog {
            title: "Export the usage as CSV"
            fileMode: FileDialog.SaveFile
            nameFilters: ["CSV files (*.csv)"]
            defaultSuffix: "csv"
            onAccepted: root.exportTo(selectedFile)
        }
        onLoaded: item.open()
    }

    LBanner {
        id: panelBanner
        width: parent.width
    }

    Flow {
        id: header
        anchors.top: panelBanner.visible ? panelBanner.bottom : parent.top
        anchors.left: parent.left
        anchors.right: parent.right
        anchors.margins: Theme.spacing
        spacing: Theme.spacing

        Repeater {
            model: [{id: "month", text: "This month"}, {id: "day", text: "Today"}]
            delegate: LButton {
                required property var modelData
                text: modelData.text
                primary: root.tab === modelData.id
                Accessible.role: Accessible.PageTab
                Accessible.checkable: true
                Accessible.checked: primary
                onClicked: root.setTab(modelData.id)
            }
        }

        Text {
            height: exportButton.height
            leftPadding: Theme.spacing
            verticalAlignment: Text.AlignVCenter
            text: "By"
            textFormat: Text.PlainText
            color: Theme.textMuted
            font.pixelSize: Theme.fontSize
        }

        Repeater {
            model: UsageText.dimensions
            delegate: LButton {
                required property var modelData
                text: modelData.text
                primary: root.dimension === modelData.id
                Accessible.role: Accessible.RadioButton
                Accessible.name: "By " + modelData.text.toLowerCase()
                Accessible.checkable: true
                Accessible.checked: primary
                onClicked: root.setDimension(modelData.id)
            }
        }

        LButton {
            id: exportButton
            text: "Export CSV…"
            enabled: root.current.loaded && !csvExport.busy
            Accessible.name: "Export the rows shown as CSV"
            onClicked: root.chooseFile()
        }
    }

    Flickable {
        id: scroller

        readonly property bool wide: width >= 900

        anchors.top: header.bottom
        anchors.left: parent.left
        anchors.right: parent.right
        anchors.bottom: parent.bottom
        anchors.margins: Theme.spacing
        clip: true
        contentWidth: width
        contentHeight: content.height
        boundsBehavior: Flickable.StopAtBounds
        // The keyboard scrolls it: one tab stop, then the arrows, Page Up and Page Down, Home
        // and End.
        activeFocusOnTab: true

        Accessible.role: Accessible.Pane
        Accessible.name: root.title + " figures"

        function scrollBy(dy) {
            contentY = Math.max(0, Math.min(contentHeight - height, contentY + dy))
        }

        Keys.onUpPressed: scrollBy(-40)
        Keys.onDownPressed: scrollBy(40)
        Keys.onPressed: event => {
            if (event.key === Qt.Key_PageUp)
                scrollBy(-height)
            else if (event.key === Qt.Key_PageDown)
                scrollBy(height)
            else if (event.key === Qt.Key_Home)
                contentY = 0
            else if (event.key === Qt.Key_End)
                scrollBy(contentHeight)
            else
                return
            event.accepted = true
        }

        Grid {
            id: content
            width: scroller.width
            columns: scroller.wide ? 2 : 1
            spacing: 2 * Theme.spacing

            Item {
                width: scroller.wide ? Math.floor(content.width * 0.58) : content.width
                height: root.tab === "day" ? dayBreakdown.height : monthBreakdown.height

                SpendBreakdown {
                    id: monthBreakdown
                    width: parent.width
                    core: root.core
                    period: "month"
                    dimension: root.dimension
                    visible: root.tab === "month"
                }

                SpendBreakdown {
                    id: dayBreakdown
                    width: parent.width
                    core: root.core
                    period: "day"
                    dimension: root.dimension
                    visible: root.tab === "day"
                }
            }

            Column {
                width: scroller.wide ? content.width - content.spacing - Math.floor(content.width * 0.58)
                                     : content.width
                spacing: Theme.spacing

                CapGauges {
                    id: capGauges
                    width: parent.width
                    core: root.core
                }

                AccountsList {
                    id: accountsList
                    width: parent.width
                    core: root.core
                }

                ClaudeCodeSection {
                    id: claudeCodeSection
                    width: parent.width
                }
            }
        }
    }

    // Where the figures are scrolled to, and that there is more.
    Rectangle {
        visible: scroller.visibleArea.heightRatio < 1
        x: scroller.x + scroller.width - width
        y: scroller.y + scroller.visibleArea.yPosition * scroller.height
        width: 3
        height: Math.max(16, scroller.visibleArea.heightRatio * scroller.height)
        radius: 1
        color: Theme.textMuted
    }

    // The keyboard focus on the figures.
    Rectangle {
        visible: scroller.activeFocus
        x: scroller.x - 2
        y: scroller.y - 2
        width: scroller.width + 4
        height: scroller.height + 4
        color: "transparent"
        border.color: Theme.focus
        border.width: Theme.focusWidth
    }
}
