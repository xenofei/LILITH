// The cap gauges (APP-121): `caps.get` as one gauge per cap, settled plus held money against its
// limit in the display currency, as caps.get gives it. A cap the core can't measure (its value
// in suite.toml doesn't read, or an amount under it has no dated rate) shows as such, never as a
// ratio (it counts as exceeded). The 50, 80 and 100 % alerts of the `caps` topic (CORE-089) are
// one line each, newest first, never stacked; a repeat of the same cap, level and month adds
// none, and a read of a new month drops older months'. The panel decides when to read() again:
// a read while one is out waits for it, then reads once more.
//
// Every Text of its own is plain text, and the cap names, currencies, alert and error text go
// through `core.shownText` (P4a-R8). Amounts show through MoneyText (owner decision 2026-10-29).
pragma ComponentBehavior: Bound
import QtQuick
import Lilith.Core
import Lilith.UiKit
import "UsageText.js" as UsageText

LCard {
    id: root

    required property var core
    // The last CapsView read, null until the first.
    property var view: null
    property var error: null
    // [{key, period, text}], newest first.
    property var alerts: []
    readonly property string currency: view ? view.display_currency : ""
    property var _call: null
    property bool _due: false

    title: "Spending caps" + (view ? " · " + safe(view.period, 16) : "")

    function money(amount, currency) {
        return MoneyText.text(String(amount), String(currency))
    }

    function safe(s, n) {
        return UsageText.shown(core, s, n)
    }

    // Reads caps.get again (see the header for a read still out).
    function read() {
        if (_call) {
            _due = true
            return
        }
        _call = CoreCall.call(core, "caps.get", {}, {
            owner: root,
            ok: r => {
                root._call = null
                root.view = r.structured
                root.error = null
                const period = root.view ? root.view.period : ""
                // Only older months' alerts go ("YYYY-MM" compares in order): a late read of
                // last month never drops this month's alert, which the core sends once.
                if (period !== "" && root.alerts.some(a => a.period < period))
                    root.alerts = root.alerts.filter(a => a.period >= period)
                root.again()
            },
            error: e => {
                root._call = null
                root.error = e
                root.again()
            }
        })
    }

    function again() {
        if (_due) {
            _due = false
            read()
        }
    }

    // An `alert` event's data.
    function addAlert(a) {
        const key = [a.cap, a.threshold_pct, a.period].join("|")
        if (alerts.some(x => x.key === key))
            return
        const text = safe(UsageText.alertText(a, money), 300)
        alerts = [{key: key, period: a.period, text: text}].concat(alerts)
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
            textFormat: Text.PlainText
            color: Theme.textMuted
            font.pixelSize: Theme.fontSize
            wrapMode: Text.Wrap
            Accessible.role: Accessible.StaticText
            Accessible.name: text
        }

        Text {
            width: parent.width
            visible: root.view === null
            text: root.error ? "Couldn't read the caps" + (root.error.message
                               ? ": " + root.safe(root.error.message, 300) : ".")
                             : "Reading the caps…"
            textFormat: Text.PlainText
            color: root.error ? Theme.danger : Theme.textMuted
            font.pixelSize: Theme.fontSize
            wrapMode: Text.Wrap
            Accessible.role: Accessible.StaticText
            Accessible.name: text
        }

        Repeater {
            model: root.view ? root.view.caps : []
            // A gauge, or for a cap that can't be measured a line saying so (never a gauge).
            delegate: Loader {
                id: capLoader

                required property var modelData
                readonly property string name: root.safe(UsageText.capName(modelData.cap))

                width: body.width
                sourceComponent: modelData.invalid ? invalidCap : gaugeCap

                Component {
                    id: gaugeCap
                    LGauge {
                        objectName: "cap_" + capLoader.modelData.cap
                        width: capLoader.width
                        label: root.safe(capLoader.name + ": "
                                         + root.money(capLoader.modelData.settled, root.currency) + " settled + "
                                         + root.money(capLoader.modelData.held, root.currency) + " held of "
                                         + root.money(capLoader.modelData.limit, root.currency), 200)
                        // Floored to the whole percent, as the alerts are.
                        value: Math.floor(capLoader.modelData.ratio_permille / 10) / 100
                    }
                }

                Component {
                    id: invalidCap
                    Text {
                        objectName: "cap_" + capLoader.modelData.cap + "_invalid"
                        width: capLoader.width
                        text: capLoader.name + ": can't be measured, so it counts as exceeded. Its value in "
                              + "suite.toml doesn't read, or an amount under it is in a currency with no dated rate."
                        textFormat: Text.PlainText
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
                    textFormat: Text.PlainText
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
