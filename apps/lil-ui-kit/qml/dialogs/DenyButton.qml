// Deny, and a cap prompt's Cancel (P4a-R10): it takes any source, a pointer, UI Automation or a key,
// but a key only from a fresh press: a key held down since before the dialog came up repeats, and
// a repeat never answers. `activated(kind)` is `pointer`, `space` or `enter`.
import QtQuick
import QtQuick.Templates as T
import Lilith.UiKit
import "../core/CoreGate.js" as CoreGate

T.Button {
    id: control

    // As on the base controls (core/CoreGate.js).
    property bool requiresCore: false
    property var core: null
    enabled: control.requiresCore || true

    Binding {
        target: control
        property: "enabled"
        when: control.requiresCore && !CoreGate.ready(control)
        value: false
        restoreMode: Binding.RestoreBindingOrValue
    }

    signal activated(string kind)

    property bool _spaceDown: false

    implicitWidth: Math.max(implicitBackgroundWidth + leftInset + rightInset,
                            implicitContentWidth + leftPadding + rightPadding)
    implicitHeight: Math.max(implicitBackgroundHeight + topInset + bottomInset,
                             implicitContentHeight + topPadding + bottomPadding)
    padding: 6
    horizontalPadding: 12
    focusPolicy: Qt.StrongFocus
    font.pixelSize: Theme.fontSize
    font.family: Theme.fontFamily

    Accessible.name: text

    onClicked: activated("pointer")
    onActiveFocusChanged: if (!activeFocus) _spaceDown = false

    Keys.priority: Keys.BeforeItem
    Keys.onPressed: event => {
        const space = event.key === Qt.Key_Space
        if (!space && event.key !== Qt.Key_Return && event.key !== Qt.Key_Enter)
            return
        event.accepted = true
        if (event.isAutoRepeat)
            return
        if (space)
            _spaceDown = true
        else
            activated("enter")
    }
    Keys.onReleased: event => {
        if (event.key === Qt.Key_Space) {
            event.accepted = true
            if (_spaceDown && !event.isAutoRepeat) {
                _spaceDown = false
                activated("space")
            }
        }
    }

    contentItem: Text {
        text: control.text
        textFormat: Text.PlainText
        font: control.font
        color: control.enabled ? Theme.text : Theme.textMuted
        horizontalAlignment: Text.AlignHCenter
        verticalAlignment: Text.AlignVCenter
    }

    background: Rectangle {
        implicitWidth: 72
        implicitHeight: 28
        radius: Theme.radius
        color: control.down ? Theme.bg : Theme.surfaceRaised
        opacity: control.enabled ? 1 : 0.6
        border.color: control.activeFocus ? Theme.focus : Theme.surfaceRaised
        border.width: control.activeFocus ? Theme.focusWidth : 1
    }
}
