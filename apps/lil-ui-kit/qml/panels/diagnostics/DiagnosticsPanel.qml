// The Diagnostics panel (ARCH-121, ARCH-126, ARCH-127, APP-134, APP-136, CORE-164, QA-083): the
// versions, the engines with their launch profile, the queue, the telemetry summary, the status
// lines (backups, keep-awake, this start's times), the notification path, the recent errors by
// code, the event log and the model inventory, with "Export diagnostics bundle".
//
// Each card reads its tools when the panel is first shown, reads them again each time it is shown
// again (at most once in 2 s, so "Run lil doctor to fix" clears once you have), and follows its
// topics while shown (ToolRead). Behind a tab it reads nothing; it only keeps the topics followed,
// so that the event log hears them from the start. `shell` and `eventLog` are the shell's, filled in
// when it makes the dock (ShellWindow.createDock): with no shell the buttons that open another panel
// do nothing, and with no `eventLog` the panel keeps its own EventLogModel.
//
// The export: you pick the file in a save dialog (the panel never names a path), then
// `diagnostics.bundle` writes its zip in the core's folder and BundleExport copies it to your file,
// off the GUI thread. Nothing is sent anywhere (ARCH-122). "Show in Explorer" shows your file, or,
// when the copy failed, the core's. Paths can name your files: they are shown as untrusted text and
// logged nowhere new.
pragma ComponentBehavior: Bound
import QtQuick
import QtQuick.Dialogs
import Lilith.Core
import Lilith.Shim
import Lilith.UiKit
import "DiagnosticsText.js" as Words

