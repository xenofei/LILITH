// A job's live preview (APP-031, APP-037): follows preview:<jobId> and job:<jobId> through
// CoreCall, for as long as it exists, and shows each frame's image://lilpreview/<job_id>/<frame>
// URL as the bridge gives it, a new one per frame, with caching off and loading off the GUI
// thread; it keeps the frame shown while the next loads. Once the job ends it shows nothing
// (the provider drops the frame). No image bytes reach QML: only the URL.
//
//   PreviewImage { core: Core; jobId: job.job_id }
import QtQuick
import Lilith.UiKit

Image {
    id: root

    // The Core singleton, or any object with its API (a test's FakeCore).
    property var core: null
    property string jobId: ""

    cache: false
    asynchronous: true
    retainWhileLoading: true
    fillMode: Image.PreserveAspectFit

    property var _subscriptions: []
    property bool _complete: false

    function _follow() {
        for (const s of _subscriptions)
            s.unsubscribe()
        _subscriptions = []
        source = ""
        if (!core || !jobId)
            return
        _subscriptions = [
            CoreCall.subscribe(core, "preview:" + jobId, root, (kind, seq, data) => {
                if (kind === "event" && data && data.url)
                    root.source = data.url
            }),
            CoreCall.subscribe(core, "job:" + jobId, root, (kind, seq, data) => {
                const status = data && data.kind === "status" && data.job ? data.job.status : ""
                if (status === "succeeded" || status === "failed" || status === "cancelled")
                    root.source = ""
            })
        ]
    }

    onCoreChanged: if (_complete) _follow()
    onJobIdChanged: if (_complete) _follow()
    Component.onCompleted: {
        _complete = true
        _follow()
    }
}
