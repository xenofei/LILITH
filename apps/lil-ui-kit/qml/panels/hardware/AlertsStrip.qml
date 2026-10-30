// The Hardware panel's alerts (APP-132): a banner for each kind of thermal or throttling alert of
// the `telemetry` topic (LBanner, plain text). It isn't dismissed by hand: the alert's own
// clearing ends it.
//
// The core raises and clears an alert with no hysteresis, so a GPU at its thermal limit can flip
// one every sample. So the strip keeps one banner per kind and updates it in place: a cleared
// alert stays a minute (`holdMs`), as "Cleared at HH:mm", and comes back to the same banner if it
// is raised again meanwhile; and each kind is announced at most once a minute
// (`announceEveryMs`), so a screen reader isn't told every few seconds, and the panel below
// doesn't jump. `announcements` counts what was announced (for the tests). Hidden while it holds
// no alert.
import QtQuick
import Lilith.UiKit
import "HardwareFormat.js" as Format

Column {
    id: root

    // The Core (or FakeCore): an alert kind this build doesn't know is shown through shownText.
    property var core: null
    property int holdMs: 60000
    property int announceEveryMs: 60000
    property int announcements: 0
    readonly property int count: alertModel.count

    // Unix ms each kind was last announced at.
    property var _announced: ({})

    // The kinds raised now.
    function activeKinds() {
        const out = []
        for (let i = 0; i < alertModel.count; ++i) {
            if (alertModel.get(i).active)
                out.push(alertModel.get(i).alertKind)
        }
        return out
    }

    // A sample's `alerts` (the state at `sampleTs`, an RFC 3339 time): the kinds not in it are
    // cleared, unless raised after the sample (an alert event can overtake an older sample).
    function setAll(alerts, sampleTs) {
        const kinds = alerts.map(a => String(a.kind))
        const at = Date.parse(sampleTs || "")
        for (let i = 0; i < alertModel.count; ++i) {
            const row = alertModel.get(i)
            if (!row.active || kinds.indexOf(row.alertKind) >= 0)
                continue
            if (!isNaN(at) && Date.parse(row.since) > at)
                continue
            clearAt(i)
        }
        for (const a of alerts)
            upsert(a)
    }

    // A raised alert (the topic's `alert` event, or one in a sample).
    function upsert(alert) {
        const kind = String(alert.kind)
        const fields = {alertId: String(alert.id), value: Number(alert.value), since: String(alert.since || ""),
                        active: true, clearedMs: 0}
        for (let i = 0; i < alertModel.count; ++i) {
            if (alertModel.get(i).alertKind !== kind)
                continue
            const was = alertModel.get(i).active
            alertModel.set(i, fields)
            if (!was)
                announce(i)
            return
        }
        alertModel.append(Object.assign({alertKind: kind}, fields))
        announce(alertModel.count - 1)
    }

    // A cleared alert (`alert_cleared`, by id).
    function clear(id) {
        for (let i = 0; i < alertModel.count; ++i) {
            if (alertModel.get(i).alertId === id && alertModel.get(i).active)
                clearAt(i)
        }
    }

    function clearAt(i) {
        alertModel.setProperty(i, "active", false)
        alertModel.setProperty(i, "clearedMs", Date.now())
        holdCheck.start()
    }

    function announce(i) {
        const kind = alertModel.get(i).alertKind
        const now = Date.now()
        if (_announced[kind] !== undefined && now - _announced[kind] < announceEveryMs)
            return
        _announced[kind] = now
        const banner = banners.itemAt(i)
        if (banner) {
            banner.announce()
            announcements = announcements + 1
        }
    }

    function textOf(kind, value, sinceUtc, active, clearedMs) {
        const since = Format.localTime(sinceUtc)
        const at = since ? " (since " + since + ")" : ""
        let text
        switch (kind) {
        case "temperature":
            text = "The GPU is hot: " + value + " °C" + at + "."
            break
        case "throttle_thermal":
            text = "The GPU is slowing down to cool off" + at + "."
            break
        case "throttle_power_brake":
            text = "The GPU's power brake is on: it is running slowly" + at + "."
            break
        default:
            text = "Hardware alert: " + (core ? core.shownText(kind, 40) : "") + at + "."
        }
        return active ? text : "Cleared at " + Qt.formatTime(new Date(clearedMs), "HH:mm") + ": " + text
    }

    visible: alertModel.count > 0
    spacing: 4

    // Removes the cleared alerts held long enough.
    Timer {
        id: holdCheck
        interval: Math.max(50, Math.min(1000, root.holdMs / 4))
        repeat: true
        onTriggered: {
            let held = false
            for (let i = alertModel.count - 1; i >= 0; --i) {
                const row = alertModel.get(i)
                if (row.active)
                    continue
                if (Date.now() - row.clearedMs >= root.holdMs)
                    alertModel.remove(i)
                else
                    held = true
            }
            if (!held)
                stop()
        }
    }

    ListModel { id: alertModel }

    Repeater {
        id: banners
        model: alertModel

        delegate: LBanner {
            required property string alertKind
            required property real value
            required property string since
            required property bool active
            required property real clearedMs
            width: root.width
            closable: false
            kind: !active ? "ok" : alertKind === "temperature" ? "danger" : "warning"
            text: root.textOf(alertKind, value, since, active, clearedMs)
            // Shown without its own announcement: the strip announces (see the header).
            Component.onCompleted: show(undefined, false)
        }
    }
}
