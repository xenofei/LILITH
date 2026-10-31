// The confirm window (APP-048, APP-049, P4a-R11): the minimal window a notification's Open, a `confirm`
// link or `lil confirm-window` opens when the app isn't running. It shows one ConfirmDialog per
// confirmation it was asked about, or with no id every pending one, and a later link adds to it.
// `handleLaunchJson` takes the launches as the shell does (lil_qt::args::launch_json). A notification's
// Deny comes to this process's pipe too: the bridge sends it, and a launch of kind `deny` only tells
// the window, which doesn't take the focus, so the card can say where the deny came from.
//
// While the core isn't connected the buttons are off and a banner says so; the dialogs stay, and a
// resync reads their live state (ConfirmationList).
import QtQuick
import Lilith.Core
import Lilith.Shim
import Lilith.UiKit

Window {
    id: root

    required property var core
    // The shim the window is activated through: LilShim, or a test's fake.
    property var shim: LilShim
    property var pointerSource: ConfirmInput
    // Every pending confirmation shows, not only those asked about.
    property bool showAll: false
    // id -> true: a notification's Deny went out for it.
    property var toastDenied: ({})
    readonly property alias list: list
    readonly property alias banner: banner
    readonly property alias instanceBanner: instanceBanner
    readonly property alias shown: shown
    readonly property alias dialogs: dialogs
    readonly property alias emptyText: emptyText

    width: 620
    height: 640
    visible: false
    title: "LILITH — Confirm"
    color: Theme.bg

    function handleLaunchJson(json) {
        handleLaunch(JSON.parse(json))
    }

    function handleLaunch(launch) {
        // This start runs without its pipe: the bridge's words for why.
        if (typeof launch.pipe_notice === "string" && launch.pipe_notice !== "")
            instanceBanner.show(launch.pipe_notice)
        if (launch.activate !== false)
            shim.activate(root, launch.focus !== false)
        if (launch.kind === "confirm") {
            add(launch.id)
        } else if (launch.kind === "confirm_list") {
            showAll = true
            _addPending()
        } else if (launch.kind === "deny" && typeof launch.id === "string") {
            const denied = Object.assign({}, toastDenied)
            denied[launch.id] = true
            toastDenied = denied
        }
    }

    function add(id) {
        for (let i = 0; i < shown.count; ++i) {
            if (shown.get(i).cid === id)
                return
        }
        shown.append({cid: id})
    }

    function _addPending() {
        for (let i = list.pendingModel.count - 1; i >= 0; --i)
            add(list.pendingModel.get(i).cid)
    }

    // The dialog on `id`, once made.
    function dialogOf(id) {
        for (let i = 0; i < dialogs.count; ++i) {
            const d = dialogs.itemAt(i)
            if (d && d.confirmationId === id)
                return d
        }
        return null
    }

    ListModel { id: shown }

    ConfirmationList {
        id: list
        core: root.core
        onLoadedChanged: if (loaded && root.showAll) root._addPending()
    }

    Connections {
        target: list.pendingModel
        function onCountChanged() {
            if (root.showAll)
                root._addPending()
        }
    }

    LBanner {
        id: banner
        width: parent.width
        kind: "warning"
        closable: false
        visible: !!root.core && root.core.connection !== "ready"
        text: !root.core ? ""
            : root.core.connection === "reconnecting" ? "Reconnecting… The buttons come back when the core is connected."
            : root.core.connection === "connecting" ? "Connecting to the core…"
            : "LILITH can't reach the core: nothing here can be answered until it can."
    }

    LBanner {
        id: instanceBanner
        anchors.top: banner.visible ? banner.bottom : parent.top
        width: parent.width
        kind: "warning"
    }

    readonly property real _bannersHeight: (banner.visible ? banner.height : 0)
                                           + (instanceBanner.visible ? instanceBanner.height : 0)

    Text {
        id: emptyText
        x: 2 * Theme.spacing
        y: root._bannersHeight + 2 * Theme.spacing
        width: parent.width - 4 * Theme.spacing
        visible: shown.count === 0 && list.loaded
        text: "Nothing is waiting for your answer."
        textFormat: Text.PlainText
        color: Theme.textMuted
        font.pixelSize: Theme.fontSize
        wrapMode: Text.Wrap
        Accessible.role: Accessible.StaticText
        Accessible.name: text
    }

    CardColumn {
        id: column
        x: 2 * Theme.spacing
        y: root._bannersHeight + 2 * Theme.spacing
        width: parent.width - 4 * Theme.spacing
        height: parent.height - y - 2 * Theme.spacing

        Repeater {
            id: dialogs
            model: shown
            delegate: ConfirmDialog {
                required property string cid
                width: column.width
                core: root.core
                confirmationId: cid
                view: list.view(cid)
                loaded: list.loaded
                pointerSource: root.pointerSource
                fromToast: root.toastDenied[cid] === true
                onAnswered: confirmation => list.apply(confirmation)
            }
        }
    }
}
