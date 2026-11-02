// One setting's switch (APP-146): it shows `value`, the core's answer, and never its own guess. A
// flip goes up as `flipped(on)` and the switch goes straight back to `value`, which moves when the
// core says so. While `busy` (a set of this one is out) it says so and a flip does nothing, and so
// does a flip within the double-click interval of the last one sent, since the core's answer can come
// before a double-click's second click: a double-click sends one set either way. `live` false holds it:
// the core is away, or the value may be old.
import QtQuick
import Lilith.UiKit

Column {
    id: root

    property alias text: toggle.text
    property bool value
    property bool busy: false
    property bool live: true
    // A line under it, also its accessible description; "" for none.
    property string note

    readonly property alias toggle: toggle

    signal flipped(bool on)

    property real _lastSent: 0

    width: parent ? parent.width : implicitWidth
    spacing: 2

    Row {
        spacing: Theme.spacing

        LToggle {
            id: toggle
            requiresCore: true
            enabled: root.live
            checked: root.value
            Accessible.description: root.note
            onToggled: {
                const on = checked
                // A press leaves the binding in place but the switch moved: back to the core's word.
                checked = Qt.binding(() => root.value)
                const now = Date.now()
                if (root.busy || now - root._lastSent < Qt.styleHints.mouseDoubleClickInterval)
                    return
                root._lastSent = now
                root.flipped(on)
            }
        }

        LineText {
            visible: root.busy
            width: implicitWidth
            anchors.verticalCenter: parent.verticalCenter
            tone: "muted"
            text: "Saving…"
        }
    }

    LineText {
        visible: root.note !== ""
        tone: "muted"
        leftPadding: toggle.indicator.width + 2 * Theme.spacing
        text: root.note
    }
}
