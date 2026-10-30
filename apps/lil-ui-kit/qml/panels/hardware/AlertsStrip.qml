// The Hardware panel's alerts (APP-132): a banner for each active thermal or throttling alert of
// the `telemetry` topic, announced once, when it appears (LBanner), gone when it clears. It isn't
// dismissed by hand: the alert's own clearing ends it. Hidden while no alert is active.
//
// The alerts are kept by id, so the snapshot a sample carries every second updates a banner in
// place and never makes (and announces) it again.
import QtQuick
import Lilith.UiKit
import "HardwareFormat.js" as Format

Column {
    id: root

    readonly property int count: alertModel.count

    // The active alerts, [{id, kind, value, since}]: the snapshot's `alerts`.
    function setAll(alerts) {
        const ids = alerts.map(a => a.id)
        for (let i = alertModel.count - 1; i >= 0; --i) {
            if (ids.indexOf(alertModel.get(i).alertId) < 0)
                alertModel.remove(i)
        }
        for (const a of alerts)
            upsert(a)
    }

    // A raised alert (the topic's `alert` event).
    function upsert(alert) {
        const row = {alertId: alert.id, alertKind: alert.kind, value: alert.value, since: alert.since || ""}
        for (let i = 0; i < alertModel.count; ++i) {
            if (alertModel.get(i).alertId === alert.id) {
                alertModel.set(i, row)
                return
            }
        }
        alertModel.append(row)
    }

    // A cleared alert (`alert_cleared`).
    function clear(id) {
        for (let i = alertModel.count - 1; i >= 0; --i) {
            if (alertModel.get(i).alertId === id)
                alertModel.remove(i)
        }
    }

    function textOf(kind, value, sinceUtc) {
        const since = Format.localTime(sinceUtc)
        const at = since ? " (since " + since + ")" : ""
        switch (kind) {
        case "temperature":
            return "The GPU is hot: " + value + " °C" + at + "."
        case "throttle_thermal":
            return "The GPU is slowing down to cool off" + at + "."
        case "throttle_power_brake":
            return "The GPU's power brake is on: it is running slowly" + at + "."
        default:
            return "Hardware alert: " + kind + at + "."
        }
    }

    visible: alertModel.count > 0
    spacing: 4

    ListModel { id: alertModel }

    Repeater {
        model: alertModel

        delegate: LBanner {
            required property string alertKind
            required property var value
            required property string since
            width: root.width
            closable: false
            kind: alertKind === "temperature" ? "danger" : "warning"
            text: root.textOf(alertKind, value, since)
            Component.onCompleted: show()
        }
    }
}
