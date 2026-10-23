// The paged 10,000-row list (S0.6). While `scrolling`, it scrolls 200 rows a second, moved every
// frame (FrameAnimation), and asks for the next 200-row page when it nears the loaded end.
import QtQuick
import SpikeBridge

Rectangle {
    id: root
    color: "#2e3440"
    property bool scrolling: false
    readonly property int rowHeight: 24
    readonly property int loadedRows: gridModel.loaded
    readonly property int fetches: gridModel.fetches
    property real rowsScrolled: 0
    // S0.6: how many 1 s samples saw the list on screen (its dock current and laid out).
    property int visibleSeconds: 0
    property int hiddenSeconds: 0
    function resetVisibility() { visibleSeconds = 0; hiddenSeconds = 0 }
    function visibleRange() {
        return [list.indexAt(10, list.contentY + 1), list.indexAt(10, list.contentY + list.height - 2)]
    }
    Timer {
        interval: 1000
        repeat: true
        running: root.scrolling
        onTriggered: {
            if (root.visible && list.visible && root.width > 0 && root.height > 0 && root.Window.window)
                root.visibleSeconds++
            else
                root.hiddenSeconds++
        }
    }

    GridModel { id: gridModel }

    ListView {
        id: list
        anchors.fill: parent
        clip: true
        model: gridModel
        reuseItems: true
        delegate: Rectangle {
            required property int index
            required property string label
            required property int value
            required property string status
            width: ListView.view.width
            height: root.rowHeight
            color: index % 2 ? "#3b4252" : "#434c5e"
            Row {
                anchors.verticalCenter: parent.verticalCenter
                x: 8
                spacing: 16
                Text { text: label; color: "#eceff4"; width: 120; font.pixelSize: 12 }
                Text { text: value; color: "#a3be8c"; width: 50; font.pixelSize: 12 }
                Text { text: status; color: "#88c0d0"; font.pixelSize: 12 }
            }
        }
        Component.onCompleted: if (gridModel.loaded === 0) gridModel.fetchPage()
    }

    FrameAnimation {
        running: root.scrolling
        onTriggered: {
            const step = 200 * root.rowHeight * frameTime
            root.rowsScrolled += 200 * frameTime
            const maxY = list.contentHeight - list.height
            if (list.contentY + step >= maxY - 10 * root.rowHeight) {
                if (gridModel.loaded < 10000)
                    gridModel.fetchPage()
                else {
                    list.contentY = 0
                    return
                }
            }
            list.contentY = Math.min(list.contentY + step, Math.max(0, list.contentHeight - list.height))
        }
    }
}
