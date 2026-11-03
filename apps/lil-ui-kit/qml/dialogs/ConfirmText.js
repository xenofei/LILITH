// The words of a confirmation (APP-046, APP-115), for the confirm dialog, the cap prompt, the
// Confirmations panel and the confirm window. Everything from outside goes through `shown`
// (ConfirmInput.shownLine, which marks every invisible character, then shown_text's limit). The
// arguments themselves are lil_qt::confirm_text's, from the JSON as the core sent it: `view.shown`.
.pragma library

function who(callerKind, shown) {
    const kind = String(callerKind)
    if (kind.startsWith("ui:"))
        return "The " + shown(kind.slice(3), 60) + " app (you)"
    switch (kind) {
    case "mcp": return "Claude Code"
    case "assistant": return "The assistant"
    case "cli": return "The command line (lil)"
    default: return shown(kind, 60)
    }
}

function coversCap(view) {
    return !!view && (view.covers || []).indexOf("cap") >= 0
}

function coversConfirm(view) {
    return !!view && (view.covers || []).indexOf("confirm") >= 0
}

// What the dialog shows of this confirmation, as one string: when it changes, the gate re-arms.
function fingerprint(view) {
    if (!view)
        return ""
    return [view.id, view.tool, view.caller_kind, view.project || "", JSON.stringify(view.covers),
            JSON.stringify(view.cap || null), view.shown ? view.shown.digest : "unread"].join("|")
}

// `text` laid out left to right, line by line: right-to-left letters in a value can't then reorder
// what is around them across the line's start. A first strong mark, which isn't drawn.
function ltr(text) {
    return "‎" + String(text).replace(/\n/g, "\n‎")
}

// A name from outside set apart from the words around it (a first strong isolate): right-to-left
// letters in it lay out only the name, never the words or the countdown beside it.
function isolate(text) {
    return "⁨" + text + "⁩"
}

// "14:02" in local time, from an RFC 3339 time.
function clock(iso) {
    return Qt.formatTime(new Date(iso), "HH:mm")
}

// What has become of it, once it isn't pending. `fromToast`: the core took a notification's Deny
// for it.
function outcome(view, fromToast) {
    switch (view.state) {
    case "approved":
    case "denied":
        if (view.state === "denied" && fromToast)
            return "Denied from the notification"
        return (view.confirmed_via === "claude_code" ? "Answered in Claude Code" : "Answered in the app")
               + (view.answered ? " at " + clock(view.answered) : "") + ": " + view.state
    case "expired":
        return "This confirmation expired at " + clock(view.expires_at) + ", unanswered."
    case "cancelled":
        return "This confirmation was cancelled: what asked for it has gone."
    default:
        return ""
    }
}

// The time left as "12:04", or "" once it has run out.
function timeLeft(expiresAt, nowMs) {
    const left = Math.ceil((new Date(expiresAt).getTime() - nowMs) / 1000)
    if (left <= 0)
        return ""
    const seconds = left % 60
    return Math.floor(left / 60) + ":" + (seconds < 10 ? "0" : "") + seconds
}

// A refused or failed answer in plain words, never the code.
function errorWords(error, shown) {
    switch (error ? error.code : "") {
    case "confirm.expired": return "This confirmation has ended (it expired or was cancelled), so that answer didn't count."
    case "confirm.already_answered": return "This confirmation was answered somewhere else first."
    case "confirm.unknown": return "LILITH doesn't know this confirmation any more."
    default: return error && error.message ? shown(String(error.message), 300) : "That answer didn't reach the core."
    }
}
