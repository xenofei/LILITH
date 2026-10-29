// The base controls work from the keyboard (APP-156): Space, Enter and Return press the buttons
// and flip the toggle, Escape closes a banner, Tab moves through them in order. And every text a
// control shows is plain text: markup in it shows as written, and an <img> in it loads nothing.
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

        // Tab order: banner, its Dismiss button, then the controls in order.
        focusOn(p.banner)
        keyClick(Qt.Key_Tab)
        keyClick(Qt.Key_Tab)
        tryVerify(() => p.button.activeFocus, 2000, "Tab reaches the button after the banner's Dismiss button")
        keyClick(Qt.Key_Tab)
        tryVerify(() => p.iconButton.activeFocus, 2000, "then the icon button")

        // Escape closes the banner, from the banner or from its Dismiss button.
        focusOn(p.banner)
        keyClick(Qt.Key_Escape)
        verify(!p.banner.visible, "Escape closes the banner")
        tryVerify(() => p.button.activeFocus, 2000, "and the focus moves on to the next control")
        compare(p.closed, 1)
        p.banner.show()
        focusOn(p.banner)
        keyClick(Qt.Key_Tab)
        tryVerify(() => p.banner.closeButton.activeFocus, 2000, "Tab goes to the banner's Dismiss button")
        keyClick(Qt.Key_Space)
        verify(!p.banner.visible, "its Dismiss button closes it")
        compare(p.closed, 2)
    }

    // Markup any panel may pass on from outside (a .lilref title, a job's prompt): a bold tag and
    // an image whose load the test runner's network access manager would count.
    readonly property string markup: '<b>x</b><img src="http://127.0.0.1:9/probe.png" width="40" height="40">'

    Component {
        id: markupPage
        Column {
            property alias card: card
            property alias banner: banner
            property alias badge: badge
            property alias button: button
            property alias iconButton: iconButton
            property alias toggle: toggle
            property alias field: field
            property alias gauge: gauge
            LCard { id: card; width: 400; height: 60; title: tc.markup }
            LBanner { id: banner; width: 400; text: tc.markup }
            LBadge { id: badge; text: tc.markup }
            LButton { id: button; text: tc.markup }
            LIconButton { id: iconButton; glyph: tc.markup; text: "Probe" }
            LToggle { id: toggle; text: tc.markup }
            LTextField { id: field; label: "Probe"; placeholderText: tc.markup }
            LGauge { id: gauge; width: 400; label: tc.markup }
        }
    }

    Text {
        id: plainTwin
        visible: false
        textFormat: Text.PlainText
    }

    function textShowing(item, want) {
        for (let i = 0; i < item.children.length; ++i) {
            const c = item.children[i]
            if (c.text === want && c.textFormat !== undefined && c.contentWidth !== undefined)
                return c
            const found = textShowing(c, want)
            if (found)
                return found
        }
        // Templates keep their contentItem outside children until it is parented.
        if (item.contentItem && item.contentItem.text === want)
            return item.contentItem
        return null
    }

    function test_markup_shows_as_written_and_loads_nothing() { // spec: APP-156, AI-222
        const before = TestFiles.networkRequests()
        const p = createTemporaryObject(markupPage, tc)
        p.banner.show(undefined, false)
        for (const name of ["card", "banner", "badge", "button", "iconButton", "toggle", "field", "gauge"]) {
            const text = textShowing(p[name], markup)
            verify(text, name + " shows the markup's text")
            compare(text.textFormat, Text.PlainText, name + " shows plain text")
            plainTwin.font = text.font
            plainTwin.text = markup
            // Rendered markup is one bold x and an image; the literal text, unless it is cut to
            // fit or wrapped, is as wide as its plain twin's.
            if (!text.truncated && text.wrapMode === Text.NoWrap)
                verify(Math.abs(text.contentWidth - plainTwin.contentWidth) < 2,
                       name + " lays out the literal text: " + text.contentWidth + " vs " + plainTwin.contentWidth)
        }
        wait(500)
        compare(TestFiles.networkRequests(), before, "no control loaded the <img>")
    }
}
