// One confirmation's facts (APP-115, APP-045): who asked, the exact action, the files, packages and
// scripts it names, the kind of request and the time left. The tool and every argument are what the
// core sent, not a summary, shown as plain text through Core.shownText (P4a-R8): U+202E is a marker,
// control characters are gone, and a value too long to show says so. Children go under the facts
// (the panel's button).
import QtQuick
import Lilith.Core
import Lilith.UiKit
import "../../dialogs/ConfirmText.js" as Words

LCard {
    id: root

    required property var core
    required property string confirmationId
    property var view: null
    default property alias footer: footerSlot.data

    // The arguments as shown.
    readonly property var argumentRows: view ? Words.argumentRows(view.arguments, _shown) : []
    readonly property var files: view ? Words.filesOf(view.arguments, _shown) : []
    readonly property string whoText: view ? Words.who(view.caller_kind, _shown) : ""
    readonly property string actionText: view ? _shown(view.tool, 200) : ""
    readonly property string kindText: !view ? ""
        : Words.coversCap(view) && (view.covers || []).indexOf("confirm") >= 0 ? "Agent request and spending cap"
        : Words.coversCap(view) ? "Spending cap" : "Agent request"
    property real nowMs: Date.now()
    readonly property string leftText: view && view.state === "pending" ? Words.timeLeft(view.expires_at, nowMs) : ""
    readonly property bool ranOut: !!view && view.state === "pending" && leftText === ""
    readonly property string countdownText: leftText !== "" ? "Expires in " + leftText : ""
    readonly property alias factsLine: factsLine

    function _shown(text, max) {
        return Core.shownText(String(text), max)
    }

    title: !view ? _shown(confirmationId, 60) : whoText + " asks to run " + actionText

    Timer {
        interval: 1000
        repeat: true
        running: !!root.view && root.view.state === "pending"
        onTriggered: root.nowMs = Date.now()
    }

    Column {
        id: facts
        width: parent.width
        spacing: Theme.spacing / 2

        Text {
            id: factsLine
            width: parent.width
            visible: !!root.view
            text: root.kindText + (root.view && root.view.project ? " · project " + root._shown(root.view.project, 100) : "")
                  + (root.countdownText !== "" ? " · " + root.countdownText : "")
            textFormat: Text.PlainText
            color: Theme.textMuted
            font.pixelSize: Theme.fontSize
            wrapMode: Text.Wrap
            Accessible.role: Accessible.StaticText
            Accessible.name: text
        }

        Text {
            objectName: "files"
            width: parent.width
            visible: root.files.length > 0
            text: "Files, packages and scripts:\n" + root.files.join("\n")
            textFormat: Text.PlainText
            color: Theme.text
            font.pixelSize: Theme.fontSize
            wrapMode: Text.WrapAnywhere
            Accessible.role: Accessible.StaticText
            Accessible.name: text
        }

        Text {
            width: parent.width
            visible: root.argumentRows.length > 0
            text: "Exact action:"
            textFormat: Text.PlainText
            color: Theme.textMuted
            font.pixelSize: Theme.fontSize
        }

        Repeater {
            model: root.argumentRows
            delegate: Column {
                required property var modelData
                width: facts.width

                Text {
                    width: parent.width
                    text: (modelData.key !== "" ? modelData.key + ": " : "") + modelData.text
                    textFormat: Text.PlainText
                    color: Theme.text
                    font.pixelSize: Theme.fontSize
                    wrapMode: Text.WrapAnywhere
                    Accessible.role: Accessible.StaticText
                    Accessible.name: text
                }

                Text {
                    width: parent.width
                    visible: modelData.cut
                    text: "This value is longer than what is shown here, so part of it can't be read."
                    textFormat: Text.PlainText
                    color: Theme.warning
                    font.pixelSize: Theme.fontSize
                    wrapMode: Text.Wrap
                }
            }
        }

        Item {
            id: footerSlot
            width: parent.width
            height: childrenRect.height
        }
    }
}
