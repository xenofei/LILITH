// The Confirmations panel's History tab (APP-046): past answers, newest first, each with how it was
// given (`confirmed_via`). One tab stop; its rows are named for a screen reader. While the lists
// can't be read (a history too large for the core to send, say), the tab says so above its rows.
import QtQuick
import QtQuick.Controls.Basic as QQC
import Lilith.Core
import Lilith.UiKit
import "../../dialogs/ConfirmText.js" as Words

ListView {
    id: root

    required property var list
    // Why the lists couldn't be read, shown text; empty once a read is in.
    readonly property string errorText: list.error
        ? "Couldn't read the past confirmations: " + Words.errorWords(list.error, (t, n) => ConfirmInput.shownLine(t, n))
        : ""
    readonly property Item errorItem: headerItem

    activeFocusOnTab: true
    clip: true
    model: list.historyModel
    spacing: Theme.spacing / 2
    boundsBehavior: Flickable.StopAtBounds

    Accessible.role: Accessible.List
    Accessible.name: "Past confirmations"

    QQC.ScrollBar.vertical: QQC.ScrollBar {}

    header: Text {
        width: ListView.view.width
        height: visible ? implicitHeight + Theme.spacing : 0
        visible: text !== ""
        text: root.errorText
        textFormat: Text.PlainText
        color: Theme.textMuted
        font.pixelSize: Theme.fontSize
        wrapMode: Text.Wrap
        Accessible.role: Accessible.StaticText
        Accessible.name: text
    }

    delegate: Rectangle {
        id: row
        required property string cid
        readonly property var view: root.list.view(cid)
        readonly property string actionText: view ? ConfirmInput.shownLine(String(view.tool), 200) : ""
        readonly property string whoText: view ? Words.who(view.caller_kind, (t, n) => ConfirmInput.shownLine(t, n)) : ""
        readonly property string outcomeText: view ? Words.outcome(view, false) : ""

        width: ListView.view.width
        height: rowText.implicitHeight + Theme.spacing
        color: ListView.isCurrentItem && root.activeFocus ? Theme.surfaceRaised : "transparent"
        radius: Theme.radius

        Accessible.role: Accessible.ListItem
        Accessible.name: rowText.text

        Text {
            id: rowText
            x: Theme.spacing / 2
            y: Theme.spacing / 2
            width: parent.width - Theme.spacing
            text: row.actionText + ", from " + row.whoText + ". " + row.outcomeText
            textFormat: Text.PlainText
            color: Theme.text
            font.pixelSize: Theme.fontSize
            wrapMode: Text.WrapAnywhere
        }
    }
}
