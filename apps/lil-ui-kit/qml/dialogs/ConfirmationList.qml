// The confirmations, pending and past (APP-115), for the Confirmations panel and the confirm window:
// `app/confirmations.list` for each, kept live from the `confirmations` topic ({kind, confirmation},
// Appendix F). Not a JsonListModel: that pages a tool's `structured` output, and this is an `app/*`
// method with no pages.
//
// It reloads on the topic's `resync_required` or `snapshot`, never on `ready`, but for a first load
// that hasn't come in (the session wasn't connected yet), which also retries after 1 s, doubling to
// 30 s. `loaded` is false until a load is in, and a dialog holds its answers meanwhile. Events during
// a load are held and applied after it. Each event is the row's full state, and they can overtake
// one another, so one that says pending never undoes an ending. The rows stay made across a reload.
//
// Each row carries `shown`: its arguments as the card shows them, from the JSON as it came
// (ConfirmInput.shownActions), {rows: [{key, text, cut}], over, digest}. QML's own parse rounds big
// integers and reorders integer-like keys, so `arguments` is never what is shown.
import QtQuick
import QtQml.Models
import Lilith.Core
import Lilith.UiKit

Item {
    id: root

    required property var core
    readonly property alias pendingModel: pending   // role `cid`, newest first
    readonly property alias historyModel: history   // role `cid`, newest first
    property bool loaded: false
    // The last failed load's error body, until one comes in.
    property var error: null
    // Bumped when a row changes: view(id) in a binding follows it.
    property int revision: 0
    // How many of each list one load asks for (the method's most).
    readonly property int listLimit: 500

    property var _rows: ({})
    property var _held: []
    property int _generation: 0
    property int _retryMs: 1000

    function view(id) {
        return revision >= 0 && Object.prototype.hasOwnProperty.call(_rows, id) ? _rows[id] : null
    }

    function _withShown(rows, raw) {
        const shown = JSON.parse(ConfirmInput.shownActions(raw))
        for (const v of rows)
            v.shown = shown[v.id]
        return rows
    }

    function reload() {
        const generation = ++_generation
        loaded = false
        retry.stop()
        const got = {}
        const fetch = state => CoreCall.appCall(core, "app/confirmations.list", {state: state, limit: listLimit}, {
            owner: root,
            ok: (result, raw) => {
                if (generation !== _generation)
                    return
                got[state] = _withShown(result.confirmations, raw)
                if (got.pending && got.history)
                    _apply(got.pending, got.history)
            },
            error: e => {
                if (generation !== _generation)
                    return
                ++_generation
                error = e
                retry.interval = _retryMs
                _retryMs = Math.min(_retryMs * 2, 30000)
                retry.start()
            }
        })
        fetch("pending")
        fetch("history")
    }

    function _apply(pendingRows, historyRows) {
        const rows = {}
        for (const v of historyRows.concat(pendingRows))
            rows[v.id] = v
        _rows = rows
        _sync(pending, pendingRows.map(v => v.id))
        _sync(history, historyRows.map(v => v.id))
        error = null
        _retryMs = 1000
        loaded = true
        ++revision
        const held = _held
        _held = []
        held.forEach(_upsert)
    }

    // Makes `model` hold `ids`, in order, keeping the rows it holds already.
    function _sync(model, ids) {
        for (let i = model.count - 1; i >= 0; --i) {
            if (ids.indexOf(model.get(i).cid) < 0)
                model.remove(i)
        }
        ids.forEach((id, at) => {
            let found = -1
            for (let i = at; i < model.count; ++i) {
                if (model.get(i).cid === id) {
                    found = i
                    break
                }
            }
            if (found < 0)
                model.insert(at, {cid: id})
            else if (found !== at)
                model.move(found, at, 1)
        })
    }

    function _drop(model, id) {
        for (let i = 0; i < model.count; ++i) {
            if (model.get(i).cid === id) {
                model.remove(i)
                return
            }
        }
    }

    function _has(model, id) {
        for (let i = 0; i < model.count; ++i) {
            if (model.get(i).cid === id)
                return true
        }
        return false
    }

    // A row's new state, from an event, or an answer's own result (which has no `shown`: a row's
    // arguments never change).
    function apply(v) {
        if (loaded)
            _upsert(v)
        else
            _held = _held.concat([v])
    }

    function _upsert(v) {
        const old = view(v.id)
        if (old && old.state !== "pending" && v.state === "pending")
            return
        if (!v.shown)
            v.shown = old && old.shown ? old.shown : {rows: [], over: true, digest: ""}
        _rows[v.id] = v
        if (v.state === "pending") {
            if (!_has(pending, v.id))
                pending.insert(0, {cid: v.id})
        } else {
            _drop(pending, v.id)
            if (!_has(history, v.id))
                history.insert(0, {cid: v.id})
        }
        ++revision
    }

    ListModel { id: pending }
    ListModel { id: history }

    Timer {
        id: retry
        onTriggered: root.reload()
    }

    Connections {
        target: root.core
        ignoreUnknownSignals: true
        function onConnectionChanged() {
            if (!root.loaded && root.core.connection === "ready")
                root.reload()
        }
    }

    Component.onCompleted: {
        CoreCall.subscribe(core, "confirmations", root, (kind, seq, data, raw) => {
            if (kind === "snapshot" || kind === "resync_required")
                root.reload()
            else if (kind === "event" && data && data.confirmation)
                root.apply(root._withShown([data.confirmation], raw)[0])
        })
        reload()
    }
}
