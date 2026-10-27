.pragma library

// CoreCall: callbacks over the Core object's request signals (APP-028, APP-029).
//
//   const call = CoreCall.call(Core, "asset.get", {project, asset_id}, {
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
// pending confirmation, which is then its outcome. Core is anything with Core's API: the
// singleton, or the tests' FakeCore.

function call(core, tool, args, handlers) {
    return track(core, core.call(tool, JSON.stringify(args || {})), handlers || {})
}

// The same for an `app/*` method (never retried: Core.appCall's pending ends the request).
function appCall(core, method, params, handlers) {
    return track(core, core.appCall(method, JSON.stringify(params || {})), handlers || {})
}

function track(core, id, h) {
    const autoRetry = h.autoRetry === undefined ? true : h.autoRetry
    let open = true

    function close() {
        if (!open)
            return false
        open = false
        core.callFinished.disconnect(onFinished)
        core.callFailed.disconnect(onFailed)
        core.callPendingConfirmation.disconnect(onPending)
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
            core.cancel(id)
        }
        if (h.pending)
            h.pending(pending)
    }

    // Core delivers every outcome after `call` has returned, so connecting now misses none.
    core.callFinished.connect(onFinished)
    core.callFailed.connect(onFailed)
    core.callPendingConfirmation.connect(onPending)
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
