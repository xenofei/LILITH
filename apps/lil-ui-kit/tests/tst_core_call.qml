// CoreCall over Core's request signals (APP-028, APP-029): each outcome reaches its callback
// once, a pending confirmation keeps the request open for the automatic retry, a cancel drops
// the callback, and Core hands QML a request id (a string), never a request object.
import QtQuick
import QtTest
import Lilith.Core
import Lilith.UiKit

TestCase {
    id: tc
    name: "CoreCall"

    Component {
        id: fakeComponent
        FakeCore {}
    }

    function fake() {
        return createTemporaryObject(fakeComponent, tc)
    }

    // Records what reached which callback.
    function recorder() {
        const got = {ok: [], error: [], pending: []}
        got.handlers = {
            ok: r => got.ok.push(r),
            error: e => got.error.push(e),
            pending: p => got.pending.push(p)
        }
        return got
    }

    readonly property var pendingOutcome: ({
        status: "pending_confirmation",
        confirmation_id: "cf_0000000000000001",
        retry_token: "ik_00000000000000a1",
        expires_at: "2026-10-26T12:30:00Z",
        covers: ["cap"]
    })

    function test_ok_error_and_pending_reach_their_callbacks() {
        const core = fake()
        const a = recorder(), b = recorder(), c = recorder()
        const okCall = CoreCall.call(core, "test.echo", {text: "hi"}, a.handlers)
        const errorCall = CoreCall.call(core, "test.write", {label: "x", fail: true}, b.handlers)
        c.handlers.autoRetry = false
        const pendingCall = CoreCall.call(core, "test.destructive", {cost: "5"}, c.handlers)
        compare(core.calls.length, 3)
        compare(core.calls[0].name, "test.echo")
        compare(core.calls[0].args.text, "hi")

        core.respond(okCall.id, {status: "ok", structured: {text: "hi"}, artifacts: [], summary_text: "echoed"})
        core.respond(errorCall.id, {status: "error", error: {code: "params.invalid", message: "no"}})
        core.respond(pendingCall.id, pendingOutcome)

        compare(a.ok.length, 1)
        compare(a.ok[0].structured.text, "hi")
        compare(a.error.length + a.pending.length, 0)
        compare(b.error.length, 1)
        compare(b.error[0].code, "params.invalid")
        compare(b.ok.length + b.pending.length, 0)
        compare(c.pending.length, 1)
        compare(c.pending[0].retry_token, "ik_00000000000000a1")
        compare(c.pending[0].covers, ["cap"])
        compare(c.ok.length + c.error.length, 0)
        // Without autoRetry the request ends at the pending confirmation.
        verify(core.cancelled.indexOf(pendingCall.id) >= 0, "the request was given up")

        // A second outcome for a settled request reaches nobody.
        core.respond(okCall.id, {status: "ok", structured: {}, artifacts: [], summary_text: ""})
        compare(a.ok.length, 1)
        verify(!okCall.cancel(), "a settled call can't be cancelled")
    }

    function test_auto_retry_resends_with_the_retry_token() {
        const core = fake()
        const got = recorder()
        const call = CoreCall.call(core, "test.destructive", {cost: "5"}, got.handlers)
        core.respond(call.id, pendingOutcome)
        compare(got.pending.length, 1, "the pending confirmation is told")
        compare(got.ok.length + got.error.length, 0, "and the call stays open")
        compare(core.cancelled.length, 0)

        // Another confirmation's answer changes nothing.
        core.emit("confirmations", "event",
                  {kind: "answered", confirmation: {id: "cf_0000000000000002", state: "approved"}})
        compare(core.resends.length, 0)
        // Its own: one resend, with its retry token.
        const answered = {kind: "answered", confirmation: {id: "cf_0000000000000001", state: "approved"}}
        core.emit("confirmations", "event", answered)
        compare(core.resends.length, 1)
        compare(core.resends[0].id, call.id)
        compare(core.resends[0].retryToken, "ik_00000000000000a1")
        core.emit("confirmations", "event", answered)
        compare(core.resends.length, 1, "never twice")

        core.respond(call.id, {status: "ok", structured: {count: 1}, artifacts: [], summary_text: "run 1"})
        compare(got.ok.length, 1, "the original callback settles with the result")
        compare(got.ok[0].structured.count, 1)
        compare(got.error.length, 0)

        // A denial settles it with confirm.denied, without a resend.
        const denied = recorder()
        const other = CoreCall.call(core, "test.destructive", {cost: "5"}, denied.handlers)
        core.respond(other.id, Object.assign({}, pendingOutcome, {confirmation_id: "cf_0000000000000003"}))
        core.emit("confirmations", "event",
                  {kind: "answered", confirmation: {id: "cf_0000000000000003", state: "denied"}})
        compare(denied.error.length, 1)
        compare(denied.error[0].code, "confirm.denied")
        compare(core.resends.length, 1)
    }

    function test_cancel_drops_the_callback() {
        const core = fake()
        const got = recorder()
        const call = CoreCall.call(core, "test.echo", {text: "late"}, got.handlers)
        verify(call.cancel(), "an open call is cancelled")
        compare(core.cancelled, [call.id])
        verify(!call.cancel(), "only once")
        core.respond(call.id, {status: "ok", structured: {text: "late"}, artifacts: [], summary_text: ""})
        compare(got.ok.length + got.error.length + got.pending.length, 0)
        verify(!core.cancel("rq_unknown"), "an unknown id is false")
    }

    SignalSpy {
        id: failedSpy
        target: Core
        signalName: "callFailed"
    }

    function test_no_request_object_is_returned_to_qml() {
        // The real Core: in a process with no core session, every call fails core.unreachable,
        // after `call` has returned.
        tryCompare(Core, "connection", "unavailable", 5000)
        failedSpy.clear()
        const id = Core.call("test.echo", JSON.stringify({text: "hi"}))
        compare(typeof id, "string")
        verify(/^rq_\d+$/.test(id), id)
        compare(failedSpy.count, 0, "nothing is signalled before the call returns")
        failedSpy.wait(5000)
        compare(failedSpy.signalArguments[0][0], id)
        const error = JSON.parse(failedSpy.signalArguments[0][1])
        compare(error.code, "core.unreachable")
        verify(/^tr_[0-9a-f]{16}$/.test(error.trace_id), error.trace_id)

        const other = Core.appCall("app/confirmations.list", JSON.stringify({state: "pending"}))
        compare(typeof other, "string")
        verify(other !== id)
        verify(Core.cancel(other), "an open request is cancelled")
        verify(!Core.cancel(other), "only once")
        verify(!Core.cancel("rq_0"), "an unknown id is false")
        compare(Core.lastSeq("confirmations"), -1)
        // Events come as coreEvent, which leaves QObject::event alone.
        compare(typeof Core.coreEvent, "function")
        compare(typeof Core.coreEvent.connect, "function")
        compare(Core.shownText("a‮b", 20), "a⟨U+202E⟩b")

        // Arguments that aren't a JSON object fail params.invalid.
        failedSpy.clear()
        const bad = Core.call("test.echo", "[1]")
        failedSpy.wait(5000)
        compare(failedSpy.signalArguments[0][0], bad)
        compare(JSON.parse(failedSpy.signalArguments[0][1]).code, "params.invalid")
    }
}
