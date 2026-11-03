// The core connection's banner (APP-034, APP-036, ARCH-100), from `Core.connection`:
//
// - `reconnecting`: "Reconnecting…", with AI and workspace actions off meanwhile.
// - `incompatible`: by direction, as the core's protocol major is above this app's or not. A newer core
//   can't be drained by this app's own `lil` (an older client never drains, ARCH-023), so the way is to
//   restart LILITH on its newer build. An older core is one this app failed to replace itself (a start
//   that failed): "restart the core when it's idle" (APP-036), and "Try again" (`Core.retryNow`) is
//   how, since its attempt may start a core and so drains and replaces the older one once it is idle.
//   Nothing reconnects by itself after `lil core stop`. The session stays `incompatible` while it
//   does, so the banner says it is trying, until the state changes or `retryShownMs` is out (the
//   client waits at most 60 s). A protocol it can't read says no direction and offers no button:
//   the client treats it as not older, and would only be incompatible again.
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

    // The protocol major this app speaks (lil_protocol::session::PROTOCOL).
    readonly property int appMajor: 1
    readonly property real coreMajor: core ? parseInt(String(core.coreProtocol)) : NaN
    // " (protocol 2.0)", or nothing when the core named none.
    readonly property string protocolWords: core && String(core.coreProtocol) !== ""
                                            ? " (protocol " + core.shownText(String(core.coreProtocol), 16) + ")" : ""
    readonly property bool coreIsNewer: coreMajor > appMajor
    readonly property bool coreIsOlder: coreMajor < appMajor
    readonly property string connection: core ? core.connection : ""
    readonly property var cause: core && connection === "unavailable" ? core.unavailable : null
    readonly property bool refusedWorkspace: CoreGate.refusedWorkspace(core)
                                             || (connection === "ready" && movedReason !== "")
    property alias retryButton: retryButton
    property alias defaultButton: defaultButton
    property string _useError
    // "Try again" was pressed on an older core and the session has not changed state since.
    property bool _retrying: false
    // How long that is shown if nothing changes: the client's own wait is 60 s.
    property int retryShownMs: 65000

    closable: false
    kind: connection === "reconnecting" ? "warning" : "danger"
    visible: text !== ""
    text: {
        const reason = cause && cause.reason ? core.shownText(String(cause.reason), 300) : ""
        switch (connection) {
        case "reconnecting":
            return "Reconnecting… AI and workspace actions are off until the core is back."
        case "incompatible":
            if (coreIsNewer)
                return "This app is older than the running core" + protocolWords + ". Restart LILITH to use it."
            if (!coreIsOlder)
                return "This app can't use the running core" + protocolWords + ". Restart LILITH, and the core when it's idle."
            if (_retrying)
                return "Trying again: replacing the core once nothing is running…"
            return "This app can't use the running core" + protocolWords
                   + ". Restart the core when it's idle: Try again replaces it once nothing is running."
        case "unavailable":
            if (_useError !== "")
                return _useError
            // The core's own lead, for the bar's label as well (runtime.rs `unavailable_json`).
            const lead = cause && cause.lead ? cause.lead : "The core isn't available"
            return reason !== "" ? lead + ": " + reason : lead + "."
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
    onConnectionChanged: {
        _useError = ""
        _retrying = false
    }
    // A failed try belongs to the refusal it was made for.
    onMovedReasonChanged: _useError = ""

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

    Timer {
        id: retryShown
        interval: root.retryShownMs
        onTriggered: root._retrying = false
    }

    actions: [
        LButton {
            id: retryButton
            primary: true
            visible: root.connection === "unavailable" && !root.refusedWorkspace
                     && !(root.cause && root.cause.retry === "never")
                     || root.connection === "incompatible" && root.coreIsOlder
            text: "Try again"
            onClicked: {
                // It keeps the focus while it runs (the banner's words say so), and ignores a click.
                if (root._retrying)
                    return
                root.core.retryNow()
                // The state doesn't change while an older core is replaced: say that it is working.
                if (root.connection === "incompatible") {
                    root._retrying = true
                    retryShown.restart()
                }
            }
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
