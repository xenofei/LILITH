// A button that takes only a real pointer click (P4a-R10): Approve, and a cap prompt's Allow once
// and Raise. It isn't an AbstractButton on purpose: nothing but its MouseArea can make it ask for
// an answer, and that asks the native gate (ConfirmInput, crate::pointer_gate) whether the hand made
// this press and this click. UI Automation's Invoke (`Accessible.onPressAction`) and the keyboard
// only say why they don't count. The role, name and tab stop are a button's, so a screen reader
// finds it and hears the note next to it (`refused` shows the reason there).
//
// `targetKey` is everything it stands for. When that changes it is a new button to the gate, and a
// click whose press began on the old key doesn't count, so what is sent is what it said at the press.
import QtQuick
import Lilith.Core
import Lilith.UiKit
import "../core/CoreGate.js" as CoreGate

Item {
    id: root

    // As on the base controls (core/CoreGate.js).
    property bool requiresCore: false
    property var core: null
    enabled: root.requiresCore || true

    Binding {
        target: root
        property: "enabled"
        when: root.requiresCore && !CoreGate.ready(root)
        value: false
        restoreMode: Binding.RestoreBindingOrValue
    }

    property string text: "Approve"
    property string targetKey
    // The native gate: ConfirmInput, or a test's fake with watch, unwatch, notePress and takeClick.
    property var pointerSource: ConfirmInput
    property bool primary: true
    property bool _watching: false
    property string _pressKey

    // The click is the hand's as far as Windows can tell; the caller still asks the answer gate.
    signal clicked()
    // The refusal's name (ConfirmInput.refusalText).
    signal refused(string name)

    implicitWidth: label.implicitWidth + 24
    implicitHeight: Math.max(label.implicitHeight + 12, 28)
    activeFocusOnTab: true

    Accessible.role: Accessible.Button
    Accessible.name: text
    Accessible.description: "Needs a click from your mouse or touchpad"
    Accessible.onPressAction: root.refused("not_pointer")

    Keys.onPressed: event => {
        if (event.key === Qt.Key_Space || event.key === Qt.Key_Return || event.key === Qt.Key_Enter) {
            event.accepted = true
            if (!event.isAutoRepeat)
                root.refused("key")
        }
    }

    onTargetKeyChanged: if (_watching) pointerSource.watch(root)
    Component.onCompleted: {
        pointerSource.watch(root)
        _watching = true
    }
    Component.onDestruction: pointerSource.unwatch(root)

    Rectangle {
        anchors.fill: parent
        radius: Theme.radius
        color: root.primary ? (area.pressed ? Qt.darker(Theme.accent, 1.2) : Theme.accent)
                            : (area.pressed ? Theme.bg : Theme.surfaceRaised)
        opacity: root.enabled ? 1 : 0.6
        border.color: root.activeFocus ? Theme.focus : Theme.surfaceRaised
        border.width: root.activeFocus ? Theme.focusWidth : 1
    }

    Text {
        id: label
        anchors.centerIn: parent
        text: root.text
        textFormat: Text.PlainText
        font.pixelSize: Theme.fontSize
        font.family: Theme.fontFamily
        color: !root.enabled ? Theme.textMuted : root.primary ? Theme.bg : Theme.text
    }

    MouseArea {
        id: area
        anchors.fill: parent
        acceptedButtons: Qt.LeftButton
        onPressed: {
            root._pressKey = root.targetKey
            root.pointerSource.notePress(root)
        }
        onClicked: {
            // Taken whatever comes next: a token is good for one click.
            const refusal = root.pointerSource.takeClick(root)
            if (root._pressKey !== root.targetKey)
                root.refused("changed")
            else if (refusal !== "")
                root.refused(refusal)
            else
                root.clicked()
        }
    }
}
