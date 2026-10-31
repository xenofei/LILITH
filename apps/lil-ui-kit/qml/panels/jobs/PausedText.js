// A lane's pause in words, shared by the Jobs panel's lane headers and the top bar's badge. An
// unknown reason or lane shows by its name, as plain text cut by Core.shownText (P4a-R8, P4a-R40).
.pragma library

function shown(core, text, max) {
    return core ? core.shownText(String(text), max) : ""
}

// "by you and while fullscreen", "" with no reasons.
function reasonWords(reasons, core) {
    const parts = (reasons || []).map(r => r === "manual" ? "by you" : r === "fullscreen" ? "while fullscreen"
                                           : "by " + shown(core, r, 40))
    if (parts.length === 0)
        return ""
    const last = parts.pop()
    return (parts.length ? parts.join(", ") + " and " : "") + last
}

// "Paused by you", "Paused while fullscreen", "Paused".
function label(reasons, core) {
    const words = reasonWords(reasons, core)
    return words === "" ? "Paused" : "Paused " + words
}

function laneLabel(lane, core) {
    return lane === "gpu-heavy" ? "GPU work" : lane === "cpu" ? "CPU work" : shown(core, lane, 40)
}
