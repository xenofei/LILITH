// The Settings panel (APP-146, APP-022, ARCH-127, CORE-160; P4a-R26, P4a-R36): a switch per
// notification category, "Quiet while fullscreen", fullscreen auto-pause, keep-awake, and the
// notification path with "Set up".
//
// Everything it shows is the core's word. `settings` holds `desktop.get`'s last answer; each switch
// sends one key (`app/desktop.set`) and shows the answer, never its own guess. While a set of a switch
// is out, that switch holds (a double-click sends one set); an event from another app changes every
// other switch. Sets of two switches can answer out of order, so while another is out an answer speaks
// only for its own key, and the last one to settle reads again. A refusal leaves the switch as it was
// and says why; an answer lost with the connection may have been written, so the panel says so and
// reads again when the core is back.
//
// It reads when it is shown (never the core's 10 s cache, so what you fixed elsewhere shows), follows
// the `desktop` topic while shown, and reads again on a `resync_required` or a snapshot (ToolRead). A
// read that fails says so; a failed read is retried when `ready` comes. No switch takes a write before
// the first answer, or while what it shows may be old (`fresh`): then they are dimmed and held.
//
// "Set up" runs on the setup bridge, which the first-start card shares (`setup`, filled in by the shell
// when it makes the dock): whichever pressed it, each hears the report, and its re-read is the path
// shown here. A report with no path (the core didn't answer) reads again through the session.
pragma ComponentBehavior: Bound
import QtQuick
import Lilith.Core
import Lilith.UiKit
import "../diagnostics/DiagnosticsText.js" as Words

