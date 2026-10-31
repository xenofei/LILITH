// One tool a Diagnostics card reads (Task 25; ARCH-100, APP-033), and the topic it follows, while
// `active` (the panel is shown). Each time it becomes active it reads again, at most once in 2 s
// (`showIntervalMs`); while it isn't, it follows nothing and reads nothing.
//
// - A `resync_required` or a snapshot of the topic reads again, and `reloading` tells the card, for a
//   tool beside this one that has no topic; every other event goes to `topicEvent`, for the card to
//   apply as full state or to `request()` a read.
// - `request()` is at most one read a second: the first of a burst at once, the rest as one read
//   when the second has passed. Reads don't overlap.
// - An answer can be older than an event: a card that applies an event as full state calls
//   `eventApplied()`. `answered(structured, current)` then has `current` false for a read that was
//   open at that moment, the card keeps the event's state, and a fresh read follows.
// - The connection's own state never reads again, but for a read that failed (the session wasn't
//   connected, or the core was away), also when it is still open as `ready` comes: there is no good
//   answer in hand for that to overwrite.
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
    property bool active: true

    property var result: null
    property var error: null
    property bool loaded: false

    readonly property int showIntervalMs: 2000

    signal topicEvent(var data)
    signal reloading()
    signal answered(var structured, bool current)

    property bool _complete: false
    property bool _reading: false
    property bool _due: false
    property bool _showDue: false
    property int _events: 0
    property int _eventsAtStart: 0
    property var _subscription: null

    function eventApplied() {
        _events++
    }

    function read() {
        if (_reading) {
            _due = true
            return
        }
        _reading = true
        _eventsAtStart = _events
        CoreCall.call(core, tool, args, {
            owner: root,
            ok: r => {
                const current = root._events === root._eventsAtStart
                root.result = r.structured || null
                root.error = null
                // Before `loaded`, so what follows `loaded` sees the card's new state.
                root.answered(root.result, current)
                root.loaded = true
                if (!current)
                    root._due = true
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
    }

    function request() {
        if (!active)
            return
        if (_gate.running) {
            _due = true
            return
        }
        read()
        _gate.start()
    }

    function _shown() {
        if (topic !== "" && !_subscription) {
            _subscription = CoreCall.subscribe(core, topic, root, (kind, seq, data) => {
                if (kind === "event")
                    root.topicEvent(data)
                else {
                    root.request()
                    root.reloading()
                }
            })
        }
        if (_showGate.running) {
            _showDue = true
            return
        }
        _showGate.start()
        request()
    }

    function _hidden() {
        if (_subscription) {
            _subscription.unsubscribe()
            _subscription = null
        }
        _gate.stop()
        _due = false
        _showDue = false
    }

    onActiveChanged: {
        if (!_complete)
            return
        if (active)
            _shown()
        else
            _hidden()
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

    property Timer _showGate: Timer {
        interval: root.showIntervalMs
        onTriggered: {
            if (!root._showDue)
                return
            root._showDue = false
            root.request()
            start()
        }
    }

    property Connections _session: Connections {
        target: root.core
        function onConnectionChanged() {
            if (root.core.connection !== "ready" || !root.active || (root.loaded && !root.error))
                return
            if (root._reading)
                root._due = true
            else
                root.read()
        }
    }

    Component.onCompleted: {
        _complete = true
        if (active)
            _shown()
    }
}
