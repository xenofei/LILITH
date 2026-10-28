// The dark theme's tokens (APP-012) and their contrast (WCAG 2.2: 4.5:1 for text, 3:1 for the
// focus ring).
import QtQuick
import QtTest
import Lilith.Shim
import Lilith.UiKit

TestCase {
    name: "Theme"

    // WCAG 2.2 relative luminance of a QML color.
    function luminance(c) {
        const lin = v => v <= 0.04045 ? v / 12.92 : Math.pow((v + 0.055) / 1.055, 2.4)
        return 0.2126 * lin(c.r) + 0.7152 * lin(c.g) + 0.0722 * lin(c.b)
    }

    function contrast(a, b) {
        const la = luminance(a), lb = luminance(b)
        return (Math.max(la, lb) + 0.05) / (Math.min(la, lb) + 0.05)
    }

    function test_every_token_is_a_color() {
        for (const token of ["bg", "surface", "surfaceRaised", "text", "textMuted", "accent", "danger",
                             "warning", "ok", "focus"]) {
            verify(Theme[token] !== undefined, token)
            verify(Theme[token].a === 1, token + " is opaque")
        }
        verify(luminance(Theme.bg) < 0.05 && luminance(Theme.text) > 0.6, "the theme is dark")
    }

    function test_the_font_is_the_themes() { // spec: APP-012
        compare(Theme.fontFamily, "Inter")
        compare(Qt.application.font.family, Theme.fontFamily, "the application font")
        // What the font database really matches, regular and bold: the bundled faces, not a
        // fallback.
        compare(LilShim.resolvedFont(400).family, "Inter", "regular resolves to Inter")
        compare(LilShim.resolvedFont(700).family, "Inter", "bold resolves to Inter")
        // The face behind the font (QRawFont): a bold synthesised from Regular would say
        // "Regular" here.
        compare(LilShim.resolvedFont(400).style, "Regular", "regular is the bundled Regular face")
        compare(LilShim.resolvedFont(700).style, "Bold", "bold is the bundled Bold face")
        verify(Qt.colorEqual(Theme.window, Theme.bg) && Qt.colorEqual(Theme.panel, Theme.surface),
               "the deprecated aliases")
    }

    function test_text_contrast_is_at_least_4_5_to_1() { // spec: APP-012, APP-156
        for (const back of ["bg", "surface", "surfaceRaised"]) {
            for (const fore of ["text", "textMuted"]) {
                const ratio = contrast(Theme[fore], Theme[back])
                verify(ratio >= 4.5, fore + " on " + back + " is " + ratio.toFixed(2) + ":1")
            }
            const ring = contrast(Theme.focus, Theme[back])
            verify(ring >= 3, "focus on " + back + " is " + ring.toFixed(2) + ":1")
        }
    }
}
