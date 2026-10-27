// FakeCore: Core's API (module Lilith.Core, APP-028) for the QML tests, scripted by the test.
//
// A test reads what was asked (`calls`, `cancelled`, `subscribed`, `subscriberCount(topic)`,
// `resends`) and answers:
//   respond(id, outcome)    the wire outcome of a request: {status: "ok", ...result},
//                           {status: "error", error} or {status: "pending_confirmation", ...}
//   emit(topic, kind, data) an event (kind "event", "snapshot" or "resync_required"); each topic
//                           counts its own seq
// As Core does, a tool call's pending confirmation keeps the request open (unless the caller
// cancels it), and an answer to it on the `confirmations` topic resends the call once with its
// retry token (recorded in `resends`, answered again with respond(id, ...)) or settles it with
// confirm.denied or confirm.expired. Outcomes are signalled at once (Core's come later, from the
// event loop): a test responds only after the call has returned.
import QtQuick

QtObject {
    id: fake

    // Core's properties.
    property string connection: "ready"
    property string coreProtocol: "1.2"
    property var workspace: null
    property string project: ""
    property string channelScheme: "lil-test-fake"
    property var startTimes: ({})
    readonly property string buildId: "0.0.0+fake"

    // What the code under test asked.
    property var calls: []        // {id, kind: "call" | "appCall" | "setWorkspace", name, args}
    property var cancelled: []    // request ids
    property var subscribed: []   // the topics followed, in the order they were first followed
    property var resends: []      // {id, retryToken}

    signal callFinished(string id, string resultJson)
    signal callFailed(string id, string errorJson)
    signal callPendingConfirmation(string id, string pendingJson)
    signal coreEvent(string topic, string kind, real seq, string dataJson)

    property int _next: 1
    property var _open: ({})      // id -> {kind, confirmation, retryToken}
    property var _seqs: ({})
    property var _counts: ({})    // topic -> subscribers, counted as Core counts them

    function _request(kind, name, argsJson) {
        const id = "rq_fake_" + _next++
        calls = calls.concat([{id: id, kind: kind, name: name, args: argsJson ? JSON.parse(argsJson) : {}}])
        _open[id] = {kind: kind}
        return id
    }

    function call(tool, argsJson) { return _request("call", tool, argsJson) }
    function appCall(method, paramsJson) { return _request("appCall", method, paramsJson) }
    function setWorkspace(id) { return _request("setWorkspace", id, "") }

    function cancel(id) {
        if (!(id in _open))
            return false
        delete _open[id]
        cancelled = cancelled.concat([id])
        return true
    }

    // Counted per topic, as Core does: the first subscriber follows it, the last one's
    // unsubscribe ends it, and an unsubscribe of a topic not followed does nothing.
    function subscribe(topic) {
        const n = (_counts[topic] || 0) + 1
        _counts[topic] = n
        if (n === 1)
            subscribed = subscribed.concat([topic])
    }
    function unsubscribe(topic) {
        const n = _counts[topic] || 0
        if (n === 0)
            return
        if (n === 1) {
            delete _counts[topic]
            subscribed = subscribed.filter(t => t !== topic)
        } else {
            _counts[topic] = n - 1
        }
    }
    function subscriberCount(topic) { return _counts[topic] || 0 }
    function retryNow() {}
    function lastSeq(topic) { return topic in _seqs ? _seqs[topic] : -1 }
    function shownText(text, maxChars) {
        return text.length <= maxChars ? text : text.slice(0, Math.max(0, maxChars - 1)) + "…"
    }

    // The test's side.
    function respond(id, outcome) {
        if (!(id in _open))
            return false
        const request = _open[id]
        const rest = Object.assign({}, outcome)
        delete rest.status
        if (outcome.status === "pending_confirmation") {
            if (request.kind === "call") {
                request.confirmation = outcome.confirmation_id
                request.retryToken = outcome.retry_token
            } else {
                delete _open[id]
            }
            callPendingConfirmation(id, JSON.stringify(rest))
            return true
        }
        delete _open[id]
        if (outcome.status === "ok")
            callFinished(id, JSON.stringify(rest))
        else
            callFailed(id, JSON.stringify(outcome.error))
        return true
    }

    function emit(topic, kind, data) {
        const seq = (topic in _seqs ? _seqs[topic] : 0) + 1
        _seqs[topic] = seq
        coreEvent(topic, kind, seq, JSON.stringify(data === undefined ? null : data))
        if (topic === "confirmations" && kind === "event")
            _answered(data)
    }

    function _answered(data) {
        const confirmation = data.confirmation || {}
        for (const id in _open) {
            const request = _open[id]
            if (request.confirmation !== confirmation.id || request.resent)
                continue
            const state = data.kind === "answered" ? confirmation.state : data.kind
            if (state === "approved") {
                request.resent = true
                resends = resends.concat([{id: id, retryToken: request.retryToken}])
            } else if (state === "denied" || state === "expired" || state === "cancelled") {
                delete _open[id]
                const code = state === "denied" ? "confirm.denied" : "confirm.expired"
                callFailed(id, JSON.stringify({code: code, category: state === "denied" ? "declined" : "cancelled",
                                               message: "The confirmation was " + state + ".",
                                               trace_id: "t_fake", retry: "never"}))
            }
        }
    }
}
