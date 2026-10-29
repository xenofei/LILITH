// One tab of the Usage & spend panel (APP-116 to APP-120): one `usage.summary` of `period`
// ("month" or "day") grouped by `dimension`, read again by reload(). Its sections keep the
// billing bases apart (APP-118): the totals, money, subscription quota (its own units), local
// work, spend outside LILITH (unattributed, APP-116) and, for a month, the fixed costs (APP-119).
// Every figure is the core's own (decimal strings with their currency codes); nothing is added
// up here. Each row shows the record units it carries (tokens, images, seconds: APP-117). Amounts
// show through MoneyText ("1,234.57 USD", owner decision 2026-10-29).
//
// Every Text is plain text, and every string from the usage logs or suite.toml (names,
// currencies, error text) is shown through `core.shownText` (P4a-R8): markup shows as written
// and loads nothing.
pragma ComponentBehavior: Bound
import QtQuick
import Lilith.Core
import Lilith.UiKit
import "UsageText.js" as UsageText

Column {
    id: root

    required property var core
    required property string period
    // The `group_by` field asked for; the rows shown are by `shownDimension`, the one the
    // summary in hand was read with.
    property string dimension: "provider"
    property string shownDimension: dimension
    // The last summary read, null until the first.
    property var summary: null
    property bool loaded: false
    // The local month ("YYYY-MM", CORE-012) the summary in hand was read in: its fixed costs'
    // month in an export when it has no rows.
    property string readMonth
    // The last read's error body, null after a read that worked.
    property var error: null
    property var _call: null
    // The dimension the open read asked for, and whether another read waits for it.
    property string _asked
    property bool _due: false

    readonly property string periodWord: period === "month" ? "this month" : "today"
    readonly property var bases: summary && summary.bases ? summary.bases : ({})

    spacing: Theme.spacing

    Accessible.role: Accessible.Grouping
    Accessible.name: period === "month" ? "Spend this month" : "Spend today"

    // The rows of `basis` as shown (UsageText.shownRows).
    function shown(basis) {
        return UsageText.shownRows(summary, basis, shownDimension, count)
    }

    // `amount` in `currency`, as the apps show money (MoneyText, owner decision 2026-10-29).
    function money(amount, currency) {
        return MoneyText.text(String(amount), String(currency))
    }

    // A count with thousands separators ("412,000").
    function count(value) {
        return MoneyText.count(String(value))
    }

    // `s` as shown (core.shownText): at most `n` characters, 80 by default.
    function safe(s, n) {
        return UsageText.shown(core, s, n)
    }

    // Reads the summary again. A read still open for the same dimension isn't dropped: one more
    // read follows it once it settles. One for another dimension is dropped.
    function reload() {
        if (_call && _asked === dimension) {
            _due = true
            return
        }
        if (_call)
            _call.cancel()
        _due = false
        const asked = dimension
        const month = UsageText.localMonth()
        _asked = asked
        _call = CoreCall.call(core, "usage.summary", {period: period, group_by: [asked]}, {
            owner: root,
            ok: r => {
                root._call = null
                root.shownDimension = asked
                root.readMonth = month
                root.summary = r.structured
                root.error = null
                root.loaded = true
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
            reload()
        }
    }

    component Line: Text {
        width: parent ? parent.width : 0
        color: Theme.text
        font.pixelSize: Theme.fontSize
        textFormat: Text.PlainText
        wrapMode: Text.Wrap
        Accessible.role: Accessible.StaticText
        Accessible.name: text
    }

    component Note: Line {
        color: Theme.textMuted
        font.pixelSize: Theme.fontSize - 1
    }

    component Heading: Line {
        font.pixelSize: Theme.fontSize + 1
        font.bold: true
    }

    // One row of a section: its label, the raw value beside it when it reads differently (muted),
    // what it counts, and its marks.
    component UsageRow: Item {
        id: line

        required property var modelData
        property string figures
        readonly property string label: root.safe(modelData.label)
        readonly property string detail: modelData.detail !== "" ? root.safe(modelData.detail) : ""
        readonly property string shownFigures: root.safe(figures, 300)

        width: parent ? parent.width : 0
        height: Math.max(labelText.implicitHeight, figuresText.implicitHeight) + 4

        Accessible.role: Accessible.StaticText
        Accessible.name: [label, detail, shownFigures].concat(modelData.marks).filter(s => s !== "").join(", ")

        Text {
            id: labelText
            width: Math.min(implicitWidth, line.width * 0.34 - (detailText.visible ? detailText.width + 4 : 0))
            text: line.label
            textFormat: Text.PlainText
            color: Theme.text
            font.pixelSize: Theme.fontSize
            elide: Text.ElideRight
        }

        Text {
            id: detailText
            visible: line.detail !== ""
            x: labelText.width + 4
            width: Math.min(implicitWidth, line.width * 0.17)
            text: line.detail
            textFormat: Text.PlainText
            color: Theme.textMuted
            font.pixelSize: Theme.fontSize
            elide: Text.ElideRight
        }

        Text {
            id: figuresText
            x: line.width * 0.34 + Theme.spacing
            width: line.width - x - marksRow.width - Theme.spacing
            text: line.shownFigures
            textFormat: Text.PlainText
            color: Theme.text
            font.pixelSize: Theme.fontSize
            wrapMode: Text.Wrap
        }

        Row {
            id: marksRow
            anchors.right: parent.right
            spacing: 4

            Repeater {
                model: line.modelData.marks
                delegate: LBadge {
                    required property string modelData
                    text: modelData
                    kind: modelData.indexOf("estimated") >= 0 ? "warning" : "ok"
                }
            }
        }
    }

    Line {
        visible: !root.loaded && !root.error
        text: "Reading the usage…"
        color: Theme.textMuted
    }

    Line {
        visible: !!root.error
        text: "Couldn't read the usage" + (root.error && root.error.message
              ? ": " + root.safe(root.error.message, 300) : ".")
        color: Theme.danger
    }

    LButton {
        visible: !!root.error
        text: "Try again"
        onClicked: root.reload()
    }

    Column {
        objectName: "section_totals"
        width: parent.width
        visible: root.loaded
        spacing: 2

        Line {
            text: "Spend " + root.periodWord + ": " + (root.summary ? root.safe(root.money(root.summary.display_total, root.summary.display_currency), 60) : "")
            font.pixelSize: Theme.fontSize + 4
            font.bold: true
        }
        Note {
            text: "Money only, converted at your dated rates" + (root.period === "month"
                  ? ", fixed costs included." : ".")
        }
        Line {
            visible: text !== ""
            text: root.summary && UsageText.amounts(root.summary.by_currency, root.money) !== ""
                  ? "By currency: " + root.safe(UsageText.amounts(root.summary.by_currency, root.money), 300) : ""
        }
        Line {
            visible: !!(root.summary && root.summary.unconverted && root.summary.unconverted.length)
            text: visible ? "No rate for " + root.safe(root.summary.unconverted.join(", "), 200)
                            + ": left out of the converted total. Add a dated rate for it in suite.toml." : ""
            color: Theme.warning
        }
        Line {
            visible: !!(root.summary && root.summary.overflow)
            text: "Some figures were too large to add up exactly; the totals leave them out."
            color: Theme.warning
        }
    }

    Column {
        objectName: "section_money"
        width: parent.width
        visible: root.loaded
        spacing: 2

        Heading { text: "Money" }
        Note {
            readonly property var m: root.bases.money || ({})
            text: UsageText.records(m.records || 0, root.count) + ": " + root.count(m.estimated || 0)
                  + " estimated, " + root.count(m.reconciled || 0) + " reconciled"
        }
        Repeater {
            id: moneyRows
            model: UsageText.shownRows(root.summary, "money", root.shownDimension, root.count)
            delegate: UsageRow {
                figures: [UsageText.amounts(modelData.byCurrency, root.money), UsageText.unitText(modelData.units, root.count),
                          UsageText.records(modelData.records, root.count)].filter(s => s !== "").join("  ·  ")
            }
        }
        Note {
            visible: moneyRows.count === 0
            text: "No money spent " + root.periodWord + "."
        }
    }

    Column {
        objectName: "section_subscription_quota"
        width: parent.width
        visible: root.loaded
        spacing: 2

        Heading { text: "Subscription quota" }
        Note { text: "In each plan's own units: not money, and not in the spend total." }
        Repeater {
            id: quotaRows
            model: UsageText.shownRows(root.summary, "subscription_quota", root.shownDimension, root.count)
            delegate: UsageRow {
                figures: [UsageText.unitText(modelData.units, root.count), UsageText.records(modelData.records, root.count)]
                         .filter(s => s !== "").join("  ·  ")
            }
        }
        Note {
            visible: quotaRows.count === 0
            text: "No subscription use " + root.periodWord + "."
        }
    }

    Column {
        objectName: "section_local"
        width: parent.width
        visible: root.loaded
        spacing: 2

        Heading { text: "Local work" }
        Note {
            readonly property var l: root.bases.local || ({})
            text: "GPU time and energy on this PC" + (UsageText.unitText(UsageText.unitsOf("local", l), root.count) !== ""
                  ? ": " + UsageText.unitText(UsageText.unitsOf("local", l), root.count) : ".")
        }
        Repeater {
            id: localRows
            model: UsageText.shownRows(root.summary, "local", root.shownDimension, root.count)
            delegate: UsageRow {
                figures: [UsageText.unitText(modelData.units, root.count),
                          UsageText.amounts(modelData.energyCost, root.money) !== ""
                          ? "energy " + UsageText.amounts(modelData.energyCost, root.money) : "",
                          UsageText.records(modelData.records, root.count)].filter(s => s !== "").join("  ·  ")
            }
        }
        Note {
            visible: localRows.count === 0
            text: "No local work " + root.periodWord + "."
        }
    }

    Column {
        objectName: "section_unattributed"
        width: parent.width
        visible: root.loaded
        spacing: 2

        Heading { text: "Spend outside LILITH" }
        Note {
            readonly property var u: root.summary && root.summary.unattributed ? root.summary.unattributed : ({})
            text: "Paid ComfyUI work submitted outside the suite (unattributed), never in the totals above"
                  + (u.records ? ": " + UsageText.records(u.records, root.count)
                     + (UsageText.amounts(u.by_currency, root.money) !== ""
                        ? ", " + root.safe(UsageText.amounts(u.by_currency, root.money), 300) : "") : ".")
        }
        Repeater {
            model: UsageText.shownRows(root.summary, "unattributed", root.shownDimension, root.count)
            delegate: UsageRow {
                figures: [UsageText.amounts(modelData.byCurrency, root.money), UsageText.unitText(modelData.units, root.count),
                          UsageText.records(modelData.records, root.count)].filter(s => s !== "").join("  ·  ")
            }
        }
    }

    Column {
        objectName: "section_fixed_costs"
        width: parent.width
        visible: root.loaded && root.period === "month"
        spacing: 2

        Heading { text: "Fixed costs" }
        Note { text: "Included in the month's total. Caps don't apply to them." }
        Repeater {
            id: fixedRows
            model: root.summary && root.summary.fixed_costs ? root.summary.fixed_costs : []
            delegate: Line {
                required property var modelData
                readonly property string own: root.safe(root.money(modelData.amount, modelData.currency), 60)
                readonly property string shownAs: modelData.display_amount === null
                    ? "no rate: not in the total"
                    : modelData.currency === root.summary.display_currency
                      ? "" : root.safe(root.money(modelData.display_amount, root.summary.display_currency), 60)
                text: root.safe(modelData.name) + ": " + own + (shownAs !== "" ? " (" + shownAs + ")" : "")
                color: modelData.display_amount === null ? Theme.warning : Theme.text
            }
        }
        Note {
            visible: fixedRows.count === 0
            text: "No fixed costs set."
        }
    }
}
