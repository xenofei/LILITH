// The suite's theme (singleton). Task 18 fills it out; these are the values the shell uses now.
pragma Singleton
import QtQuick

QtObject {
    readonly property color window: "#1f2229"
    readonly property color panel: "#2a2e37"
    readonly property color text: "#e3e5ea"
    readonly property color textMuted: "#9aa0ab"
    readonly property color accent: "#7aa2f7"
    readonly property int spacing: 8
    readonly property int radius: 4
    readonly property int fontSize: 13
}
