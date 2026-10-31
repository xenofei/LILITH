// The cap prompt (APP-054, APP-045): what the call would pass, and raise the cap, allow once or
// cancel. It appears inline at the point of action, and inside the confirm dialog. Answering it
// anywhere clears it everywhere: it follows the `confirmations` topic for its own confirmation, and
// reads the list after a resync, in case the answer's event was missed.
//
// Allow once and Raise are ApproveButtons, a real pointer click only (P4a-R10). Cancel sends `deny`,
// from any source, and has the focus first. While it is open the shell's shortcuts stay quiet
// (OpenPrompts). `allow_once` passes the caps for this one call and never changes them; `raise` sends
// `raise_to {cap, amount}`, and the core sets that cap first.
import QtQuick
import Lilith.Core
import Lilith.UiKit

Column {
    id: root

    required property var core
    required property string confirmationId
    // ConfirmationView.cap: [{cap, limit, settled, held, estimate, currency, rate}], as the core sends it.
    property var cap: []
    property var pointerSource: ConfirmInput
    // Still waiting for its answer.
    property bool open: true
    // False inside a dialog that follows the confirmation itself.
    property bool followTopic: true

    readonly property alias bar: bar
    readonly property alias cancelButton: cancelButton
    readonly property alias allowOnceButton: allowOnceButton
    readonly property alias raises: raises

    signal answered(var confirmation)

    // Its answer came, from here or anywhere.
    property bool ended: false
    onConfirmationIdChanged: ended = false

    visible: open && !ended
    spacing: Theme.spacing

    function capName(id) {
        const name = String(id)
        if (name === "monthly")
            return "The monthly cap"
        if (name === "per_call_max")
            return "The per-call cap"
        if (name.startsWith("provider:"))
            return "The cap for " + Core.shownText(name.slice(9), 60)
        return Core.shownText(name, 60)
    }

    function hitWords(hit) {
        const money = amount => MoneyText.text(String(amount), String(hit.currency))
        return capName(hit.cap) + " is " + money(hit.limit) + ": " + money(hit.settled) + " spent, "
               + money(hit.held) + " held, and this call would add about " + money(hit.estimate) + "."
    }

    function focusCancel() {
        cancelButton.forceActiveFocus(Qt.TabFocusReason)
    }

    function _raise(capId, amount, label) {
        if (!/^[0-9]+(\.[0-9]+)?$/.test(amount)) {
            bar.reason = "Enter the new limit as a number, like 25 or 12.50."
            return
        }
        bar.approve(label, {action: "raise", raise_to: {cap: capId, amount: amount}})
    }

    readonly property bool _holds: visible
    property bool _counted: false
    function _syncHold() {
        if (_holds !== _counted) {
            _counted = _holds
            OpenPrompts.count += _holds ? 1 : -1
        }
    }
    on_HoldsChanged: _syncHold()
    Component.onDestruction: if (_counted) OpenPrompts.count -= 1

    Component.onCompleted: {
        _syncHold()
        if (!followTopic)
            return
        CoreCall.subscribe(core, "confirmations", root, (kind, seq, data) => {
            if (kind === "event" && data && data.confirmation && data.confirmation.id === confirmationId
                    && data.confirmation.state !== "pending") {
                root.ended = true
            } else if (kind === "resync_required" || kind === "snapshot") {
                CoreCall.appCall(core, "app/confirmations.list", {state: "pending", limit: 500}, {
                    owner: root,
                    ok: result => {
                        if (!result.confirmations.some(v => v.id === confirmationId))
                            root.ended = true
                    }
                })
            }
        })
    }

    Repeater {
        model: root.cap || []
        delegate: Text {
            required property var modelData
            width: root.width
            text: root.hitWords(modelData)
            textFormat: Text.PlainText
            color: Theme.text
            font.pixelSize: Theme.fontSize
            wrapMode: Text.Wrap
            Accessible.role: Accessible.StaticText
            Accessible.name: text
        }
    }

    AnswerBar {
        id: bar
        width: parent.width
        core: root.core
        confirmationId: root.confirmationId
        open: root.open && !root.ended
        onAnswered: confirmation => {
            root.ended = true
            root.answered(confirmation)
        }

        DenyButton {
            id: cancelButton
            text: "Cancel"
            enabled: bar.live
            Accessible.description: "Denies this call"
            onActivated: kind => bar.deny(kind, cancelButton)
        }

        ApproveButton {
            id: allowOnceButton
            text: "Allow once"
            targetKey: root.confirmationId
            pointerSource: root.pointerSource
            enabled: bar.live
            onClicked: bar.approve(text, {action: "allow_once"})
            onRefused: name => bar.refuse(name, text)
        }
    }

    Repeater {
        id: raises
        model: root.cap || []
        delegate: Row {
            id: raiseRow
            required property var modelData
            readonly property alias field: field
            readonly property alias button: raiseButton
            spacing: Theme.spacing

            LTextField {
                id: field
                width: 140
                label: "New limit for " + root.capName(raiseRow.modelData.cap)
                placeholderText: "New limit"
                enabled: bar.live
            }

            ApproveButton {
                id: raiseButton
                text: "Raise the cap"
                primary: false
                targetKey: root.confirmationId + "/" + String(raiseRow.modelData.cap)
                pointerSource: root.pointerSource
                enabled: bar.live
                onClicked: root._raise(raiseRow.modelData.cap, field.text.trim(), text)
                onRefused: name => bar.refuse(name, text)
            }
        }
    }

    Text {
        width: parent.width
        text: "Allow once and Raise the cap need a click from your mouse or touchpad. Cancel works from the keyboard."
        textFormat: Text.PlainText
        color: Theme.textMuted
        font.pixelSize: Theme.fontSize
        wrapMode: Text.Wrap
    }
}
