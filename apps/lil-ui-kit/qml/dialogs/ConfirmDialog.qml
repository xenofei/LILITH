// The shared dialog (APP-044, APP-115, P4a-R10): one confirmation, what it asks and its answer. The
// panel shows it in a modal popup and the confirm window in its list. Deny (or a cap's Cancel) has the
// focus first, and Approve is never a default button: Approve, Allow once and Raise take only a real
// pointer click, and the note next to them says so. Once it isn't pending it says what became of it,
// with no buttons. A confirmation that covers a cap answers through the cap prompt (APP-045).
import QtQuick
import Lilith.Core
import Lilith.UiKit
import "ConfirmText.js" as Words

Column {
    id: root

    required property var core
    required property string confirmationId
    // The ConfirmationView, or null when the list doesn't hold it.
    property var view: null
    // The list has loaded: a null view means it is unknown, and an answer may go out.
    property bool loaded: true
    property var pointerSource: ConfirmInput
    // A notification's Deny went out for it.
    property bool fromToast: false
    // How tall the card may be before it scrolls (a popup's limit).
    property real cardMaxHeight: 1e7

    // The core's row after an answer given here.
    signal answered(var confirmation)

    readonly property bool pending: !!view && view.state === "pending"
    readonly property bool isCap: Words.coversCap(view)
    readonly property alias card: card
    readonly property alias bar: bar
    readonly property alias denyButton: denyButton
    readonly property alias approveButton: approveButton
    readonly property alias capPrompt: capPromptLoader.item
    readonly property alias stateLine: stateLine

    spacing: Theme.spacing

    function focusDeny() {
        if (isCap && capPromptLoader.item)
            capPromptLoader.item.focusCancel()
        else
            denyButton.forceActiveFocus(Qt.TabFocusReason)
    }

    Flickable {
        width: parent.width
        height: Math.min(card.implicitHeight, root.cardMaxHeight)
        contentWidth: width
        contentHeight: card.implicitHeight
        clip: true
        boundsBehavior: Flickable.StopAtBounds
        interactive: contentHeight > height

        ConfirmationCard {
            id: card
            width: parent.width
            height: implicitHeight
            core: root.core
            confirmationId: root.confirmationId
            view: root.view
        }
    }

    Text {
        id: stateLine
        width: parent.width
        visible: text !== ""
        text: !root.view ? (root.loaded ? "LILITH doesn't know this confirmation: it may have ended long ago, or belong to a workspace that isn't open." : "")
            : root.pending ? (card.ranOut ? "This confirmation has run out of time." : "")
            : Words.outcome(root.view, root.fromToast)
        textFormat: Text.PlainText
        color: Theme.text
        font.pixelSize: Theme.fontSize
        wrapMode: Text.Wrap
        Accessible.role: Accessible.StaticText
        Accessible.name: text
    }

    AnswerBar {
        id: bar
        width: parent.width
        visible: root.pending && !root.isCap
        core: root.core
        confirmationId: root.confirmationId
        open: root.pending && root.loaded && !card.ranOut
        onAnswered: confirmation => root.answered(confirmation)

        DenyButton {
            id: denyButton
            text: "Deny"
            enabled: bar.live
            onActivated: kind => bar.deny(kind, denyButton)
        }

        ApproveButton {
            id: approveButton
            text: "Approve"
            targetKey: root.confirmationId
            pointerSource: root.pointerSource
            enabled: bar.live
            onClicked: bar.approve(text)
            onRefused: name => bar.refuse(name, text)
        }
    }

    Text {
        width: parent.width
        visible: bar.visible
        text: "Approve needs a click from your mouse or touchpad. Deny works from the keyboard."
        textFormat: Text.PlainText
        color: Theme.textMuted
        font.pixelSize: Theme.fontSize
        wrapMode: Text.Wrap
    }

    Loader {
        id: capPromptLoader
        width: parent.width
        active: root.pending && root.isCap
        sourceComponent: CapPrompt {
            width: capPromptLoader.width
            core: root.core
            confirmationId: root.confirmationId
            cap: root.view ? root.view.cap : []
            pointerSource: root.pointerSource
            open: !card.ranOut && root.loaded
            followTopic: false
            onAnswered: confirmation => root.answered(confirmation)
        }
    }
}
