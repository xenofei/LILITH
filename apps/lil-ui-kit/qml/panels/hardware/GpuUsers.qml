// "Who's using the GPU" (APP-127): the snapshot's `gpu_users`, the most dedicated GPU memory
// first, each with its MiB and, when the GPU reports it, its share of the GPU. The app's own
// process (role `app`) is "LILITH (this app)" and ComfyUI's (role `comfyui`) "ComfyUI"; any other
// name comes from the system and is untrusted, so it is shown through Core.shownText (P4a-R8).
import QtQuick
import Lilith.UiKit
import "HardwareFormat.js" as Format

LCard {
    id: root

    required property var core
    property var users: []

    // The longest name shown.
    readonly property int nameChars: 48

    title: "Using the GPU"

    function nameOf(user) {
        if (user.role === "app")
            return "LILITH (this app)"
        if (user.role === "comfyui")
            return "ComfyUI"
        return root.core ? root.core.shownText(String(user.name || ("pid " + user.pid)), root.nameChars) : ""
    }

    Column {
        width: parent.width
        spacing: 4

        Text {
            visible: root.users.length === 0
            text: "No process is using GPU memory."
            color: Theme.textMuted
            font.pixelSize: Theme.fontSize
        }

        Repeater {
            model: root.users

            delegate: Item {
                id: row
                objectName: "gpuUser"
                required property var modelData
                readonly property string name: root.nameOf(modelData)
                readonly property string share: modelData.sm_permille !== undefined
                                                ? Format.percent(modelData.sm_permille / 10) : ""
                width: parent.width
                height: nameText.implicitHeight

                Accessible.role: Accessible.StaticText
                Accessible.name: name + ": " + Format.mib(modelData.vram_local_mib)
                                 + (share ? ", " + share + " of the GPU" : "")

                Text {
                    id: nameText
                    width: parent.width - amount.implicitWidth - Theme.spacing
                    text: row.name
                    color: Theme.text
                    font.pixelSize: Theme.fontSize
                    elide: Text.ElideRight
                    textFormat: Text.PlainText
                }

                Text {
                    id: amount
                    anchors.right: parent.right
                    text: Format.mib(row.modelData.vram_local_mib) + (row.share ? "  ·  " + row.share : "")
                    color: Theme.textMuted
                    font.pixelSize: Theme.fontSize
                }
            }
        }
    }
}
