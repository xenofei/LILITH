// The words and small formats the Diagnostics cards and the Backups panel share.
.pragma library

// What every card says of a version or a figure it can't read: never an empty line.
const notKnown = "not known"

// The backup status line (APP-134) and the Backups panel's empty state (APP-133).
const noBackups = "No backups yet: backups arrive in a later version of LILITH"

// Untrusted text as shown (Core.shownText, P4a-R8); nothing once the core has gone.
function shown(core, text, maxChars) {
    if (!core || text === null || text === undefined)
        return ""
    return core.shownText(String(text), maxChars || 200)
}

// An RFC 3339 time in this PC's time; a time that doesn't read is shown as it came.
function when(core, iso) {
    const date = new Date(iso)
    return isNaN(date.getTime()) ? shown(core, iso, 40) : Qt.formatDateTime(date, "yyyy-MM-dd HH:mm:ss")
}

// An ErrorBody in words: its message, its hint, and its fix's label unless the panel offers that
// fix as a button (`show_job`). An action this build doesn't know is shown by its label and does
// nothing else.
function errorText(core, error) {
    if (!error)
        return ""
    const parts = [shown(core, error.message, 400)]
    const fix = error.fix || null
    if (fix && fix.user_hint)
        parts.push(shown(core, fix.user_hint, 300))
    if (fix && fix.ui_action && fix.ui_action.id !== "show_job")
        parts.push(shown(core, fix.ui_action.label, 80))
    return parts.filter(p => p !== "").join(" ")
}

// The job an ErrorBody's `show_job` fix opens, or "" when it has none or names no job.
function fixJob(error) {
    const action = error && error.fix ? error.fix.ui_action : null
    const job = error && error.data ? error.data.job_id : null
    return action && action.id === "show_job" && typeof job === "string" ? job : ""
}
