// The notification path's words (APP-146, APP-022; Appendix G "Ready"), in one place: Diagnostics'
// NotificationPathCard shows them, and the Settings panel and the first-start card reuse them. Each
// problem names the part and ends with what to do. `lil doctor` writes a missing or wrong shortcut
// and a missing own scheme, and only warns about a scheme another build holds; what Windows decides
// (the toast reasons) is yours to change there, and the doctor checks the rest of the path. The
// Settings panel's "Set up" is offered only for what it can set up (`setupOffered`), and not at all
// while the app isn't where the core looks for it (`appMissing`, the bridge's), when neither can.
.pragma library

// What the card says when nothing is in the way, as far as the path goes: the notifier's setting, the
// shortcut and the handlers. Windows' Do Not Disturb isn't among them. `not_yet_shown` counts as reachable.
const reachable = "Nothing LILITH can check stands in the way of its notifications."

const doctorFixes = "Run lil doctor to fix, or use \"Set up\" in the Settings panel."
const appMissingFix = "It can't be set up while lil-image.exe isn't where the core looks for it: build or "
                      + "install the app, then run lil doctor."

// What Windows decides, and what to do after: `then` ends the sentence, by what the card has beside it.
function toastSentence(reason, then) {
    switch (reason) {
    case "disabled_for_app":
        return "Notifications are off for LILITH in Windows Settings. Turn them on there, " + then
    case "disabled_for_user":
        return "Notifications are off in Windows Settings. Turn them on there, " + then
    case "disabled_by_policy":
        return "A policy blocks notifications. Change the policy on this PC, " + then
    case "elevated":
        return "The core runs as administrator, so Windows won't show its notifications. Start LILITH "
               + "without administrator rights, " + then
    case "no_platform":
        return "Windows isn't showing LILITH's notifications on this PC. Run lil doctor to check the "
               + "shortcut and the link handlers."
    }
    return "Windows won't show LILITH's notifications (" + reason + "). Run lil doctor to check the "
           + "shortcut and the link handlers."
}

// One sentence for each part of `path` ({toasts, toasts_reason?, shortcut, handlers}) that keeps a
// notification from you; empty when none does. A reason this build doesn't know is shown by its
// name, so pass the result through Core.shownText. `appMissing`: the setup is withheld, so no
// sentence names it. `checkHere`: the card has a "Check again" button beside these words.
function problems(path, appMissing, checkHere) {
    const out = []
    if (!path)
        return out
    const fix = appMissing ? appMissingFix : doctorFixes
    if (path.toasts === "unavailable")
        out.push(toastSentence(path.toasts_reason,
                               checkHere ? "then press Check again." : "then run lil doctor to check the rest."))
    if (path.shortcut === "missing")
        out.push("The Start-menu shortcut is missing. " + fix)
    else if (path.shortcut === "wrong")
        out.push("The Start-menu shortcut isn't right for this LILITH: it opens another program or build, or "
                 + "its notification identity is incomplete. " + fix)
    if (path.handlers === "missing")
        out.push("This LILITH's lil: links aren't registered. " + fix)
    else if (path.handlers === "other_channel")
        out.push("Another LILITH build or another program handles lil: links. Run lil doctor to see which one.")
    return out
}

// Whether "Set up" has something to write: the own shortcut, or the own scheme. A scheme another exe
// holds has no fix in the app, and neither has anything of Windows' or `.lilref`.
function setupOffered(path) {
    return !!path && (path.shortcut === "missing" || path.shortcut === "wrong" || path.handlers === "missing")
}

// What it would write, in words.
function setupWrites(path) {
    const shortcut = !!path && (path.shortcut === "missing" || path.shortcut === "wrong")
    const scheme = !!path && path.handlers === "missing"
    return shortcut && scheme ? "a Start-menu shortcut and a link handler"
         : shortcut ? "a Start-menu shortcut" : "a link handler"
}

// Windows' own page, opened with this fixed string only, and what the button says.
const windowsSettingsUrl = "ms-settings:notifications"
const windowsSettingsLabel = "Open Windows notification settings"

