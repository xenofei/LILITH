// A web view on the suite's profile (SuiteWebProfile). Module Lilith.Web is the only code that
// imports QtWebEngine (APP-137): its plugin DLL, and with it the WebEngine DLLs, load only when a
// Loader first creates this panel. No panel list names it yet (Task 15).
import QtQuick
import QtWebEngine

Item {
    id: root

    property url url: "about:blank"
    // The view, made once the profile exists (Component.onCompleted).
    property WebEngineView view: null
    readonly property WebEngineProfile profile: view ? view.profile : null

    Component {
        id: profileComponent
        SuiteWebProfile {}
    }

    Component {
        id: viewComponent
        WebEngineView {
            anchors.fill: parent
        }
    }

    Component.onCompleted: {
        const proto = profileComponent.createObject(root)
        root.view = viewComponent.createObject(root, { profile: proto.instance() })
        root.view.url = root.url
    }

    onUrlChanged: if (root.view) root.view.url = root.url
}
