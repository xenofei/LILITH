// The buttons of one confirmation, whoever shows them (P4a-R10, APP-044): it holds the answer gate's
// clock, sends the one answer and says why an answer didn't count. Its children are the buttons:
// each ApproveButton's `clicked` calls approve(label, cap), its `refused` calls refuse(name, label);
// a DenyButton's `activated` calls deny(kind, button).
//
// The clock runs from when the bar came up with its window in front, and starts again whenever
// anything could have put a button under a hand that meant something else: the window leaves the
// front, moves or is resized (as the Hello window's does), what is shown changes (`shownKey`), rearm(),
// or the buttons come back on (`live`). `buttonsOn` is what the buttons' `enabled` binds to: the
// confirmation is open and no answer is out or sent. Each button is also marked `requiresCore`
// (core/CoreGate.js), and `live` is both together: the core is ready as well.
//
// A refusal is never silent: `reason` shows under the buttons, is announced, and its name goes to
// the log, with nothing of the input, at most one line a second, a hand's refusals included.
import QtQuick
import Lilith.Core
import Lilith.UiKit
import "ConfirmText.js" as Words
import "../core/CoreGate.js" as CoreGate

Column {
    id: root

    required property var core
    required property string confirmationId
    // Everything the owner reads about what these buttons answer.
    property string shownKey
    // Still waiting for an answer, as far as the dialog knows.
    property bool open: true
    property bool inFront: Window.active

    property real visibleSinceMs: -1
    property bool busy: false
    // The answer went through: nothing more goes out.
    property bool sent: false
    property string reason
    property real _reasonAtMs: -1000
    readonly property bool connected: CoreGate.ready(root)
    readonly property bool buttonsOn: open && !busy && !sent
    readonly property bool live: buttonsOn && connected
    // This shows in the shell's modal dialog: its Close leads to the shell's banner, which has the
    // way out of what doesn't clear by itself (a refused workspace, a failed start).
    property bool bannerBehind: false
    readonly property string awayWords: {
        const connection = core ? core.connection : ""
        if (connection === "reconnecting" || connection === "connecting")
            return "Not connected to the core yet: the buttons come back when it is."
        return bannerBehind
            ? "Not connected to the core, and this may not clear by itself. Close this and use the banner in the main LILITH window. Closing answers nothing: this stays waiting."
            : "Not connected to the core, so this can't be answered for now. It is still waiting."
    }
    readonly property alias reasonLine: reasonLine
    default property alias buttons: row.data

    // The core's row after the answer.
    signal answered(var confirmation)

    spacing: Theme.spacing

    onInFrontChanged: {
        restartClock()
        // Old words go. Ones just set stay: the activation can reach QML right after the click that
        // caused it was refused, and those words say what to do next.
        if (inFront && ConfirmInput.nowMs() - _reasonAtMs >= 1000)
            reason = ""
    }
    onReasonChanged: _reasonAtMs = ConfirmInput.nowMs()
    onVisibleChanged: restartClock()
    // Words from before it could answer again are old news. An answer that fails with the core still
    // there sets its words after this.
    onLiveChanged: if (live) rearm()
    onShownKeyChanged: rearm()
    Component.onCompleted: restartClock()
    // Another confirmation in the same dialog: a new answer, and the clock starts again.
    onConfirmationIdChanged: {
        busy = false
        sent = false
        rearm()
    }

    Connections {
        target: root.Window.window
        function onXChanged() { root.restartClock() }
        function onYChanged() { root.restartClock() }
        function onWidthChanged() { root.restartClock() }
        function onHeightChanged() { root.restartClock() }
    }

    function restartClock() {
        visibleSinceMs = inFront && visible ? ConfirmInput.nowMs() : -1
    }

    // What these buttons answer has changed: a click needs the half second again, and any words
    // about the old one go.
    function rearm() {
        restartClock()
        reason = ""
    }

    function refuse(name, label) {
        reason = ConfirmInput.refusalText(name, label)
        ConfirmInput.logRefusal(name)
        Accessible.announce(reason)
    }

    function _passes(kind, focused, label) {
        const name = ConfirmInput.gateRefusal(kind, focused, visibleSinceMs, inFront, ConfirmInput.nowMs())
        if (name === "")
            return true
        refuse(name, label)
        return false
    }

    // A click the native gate has passed, on the button labelled `label`.
    function approve(label, cap) {
        if (live && _passes("pointer", true, label))
            _send("approve", cap)
    }

    function deny(kind, button) {
        if (live && _passes(kind, kind === "pointer" || button.activeFocus, button.text))
            _send("deny")
    }

    function _send(answer, cap) {
        busy = true
        reason = ""
        const id = confirmationId
        const params = {confirmation_id: id, answer: answer}
        if (cap)
            params.cap = cap
        CoreCall.appCall(core, "app/confirmation.answer", params, {
            owner: root,
            ok: result => {
                // The dialog may have moved on to another confirmation meanwhile.
                if (id === confirmationId) {
                    busy = false
                    sent = true
                }
                root.answered(result.confirmation)
            },
            error: error => {
                if (id !== confirmationId)
                    return
                busy = false
                reason = Words.errorWords(error, (t, n) => ConfirmInput.shownLine(t, n))
                Accessible.announce(reason)
            }
        })
    }

    Flow {
        id: row
        width: parent.width
        spacing: Theme.spacing
    }

    Text {
        id: reasonLine
        width: parent.width
        // Its room is kept: a popup is centred, so a reason arriving would move the buttons.
        height: Math.max(implicitHeight, 2 * (font.pixelSize + 4))
        // With the core away, what is true now, whatever failed on the way there.
        text: root.open && !root.connected ? root.awayWords : root.reason
        textFormat: Text.PlainText
        color: Theme.warning
        font.pixelSize: Theme.fontSize
        wrapMode: Text.Wrap
        Accessible.role: Accessible.StaticText
        Accessible.name: text
    }
}
