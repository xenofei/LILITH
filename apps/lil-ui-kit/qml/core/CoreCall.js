.pragma library

// CoreCall: callbacks over the Core object's request signals (APP-028, APP-029).
//
//   const call = CoreCall.call(Core, "asset.get", {project, asset_id}, {
//       owner: root,              // required: the QML object the call belongs to
//       ok: result => ...,        // the tool's output: {structured, artifacts, summary_text}
//       error: error => ...,      // the error body: {code, category, message, ...}
//       pending: pending => ...,  // {confirmation_id, retry_token, expires_at, covers}
//       autoRetry: true,          // the default
//   })
//   call.cancel()                 // drops the callbacks (and Core's request); false once settled
//
// Each call settles once: ok or error. A pending confirmation is told to `pending` and, with
// autoRetry (the default), the request stays open: Core waits for the confirmation's answer,
// sends the call again once with its retry token when it is approved, and the result (or
// confirm.denied / confirm.expired) settles it. Without autoRetry the request ends at the
// pending confirmation, which is then its outcome. `appCall` is never retried: its pending
// confirmation always ends it.
//
// A call belongs to its `owner`, the object whose handlers it runs (a panel, a delegate): the
// callbacks are connected with the owner as their receiver, so once the owner is destroyed none
// of them runs, and a call still open is cancelled (Core drops its request, a pending one
// included; an OwnerWatch child of the owner tells it). Core is anything with Core's API: the
// singleton, or the tests' FakeCore.

let watchComponent = null

// A watch on `owner` that calls `gone` when the owner is destroyed.
function watch(owner, gone) {
    if (!watchComponent)
        watchComponent = Qt.createComponent("Lilith.UiKit", "OwnerWatch")
    return watchComponent.createObject(owner, {gone: gone})
}

function call(core, tool, args, handlers) {
    const h = owned(handlers)
    return track(core, core.call(tool, JSON.stringify(args || {})), h, false)
}

// The same for an `app/*` method.
function appCall(core, method, params, handlers) {
    const h = owned(handlers)
    return track(core, core.appCall(method, JSON.stringify(params || {})), h, true)
}

function owned(handlers) {
    const h = handlers || {}
    if (!h.owner)
        throw new Error("CoreCall: handlers.owner, the QML object the call belongs to, is required")
    return h
}

function track(core, id, h, endsAtPending) {
    const autoRetry = !endsAtPending && (h.autoRetry === undefined ? true : h.autoRetry)
    const owner = h.owner
    let open = true
    let watcher = null

    function close() {
        if (!open)
            return false
        open = false
        core.callFinished.disconnect(owner, onFinished)
        core.callFailed.disconnect(owner, onFailed)
        core.callPendingConfirmation.disconnect(owner, onPending)
        if (watcher) {
            watcher.gone = null
            watcher.destroy()
            watcher = null
        }
        return true
    }
    function onFinished(rid, json) {
        if (rid !== id || !close())
            return
        if (h.ok)
            h.ok(JSON.parse(json))
    }
    function onFailed(rid, json) {
        if (rid !== id || !close())
            return
        if (h.error)
            h.error(JSON.parse(json))
    }
    function onPending(rid, json) {
        if (rid !== id || !open)
            return
        const pending = JSON.parse(json)
        if (!autoRetry) {
            close()
            // An app call's request has ended already; a tool call's is given up.
            if (!endsAtPending)
                core.cancel(id)
        }
        if (h.pending)
            h.pending(pending)
    }
    // The owner is going: none of its handlers runs again (Qt drops the connections whose
    // receiver it is), and Core forgets the request.
    function onOwnerGone() {
        if (!open)
            return
        open = false
        watcher = null
        core.cancel(id)
    }

    // Core delivers every outcome after `call` has returned, so connecting now misses none.
    core.callFinished.connect(owner, onFinished)
    core.callFailed.connect(owner, onFailed)
    core.callPendingConfirmation.connect(owner, onPending)
    watcher = watch(owner, onOwnerGone)
    return {
        id: id,
        cancel: function () {
            if (!close())
                return false
            core.cancel(id)
            return true
        }
    }
}
