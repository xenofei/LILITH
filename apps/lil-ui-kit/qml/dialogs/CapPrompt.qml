// The cap prompt (APP-054, APP-045): what the call would pass, and raise the cap, allow once or
// cancel. It appears inline at the point of action, and inside the confirm dialog. Answering it
// anywhere clears it everywhere: it follows the `confirmations` topic for its own confirmation, and
// reads the list after a resync, in case the answer's event was missed.
//
// Allow once and Raise are ApproveButtons, a real pointer click only (P4a-R10). Cancel sends `deny`,
// from any source, and has the focus first. While it is open the shell's shortcuts stay quiet
// (OpenPrompts). `allow_once` passes the caps for this one call and never changes them; `raise` sends
// `raise_to {cap, amount}`, and the core sets that cap first.
//
// The amount is on the Raise button, through MoneyText, next to the limit it replaces. Any change to
// the field, from the keyboard or from a program, is treated alike, since nothing can tell them apart:
// the button is a new one to the gate (the pointer must leave and come back), the half second starts
// again, and the button shows the new amount. What is sent is what the button said when the click
// began: a change between the press and the release refuses the click.
//
// On a confirmation that also covers an agent's request, the buttons say so, by the tool's name
// (`alsoApproves`): they approve that too.
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
    // Everything the owner reads about what the buttons answer (the dialog's).
    property string shownKey
    // The tool these buttons also approve, as shown; empty when it covers only the cap.
    property string alsoApproves
    // Whether the buttons that approve may turn on, and why not, next to them (the dialog's).
    property bool approvable: true
    property string blockedWhy

    readonly property alias bar: bar
    readonly property alias cancelButton: cancelButton
    readonly property alias allowOnceButton: allowOnceButton
    readonly property alias raises: raises
    readonly property alias blockedLine: blockedLine

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
            return "The cap for " + ConfirmInput.shownLine(name.slice(9), 60)
        return ConfirmInput.shownLine(name, 60)
    }

    function hitWords(hit) {
        const money = amount => MoneyText.text(String(amount), String(hit.currency))
        return capName(hit.cap) + " is " + money(hit.limit) + ": " + money(hit.settled) + " spent, "
               + money(hit.held) + " held, and this call would add about " + money(hit.estimate) + "."
    }

    function focusCancel() {
        cancelButton.forceActiveFocus(Qt.TabFocusReason)
    }

    // Why `amount` can't be the new limit of `hit`, by name, or "" (lil_qt::confirm_text): what is
    // sent must be what the button says, and a raise is higher.
    function raiseRefusal(amount, hit) {
        return ConfirmInput.raiseRefusal(amount, String(hit.limit), String(hit.currency))
    }

    function raiseLabel(amount, hit) {
        if (raiseRefusal(amount, hit) !== "")
            return "Raise the cap"
        return "Raise the cap to " + MoneyText.text(amount, String(hit.currency))
               + " and approve" + (alsoApproves !== "" ? " " + alsoApproves : "")
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

    Text {
        width: parent.width
        visible: root.alsoApproves !== ""
        text: "These buttons also approve the request above, not only the money."
        textFormat: Text.PlainText
        color: Theme.warning
        font.pixelSize: Theme.fontSize
        wrapMode: Text.Wrap
        Accessible.role: Accessible.StaticText
        Accessible.name: text
    }

    AnswerBar {
        id: bar
        width: parent.width
        core: root.core
        confirmationId: root.confirmationId
        shownKey: root.shownKey
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
            text: root.alsoApproves !== "" ? "Allow once and approve " + root.alsoApproves : "Allow once"
            targetKey: root.confirmationId + "|allow|" + root.shownKey + "|" + text
            pointerSource: root.pointerSource
            enabled: bar.live && root.approvable
            onClicked: bar.approve(text, {action: "allow_once"})
            onRefused: name => bar.refuse(name, text)
        }
    }

    Repeater {
        id: raises
        model: root.cap || []
        delegate: Flow {
            id: raiseRow
            required property var modelData
            readonly property alias field: field
            readonly property alias button: raiseButton
            readonly property string amount: field.text.trim()
            width: root.width
            spacing: Theme.spacing

            LTextField {
                id: field
                width: 140
                label: "New limit for " + root.capName(raiseRow.modelData.cap)
                placeholderText: "New limit"
                enabled: bar.live
                // Whoever changed it: the gate re-arms.
                onTextChanged: bar.rearm()
            }

            ApproveButton {
                id: raiseButton
                text: root.raiseLabel(raiseRow.amount, raiseRow.modelData)
                primary: false
                targetKey: root.confirmationId + "|raise|" + root.shownKey + "|"
                           + String(raiseRow.modelData.cap) + "|" + raiseRow.amount + "|" + text
                pointerSource: root.pointerSource
                enabled: bar.live && root.approvable
                onClicked: {
                    const refusal = root.raiseRefusal(raiseRow.amount, raiseRow.modelData)
                    if (refusal === "")
                        bar.approve(text, {action: "raise",
                                           raise_to: {cap: raiseRow.modelData.cap, amount: raiseRow.amount}})
                    else
                        bar.refuse(refusal, text)
                }
                onRefused: name => bar.refuse(name, text)
            }

            Text {
                height: raiseButton.height
                verticalAlignment: Text.AlignVCenter
                text: "Limit when this was asked: " + MoneyText.text(String(raiseRow.modelData.limit), String(raiseRow.modelData.currency))
                textFormat: Text.PlainText
                color: Theme.textMuted
                font.pixelSize: Theme.fontSize
            }
        }
    }

    Text {
        id: blockedLine
        width: parent.width
        visible: !root.approvable && root.blockedWhy !== ""
        text: root.blockedWhy
        textFormat: Text.PlainText
        color: Theme.warning
        font.pixelSize: Theme.fontSize
        wrapMode: Text.Wrap
        Accessible.role: Accessible.StaticText
        Accessible.name: text
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
