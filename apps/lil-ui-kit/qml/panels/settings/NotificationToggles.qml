// The notification categories and "Quiet while fullscreen" (APP-146; owner decisions 2026-10-22 and
// 2026-10-24): one switch each, as `settings` (the core's last word) has them.
pragma ComponentBehavior: Bound
import QtQuick
import Lilith.UiKit
import "../diagnostics/DiagnosticsText.js" as Words
import "../diagnostics/NotificationText.js" as NotificationText

LCard {
    id: root

    required property var core
    required property var settings
    // The settings' ids with a set out (SettingsPanel).
    required property var setsOut
    required property bool live

    // The panel's `flip`.
    signal flip(string id, bool on, string label)

    // Kept while the same categories come back, so the switches, and the keyboard's focus on one,
    // outlive every answer.
    property var rows: []
    readonly property string _rowKeys: settings ? Object.keys(settings.notifications || {}).sort().join("\n") : ""
    on_RowKeysChanged: rows = settings ? NotificationText.categoryRows(settings.notifications) : []
    Component.onCompleted: rows = settings ? NotificationText.categoryRows(settings.notifications) : []

    title: "Notifications"

    Column {
        width: parent.width
        spacing: Theme.spacing

        Repeater {
            model: root.rows
            delegate: SettingToggle {
                required property var modelData
                readonly property string settingId: "notifications." + modelData.key
                // A key this build doesn't know comes from the core: shown as text.
                text: modelData.label === modelData.key ? Words.shown(root.core, modelData.key, 80) : modelData.label
                note: modelData.note || ""
                value: !!root.settings && !!root.settings.notifications
                       && root.settings.notifications[modelData.key] === true
                busy: root.setsOut[settingId] === true
                live: root.live
                held: !!modelData.held
                onFlipped: on => root.flip(settingId, on, modelData.label)
            }
        }

        SettingToggle {
            text: "Quiet while fullscreen"
            note: "Notifications go to the notification centre without popping up."
            value: !!root.settings && root.settings.quiet_while_fullscreen === true
            busy: root.setsOut["quiet_while_fullscreen"] === true
            live: root.live
            onFlipped: on => root.flip("quiet_while_fullscreen", on, text)
        }
    }
}