Rectangle {
    id: root

    required property var core
    required property string title

    // The shell (openPanelOn), or null.
    property var shell: null
    // The shell's EventLogModel, or null.
    property var eventLog: null
    // What reaches Explorer and the app's versions: LilShim, or a test's fake.
    property var shim: LilShim

    // The bundle Show in Explorer shows; "" until there is one.
    property string exportedPath
    // The job the last failed export's fix opens; "" for none.
    property string fixJobId
    property bool exporting: false

    // Whether the cards are live. What the shell does while it lays itself out isn't a show (each
    // tab can be in front for a moment), nor a flicker (floating and re-docking moves the item) a
    // hide.
    property bool shown: false
    readonly property bool isOnScreen: visible && width > 0 && height > 0
                                       && Window.window !== null && Window.window.visible
                                       && (!shell || shell.layoutSettled !== false)

    readonly property alias versions: versionsCard
    readonly property alias engines: enginesCard
    readonly property alias providers: providersCard
    readonly property alias queue: queueCard
    readonly property alias telemetry: telemetryCard
    readonly property alias status: statusLines
    readonly property alias notifications: notificationCard
    readonly property alias errors: recentErrors
    readonly property alias log: eventLogCard
    readonly property alias models: modelsCard
    readonly property alias ownLog: ownEventLog
    readonly property alias banner: panelBanner
    readonly property alias exportButton: exportButton
    readonly property alias explorerButton: explorerButton
    readonly property alias fixButton: fixButton
    readonly property alias exporter: bundleExport
    readonly property alias scroller: scroller

    color: Theme.surface

    Accessible.role: Accessible.Pane
    Accessible.name: title

    onIsOnScreenChanged: settle.restart()

    Component.onCompleted: {
        settle.restart()
        for (const topic of ["engines", "diagnostics", "desktop", "telemetry", "jobs"])
            CoreCall.subscribe(core, topic, root)
    }

    Timer {
        id: settle
        interval: root.isOnScreen ? 50 : 500
        onTriggered: root.shown = root.isOnScreen
    }

    function showJob(jobId) {
        if (shell)
            shell.openPanelOn("jobs", "showJob", jobId, true)
    }

    function chooseFile() {
        if (dialogLoader.item)
            (dialogLoader.item as FileDialog).open()
        else
            dialogLoader.active = true
    }

    // Selects the bundle in Explorer, or says why it can't.
    function showBundle() {
        if (!shim.showInExplorer(exportedPath))
            showProblem("Explorer can't show " + Words.shown(core, exportedPath, 260)
                        + ": it has moved since, or it isn't a .zip file.")
    }

    // Exports a new bundle to `file`, a file: URL you picked.
    function exportTo(file) {
        exporting = true
        fixJobId = ""
        panelBanner.close()
        CoreCall.call(core, "diagnostics.bundle", {}, {
            owner: root,
            ok: r => root.copyBundle(file, r.structured),
            error: e => {
                root.exporting = false
                root.fixJobId = Words.fixJob(e)
                root.showProblem("The diagnostics bundle wasn't made: "
                                 + Words.errorText(root.core, e, root.fixJobId !== ""))
            }
        })
    }

    function copyBundle(file, bundle) {
        _bundle = bundle
        if (!bundleExport.save(file, bundle.path)) {
            exporting = false
            exportedPath = bundle.path
            showProblem("The diagnostics bundle wasn't saved: pick a file on this PC. The bundle is still at "
                        + Words.shown(core, bundle.path, 260) + ".")
        }
    }

    function showProblem(text) {
        panelBanner.kind = "danger"
        panelBanner.show(text)
    }

    property var _bundle: null

    readonly property ToolRead desktopRead: ToolRead {
        core: root.core
        tool: "desktop.get"
        // Never the core's 10 s cache: what you fixed a moment ago shows.
        args: ({fresh: true})
        topic: "desktop"
        active: root.shown
        onTopicEvent: data => {
            // The event's notification path is as it was at the change: read it afresh.
            if (data && data.kind === "settings")
                root.desktopRead.request()
        }
    }

    // Only while the shell gives none.
    EventLogModel {
        id: ownEventLog
        core: root.eventLog ? null : root.core
    }

    BundleExport {
        id: bundleExport
        onSaved: path => {
            const b = root._bundle || {}
            root.exporting = false
            root.exportedPath = path
            panelBanner.kind = "ok"
            panelBanner.show("Saved the diagnostics bundle as " + Words.shown(root.core, path, 260) + ": "
                             + MoneyText.count(String((b.files || []).length)) + " files, "
                             + MoneyText.count(String(b.redactions || 0)) + " values redacted. Nothing is sent "
                             + "anywhere: it leaves this PC only when you hand it on.")
        }
        onFailed: (path, why, bundleRead) => {
            const b = root._bundle || {}
            root.exporting = false
            root.exportedPath = bundleRead ? b.path || "" : ""
            root.showProblem("The diagnostics bundle wasn't saved as " + Words.shown(root.core, path, 260) + ": "
                             + Words.shown(root.core, why, 400)
                             + (bundleRead ? " The bundle is still at " + Words.shown(root.core, b.path, 260) + "." : ""))
        }
    }

    Loader {
        id: dialogLoader
        active: false
        sourceComponent: FileDialog {
            title: "Export the diagnostics bundle"
            fileMode: FileDialog.SaveFile
            nameFilters: ["ZIP files (*.zip)"]
            defaultSuffix: "zip"
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

        LButton {
            id: exportButton
            text: root.exporting || bundleExport.busy ? "Exporting…" : "Export diagnostics bundle"
            enabled: !root.exporting && !bundleExport.busy
            onClicked: root.chooseFile()
        }

        LButton {
            id: explorerButton
            visible: root.exportedPath !== ""
            text: "Show in Explorer"
            onClicked: root.showBundle()
        }

        LButton {
            id: fixButton
            visible: root.fixJobId !== ""
            text: "Show the job"
            onClicked: root.showJob(root.fixJobId)
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
        // The keyboard scrolls it: one tab stop, then the arrows, Page Up and Page Down, Home and
        // End.
        activeFocusOnTab: true

        Accessible.role: Accessible.Pane
        Accessible.name: root.title + " figures"

        function scrollBy(dy) {
            contentY = Math.max(0, Math.min(contentHeight - height, contentY + dy))
        }

        // A tab stop in the cards that is scrolled away comes into view.
        function reveal(item) {
            if (!item || item === scroller)
                return
            let inside = false
            for (let p = item.parent; p && !inside; p = p.parent)
                inside = p === content
            if (!inside)
                return
            const top = item.mapToItem(content, 0, 0).y
            if (top < contentY)
                contentY = Math.max(0, top)
            else if (top + item.height > contentY + height)
                contentY = Math.min(Math.max(0, contentHeight - height), top + item.height - height)
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

        Connections {
            target: root.Window.window
            function onActiveFocusItemChanged() {
                scroller.reveal(root.Window.window.activeFocusItem)
            }
        }

        Grid {
            id: content
            width: scroller.width
            columns: scroller.wide ? 2 : 1
            spacing: 2 * Theme.spacing

            readonly property real columnWidth: scroller.wide ? Math.floor((width - spacing) / 2) : width

            Column {
                width: content.columnWidth
                spacing: Theme.spacing

                VersionsCard {
                    id: versionsCard
                    width: parent.width
                    core: root.core
                    shim: root.shim
                    engines: enginesCard.engines
                    gpu: telemetryCard.gpu
                }

                EnginesCard {
                    id: enginesCard
                    width: parent.width
                    core: root.core
                    active: root.shown
                    gpu: telemetryCard.gpu
                    gpuRead: telemetryCard.read
                }

                ProvidersCard {
                    id: providersCard
                    width: parent.width
                }

                QueueSummary {
                    id: queueCard
                    width: parent.width
                    core: root.core
                    active: root.shown
                }

                TelemetrySummary {
                    id: telemetryCard
                    width: parent.width
                    core: root.core
                    active: root.shown
                }

                StatusLines {
                    id: statusLines
                    width: parent.width
                    core: root.core
                    settings: root.desktopRead.result
                    canOpenBackups: !!root.shell
                    onOpenBackups: {
                        if (root.shell)
                            root.shell.openPanelOn("backups", "", "", true)
                    }
                }

                NotificationPathCard {
                    id: notificationCard
                    width: parent.width
                    core: root.core
                    path: root.desktopRead.result ? root.desktopRead.result.notification_path : null
                    problem: Words.errorText(root.core, root.desktopRead.error)
                }
            }

            Column {
                width: content.columnWidth
                spacing: Theme.spacing

                RecentErrors {
                    id: recentErrors
                    width: parent.width
                    core: root.core
                    active: root.shown
                    onShowJob: jobId => root.showJob(jobId)
                }

                EventLog {
                    id: eventLogCard
                    width: parent.width
                    core: root.core
                    model: root.eventLog ? root.eventLog : ownEventLog
                }

                ModelsReport {
                    id: modelsCard
                    width: parent.width
                    core: root.core
                    active: root.shown
                    enginesKnown: enginesCard.known
                    notConfigured: enginesCard.notConfigured
                    attachedWithNoInventory: enginesCard.attachedWithNoInventory
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
