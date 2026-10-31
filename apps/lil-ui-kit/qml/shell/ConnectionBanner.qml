// The core connection's banner (APP-034, APP-036, ARCH-100), from `Core.connection`: "Reconnecting…"
// (AI and workspace actions are off meanwhile), an incompatible core ("restart the core when it's
// idle"; the session goes on trying by itself, and an older app never drains a newer core,
// ARCH-023), and an unavailable one, with "Try again" (`Core.retryNow`: after a failed start the
// session only looks for a core someone else started until then). `connecting` and `ready` show
// nothing. It can't be dismissed: it goes when the state does.
import QtQuick
import Lilith.UiKit

LBanner {
    id: root

    required property var core

    readonly property string connection: core ? core.connection : ""
    property alias retryButton: retryButton

    closable: false
    kind: connection === "reconnecting" ? "warning" : "danger"
    visible: text !== ""
    text: {
        switch (connection) {
        case "reconnecting":
            return "Reconnecting… AI and workspace actions are off until the core is back."
        case "incompatible":
            return "This app can't use the running core (protocol " + core.shownText(String(core.coreProtocol), 16)
                   + "). Restart the core when it's idle."
        case "unavailable":
            return "The core isn't available. AI and workspace actions are off until it is."
        default:
            return ""
        }
    }
    onTextChanged: {
        if (text !== "")
            announce()
    }

    actions: [
        LButton {
            id: retryButton
            primary: true
            visible: root.connection === "unavailable"
            text: "Try again"
            onClicked: root.core.retryNow()
        }
    ]
}
