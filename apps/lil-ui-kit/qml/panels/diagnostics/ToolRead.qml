// One tool a Diagnostics card reads (Task 25; ARCH-100, APP-033): once when made and, when it is
// given a topic, followed from then on.
//
// - A `resync_required` or a snapshot of the topic reads again; every other event goes to
//   `topicEvent`, for the card to apply as full state or to `request()` a read.
// - `request()` is at most one read a second: the first of a burst at once, the rest as one read
//   when the second has passed. Reads don't overlap.
// - The connection's own state never reads again, but for a first read that failed (the session
//   wasn't connected yet).
// - `result` is the last good answer's `structured`, and stays through a later failure; `error` is
//   the last read's ErrorBody, null once a read succeeds.
import QtQuick
import Lilith.UiKit

QtObject {
    id: root

    required property var core
    required property string tool
    property var args: ({})
    property string topic

    property var result: null
    property var error: null
    property bool loaded: false

    signal topicEvent(var data)
    signal done()

    property bool _reading: false
    property bool _due: false

    function read() {
        if (_reading) {
            _due = true
            return
        }
        _reading = true
        CoreCall.call(core, tool, args, {
            owner: root,
            ok: r => {
                root.result = r.structured || null
                root.error = null
                root.loaded = true
                root._settled()
            },
            error: e => {
                root.error = e
                root._settled()
            }
        })
    }

    function _settled() {
        _reading = false
        if (_due) {
            _due = false
            request()
        }
        done()
    }

    function request() {
        if (_gate.running) {
            _due = true
            return
        }
        read()
        _gate.start()
    }

    property Timer _gate: Timer {
        interval: 1000
        onTriggered: {
            if (!root._due)
                return
            root._due = false
            root.read()
            start()
        }
    }

    property Connections _session: Connections {
        target: root.core
        function onConnectionChanged() {
            if (root.core.connection === "ready" && !root.loaded && !root._reading)
                root.read()
        }
    }

    Component.onCompleted: {
        if (topic !== "") {
            CoreCall.subscribe(core, topic, root, (kind, seq, data) => {
                if (kind === "event")
                    root.topicEvent(data)
                else
                    root.request()
            })
        }
        request()
    }
}