// Whether that page can turn them on: not a policy's, an administrator's or a missing platform's.
function windowsCanFix(path) {
    return !!path && path.toasts === "unavailable"
           && (path.toasts_reason === "disabled_for_app" || path.toasts_reason === "disabled_for_user")
}

// A setup's report (lil_qt::setup::SetupReport) in words, each part apart; `shown(text, max)` makes
// what comes from outside safe to show.
function setupOutcome(report, shown) {
    if (!report)
        return ""
    if (report.stopped)
        return "Set up stopped with an error, so what it wrote isn't known. Run lil doctor to check."
    if (report.app_missing)
        return "Nothing was written: lil-image.exe isn't at " + shown(report.app_missing, 260)
               + ". Build or install the app, then run lil doctor."
    const parts = []
    const wroteShortcut = report.shortcut.state === "written"
    const wroteScheme = report.scheme.state === "written"
    if (wroteShortcut && wroteScheme)
        parts.push("The Start-menu shortcut and this LILITH's lil: links are set up.")
    else if (wroteShortcut)
        parts.push("The Start-menu shortcut is set up.")
    else if (wroteScheme)
        parts.push("This LILITH's lil: links are set up.")
    if (report.shortcut.state === "failed")
        parts.push(sentence("The Start-menu shortcut couldn't be written: " + shown(report.shortcut.why, 300)))
    if (report.scheme.state === "failed")
        parts.push(sentence("This LILITH's lil: links couldn't be registered: " + shown(report.scheme.why, 300)))
    if (report.reread_error)
        parts.push(sentence("The core couldn't say how things stand now: " + shown(report.reread_error, 300)))
    if (parts.length === 0)
        parts.push("Nothing needed writing from here. Run lil doctor to check the rest.")
    return parts.join(" ")
}

// `text` and a full stop, unless it ends a sentence already.
function sentence(text) {
    return /[.!?]$/.test(text) ? text : text + "."
}

// Whether that report has something that went wrong.
function setupFailed(report) {
    return !!report && (!!report.stopped || !!report.app_missing || !!report.reread_error
                        || report.shortcut.state === "failed" || report.scheme.state === "failed")
}

// `report` once the core has read `path` afresh: "couldn't say" is over, and so is the failure of a part
// the core now reads as fine, however it got fixed; its `notification_path` is that reading, the newest.
// Null when that leaves nothing to say.
function reportAfterRead(report, path) {
    if (!report || !path)
        return report
    const fine = part => part.state === "failed" ? {state: "ok"} : part
    const after = Object.assign({}, report, {reread_error: null, notification_path: path})
    if (path.shortcut === "ok")
        after.shortcut = fine(report.shortcut)
    if (path.handlers === "ok")
        after.scheme = fine(report.scheme)
    if (path.shortcut === "ok" && path.handlers === "ok")
        after.stopped = false
    const wasFailed = setupFailed(report)
    const said = after.shortcut.state === "written" || after.scheme.state === "written"
    return wasFailed && !setupFailed(after) && !said ? null : after
}

// The notification categories this build knows, in the order they show, with what each is called
// and, for some, a line under it. `director_decision` stays off the controls until the director plan.
const categories = [
    {key: "job_finished", label: "Job finished"},
    {key: "job_failed", label: "Job failed", note: "Also covers the \"Jobs held after a restart\" notification."},
    {key: "confirmation_waiting", label: "Confirmation waiting"},
    {key: "cap_alert", label: "Spending alerts"},
    {key: "director_decision", label: "Director-task decisions", held: true,
     note: "Director tasks arrive in a later version of LILITH, so this isn't used yet."},
    {key: "agent_activity", label: "Claude Code and agent activity"}
]

// The rows for `desktop.get`'s `notifications`: the categories above that it lists, then any other it
// lists, by its key (Appendix G: the set is open).
function categoryRows(notifications) {
    if (!notifications)
        return []
    const rows = categories.filter(c => c.key in notifications)
    const others = Object.keys(notifications).filter(k => !categories.some(c => c.key === k)).sort()
    return rows.concat(others.map(k => ({key: k, label: k})))
}
