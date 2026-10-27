// The app's main window: a KDDockWidgets main window (DockingArea) that the panels dock into
// (APP-001). It starts hidden; main.cpp shows it through lilshim::activate, with or without the
// focus. Later tasks add the top bar, the panels and the open requests.
import QtQuick
import com.kdab.dockwidgets 2.0 as KDDW
import Lilith.UiKit

Window {
    id: root

    property alias dockingArea: dockingArea

    width: 1280
    height: 800
    visible: false
    title: "LILITH"
    color: Theme.window

    KDDW.DockingArea {
        id: dockingArea
        anchors.fill: parent
        uniqueName: "image-main"
    }
}
