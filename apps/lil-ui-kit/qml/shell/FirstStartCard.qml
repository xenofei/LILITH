// The notification setup offered on the app's first start (APP-022; owner decision 2026-10-22): the
// same "Set up" as the Settings panel's, on the shared bridge, shown only while it has something to
// write. "Not now" puts it away. `state.json` was written as the shell started
// (ShellState.claimFirstStart), so a later start never shows it, whatever was pressed here; Settings
// still has the button.
//
// It lies in the shell's top column, under the banners and above the docks, so it can't cover a
// banner's way out, and it asks nothing of the workspace: a first start with no workspace has both,
// one above the other. It stays away until the core has said what is missing: with no answer there
// is nothing to offer.
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

    // Taken once, as the shell starts.
    property bool firstStart: false
    property bool dismissed: false
    // The core's reading, from its own read or the setup's.
    property var path: null
    // What the last setup did, or null.
    property var report: null
    property string _problem

    readonly property bool wanted: firstStart && !dismissed && !!setup && !setup.appMissing
                                   && NotificationText.setupOffered(path)

    readonly property alias setUpButton: setUpButton
    readonly property alias notNowButton: notNowButton

    closable: false
    text: {
        if (_problem !== "")
            return _problem
        const outcome = NotificationText.setupOutcome(report, (t, max) => Words.shown(core, t, max))
        if (outcome !== "")
            return outcome
        const other = path && path.handlers === "other_channel"
            ? " Another LILITH build or another program handles lil: links: run lil doctor to see which one." : ""
        return "Set up adds what Windows needs to show LILITH's notifications and open their links: "
               + NotificationText.setupWrites(path) + ", for your user account. You can also do this later "
               + "in Settings." + other
    }

    actions: [
        LButton {
            id: setUpButton
            primary: true
            requiresCore: true
            core: root.core
            enabled: !root.setup.busy
            text: root.setup.busy ? "Setting up…" : "Set up"
            onClicked: {
                root._problem = ""
                if (!root.setup.run() && !root.setup.busy)
                    root._problem = "Set up didn't start. Run lil doctor instead."
            }
        },
        LButton {
            id: notNowButton
            text: "Not now"
            onClicked: root.dismissed = true
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
            if (!root.firstStart || root.dismissed)
                return
            const report = JSON.parse(reportJson)
            root.report = report
            if (report.notification_path) {
                pathRead.eventApplied()
                root.path = report.notification_path
            } else {
                pathRead.request()
            }
            if (root.visible && root.wanted)
                root.announce()
        }
    }

    ToolRead {
        id: pathRead
        core: root.core
        tool: "desktop.get"
        args: ({fresh: true})
        topic: "desktop"
        active: root.firstStart && !root.dismissed
        onAnswered: (structured, current) => {
            if (structured && (current || root.path === null))
                root.path = structured.notification_path
            if (current && root.report && root.report.reread_error)
                root.report = Object.assign({}, root.report, {reread_error: null})
        }
    }

    Component.onCompleted: firstStart = ShellState.claimFirstStart(stateDir)
}
