// The notification path (APP-146; Appendix G "Ready"): a sentence for each part that keeps a
// notification from you, or that nothing does. It only shows `path`, `desktop.get`'s
// `notification_path`; its words are NotificationText.js, which the Settings panel shares.
import QtQuick
import Lilith.UiKit
import "DiagnosticsText.js" as Words
import "NotificationText.js" as NotificationText

LCard {
    id: root

    required property var core
    // desktop.get's notification_path, null until it is read.
    property var path: null
    // Why it couldn't be read, in words; "" for none.
    property string problem

    readonly property var problems: NotificationText.problems(path)

    title: "Notifications"

    Column {
        width: parent.width
        spacing: 2

        LineText {
            visible: root.problem !== ""
            tone: "warning"
            text: root.problem
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
    }
}
