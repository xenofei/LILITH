// The base controls work from the keyboard (APP-156): Space, Enter and Return press the buttons
// and flip the toggle, Escape closes a banner, Tab moves through them in order.
import QtQuick
import QtTest
import Lilith.UiKit

TestCase {
    id: tc
    name: "Controls"
    when: windowShown
    width: 640
    height: 400

    // A visible window of its own, which takes the keyboard.
    Component {
        id: page
        Window {
            id: win
            width: 480
            height: 400
            visible: true
            property alias button: button
            property alias iconButton: iconButton
            property alias toggle: toggle
            property alias field: field
            property alias banner: banner
            property int pressed: 0
            property int iconPressed: 0
            property int closed: 0

            Column {
                spacing: 4
                LBanner { id: banner; width: 400; text: "A banner"; onClosed: win.closed++ }
                LButton { id: button; text: "Run"; onClicked: win.pressed++ }
                LIconButton { id: iconButton; glyph: "×"; text: "Remove"; onClicked: win.iconPressed++ }
                LToggle { id: toggle; text: "Keep awake" }
                LTextField { id: field; label: "Name" }
            }
        }
    }

    function focusOn(item) {
        item.forceActiveFocus(Qt.TabFocusReason)
        tryVerify(() => item.activeFocus, 2000, "the focus is on " + item)
    }

    function test_keyboard_operates_every_control() { // spec: APP-156
        const p = createTemporaryObject(page, tc)
        p.requestActivate()
        tryVerify(() => p.active, 5000, "the page window is active")
        p.banner.show()

        for (const key of [Qt.Key_Space, Qt.Key_Return, Qt.Key_Enter]) {
            focusOn(p.button)
            const before = p.pressed
            keyClick(key)
            compare(p.pressed, before + 1, "the button presses with key " + key)

            focusOn(p.iconButton)
            const iconBefore = p.iconPressed
            keyClick(key)
            compare(p.iconPressed, iconBefore + 1, "the icon button presses with key " + key)

            focusOn(p.toggle)
            const was = p.toggle.checked
            keyClick(key)
            compare(p.toggle.checked, !was, "the toggle flips with key " + key)
        }

        focusOn(p.field)
        keyClick(Qt.Key_A)
        keyClick(Qt.Key_B)
        compare(p.field.text, "ab", "the field takes typing")

        // Tab order: banner, its Close button, then the controls in order.
        focusOn(p.banner)
        keyClick(Qt.Key_Tab)
        keyClick(Qt.Key_Tab)
        tryVerify(() => p.button.activeFocus, 2000, "Tab reaches the button after the banner's Close button")
        keyClick(Qt.Key_Tab)
        tryVerify(() => p.iconButton.activeFocus, 2000, "then the icon button")

        // Escape closes the banner, from the banner or from its Close button.
        focusOn(p.banner)
        keyClick(Qt.Key_Escape)
        verify(!p.banner.visible, "Escape closes the banner")
        compare(p.closed, 1)
        p.banner.show()
        focusOn(p.banner)
        keyClick(Qt.Key_Tab)
        tryVerify(() => p.banner.closeButton.activeFocus, 2000, "Tab goes to the banner's Close button")
        keyClick(Qt.Key_Space)
        verify(!p.banner.visible, "its Close button closes it")
        compare(p.closed, 2)
    }
}
