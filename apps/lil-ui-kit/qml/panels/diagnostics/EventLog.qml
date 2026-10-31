// The event log (ARCH-121, APP-134): the newest 1,000 events of every topic the app follows, but
// for what repeats state at a high rate (EventLogModel, the shell's when it has one), oldest first.
// It stays on the newest until you scroll or arrow back, and picks it up again when you return to
// the last row. One tab stop; the arrow keys move through it. Every field comes from outside: plain
// text, cut (P4a-R8).
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
        if (!following)
            return
        list.currentIndex = root.model.rowCount() - 1
        list.positionViewAtEnd()
    }

    function lastRowIsShown() {
        return list.indexAt(10, list.contentY + list.height - 2) >= list.count - 2
    }

    title: "Event log"

    // Not `count`: at the cap a row goes as one comes, and the count stays.
    Connections {
        target: root.model
        function onRowsInserted() {
            Qt.callLater(root.followNewest)
        }
    }

    Column {
        width: parent.width
        spacing: Theme.spacing

        LineText {
            tone: "muted"
            text: list.count === 0 ? "No events yet."
                : MoneyText.count(String(list.count)) + (list.count === 1 ? " event" : " events")
                  + ", the newest at the end"
        }
        LineText {
            tone: "muted"
            text: "Telemetry samples, job progress and preview frames aren't kept here: the Telemetry card "
                  + "and the Jobs panel show them."
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

            function keyMoved() {
                root.following = currentIndex === count - 1
            }

            onVisibleChanged: {
                if (visible)
                    Qt.callLater(root.followNewest)
            }
            onMovementEnded: root.following = root.lastRowIsShown()
            Keys.onUpPressed: event => {
                root.following = false
                event.accepted = false
            }
            Keys.onDownPressed: event => {
                event.accepted = false
                Qt.callLater(list.keyMoved)
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
