// Fullscreen auto-pause (CORE-160) and keep-awake (ARCH-127): a switch each, as `settings` (the core's
// last word) has them. Keep-awake isn't shown on a PC that doesn't let the core keep it awake.
import QtQuick
import Lilith.UiKit

LCard {
    id: root

    required property var settings
    // The settings' ids with a set out (SettingsPanel).
    required property var setsOut
    required property bool live

    // The panel's `flip`.
    signal flip(string id, bool on, string label)

    title: "GPU work and this PC"

    Column {
        width: parent.width
        spacing: Theme.spacing

        SettingToggle {
            text: "Pause GPU work while a game or video is fullscreen"
            value: !!root.settings && root.settings.fullscreen_autopause === true
            busy: root.setsOut["fullscreen_autopause"] === true
            live: root.live
            onFlipped: on => root.flip("fullscreen_autopause", on, text)
        }

        SettingToggle {
            visible: !!root.settings && root.settings.keep_awake_available === true
            text: "Keep this PC awake while GPU work runs"
            value: !!root.settings && root.settings.keep_awake_gpu === true
            busy: root.setsOut["keep_awake_gpu"] === true
            live: root.live
            onFlipped: on => root.flip("keep_awake_gpu", on, text)
        }
    }
}
