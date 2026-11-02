// "Notifications on this PC" (APP-146, APP-022; P4a-R26): the notification path in words, "Set up"
// while it has something to write (the own shortcut and scheme, never `.lilref`), and Windows' own page
// while that is what can turn them on. `path` is the core's reading, null while none can be trusted.
import QtQuick
import Lilith.UiKit
import "../diagnostics/DiagnosticsText.js" as Words
import "../diagnostics/NotificationText.js" as NotificationText

LCard {
    id: root

    required property var core
    // desktop.get's notification_path, or the setup's own re-read; null when not known.
    property var path: null
    // The setup bridge (SetupBridge, or a test's fake).
    required property var setup
    // What the last setup did (lil_qt::setup::SetupReport), or null.
    property var report: null
    // Why Set up didn't start; "" for none.
    property string setupProblem
    property string _opening
    // What opens Windows' page: true when it did.
    required property var openExternally

    signal setUpPressed()
    signal checkAgain()
    // Set up had the keyboard, and going left it with nothing: the panel takes the keyboard.
    signal focusLeft()

    property bool _hadFocus: false

    readonly property bool appMissing: !!setup && setup.appMissing === true
    readonly property var problems: NotificationText.problems(path, appMissing)
    readonly property alias setUpButton: setUpButton
    readonly property alias windowsButton: windowsButton
    readonly property alias checkButton: checkButton

    // A run has ended, its report applied. Only if the button it was pressed on has gone and the
    // keyboard is nowhere of use, still on that hidden button or fallen to the window's root as LBanner
    // tells it, is there anything to pick up: else it has moved on, and stays there.
    function runEnded() {
        const focused = Window.window ? Window.window.activeFocusItem : null
        if (_hadFocus && !setUpButton.visible
                && (focused === setUpButton || String(focused).startsWith("QQuickRootItem")))
            focusLeft()
        _hadFocus = false
    }

    title: "Notifications on this PC"

    onPathChanged: _opening = ""

    Column {
        width: parent.width
        spacing: Theme.spacing

        LineText {
            visible: !root.path
            tone: "muted"
            text: "Not known until the core answers."
        }

        LineText {
            visible: !!root.path && root.problems.length === 0
            text: NotificationText.reachable
        }

        Repeater {
            model: root.problems
            delegate: LineText {
                required property var modelData
                tone: "warning"
                text: Words.shown(root.core, modelData, 400)
            }
        }

        LineText {
            visible: text !== ""
            tone: NotificationText.setupFailed(root.report) ? "warning" : ""
            text: NotificationText.setupOutcome(root.report, (t, max) => Words.shown(root.core, t, max))
        }

        LineText {
            visible: root.setupProblem !== "" || root._opening !== ""
            tone: "warning"
            text: root.setupProblem !== "" ? root.setupProblem : root._opening
        }

        LineText {
            visible: root.appMissing && NotificationText.setupOffered(root.path)
            tone: "muted"
            text: "lil-image.exe should be at " + Words.shown(root.core, root.setup ? root.setup.appPath : "", 260) + "."
        }

        Flow {
            width: parent.width
            spacing: Theme.spacing

            LButton {
                id: setUpButton
                visible: NotificationText.setupOffered(root.path) && !root.appMissing
                primary: true
                requiresCore: true
                // Stays enabled while it runs, so the keyboard stays here: the bridge runs one at a time.
                text: root.setup.busy ? "Setting up…" : "Set up"
                onClicked: {
                    root.setUpPressed()
                    root._hadFocus = activeFocus && root.setup.busy
                }
            }

            LButton {
                id: windowsButton
                visible: NotificationText.windowsCanFix(root.path)
                text: NotificationText.windowsSettingsLabel
                onClicked: {
                    root._opening = root.openExternally(NotificationText.windowsSettingsUrl)
                        ? "" : "Windows' notification settings didn't open. Find them in Windows Settings, under System."
                }
            }

            LButton {
                id: checkButton
                visible: root.problems.length > 0
                requiresCore: true
                text: "Check again"
                onClicked: root.checkAgain()
            }
        }
    }
}
