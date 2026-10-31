// The notification path's words (APP-146, APP-022; Appendix G "Ready"), in one place: Diagnostics'
// NotificationPathCard shows them and the Settings panel (Task 27) reuses them. Each problem names
// the part and ends with what to do. `lil doctor` writes a missing or wrong shortcut and a missing
// own scheme, and only warns about a scheme another build holds; what Windows decides (the toast
// reasons) is yours to change there, and the doctor checks the rest of the path. The Settings
// panel's "Set up" is offered only for what it can set up.
.pragma library

// What the card says when nothing is in the way. `not_yet_shown` counts as reachable.
const reachable = "Nothing stands in the way of LILITH's notifications."

const doctorChecks = "then run lil doctor to check the rest."
const doctorFixes = "Run lil doctor to fix, or use \"Set up\" in the Settings panel."

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
// name, so pass the result through Core.shownText.
function problems(path) {
    const out = []
    if (!path)
        return out
    if (path.toasts === "unavailable") {
        const reason = path.toasts_reason
        out.push(toastSentences[reason]
                 || "Windows won't show LILITH's notifications (" + reason + "). Run lil doctor to check the "
                    + "shortcut and the link handlers.")
    }
    if (path.shortcut === "missing")
        out.push("The Start-menu shortcut is missing. " + doctorFixes)
    else if (path.shortcut === "wrong")
        out.push("The Start-menu shortcut opens another program or build. " + doctorFixes)
    if (path.handlers === "missing")
        out.push("This LILITH's lil: links aren't registered. " + doctorFixes)
    else if (path.handlers === "other_channel")
        out.push("Another LILITH build handles lil: links. Run lil doctor to see which one.")
    return out
}
