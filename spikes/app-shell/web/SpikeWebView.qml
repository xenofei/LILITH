// SpikeWeb: the only code in the spike that imports QtWebEngine (APP-137, IMG-126). It is its
// own QML plugin DLL, loaded only when WebPanel.qml is first created.
//
// The profile is a named persistent profile made by WebEngineProfilePrototype (S0.3). The caller
// sets storageName, storagePath and cachePath. The prototype is created first (its properties
// count only until it completes), then the view is created with the prototype's instance() as
// its initial profile. Events go out through the `logged` signal, so this module imports
// nothing of the app's.
import QtQuick
import QtWebEngine

Item {
    id: root
    property string storageName: "image"
    property string storagePath: ""
    property string cachePath: ""
    property string webUrl: ""
    property var view: null

    signal logged(string event, var fields)

    Component {
        id: prototypeComponent
        WebEngineProfilePrototype {}
    }

    Component {
        id: viewComponent
        WebEngineView {
            anchors.fill: parent

            onLoadingChanged: function(info) {
                const names = {}
                names[WebEngineLoadingInfo.LoadStartedStatus] = "started"
                names[WebEngineLoadingInfo.LoadStoppedStatus] = "stopped"
                names[WebEngineLoadingInfo.LoadSucceededStatus] = "succeeded"
                names[WebEngineLoadingInfo.LoadFailedStatus] = "failed"
                root.logged("web_load", {
                    status: names[info.status] || ("unknown:" + info.status),
                    url: info.url.toString(),
                    error_code: info.errorCode,
                    error_string: info.errorString
                })
            }

            // S0.6 loads a page whose script sets the title, so this proves the page ran.
            onTitleChanged: root.logged("web_title", { title: title })

            onRenderProcessTerminated: function(status, exitCode) {
                root.logged("web_render_process_terminated", { status: status, exit_code: exitCode })
            }
        }
    }

    function load(url) {
        if (view && url !== "")
            view.url = url
    }

    Component.onCompleted: {
        const proto = prototypeComponent.createObject(root, {
            storageName: root.storageName,
            persistentStoragePath: root.storagePath,
            cachePath: root.cachePath
        })
        view = viewComponent.createObject(root, { profile: proto.instance() })
        const p = view.profile
        logged("web_profile", {
            storage_name: p.storageName,
            off_the_record: p.offTheRecord,
            persistent_storage_path: p.persistentStoragePath,
            cache_path: p.cachePath
        })
        load(webUrl)
    }

    onWebUrlChanged: load(webUrl)
}
