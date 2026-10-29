// The cap gauges (APP-121): `caps.get` as one gauge per cap, settled plus held money against its
// limit in the display currency, as caps.get gives it. A cap whose value in suite.toml doesn't
// parse shows as invalid, never as a ratio (it counts as exceeded). The 50, 80 and 100 % alerts
// of the `caps` topic (CORE-089) are one line each, newest first, never stacked; a repeat of
// the same cap, level and month adds none. The panel decides when to read() again.
pragma ComponentBehavior: Bound
import QtQuick
import Lilith.UiKit
import "UsageText.js" as UsageText

LCard {
    id: root

    required property var core
    // The last CapsView read, null until the first.
    property var view: null
    property var error: null
    // [{key, text}], newest first.
    property var alerts: []
    readonly property string currency: view ? view.display_currency : ""
    property var _call: null

    title: "Spending caps" + (view ? " · " + view.period : "")

    // Reads caps.get again; a read still open is dropped.
    function read() {
        if (_call)
            _call.cancel()
        _call = CoreCall.call(core, "caps.get", {}, {
            owner: root,
            ok: r => {
                root._call = null
                root.view = r.structured
                root.error = null
            },
            error: e => {
                root._call = null
                root.error = e
            }
        })
    }

    // An `alert` event's data.
    function addAlert(a) {
        const key = [a.cap, a.threshold_pct, a.period].join("|")
        if (alerts.some(x => x.key === key))
            return
        const text = UsageText.alertText(a)
        alerts = [{key: key, text: text}].concat(alerts)
        Accessible.announce(text)
    }

    Column {
        id: body
        width: parent.width
        spacing: Theme.spacing

        Text {
            width: parent.width
            visible: root.view !== null && root.view.caps.length === 0
            text: "No spending caps are set. Caps are optional, and only you set them."
            color: Theme.textMuted
            font.pixelSize: Theme.fontSize
            wrapMode: Text.Wrap
            Accessible.role: Accessible.StaticText
            Accessible.name: text
        }

        Text {
            width: parent.width
            visible: root.view === null
            text: root.error ? "Couldn't read the caps" + (root.error.message ? ": " + root.error.message : ".")
                             : "Reading the caps…"
            color: root.error ? Theme.danger : Theme.textMuted
            font.pixelSize: Theme.fontSize
            wrapMode: Text.Wrap
            Accessible.role: Accessible.StaticText
            Accessible.name: text
        }

        Repeater {
            model: root.view ? root.view.caps : []
            // A gauge, or for an invalid cap a line saying so (never a gauge).
            delegate: Loader {
                id: capLoader

                required property var modelData

                width: body.width
                sourceComponent: modelData.invalid ? invalidCap : gaugeCap

                Component {
                    id: gaugeCap
                    LGauge {
                        objectName: "cap_" + capLoader.modelData.cap
                        width: capLoader.width
                        label: UsageText.capName(capLoader.modelData.cap) + ": " + capLoader.modelData.settled
                               + " settled + " + capLoader.modelData.held + " held of " + capLoader.modelData.limit
                               + " " + root.currency
                        value: capLoader.modelData.ratio_permille / 1000
                    }
                }

                Component {
                    id: invalidCap
                    Text {
                        objectName: "cap_" + capLoader.modelData.cap + "_invalid"
                        width: capLoader.width
                        text: UsageText.capName(capLoader.modelData.cap) + ": invalid in suite.toml, so it counts as exceeded"
                        color: Theme.danger
                        font.pixelSize: Theme.fontSize
                        wrapMode: Text.Wrap
                        Accessible.role: Accessible.StaticText
                        Accessible.name: text
                    }
                }
            }
        }

        Column {
            objectName: "alerts"
            width: parent.width
            spacing: 2

            Repeater {
                model: root.alerts
                delegate: Text {
                    required property var modelData
                    objectName: "alert"
                    width: parent.width
                    text: modelData.text
                    color: Theme.warning
                    font.pixelSize: Theme.fontSize
                    elide: Text.ElideRight
                    Accessible.role: Accessible.StaticText
                    Accessible.name: text
                }
            }
        }
    }
}
