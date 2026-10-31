// The Confirmations panel (APP-115, APP-044, APP-046, APP-054): pending agent requests and cap
// confirmations, each a card with who asked, the exact action and the files, and a History tab.
// Answering happens only in the shared dialog (ConfirmDialog), a modal popup over the panel's window:
// a card's "Review and answer", or a `confirm` link through showConfirmation(id). Answering anywhere
// clears a card everywhere (the `confirmations` topic, ConfirmationList). While the dialog is open the
// shell's shortcuts stay quiet (OpenPrompts).
import QtQuick
import QtQuick.Templates as T
import Lilith.Core
import Lilith.UiKit
import "../../dialogs/ConfirmText.js" as Words

Rectangle {
    id: root

    required property var core
    required property string title
    // The native gate for the buttons that need a real pointer: ConfirmInput, or a test's fake.
    property var pointerSource: ConfirmInput

    // "pending" or "history".
    property string tab: "pending"
    property string _wanted
    property bool _popupCounted: false
    readonly property alias list: list
    readonly property alias banner: banner
    readonly property alias pendingTab: pendingTab
    readonly property alias historyTab: historyTab
    readonly property alias cards: cards
    readonly property alias cardRepeater: cardRepeater
    readonly property alias history: historyList
    readonly property alias emptyText: emptyText
    readonly property alias dialogPopup: popup
    readonly property alias dialog: dialog
    readonly property alias closeDialogButton: closeDialogButton

    color: Theme.surface

    Accessible.role: Accessible.Pane
    Accessible.name: title

    // A `confirm` link, or a card's button: the dialog on it, or in words why not.
    function showConfirmation(id) {
        banner.visible = false
        if (!list.loaded) {
            _wanted = id
            return
        }
        const view = list.view(id)
        if (view && view.state === "pending") {
            tab = "pending"
            dialog.confirmationId = id
            popup.open()
        } else if (view) {
            banner.show(Words.outcome(view, false))
        } else {
            banner.show("LILITH doesn't know that confirmation: it may have ended long ago, or belong to a workspace that isn't open.")
        }
    }

    ConfirmationList {
        id: list
        core: root.core
        onLoadedChanged: {
            if (loaded && root._wanted !== "") {
                const id = root._wanted
                root._wanted = ""
                root.showConfirmation(id)
            }
        }
    }

    LBanner {
        id: banner
        width: parent.width
        kind: "info"
    }

    Row {
        id: tabs
        y: banner.visible ? banner.height + Theme.spacing : Theme.spacing
        x: Theme.spacing
        spacing: Theme.spacing

        LButton {
            id: pendingTab
            text: "Pending (" + MoneyText.count(String(list.pendingModel.count)) + ")"
            primary: root.tab === "pending"
            Accessible.role: Accessible.PageTab
            onClicked: root.tab = "pending"
        }

        LButton {
            id: historyTab
            text: "History"
            primary: root.tab === "history"
            Accessible.role: Accessible.PageTab
            onClicked: root.tab = "history"
        }
    }

    Text {
        id: emptyText
        x: Theme.spacing
        y: tabs.y + tabs.height + Theme.spacing
        width: parent.width - 2 * Theme.spacing
        visible: root.tab === "pending" && list.pendingModel.count === 0
        text: list.error && !list.loaded ? Words.errorWords(list.error, (t, n) => Core.shownText(t, n))
            : list.loaded ? "Nothing is waiting for your answer." : "Reading the confirmations…"
        textFormat: Text.PlainText
        color: Theme.textMuted
        font.pixelSize: Theme.fontSize
        wrapMode: Text.Wrap
        Accessible.role: Accessible.StaticText
        Accessible.name: text
    }

    CardColumn {
        id: cards
        x: Theme.spacing
        y: tabs.y + tabs.height + Theme.spacing
        width: parent.width - 2 * Theme.spacing
        height: parent.height - y - Theme.spacing
        visible: root.tab === "pending"

        Repeater {
            id: cardRepeater
            model: list.pendingModel
            delegate: ConfirmationCard {
                id: card
                required property string cid
                readonly property alias reviewButton: reviewButton
                width: cards.width
                core: root.core
                confirmationId: cid
                view: list.view(cid)

                LButton {
                    id: reviewButton
                    text: "Review and answer"
                    Accessible.description: card.title
                    onClicked: root.showConfirmation(card.cid)
                }
            }
        }
    }

    HistoryList {
        id: historyList
        x: Theme.spacing
        y: tabs.y + tabs.height + Theme.spacing
        width: parent.width - 2 * Theme.spacing
        height: parent.height - y - Theme.spacing
        visible: root.tab === "history"
        list: list
    }

    T.Popup {
        id: popup
        parent: T.Overlay.overlay
        modal: true
        focus: true
        closePolicy: T.Popup.CloseOnEscape
        x: parent ? Math.round((parent.width - width) / 2) : 0
        y: parent ? Math.round((parent.height - height) / 2) : 0
        width: parent ? Math.min(parent.width - 40, 640) : 640
        padding: 2 * Theme.spacing
        contentHeight: popupColumn.implicitHeight
        height: contentHeight + topPadding + bottomPadding

        T.Overlay.modal: Rectangle {
            color: Qt.rgba(0, 0, 0, 0.5)
        }

        background: Rectangle {
            color: Theme.surface
            border.color: Theme.surfaceRaised
            radius: Theme.radius
        }

        onOpened: {
            OpenPrompts.count += 1
            root._popupCounted = true
            dialog.focusDeny()
        }
        onClosed: {
            OpenPrompts.count -= 1
            root._popupCounted = false
            dialog.confirmationId = ""
        }

        Column {
            id: popupColumn
            width: popup.availableWidth
            spacing: Theme.spacing

            ConfirmDialog {
                id: dialog
                width: parent.width
                core: root.core
                confirmationId: ""
                view: confirmationId !== "" ? list.view(confirmationId) : null
                loaded: list.loaded
                pointerSource: root.pointerSource
                cardMaxHeight: popup.parent ? Math.max(120, popup.parent.height - 260) : 400
                onAnswered: confirmation => list.apply(confirmation)
            }

            LButton {
                id: closeDialogButton
                visible: !dialog.pending
                text: "Close"
                onClicked: popup.close()
            }
        }
    }

    Component.onDestruction: if (_popupCounted) OpenPrompts.count -= 1
}
