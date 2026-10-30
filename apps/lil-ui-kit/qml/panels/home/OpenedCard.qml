// Home's "Opened" card (P4a-R21): what an open request showed last, a `lil:open/` link, a raw
// `lil://` ref, a `.lilref` file, a file path, an agent's `ui.show` or `open_in`, with its intent,
// or why a link was refused ("This link isn't one LILITH opens: …", APP-020). Plan 4b routes the
// target to the Inspector instead; until Task 26 builds Home, the shell holds the card.
//
// It shows a target as text only: nothing is fetched, loaded or run from it (AI-222). Every
// string from outside (the target, a `.lilref` title, a refusal) goes through the core's
// shownText (P4a-R8), so control, bidi and zero-width characters are removed or marked, and is
// shown as plain text. Its actions are Copy (the target's text to the clipboard) and Dismiss.
import QtQuick
import Lilith.Core
import Lilith.UiKit

LCard {
    id: root

    // The card's title: the `.lilref` file's title, else "Opened"; "Link refused" for a refusal.
    property string heading: "Opened"
    // The target as received (empty for a refusal), and how it is shown.
    property string targetText
    // The target as shown (shownText, at most 300 characters).
    readonly property string shownTarget: shown(targetText, 300)
    // "Open", "Edit" or "Use as input".
    property string intentText
    // Why a link was refused; empty otherwise.
    property string message
    readonly property alias copyButton: copyButton
    readonly property alias dismissButton: dismissButton

    // A launch or open request of kind target, ref or lilref: {target, title?, intent}.
    function show(launch) {
        message = ""
        targetText = launch.target || ""
        heading = launch.title ? shown(launch.title, 200) : "Opened"
        intentText = launch.intent === "edit" ? "Edit"
                   : launch.intent === "use_as_input" ? "Use as input" : "Open"
        visible = true
    }

    function showRefused(text) {
        targetText = ""
        intentText = ""
        heading = "Link refused"
        message = shown(text, 300)
        visible = true
    }

    // lil_parse::shown_text, from the bridge itself (whatever core the shell was given).
    function shown(text, max) {
        return Core.shownText(String(text), max)
    }

    visible: false
    title: heading
    width: 420

    Column {
        id: column
        width: parent.width
        spacing: Theme.spacing

        Text {
            width: parent.width
            visible: root.message === ""
            text: root.shownTarget
            color: Theme.text
            font.pixelSize: Theme.fontSize
            wrapMode: Text.WrapAnywhere
            maximumLineCount: 4
            elide: Text.ElideRight
            textFormat: Text.PlainText
            Accessible.role: Accessible.StaticText
            Accessible.name: text
        }

        Text {
            width: parent.width
            visible: root.message === "" && root.intentText !== ""
            text: "Intent: " + root.intentText
            color: Theme.textMuted
            font.pixelSize: Theme.fontSize
            textFormat: Text.PlainText
        }

        Text {
            width: parent.width
            visible: root.message !== ""
            text: root.message
            color: Theme.warning
            font.pixelSize: Theme.fontSize
            wrapMode: Text.WrapAnywhere
            textFormat: Text.PlainText
            Accessible.role: Accessible.StaticText
            Accessible.name: text
        }

        Row {
            spacing: Theme.spacing

            LButton {
                id: copyButton
                text: "Copy"
                visible: root.message === "" && root.targetText !== ""
                Accessible.description: "Copies the target to the clipboard"
                onClicked: {
                    clip.text = root.targetText
                    clip.selectAll()
                    clip.copy()
                    clip.text = ""
                }
            }

            LButton {
                id: dismissButton
                text: "Dismiss"
                onClicked: root.visible = false
            }
        }
    }

    // The clipboard's way in: QML has no clipboard of its own.
    TextEdit {
        id: clip
        visible: false
    }
}
