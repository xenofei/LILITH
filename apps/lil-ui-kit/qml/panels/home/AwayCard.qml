// Home's "While you were away" card (APP-111, P4a-R41): the lines the panel holds, newest first, as
// plain text with their times in the person's local calendar. It shows while it has a line, while
// its read has failed and so it can't say there are none, or while there is more than its read took
// in. Dismiss clears it (HomePanel).
import QtQuick
import Lilith.Core
import Lilith.UiKit

LCard {
    id: root

    required property var panel

    // The lines the card shows; the rest are counted.
    readonly property int maxLines: 20
    readonly property var lines: panel.awayItems.slice(0, maxLines)
    readonly property int moreLines: panel.awayItems.length - lines.length
    readonly property bool failed: panel.readState.jobs === "failed" || panel.readState.confirmations === "failed"
    readonly property bool stale: panel.isStale("jobs") || panel.isStale("confirmations")
    readonly property string note: {
        if (!panel.connected)
            return panel.noteFor("jobs", "")
        if (panel.isStale("jobs"))
            return panel.noteFor("jobs", "the jobs")
        return panel.isStale("confirmations") ? panel.noteFor("confirmations", "the confirmations") : ""
    }
    // Where the rows it doesn't show are.
    readonly property string whereHidden: panel.awayItems.slice(maxLines).some(item => item.kind === "confirmation")
                                          ? "Jobs and Confirmations list them." : "Jobs lists them."
    readonly property string moreText: {
        if (moreLines <= 0 && !panel.awayCut)
            return ""
        if (moreLines > 0)
            return "and " + MoneyText.count(String(moreLines)) + (panel.awayCut ? " or more. " : " more. ") + whereHidden
        return lines.length > 0 ? "and more. " + whereHidden
                                : "More jobs changed while you were away than this card reads. Jobs lists them."
    }
    readonly property alias dismissButton: dismissButton
    readonly property alias noteText: noteText
    readonly property alias moreLabel: moreLabel
    readonly property alias lineRepeater: lineRepeater

    function word(kind) {
        return kind === "finished" ? "Finished" : kind === "failed" ? "Failed" : kind === "needs_you" ? "Needs you"
             : "Confirmation waiting"
    }

    function lineColor(kind) {
        if (stale)
            return Theme.textMuted
        return kind === "failed" ? Theme.danger : kind === "finished" ? Theme.text : Theme.warning
    }

    title: "While you were away"
    visible: panel.awayOn && (panel.awayItems.length > 0 || failed || panel.awayCut)

    Column {
        width: parent.width
        spacing: 4

        Repeater {
            id: lineRepeater
            model: root.lines
            delegate: Text {
                required property var modelData
                width: parent.width
                text: [root.word(modelData.kind), root.panel.shown(modelData.title, 200),
                       root.panel.localStamp(modelData.at)].filter(p => p !== "").join(" · ")
                textFormat: Text.PlainText
                color: root.lineColor(modelData.kind)
                font.pixelSize: Theme.fontSize
                wrapMode: Text.Wrap
                Accessible.role: Accessible.StaticText
                Accessible.name: text
            }
        }

        Text {
            id: moreLabel
            width: parent.width
            visible: root.moreText !== ""
            text: root.moreText
            textFormat: Text.PlainText
            color: Theme.textMuted
            font.pixelSize: Theme.fontSize
            wrapMode: Text.Wrap
            Accessible.role: Accessible.StaticText
            Accessible.name: text
        }

        Text {
            id: noteText
            width: parent.width
            visible: root.note !== ""
            text: root.note
            textFormat: Text.PlainText
            color: Theme.warning
            font.pixelSize: Theme.fontSize
            wrapMode: Text.Wrap
            Accessible.role: Accessible.StaticText
            Accessible.name: text
        }

        LButton {
            id: dismissButton
            requiresCore: true
            visible: root.panel.awayItems.length > 0 || root.panel.awayCut
            text: "Dismiss"
            Accessible.description: "Clears this list"
            onClicked: {
                const hadFocus = activeFocus
                root.panel.dismissAway()
                // It may hide under the focus.
                if (hadFocus)
                    root.panel.scroller.forceActiveFocus(Qt.OtherFocusReason)
            }
        }
    }
}
