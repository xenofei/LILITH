// The suite's theme (singleton): dark, built from design tokens (APP-012). A light theme comes
// later. tst_theme checks the contrast: `text` and `textMuted` at least 4.5:1 on `bg`, `surface`
// and `surfaceRaised`, `focus` at least 3:1 (WCAG 2.2).
pragma Singleton
import QtQuick

QtObject {
    // The window behind everything.
    readonly property color bg: "#16181d"
    // Panels and cards.
    readonly property color surface: "#1f2229"
    // What sits on a surface: buttons, fields, title bars, banners.
    readonly property color surfaceRaised: "#2a2e37"
    readonly property color text: "#e3e5ea"
    readonly property color textMuted: "#a3a9b4"
    readonly property color accent: "#7aa2f7"
    readonly property color danger: "#f7768e"
    readonly property color warning: "#e0af68"
    readonly property color ok: "#9ece6a"
    // The keyboard focus ring.
    readonly property color focus: "#7dcfff"

    readonly property int spacing: 8
    readonly property int radius: 4
    readonly property int fontSize: 13
    readonly property int focusWidth: 2

    // The application palette built from the tokens (LilShim.applyPalette): what Qt and
    // KDDockWidgets colour from the system palette follows the theme.
    readonly property var palette: ({
        window: bg, windowText: text, base: surface, alternateBase: surfaceRaised, text: text,
        button: surfaceRaised, buttonText: text, light: surfaceRaised, mid: surfaceRaised,
        dark: bg, highlight: accent, highlightedText: bg, placeholderText: textMuted
    })
}
