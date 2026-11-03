// One confirmation's facts (APP-115, APP-045): who asked, the exact action, the kind of request and
// the time left. The tool and every argument are what the core sent, not a summary, as plain text
// (lil_qt::confirm_text, P4a-R8): every invisible character is a marker, a value's extra lines carry a
// continuation mark and its key stands apart, so a value can't pass for another argument, and a value
// that isn't all shown says so. No list of files: until a tool declares its paths, the exact arguments
// are what is true. Children go under the facts (the panel's button).
import QtQuick
import Lilith.Core
import Lilith.UiKit
import "../../dialogs/ConfirmText.js" as Words

LCard {
    id: root

    required property var core
    required property string confirmationId
    property var view: null
    // Above 0: at most this many characters of each value (the panel's list, whose button opens the
    // whole action). The dialog shows it all.
    property int previewChars: 0
    // The dialog shows the title and the facts line outside its scrolling view.
    property bool factsOutside: false
    default property alias footer: footerSlot.data

    // The arguments as shown: [{key, text, cut}].
    readonly property var argumentRows: view && view.shown ? view.shown.rows : []
    readonly property string whoText: view ? Words.who(view.caller_kind, _shown) : ""
    readonly property string actionText: view ? _shown(view.tool, 200) : ""
    readonly property string kindText: !view ? ""
        : Words.coversCap(view) && Words.coversConfirm(view) ? "Agent request and spending cap"
        : Words.coversCap(view) ? "Spending cap" : "Agent request"
    property real nowMs: Date.now()
    readonly property string leftText: view && view.state === "pending" ? Words.timeLeft(view.expires_at, nowMs) : ""
    readonly property bool ranOut: !!view && view.state === "pending" && leftText === ""
    readonly property string countdownText: leftText !== "" ? "Expires in " + leftText : ""
    readonly property alias factsLine: factsLine
    readonly property alias rowRepeater: rowRepeater

    function _shown(text, max) {
        return ConfirmInput.shownLine(String(text), max)
    }

    // Each name from outside is isolated: its own letters can't reorder the words beside it.
    readonly property string actionTitle: !view ? _shown(confirmationId, 60)
                                                : Words.ltr(Words.isolate(whoText) + " asks to run " + Words.isolate(actionText))
    readonly property string factsText: kindText + (view && view.project ? " · project " + Words.isolate(_shown(view.project, 100)) : "")
                                        + (countdownText !== "" ? " · " + countdownText : "")

    title: factsOutside ? "" : actionTitle
    Accessible.name: Words.spoken(actionTitle)

    Timer {
        interval: 1000
        repeat: true
        running: !!root.view && root.view.state === "pending"
        onTriggered: root.nowMs = Date.now()
    }

    Column {
        id: facts
        width: parent.width
        spacing: Theme.spacing / 2

        Text {
            id: factsLine
            width: parent.width
            visible: !!root.view && !root.factsOutside
            text: root.factsText
            textFormat: Text.PlainText
            color: Theme.textMuted
            font.pixelSize: Theme.fontSize
            wrapMode: Text.Wrap
            Accessible.role: Accessible.StaticText
            Accessible.name: Words.spoken(text)
        }

        Text {
            width: parent.width
            visible: root.argumentRows.length > 0 || (!!root.view && !!root.view.shown && !root.view.shown.over)
            text: root.argumentRows.length > 0 ? "Exact action:" : "No arguments."
            textFormat: Text.PlainText
            color: Theme.textMuted
            font.pixelSize: Theme.fontSize
        }

        Repeater {
            id: rowRepeater
            model: root.argumentRows
            delegate: Item {
                id: argument
                required property var modelData
                readonly property bool previewCut: root.previewChars > 0 && modelData.text.length > root.previewChars
                readonly property alias keyText: keyText
                readonly property alias valueText: valueText
                readonly property alias cutLine: cutLine
                readonly property alias rightToLeftLine: rightToLeftLine
                width: facts.width
                height: argumentColumn.implicitHeight

                // A frame a value's text can't draw.
                Rectangle {
                    width: 2
                    height: parent.height
                    color: Theme.surfaceRaised
                }

                Column {
                    id: argumentColumn
                    x: Theme.spacing
                    width: parent.width - x

                    Text {
                        id: keyText
                        width: parent.width
                        visible: text !== ""
                        text: argument.modelData.key !== "" ? Words.ltr(argument.modelData.key) : ""
                        textFormat: Text.PlainText
                        color: Theme.textMuted
                        font.pixelSize: Theme.fontSize
                        font.bold: true
                        wrapMode: Text.WrapAnywhere
                        Accessible.role: Accessible.StaticText
                        Accessible.name: Words.spoken(text)
                    }

                    Text {
                        id: valueText
                        width: parent.width
                        text: Words.ltr(argument.previewCut ? argument.modelData.text.slice(0, root.previewChars) + "…"
                                                            : argument.modelData.text)
                        textFormat: Text.PlainText
                        color: Theme.text
                        font.pixelSize: Theme.fontSize
                        wrapMode: Text.WrapAnywhere
                        Accessible.role: Accessible.StaticText
                        Accessible.name: Words.spoken(text)
                    }

                    Text {
                        id: rightToLeftLine
                        width: parent.width
                        visible: argument.modelData.rtl
                        text: "This has right-to-left letters, so its numbers and words can draw in a different order than they read."
                        textFormat: Text.PlainText
                        color: Theme.warning
                        font.pixelSize: Theme.fontSize
                        wrapMode: Text.Wrap
                        Accessible.role: Accessible.StaticText
                        Accessible.name: text
                    }

                    Text {
                        id: cutLine
                        width: parent.width
                        visible: argument.modelData.cut || argument.previewCut
                        text: argument.modelData.cut ? "This value is longer than LILITH can show, so part of it can't be read."
                                                     : "Review and answer shows all of this value."
                        textFormat: Text.PlainText
                        color: Theme.warning
                        font.pixelSize: Theme.fontSize
                        wrapMode: Text.Wrap
                        Accessible.role: Accessible.StaticText
                        Accessible.name: text
                    }
                }
            }
        }

        Item {
            id: footerSlot
            width: parent.width
            height: childrenRect.height
        }
    }
}
