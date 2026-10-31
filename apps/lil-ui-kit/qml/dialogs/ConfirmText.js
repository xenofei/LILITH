// The words of a confirmation (APP-046, APP-115), for the confirm dialog, the cap prompt, the
// Confirmations panel and the confirm window. Everything from outside goes through `shown`
// (Core.shownText, P4a-R8), which removes controls and marks bidi and zero-width characters: the
// arguments came from the agent.
.pragma library

// How much of one argument's value is shown. Past it the card says so: nothing is cut silently.
const VALUE_CHARS = 4000
// How many files are listed; the arguments show them all the same.
const FILES_MAX = 100

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

// "14:02" in local time, from an RFC 3339 time.
function clock(iso) {
    return Qt.formatTime(new Date(iso), "HH:mm")
}

// A value as its lines, each normalised and joined again, so a script reads as lines and its
// control characters are gone: {text, cut}.
function valueText(value, shown) {
    const raw = typeof value === "string" ? value : JSON.stringify(value, null, 2)
    let left = VALUE_CHARS
    const out = []
    let cut = false
    for (const line of String(raw).split(/\r\n|\r|\n/)) {
        if (left <= 0) {
            cut = true
            break
        }
        cut = cut || line.length > left
        out.push(shown(line, left))
        left -= line.length
    }
    return {text: out.join("\n"), cut: cut}
}

// The call's arguments, in the order they came: [{key, text, cut}].
function argumentRows(args, shown) {
    if (args === null || args === undefined)
        return []
    if (typeof args !== "object" || Array.isArray(args)) {
        const v = valueText(args, shown)
        return [{key: "", text: v.text, cut: v.cut}]
    }
    return Object.keys(args).map(key => {
        const v = valueText(args[key], shown)
        return {key: shown(key, 200), text: v.text, cut: v.cut}
    })
}

// The files, packages and scripts the arguments name (APP-115): the text values under a key that
// names one, however deep, one line each (a script's text isn't a name). The core sends no list of its
// own, so this reads the arguments, which the card shows in full as well.
function filesOf(args, shown) {
    const named = /(path|paths|file|files|folder|folders|dir|directory|package|packages|script|scripts|target|targets|source|destination|dest|output|url)$/i
    const out = []
    function visit(value, key) {
        if (out.length >= FILES_MAX)
            return
        if (typeof value === "string") {
            if (value.indexOf("\n") >= 0 || value.indexOf("\r") >= 0)
                return
            const text = shown(value, 500)
            if (key && out.indexOf(text) < 0)
                out.push(text)
        } else if (Array.isArray(value)) {
            value.forEach(v => visit(v, key))
        } else if (value && typeof value === "object") {
            Object.keys(value).forEach(k => visit(value[k], named.test(k)))
        }
    }
    visit(args, false)
    return out
}

// What has become of it, once it isn't pending. `fromToast`: this window was told a notification's
// Deny went out for it.
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
    case "confirm.expired": return "This confirmation has expired, so that answer didn't count."
    case "confirm.already_answered": return "This confirmation was answered somewhere else first."
    case "confirm.unknown": return "LILITH doesn't know this confirmation any more."
    default: return error && error.message ? shown(String(error.message), 300) : "That answer didn't reach the core."
    }
}
