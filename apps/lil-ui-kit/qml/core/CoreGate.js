// What the base controls' `requiresCore` asks (APP-034): the rule in one place.
//
//   LButton { requiresCore: true; text: "Pause"; enabled: panel.loaded; onClicked: ... }
//
// A control marked `requiresCore: true` is disabled unless its core's `connection` is `ready`,
// whatever its own `enabled` says, and goes back to that when the core is ready. It is the same on
// LButton, LIconButton, LToggle and LTextField. Mark every action that needs the core (AI and
// workspace actions), so that a later panel gets this by marking, never by its own binding.
//
// Its core is its own `core` property, else the nearest ancestor's `core`, so a panel's controls
// find the panel's, floating or not, and the tests' FakeCore works the same. A control in a Popup
// has no such ancestor (a popup's items are parented to the window's overlay), and sets `core`
// itself. With no core to ask it stays disabled.
.pragma library

function ready(control) {
    let core = control.core
    for (let p = control.parent; !core && p; p = p.parent)
        core = p.core
    return !!core && core.connection === "ready"
}

// The core refused the hello over its workspace (`lil workspace use` elsewhere, a renamed folder):
// the session waits, and "Try again" would repeat the refusal.
function refusedWorkspace(core) {
    const u = core && core.connection === "unavailable" ? core.unavailable : null
    return !!u && u.cause === "refused" && String(u.code).startsWith("workspace.")
}
