.pragma library

// CoreCall: callbacks over the Core object's request signals (APP-028, APP-029), and topic
// subscriptions that end with their panel.
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
//   const sub = CoreCall.subscribe(Core, "jobs", root,
//                                  (kind, seq, data) => ...)  // optional: this topic's events
//   sub.unsubscribe()             // once; the owner's destruction does it too
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
// included; an OwnerWatch child of the owner tells it). An owner that can't hold that child (not
// a QML object) is refused before anything is sent.
//
// A cancelled call is never sent again, also when it was waiting on a confirmation: that
// confirmation stays answerable (the Confirmations panel, a notification), and approving it then
// runs nothing. A cancel just after an approval can come after Core's resend reached the core;
// the call then runs and its result is dropped. A panel that must see such a call through keeps
// it in an owner that outlives the panel.
//
// A panel follows a topic only through `subscribe`: Core counts its subscribers per topic, and
// each subscription unsubscribes exactly once, when it is ended or its owner is destroyed, so a
// panel can't take away another's subscription, nor keep one after it is gone.
//
// Core is anything with Core's API: the singleton, or the tests' FakeCore.

let watchComponent = null

// A watch on `owner`, destroyed with it, which then calls its `gone`. Throws when `owner` can't
// hold one: it must be a QML object (every one has `objectName`), not a plain JS object, which
// createObject would take with a warning and leave unparented.
function watch(owner) {
    if (!watchComponent)
        watchComponent = Qt.createComponent("Lilith.UiKit", "OwnerWatch")
    const qobject = !!owner && typeof owner === "object" && typeof owner.objectName === "string"
    const watcher = qobject ? watchComponent.createObject(owner) : null
    if (!watcher)
        throw new Error("CoreCall: the owner must be the QML object the call or subscription belongs to")
    return watcher
}

function call(core, tool, args, handlers) {
    const h = handlers || {}
    const watcher = watch(h.owner)
    return track(core, core.call(tool, JSON.stringify(args || {})), h, false, watcher)
}

// The same for an `app/*` method.
function appCall(core, method, params, handlers) {
    const h = handlers || {}
    const watcher = watch(h.owner)
    return track(core, core.appCall(method, JSON.stringify(params || {})), h, true, watcher)
}

// Follows `topic` for `owner` until `unsubscribe()` or the owner's destruction, whichever comes
// first; `onEvent(kind, seq, data)`, when given, gets the topic's events, with the owner as its
// receiver.
function subscribe(core, topic, owner, onEvent) {
    const watcher = watch(owner)
    let on = true
    function onCoreEvent(t, kind, seq, json) {
        if (on && t === topic)
            onEvent(kind, seq, JSON.parse(json))
    }
    function off() {
        if (!on)
            return false
        on = false
        // The core object may have gone first (a test's FakeCore).
        try {
            if (onEvent)
                core.coreEvent.disconnect(owner, onCoreEvent)
            core.unsubscribe(topic)
        } catch (e) {
        }
        return true
    }
    watcher.gone = off
    if (onEvent)
        core.coreEvent.connect(owner, onCoreEvent)
    core.subscribe(topic)
    return {
        topic: topic,
        unsubscribe: function () {
            if (!on)
                return false
            watcher.gone = null
            watcher.destroy()
            return off()
        }
    }
}

function track(core, id, h, endsAtPending, watcher) {
    const autoRetry = !endsAtPending && (h.autoRetry === undefined ? true : h.autoRetry)
    const owner = h.owner
    let open = true

    function close() {
        if (!open)
            return false
        open = false
        try {
            core.callFinished.disconnect(owner, onFinished)
            core.callFailed.disconnect(owner, onFailed)
            core.callPendingConfirmation.disconnect(owner, onPending)
        } catch (e) {
            // The core object went first.
        }
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
        try {
            core.cancel(id)
        } catch (e) {
            // The core object went first: there is no request to forget.
        }
    }

    // Core delivers every outcome after `call` has returned, so connecting now misses none.
    core.callFinished.connect(owner, onFinished)
    core.callFailed.connect(owner, onFailed)
    core.callPendingConfirmation.connect(owner, onPending)
    watcher.gone = onOwnerGone
    return {
        id: id,
        cancel: function () {
            if (!close())
                return false
            try {
                core.cancel(id)
            } catch (e) {
                // The core object went first.
            }
            return true
        }
    }
}
