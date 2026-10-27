// The panels of the image app (ARCH-010, APP-008), in F6 order. Each entry:
//   id      the dock's unique name in the layout file (never renamed: a saved layout names it)
//   title   the title bar's and tab's text, and the panel's accessible name
//   source  the panel's QML file, relative to this file: a placeholder until its task replaces
//           it (Tasks 21 to 27)
//   area    where the Default layout docks it: "centre", "right" or "bottom" (tabbed with the
//           others there, the first one in front), or "" for a panel the Default layout leaves
//           closed
//   action  its shortcut's action in the shortcut map (lil_qt::shortcuts), or "" for none
// Shared: later tasks replace their own entry's `source` and may add entries; never reorder or
// drop one.
.pragma library

const all = [
    {id: "home", title: "Home", source: "PlaceholderPanel.qml", area: "centre", action: "panel.home"},
    {id: "jobs", title: "Jobs", source: "PlaceholderPanel.qml", area: "right", action: "panel.jobs"},
    {id: "confirmations", title: "Confirmations", source: "PlaceholderPanel.qml", area: "right",
     action: "panel.confirmations"},
    {id: "hardware", title: "Hardware", source: "PlaceholderPanel.qml", area: "bottom",
     action: "panel.hardware"},
    {id: "usage", title: "Usage & spend", source: "PlaceholderPanel.qml", area: "bottom",
     action: "panel.usage"},
    {id: "diagnostics", title: "Diagnostics", source: "PlaceholderPanel.qml", area: "bottom",
     action: "panel.diagnostics"},
    {id: "settings", title: "Settings", source: "PlaceholderPanel.qml", area: "bottom",
     action: "panel.settings"},
    // Opened from Diagnostics (APP-008, APP-133); Task 25 decides where it opens.
    {id: "backups", title: "Backups", source: "PlaceholderPanel.qml", area: "", action: ""},
]

function byId(id) {
    return all.find(p => p.id === id) || null
}

function byAction(action) {
    return all.find(p => p.action !== "" && p.action === action) || null
}

function url(panel) {
    return Qt.resolvedUrl(panel.source)
}
