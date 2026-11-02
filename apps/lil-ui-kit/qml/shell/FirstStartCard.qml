// The notification setup offered on the app's first start (APP-022; owner decision 2026-10-22): the
// same "Set up" as the Settings panel's, on the shared bridge, shown only while it has something to
// write. "Not now" puts it away.
//
// As the shell starts it only reads whether this is a first start (no `state.json*`). The core's first
// answer decides, once: `state.json` is recorded unless one is there by now (Home's own write may be
// first on a slow answer), and the card shows if something is missing. So a session the core never
// answers uses nothing up, and the next start is still a first. From then on the offer can only end:
// put away, set up, or told by the core there is nothing left to write. Then it stops reading, so a
// later resync can't bring the card back days into the session. A later start never shows it; Settings
// still has the button.
//
// It lies in the shell's top column, under the banners and above the docks, so it can't cover a
// banner's way out, and it asks nothing of the workspace: a first start with no workspace has both,
// one above the other.
pragma ComponentBehavior: Bound
import QtQuick
import Lilith.Core
import Lilith.UiKit
import "../panels/diagnostics/DiagnosticsText.js" as Words
import "../panels/diagnostics/NotificationText.js" as NotificationText

LBanner {
    id: root

    required property var core
    // The setup bridge (SetupBridge, or a test's fake).
    required property var setup
    // The app's state folder (ShellWindow's `stateDir`).
    required property string stateDir

    // Read once, as the shell starts.
    property bool firstStart: false
    // The offer is over, for good.
    property bool done: false
    // The core's reading, from its own read or the setup's.
    property var path: null
    // What the last setup did, or null.
    property var report: null
    property string _problem
    // The core has answered, and the record is made.
    property bool _decided: false
    // Set up was pressed here: its outcome is said here.
    property bool _pressed: false

    readonly property bool wanted: firstStart && !done && !!setup && !setup.appMissing
                                   && NotificationText.setupOffered(path)

    readonly property alias setUpButton: setUpButton
    readonly property alias notNowButton: notNowButton

    // The core's reading came. The first one decides; after that it can only end the offer.
    function tookPath(reading) {
        const first = !_decided
        _decided = true
        path = reading
        if (first)
            ShellState.recordFirstStart(stateDir)
        if (!wanted)
            done = true
    }

    // Windows keeps the notifications from you whatever Set up writes (an elevated start, say), as the
    // newest reading has it: the setup's own, else the card's.
    readonly property bool windowsOff: {
        const now = report && report.notification_path ? report.notification_path : path
        return !!now && now.toasts === "unavailable"
    }
    readonly property string windowsNote: windowsOff
        ? " Windows still keeps LILITH's notifications from you: Settings says why and what to do." : ""

    closable: false
    text: {
        if (_problem !== "")
            return _problem
        const outcome = NotificationText.setupOutcome(report, (t, max) => Words.shown(core, t, max))
        if (outcome !== "")
            return outcome + windowsNote
        const other = path && path.handlers === "other_channel"
            ? " Another LILITH build or another program handles lil: links: run lil doctor to see which one." : ""
        const adds = windowsOff ? "Set up adds LILITH's part of what's needed to show its notifications and open their links: "
                                : "Set up adds what Windows needs to show LILITH's notifications and open their links: "
        return adds + NotificationText.setupWrites(path) + ", for your user account. You can also do this later "
               + "in Settings." + other + windowsNote
    }

    actions: [
        LButton {
            id: setUpButton
            primary: true
            requiresCore: true
            core: root.core
            // Stays enabled while it runs, so the keyboard stays here: the bridge runs one at a time.
            text: root.setup.busy ? "Setting up…" : "Set up"
            onClicked: {
                root._problem = ""
                if (root.setup.run())
                    root._pressed = true
                else if (!root.setup.busy) {
                    root._problem = "Set up didn't start. Run lil doctor instead."
                    root.announce()
                }
            }
        },
        LButton {
            id: notNowButton
            text: "Not now"
            onClicked: root.done = true
        }
    ]

    onWantedChanged: {
        if (wanted)
            show()
        else
            close()
    }

    Connections {
        target: root.setup
        function onFinished(reportJson) {
            if (!root.firstStart)
                return
            const report = JSON.parse(reportJson)
            root.report = report
            // Said while it still shows: a set-up that works takes the card away.
            if (root._pressed && root.visible)
                root.announce()
            root._pressed = false
            if (report.notification_path) {
                pathRead.eventApplied()
                root.tookPath(report.notification_path)
            } else {
                pathRead.request()
            }
        }
    }

    ToolRead {
        id: pathRead
        core: root.core
        tool: "desktop.get"
        args: ({fresh: true})
        topic: "desktop"
        active: root.firstStart && !root.done
        onAnswered: (structured, current) => {
            if (structured && (current || root.path === null))
                root.tookPath(structured.notification_path)
            if (current)
                root.report = NotificationText.reportAfterRead(root.report, structured && structured.notification_path)
        }
    }

    Component.onCompleted: firstStart = ShellState.isFirstStart(stateDir)
}
