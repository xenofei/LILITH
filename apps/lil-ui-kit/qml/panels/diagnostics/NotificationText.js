// The notification path's words (APP-146, APP-022; Appendix G "Ready"), in one place: Diagnostics'
// NotificationPathCard shows them, and the Settings panel and the first-start card reuse them. Each
// problem names the part and ends with what to do. `lil doctor` writes a missing or wrong shortcut
// and a missing own scheme, and only warns about a scheme another build holds; what Windows decides
// (the toast reasons) is yours to change there, and the doctor checks the rest of the path. The
// Settings panel's "Set up" is offered only for what it can set up (`setupOffered`), and not at all
// while the app isn't where the core looks for it (`appMissing`, the bridge's), when neither can.
.pragma library

// What the card says when nothing is in the way. `not_yet_shown` counts as reachable.
const reachable = "Nothing stands in the way of LILITH's notifications."

const doctorChecks = "then run lil doctor to check the rest."
const doctorFixes = "Run lil doctor to fix, or use \"Set up\" in the Settings panel."
const appMissingFix = "It can't be set up while lil-image.exe isn't where the core looks for it: build or "
                      + "install the app, then run lil doctor."

const toastSentences = {
    disabled_for_app: "Notifications are off for LILITH in Windows Settings. Turn them on there, " + doctorChecks,
    disabled_for_user: "Notifications are off in Windows Settings. Turn them on there, " + doctorChecks,
    disabled_by_policy: "A policy blocks notifications. Change the policy on this PC, " + doctorChecks,
    elevated: "The core runs as administrator, so Windows won't show its notifications. Start LILITH "
              + "without administrator rights, " + doctorChecks,
    no_platform: "Windows isn't showing LILITH's notifications on this PC. Run lil doctor to check the "
                 + "shortcut and the link handlers."
}

// One sentence for each part of `path` ({toasts, toasts_reason?, shortcut, handlers}) that keeps a
// notification from you; empty when none does. A reason this build doesn't know is shown by its
// name, so pass the result through Core.shownText. `appMissing`: the setup is withheld, so no
// sentence names it.
function problems(path, appMissing) {
    const out = []
    if (!path)
        return out
    const fix = appMissing ? appMissingFix : doctorFixes
    if (path.toasts === "unavailable") {
        const reason = path.toasts_reason
        out.push(toastSentences[reason]
                 || "Windows won't show LILITH's notifications (" + reason + "). Run lil doctor to check the "
                    + "shortcut and the link handlers.")
    }
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
        parts.push("The Start-menu shortcut couldn't be written: " + shown(report.shortcut.why, 300) + ".")
    if (report.scheme.state === "failed")
        parts.push("This LILITH's lil: links couldn't be registered: " + shown(report.scheme.why, 300) + ".")
    if (report.reread_error)
        parts.push("The core couldn't say how things stand now: " + shown(report.reread_error, 300) + ".")
    if (parts.length === 0)
        parts.push("Nothing needed writing from here. Run lil doctor to check the rest.")
    return parts.join(" ")
}

// Whether that report has something that went wrong.
function setupFailed(report) {
    return !!report && (!!report.app_missing || !!report.reread_error || report.shortcut.state === "failed"
                        || report.scheme.state === "failed")
}

// The notification categories this build knows, in the order they show, with what each is called
// and, for some, a line under it. `director_decision` stays off the controls until the director plan.
const categories = [
    {key: "job_finished", label: "Job finished"},
    {key: "job_failed", label: "Job failed", note: "Also covers the \"Jobs held after a restart\" notification."},
    {key: "confirmation_waiting", label: "Confirmation waiting"},
    {key: "cap_alert", label: "Spending alerts"},
    {key: "director_decision", label: "Director-task decisions", held: true,
     note: "Director tasks arrive in a later version of LILITH."},
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
