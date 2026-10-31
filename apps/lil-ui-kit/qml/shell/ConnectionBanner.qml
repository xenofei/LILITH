// The core connection's banner (APP-034, APP-036, ARCH-100), from `Core.connection`:
//
// - `reconnecting`: "Reconnecting…", with AI and workspace actions off meanwhile.
// - `incompatible`: "restart the core when it's idle", and how. Text only: the session goes on trying
//   by itself, and an older app never drains a newer core (ARCH-023).
// - `unavailable`: the real cause, from `Core.unavailable`. "Try again" (`Core.retryNow`: after a
//   failed start the session only looks for a core someone else started until then) shows where it
//   can help. A hello refused over its workspace would only be refused again, so that offers "Use the
//   default workspace" instead, and a refusal marked `never` offers nothing.
// - `ready`, with `movedReason` (the top bar's): another client moved the default while the connection
//   stayed up, so the core refuses every call. The same words and the same way out.
//
// `connecting` and a plain `ready` show nothing. It can't be dismissed: it goes when the state does.
import QtQuick
import Lilith.UiKit
import "../core/CoreGate.js" as CoreGate

LBanner {
    id: root

    required property var core
    // Why the core refuses this session while connected, shown text; empty when it doesn't.
    property string movedReason

    readonly property string connection: core ? core.connection : ""
    readonly property var cause: core && connection === "unavailable" ? core.unavailable : null
    readonly property bool refusedWorkspace: CoreGate.refusedWorkspace(core)
                                             || (connection === "ready" && movedReason !== "")
    property alias retryButton: retryButton
    property alias defaultButton: defaultButton
    property string _useError

    closable: false
    kind: connection === "reconnecting" ? "warning" : "danger"
    visible: text !== ""
    text: {
        const reason = cause && cause.reason ? core.shownText(String(cause.reason), 300) : ""
        switch (connection) {
        case "reconnecting":
            return "Reconnecting… AI and workspace actions are off until the core is back."
        case "incompatible":
            return "This app can't use the running core (protocol " + core.shownText(String(core.coreProtocol), 16)
                   + "). Restart the core when it's idle: run lil core stop, and the app reconnects by itself."
        case "unavailable":
            if (_useError !== "")
                return _useError
            if (refusedWorkspace)
                return "The core refused this workspace: " + reason
            return reason !== "" ? "The core isn't available: " + reason : "The core isn't available."
        case "ready":
            if (movedReason === "")
                return ""
            return _useError !== "" ? _useError : "The core refused this workspace: " + movedReason
        default:
            return ""
        }
    }
    onTextChanged: {
        if (text !== "")
            announce()
    }
    onConnectionChanged: _useError = ""

    function useDefault() {
        _useError = ""
        CoreCall.useDefaultWorkspace(core, {
            owner: root,
            error: e => {
                root._useError = "Couldn't use the default workspace: "
                                 + root.core.shownText(e && e.message ? String(e.message) : "the core didn't answer.", 300)
            }
        })
    }

    actions: [
        LButton {
            id: retryButton
            primary: true
            visible: root.connection === "unavailable" && !root.refusedWorkspace
                     && !(root.cause && root.cause.retry === "never")
            text: "Try again"
            onClicked: root.core.retryNow()
        },
        LButton {
            id: defaultButton
            primary: true
            visible: root.refusedWorkspace
            text: "Use the default workspace"
            onClicked: root.useDefault()
        }
    ]
}
