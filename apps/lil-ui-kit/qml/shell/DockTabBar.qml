// The docks' tab bar, handed to KDDockWidgets by the shim's DockFactory (APP-001, APP-012). It
// derives from KDDockWidgets' TabBarBase, whose mouse area sends clicks and drags to KDDockWidgets
// (which asks getTabIndexAtPosition), and draws its own tabs without Qt Quick Controls. Each tab
// is exposed as a page tab named after its dock (APP-156; spike 10 S0.7 read it this way).
import QtQuick
import Lilith.UiKit
import "qrc:/kddockwidgets/qtquick/views/qml/" as KDDW

KDDW.TabBarBase {
    id: root

    implicitHeight: 28

    // Called by KDDockWidgets: the tab item at `index`.
    function getTabAtIndex(index) {
        return tabs.itemAt(index)
    }

    // Called by KDDockWidgets: the index of the tab under `globalPoint`, or -1.
    function getTabIndexAtPosition(globalPoint) {
        for (let i = 0; i < tabs.count; ++i) {
            const tab = tabs.itemAt(i)
            if (tab && tab.contains(tab.mapFromGlobal(globalPoint.x, globalPoint.y)))
                return i
        }
        return -1
    }

    Rectangle {
        anchors.fill: parent
        color: Theme.bg
    }

    Row {
        height: parent.height

        Repeater {
            id: tabs
            model: root.groupCpp ? root.groupCpp.tabBar.dockWidgetModel : 0

            Rectangle {
                id: tab

                required property int index
                required property string title
                readonly property int tabIndex: index
                readonly property bool current: index === root.currentTabIndex

                width: label.implicitWidth + 2 * Theme.spacing + 8
                height: root.height
                color: current ? Theme.surface : Theme.bg

                Accessible.role: Accessible.PageTab
                Accessible.name: title
                Accessible.selected: current
                Accessible.onPressAction: root.currentTabIndex = tab.index

                Rectangle {
                    width: parent.width
                    height: 2
                    color: tab.current ? Theme.accent : "transparent"
                }

                Text {
                    id: label
                    anchors.centerIn: parent
                    text: tab.title
                    color: tab.current ? Theme.text : Theme.textMuted
                    font.pixelSize: Theme.fontSize
                }
            }
        }
    }
}
