// The Confirmations panel (APP-115, APP-044, APP-046, APP-054): pending agent requests and cap
// confirmations, each a card with who asked and the exact action, and a History tab. Answering happens
// only in the shared dialog (ConfirmDialog), a modal popup over the panel's window: a card's "Review and
// answer", or a `confirm` link through showConfirmation(id). Answering anywhere clears a card everywhere
// (the `confirmations` topic, ConfirmationList). While the dialog is open the shell's shortcuts stay
// quiet (OpenPrompts).
//
// What is on screen is what a click approves, so an open dialog's confirmation is never swapped. A link
// or a card that asks for another while the dialog is open waits, and the dialog says so; the next one
// opens only when this one is closed, as a new dialog, with the gate armed from the start.
//
// While the core isn't ready nothing opens by itself: what waits, and any link, shows when the core is
// back. The dialog's words send the person to the banner, so it mustn't come straight back over it. A
// person's own click on a card still opens that one.
import QtQuick
import QtQuick.Templates as T
import Lilith.Core
import Lilith.UiKit
import "../../dialogs/ConfirmText.js" as Words
import "../../core/CoreGate.js" as CoreGate

Rectangle {
    id: root

    required property var core
    required property string title
    // The native gate for the buttons that need a real pointer: ConfirmInput, or a test's fake.
    property var pointerSource: ConfirmInput

    // "pending" or "history".
    property string tab: "pending"
    // Asked for while the lists were loading, or while the dialog was open, oldest first.
    property var _waiting: []
    property bool _popupCounted: false
    readonly property int waitingCount: _waiting.length
    readonly property int waitingMax: 20
    readonly property bool coreReady: CoreGate.ready(root)
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
    readonly property alias waitingNotice: waitingNotice

    color: Theme.surface

    Accessible.role: Accessible.Pane
    Accessible.name: title

    onCoreReadyChanged: if (coreReady) Qt.callLater(_showNext)

    // A `confirm` link. The shell also passes its focus flag, which means nothing here.
    function showConfirmation(id) {
        _show(id, false)
    }

    // The dialog on `id`, or in words why not, or a wait. `byHand`: a person's click on a card, which
    // opens whether or not the core is ready.
    function _show(id, byHand) {
        if (dialog.confirmationId !== "") {
            if (id !== dialog.confirmationId && _wait(id))
                waitingNotice.Accessible.announce(waitingNotice.text)
            return
        }
        if (!list.loaded || (!coreReady && !byHand)) {
            _wait(id)
            return
        }
        // Shown now, it doesn't come round again.
        _waiting = _waiting.filter(waiting => waiting !== id)
        banner.visible = false
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

    // Queues `id`, at most `waitingMax` of them (each pending one has its card anyway, so a program
    // can't fill this up); whether it joined.
    function _wait(id) {
        if (_waiting.indexOf(id) >= 0 || _waiting.length >= waitingMax)
            return false
        _waiting = _waiting.concat([id])
        return true
    }

    function _holdShortcuts(on) {
        if (_popupCounted !== on) {
            _popupCounted = on
            OpenPrompts.count += on ? 1 : -1
        }
    }

    function _showNext() {
        if (dialog.confirmationId !== "" || !list.loaded || !coreReady || _waiting.length === 0)
            return
        const id = _waiting[0]
        _waiting = _waiting.slice(1)
        showConfirmation(id)
        Qt.callLater(_showNext)
    }

    ConfirmationList {
        id: list
        core: root.core
        onLoadedChanged: if (loaded) root._showNext()
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
        text: list.error && !list.loaded ? Words.errorWords(list.error, (t, n) => ConfirmInput.shownLine(t, n))
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
                previewChars: 300

                LButton {
                    id: reviewButton
                    text: "Review and answer"
                    Accessible.description: card.title
                    onClicked: root._show(card.cid, true)
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
            root._holdShortcuts(true)
            dialog.focusDeny()
        }
        onClosed: {
            root._holdShortcuts(false)
            dialog.confirmationId = ""
            Qt.callLater(root._showNext)
        }

        Column {
            id: popupColumn
            width: popup.availableWidth
            spacing: Theme.spacing

            Text {
                id: waitingNotice
                width: parent.width
                visible: root.waitingCount > 0
                text: root.waitingCount === 1
                      ? "Another confirmation is waiting. It shows after you close this one" + _afterClose
                      : MoneyText.count(String(root.waitingCount)) + " more confirmations are waiting. The next shows after you close this one" + _afterClose
                readonly property string _afterClose: root.coreReady ? "." : ", once the core is back."
                textFormat: Text.PlainText
                color: Theme.warning
                font.pixelSize: Theme.fontSize
                font.bold: true
                wrapMode: Text.Wrap
                Accessible.role: Accessible.StaticText
                Accessible.name: text
            }

            ConfirmDialog {
                id: dialog
                width: parent.width
                core: root.core
                confirmationId: ""
                view: confirmationId !== "" ? list.view(confirmationId) : null
                loaded: list.loaded
                pointerSource: root.pointerSource
                cardMaxHeight: popup.parent ? Math.max(120, popup.parent.height - 320) : 400
                bannerBehind: true
                onAnswered: confirmation => list.apply(confirmation)
                onLiveAgain: if (popup.opened) Qt.callLater(dialog.focusDenyIfLost)
            }

            // Also while the core is away: the way out of that is on the banner behind the dim.
            // Closing answers nothing, and the confirmation stays waiting.
            LButton {
                id: closeDialogButton
                visible: !dialog.pending || !dialog.bar.connected
                text: "Close"
                onClicked: popup.close()
            }
        }
    }

    Component.onDestruction: _holdShortcuts(false)
}
