// FakeCore: Core's API (module Lilith.Core, APP-028) for the QML tests, scripted by the test.
//
// A test reads what was asked (`calls`, `cancelled`, `subscribed`, `subscriberCount(topic)`,
// `resends`) and answers:
//   respond(id, outcome)    the wire outcome of a request: {status: "ok", ...result},
//                           {status: "error", error} or {status: "pending_confirmation", ...}
//   emit(topic, kind, data) an event (kind "event", "snapshot" or "resync_required"); each topic
//                           counts its own seq
//   emitFromCore(topic, kind, data)
//                           the same, through the bridge's event step (previews), as Core's are
//   jobsListAnswer(args, jobs, lanes)
//                           `jobs.list`'s answer, as the real core gives it, over the live `jobs`
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

    // `jobs.list`'s wire outcome for `args` over the live `jobs`, as the real core answers it
    // (`Jobs::list`, `Active::queued`, Appendix G "Queue position"): `{order: "queue"}` lists the
    // jobs the core holds queued, by lane, then by `queue_position`, those a restart holds after
    // the others; a job it has dispatched (`submitting`, `queued_in_engine`) keeps status `queued`
    // but isn't among them and has no `queue_position`. `{status}` lists that status, newest
    // first, the dispatched jobs with the others. Pages of `args.limit`, with a cursor of its own.
    function jobsListAnswer(args, jobs, lanes) {
        const dispatched = j => j.status === "queued"
                                && (j.substatus === "submitting" || j.substatus === "queued_in_engine")
        const placeless = j => j.queue_position === undefined || j.queue_position === null
        const laneRank = j => j.lane === "gpu-heavy" ? 0 : j.lane === "cpu" ? 1 : 2
        const sorted = jobs.map(j => dispatched(j) ? Object.assign({}, j, {queue_position: undefined}) : j)
        let listed
        if (args.order === "queue") {
            listed = sorted.filter(j => j.status === "queued" && !dispatched(j)).sort((a, b) =>
                laneRank(a) - laneRank(b) || Number(placeless(a)) - Number(placeless(b))
                || (a.queue_position || 0) - (b.queue_position || 0))
        } else {
            listed = sorted.filter(j => !args.status || j.status === args.status)
                           .sort((a, b) => String(b.created).localeCompare(String(a.created)))
        }
        const at = args.cursor ? Number(args.cursor.slice(1)) : 0
        const size = args.limit || 100
        const more = at + size < listed.length
        return {status: "ok", artifacts: [], summary_text: "",
                structured: {jobs: listed.slice(at, at + size), next_cursor: more ? "c" + (at + size) : null,
                             lanes: lanes || []}}
    }

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

    // emitFromCore(topic, kind, data): as emit, but the data first goes through the bridge's own
    // event step (crates/lil-qt/src/qt/preview_feed.rs), as Core's events do: a preview frame
    // reaches the image provider and the event carries {format, bytes, url}, and a finished
    // job's frame is dropped. The test runner provides BridgeHook (main.cpp).
    function emitFromCore(topic, kind, data) {
        const seq = (topic in _seqs ? _seqs[topic] : 0) + 1
        _seqs[topic] = seq
        coreEvent(topic, kind, seq, BridgeHook.event(topic, kind, JSON.stringify(data === undefined ? null : data)))
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