Rectangle {
    id: root

    required property var core
    required property string title

    // The setup bridge: SetupBridge, or a test's fake.
    property var setup: SetupBridge
    // What opens Windows' page; true when it did. A test's fake, or Qt's own.
    property var openExternally: url => Qt.openUrlExternally(url)

    // The core's last word: DesktopSettings without its path; null before any.
    property var settings: null
    // The notification path, from the last read or the setup's own; null before any.
    property var path: null
    // Setting id ("notifications.<key>", or the field's name) -> true, while a set of it is out.
    property var setsOut: ({})
    // A topic's resync or snapshot: until a read answers, the switches may be old.
    property bool reloading: false
    // What the last setup did, or null.
    property var report: null
    property string setupProblem

    property bool _raced: false
    // A set whose answer was lost: read again as soon as the core can answer.
    property bool _lostAnswer: false
    property bool _closeOnRead: false

    // Whether the panel is live. A flicker (floating and re-docking moves the item) isn't a hide.
    property bool shown: false
    readonly property bool isOnScreen: visible && width > 0 && height > 0
                                       && Window.window !== null && Window.window.visible

    // Whether what the switches show is the core's word now.
    readonly property bool fresh: desktopRead.loaded && !desktopRead.error && !reloading
                                  && !!core && core.connection === "ready"

    // Why the switches aren't live, in words; "" when they are.
    readonly property string status: {
        if (fresh)
            return ""
        const known = settings !== null
        if (!core || core.connection !== "ready")
            return known ? "The core isn't connected, so these show what it last said and can't be changed now."
                         : "The core isn't connected, so the settings can't be read yet."
        if (desktopRead.error)
            return "The settings couldn't be read: " + Words.errorText(core, desktopRead.error)
                   + (known ? " The switches show what the core last said." : "")
        return "Reading the settings…"
    }

    readonly property alias banner: panelBanner
    readonly property alias statusLine: statusLine
    readonly property alias readAgainButton: readAgainButton
    readonly property alias notifications: notificationToggles
    readonly property alias desktopToggles: desktopToggles
    readonly property alias pathCard: pathCard
    readonly property alias scroller: scroller

    color: Theme.surface

    Accessible.role: Accessible.Pane
    Accessible.name: title

    onIsOnScreenChanged: settle.restart()

    Component.onCompleted: settle.restart()

    Timer {
        id: settle
        interval: root.isOnScreen ? 50 : 500
        onTriggered: {
            // What the last setup did is news only until the panel is left.
            if (root.isOnScreen && !root.shown) {
                root.report = null
                root.setupProblem = ""
            }
            root.shown = root.isOnScreen
        }
    }

    // The core's word on every switch but those waiting for their own answer; with `onlyId`, on
    // that one alone.
    function applySettings(s, onlyId) {
        if (!s)
            return
        const takes = id => setsOut[id] !== true && (!onlyId || onlyId === id)
        const next = Object.assign({}, settings || {})
        next.notifications = Object.assign({}, next.notifications || {})
        for (const key in s.notifications || {}) {
            if (takes("notifications." + key) || !(key in next.notifications))
                next.notifications[key] = s.notifications[key]
        }
        for (const field of ["quiet_while_fullscreen", "fullscreen_autopause", "keep_awake_gpu"]) {
            if (takes(field) || !(field in next))
                next[field] = s[field]
        }
        next.keep_awake_available = s.keep_awake_available
        settings = next
    }

    function setOut(id, out) {
        const next = Object.assign({}, setsOut)
        if (out)
            next[id] = true
        else
            delete next[id]
        setsOut = next
    }

    // One switch's flip: one key to the core. Its answer says what it is now.
    function flip(id, on, label) {
        if (setsOut[id] === true)
            return
        const dot = id.indexOf(".")
        const params = dot > 0 ? {notifications: {[id.slice(dot + 1)]: on}} : {[id]: on}
        if (Object.keys(setsOut).length > 0)
            _raced = true
        setOut(id, true)
        CoreCall.appCall(core, "app/desktop.set", params, {
            owner: root,
            ok: answer => {
                setOut(id, false)
                desktopRead.eventApplied()
                applySettings(answer, Object.keys(setsOut).length > 0 || _raced ? id : "")
                panelBanner.close()
                settled()
            },
            error: e => {
                setOut(id, false)
                refused(label, e)
                settled()
            },
            pending: () => {
                setOut(id, false)
                refused(label, {message: "The core asked for a confirmation, so it wasn't changed."})
                settled()
            }
        })
    }

    // The last set out has answered: two that raced may have answered out of order.
    function settled() {
        if (_raced && Object.keys(setsOut).length === 0) {
            _raced = false
            desktopRead.request()
        }
    }

    function refused(label, error) {
        const shownLabel = Words.shown(core, label, 80)
        panelBanner.kind = "danger"
        if (error.code === "core.unreachable") {
            _lostAnswer = true
            panelBanner.show("\"" + shownLabel + "\": the core went away before it answered, so this may have "
                             + "changed. The switch shows what the core last said, until it can be read again.")
            readAfterLoss()
        } else {
            panelBanner.show("\"" + shownLabel + "\" wasn't changed: " + Words.errorText(core, error))
        }
    }

    function readAfterLoss() {
        if (!_lostAnswer || !core || core.connection !== "ready")
            return
        _lostAnswer = false
        _closeOnRead = true
        desktopRead.request()
    }

    function pressSetUp() {
        setupProblem = ""
        if (!setup.run() && !setup.busy)
            setupProblem = "Set up didn't start. Run lil doctor instead."
    }

    readonly property ToolRead desktopRead: ToolRead {
        core: root.core
        tool: "desktop.get"
        // Never the core's 10 s cache: what you fixed a moment ago shows.
        args: ({fresh: true})
        topic: "desktop"
        active: root.shown
        onTopicEvent: data => {
            // Every switch's state; its notification path is as it was at the change, so that isn't
            // taken from here.
            if (data && data.kind === "settings" && data.settings) {
                root.desktopRead.eventApplied()
                root.applySettings(data.settings, "")
            }
        }
        onReloading: root.reloading = true
        onAnswered: (structured, current) => {
            if (structured && (current || root.path === null))
                root.path = structured.notification_path
            if (!current)
                return
            root.applySettings(structured, "")
            root.reloading = false
            if (root._closeOnRead) {
                root._closeOnRead = false
                panelBanner.close()
            }
            // The path speaks now: an old "couldn't say" doesn't stay beside it.
            if (root.report && root.report.reread_error)
                root.report = Object.assign({}, root.report, {reread_error: null})
        }
    }

    Connections {
        target: root.core
        function onConnectionChanged() {
            root.readAfterLoss()
        }
    }

    Connections {
        target: root.setup
        function onFinished(reportJson) {
            const report = JSON.parse(reportJson)
            root.report = report
            if (report.notification_path) {
                root.desktopRead.eventApplied()
                root.path = report.notification_path
            }
            if (!report.notification_path || root.desktopRead.error)
                root.desktopRead.request()
        }
    }

    LBanner {
        id: panelBanner
        width: parent.width
    }

    Row {
        id: statusRow
        visible: root.status !== ""
        anchors.top: panelBanner.visible ? panelBanner.bottom : parent.top
        anchors.left: parent.left
        anchors.right: parent.right
        anchors.margins: Theme.spacing
        spacing: Theme.spacing

        LineText {
            id: statusLine
            width: parent.width - (readAgainButton.visible ? readAgainButton.width + parent.spacing : 0)
            anchors.verticalCenter: parent.verticalCenter
            tone: root.desktopRead.error ? "warning" : "muted"
            text: root.status
        }

        LButton {
            id: readAgainButton
            visible: !!root.desktopRead.error
            requiresCore: true
            text: "Read again"
            onClicked: root.desktopRead.request()
        }
    }

    Flickable {
        id: scroller

        readonly property bool wide: width >= 900

        anchors.top: statusRow.visible ? statusRow.bottom : panelBanner.visible ? panelBanner.bottom : parent.top
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
        Accessible.name: root.title + " list"

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

            NotificationToggles {
                id: notificationToggles
                width: content.columnWidth
                visible: root.settings !== null
                opacity: root.fresh ? 1 : 0.5
                core: root.core
                settings: root.settings
                setsOut: root.setsOut
                live: root.fresh
                onFlip: (id, on, label) => root.flip(id, on, label)
            }

            Column {
                width: content.columnWidth
                spacing: 2 * Theme.spacing

                DesktopToggles {
                    id: desktopToggles
                    width: parent.width
                    visible: root.settings !== null
                    opacity: root.fresh ? 1 : 0.5
                    settings: root.settings
                    setsOut: root.setsOut
                    live: root.fresh
                    onFlip: (id, on, label) => root.flip(id, on, label)
                }

                NotificationSetup {
                    id: pathCard
                    width: parent.width
                    core: root.core
                    setup: root.setup
                    path: root.fresh ? root.path : null
                    report: root.report
                    setupProblem: root.setupProblem
                    openExternally: root.openExternally
                    onSetUpPressed: root.pressSetUp()
                    onCheckAgain: root.desktopRead.request()
                }
            }
        }
    }

    // Where the list is scrolled to, and that there is more.
    Rectangle {
        visible: scroller.visibleArea.heightRatio < 1
        x: scroller.x + scroller.width - width
        y: scroller.y + scroller.visibleArea.yPosition * scroller.height
        width: 3
        height: Math.max(16, scroller.visibleArea.heightRatio * scroller.height)
        radius: 1
        color: Theme.textMuted
    }

    // The keyboard focus on the list.
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
