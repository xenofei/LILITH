// The status lines: the backups (APP-134, with the button that opens the Backups panel), the
// keep-awake (ARCH-127, which says so when this PC can't hold it) and this start's times (APP-136,
// `Core.startTimes`).
import QtQuick
import Lilith.Core
import Lilith.UiKit
import "DiagnosticsText.js" as Words

LCard {
    id: root

    required property var core
    // desktop.get's settings, null until they are read.
    property var settings: null
    // Whether the panel has a shell to open Backups in.
    property bool canOpenBackups: true

    signal openBackups()

    function ms(value) {
        return MoneyText.count(String(Math.round(value))) + " ms"
    }

    readonly property string keepAwakeText: {
        if (!settings)
            return "Keep-awake: " + Words.notKnown
        if (!settings.keep_awake_available)
            return "This PC doesn't let LILITH keep it awake while GPU work runs."
        return settings.keep_awake_gpu ? "Keep-awake is on: this PC doesn't sleep on its idle timer while GPU work runs."
                                       : "Keep-awake is off: this PC can sleep while GPU work runs."
    }

    readonly property string startText: {
        const times = core.startTimes || {}
        const parts = []
        if (times.firstFrameMs !== undefined)
            parts.push("first frame in " + ms(times.firstFrameMs))
        if (times.interactiveMs !== undefined)
            parts.push("interactive in " + ms(times.interactiveMs))
        return "This start: " + (parts.length ? parts.join(", ") : "not measured yet")
    }

    title: "Status"

    Column {
        width: parent.width
        spacing: Theme.spacing

        LineText { text: Words.noBackups }
        LButton {
            text: "Open Backups"
            enabled: root.canOpenBackups
            onClicked: root.openBackups()
        }
        LineText { text: root.keepAwakeText }
        LineText { text: root.startText }
    }
}
