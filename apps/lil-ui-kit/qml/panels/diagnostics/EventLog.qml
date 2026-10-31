// The event log (ARCH-121, APP-134): the newest 1,000 events of every topic the app follows
// (EventLogModel, the shell's when it has one), oldest first. It stays on the newest until you
// scroll back, and picks it up again when you return to the end. One tab stop; the arrow keys move
// through it. Every field comes from outside: plain text, cut (P4a-R8).
import QtQuick
import Lilith.Core
import Lilith.UiKit
import "DiagnosticsText.js" as Words

LCard {
    id: root

    required property var core
    // The EventLogModel.
    required property var model

    readonly property alias view: list
    property bool following: true

    function followNewest() {
        if (following)
            list.positionViewAtEnd()
    }

    title: "Event log"

    Column {
        width: parent.width
        spacing: Theme.spacing

        LineText {
            tone: "muted"
            text: list.count === 0 ? "No events yet."
                : MoneyText.count(String(list.count)) + (list.count === 1 ? " event" : " events")
                  + ", the newest at the end"
        }

        ListView {
            id: list

            width: parent.width
            height: 240
            visible: count > 0
            clip: true
            model: root.model
            activeFocusOnTab: true
            boundsBehavior: Flickable.StopAtBounds
            highlightMoveDuration: 0
            highlightResizeDuration: 0

            Accessible.role: Accessible.List
            Accessible.name: "Event log"

            onCountChanged: Qt.callLater(root.followNewest)
            onMovementEnded: root.following = atYEnd
            onCurrentIndexChanged: {
                if (activeFocus)
                    root.following = currentIndex === count - 1
            }

            highlight: Rectangle {
                color: Theme.surfaceRaised
                visible: list.activeFocus
            }

            delegate: Item {
                id: row
                required property string topic
                required property string kind
                required property string text
                required property real receivedMs

                readonly property string shownTopic: Words.shown(root.core, topic, 60)
                readonly property string shownText: Words.shown(root.core, text, 300)

                width: ListView.view.width
                height: line.implicitHeight + 4

                Accessible.role: Accessible.ListItem
                Accessible.name: kind + " on " + shownTopic + ": " + Words.shown(root.core, text, 120)

                Text {
                    id: line
                    x: 4
                    y: 2
                    width: parent.width - 8
                    text: Qt.formatTime(new Date(row.receivedMs), "HH:mm:ss") + "  " + row.shownTopic + " "
                          + Words.shown(root.core, row.kind, 40) + "  " + row.shownText
                    textFormat: Text.PlainText
                    color: Theme.text
                    font.pixelSize: Theme.fontSize - 1
                    elide: Text.ElideRight
                }
            }
        }
    }

    // The keyboard focus on the list.
    Rectangle {
        visible: list.activeFocus
        parent: list
        anchors.fill: parent
        color: "transparent"
        border.color: Theme.focus
        border.width: Theme.focusWidth
        z: 2
    }
}
