// One tab of the Usage & spend panel (APP-116 to APP-120): one `usage.summary` of `period`
// ("month" or "day") grouped by `dimension`, read again by reload(). Its sections keep the
// billing bases apart (APP-118): the totals, money, subscription quota (its own units), local
// work, spend outside LILITH (unattributed, APP-116) and, for a month, the fixed costs (APP-119).
// Every figure is the core's own (decimal strings with their currency codes); nothing is added
// up here.
pragma ComponentBehavior: Bound
import QtQuick
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
    // The last read's error body, null after a read that worked.
    property var error: null
    property var _call: null

    readonly property string periodWord: period === "month" ? "this month" : "today"
    readonly property var bases: summary && summary.bases ? summary.bases : ({})

    spacing: Theme.spacing

    Accessible.role: Accessible.Grouping
    Accessible.name: period === "month" ? "Spend this month" : "Spend today"

    // The rows of `basis` as shown (UsageText.shownRows).
    function shown(basis) {
        return UsageText.shownRows(summary, basis, shownDimension)
    }

    // Reads the summary again; a read still open is dropped.
    function reload() {
        if (_call)
            _call.cancel()
        const asked = dimension
        _call = CoreCall.call(core, "usage.summary", {period: period, group_by: [asked]}, {
            owner: root,
            ok: r => {
                root._call = null
                root.shownDimension = asked
                root.summary = r.structured
                root.error = null
                root.loaded = true
            },
            error: e => {
                root._call = null
                root.error = e
            }
        })
    }

    component Line: Text {
        width: parent ? parent.width : 0
        color: Theme.text
        font.pixelSize: Theme.fontSize
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

    // One row of a section: its label (and the raw value when it reads differently), what it
    // counts, and its marks.
    component UsageRow: Item {
        id: line

        required property var modelData
        property string figures

        width: parent ? parent.width : 0
        height: Math.max(labelText.implicitHeight, figuresText.implicitHeight) + 4

        Accessible.role: Accessible.StaticText
        Accessible.name: [modelData.label, modelData.detail, figures].concat(modelData.marks)
                         .filter(s => s !== "").join(", ")

        Text {
            id: labelText
            width: line.width * 0.34
            text: line.modelData.label + (line.modelData.detail !== ""
                  ? "  <font color=\"" + Theme.textMuted + "\">" + line.modelData.detail + "</font>" : "")
            textFormat: line.modelData.detail !== "" ? Text.StyledText : Text.PlainText
            color: Theme.text
            font.pixelSize: Theme.fontSize
            elide: Text.ElideRight
        }

        Text {
            id: figuresText
            x: labelText.width + Theme.spacing
            width: line.width - x - marksRow.width - Theme.spacing
            text: line.figures
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
        text: "Couldn't read the usage" + (root.error && root.error.message ? ": " + root.error.message : ".")
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
            text: "Spend " + root.periodWord + ": " + (root.summary ? root.summary.display_total + " "
                  + root.summary.display_currency : "")
            font.pixelSize: Theme.fontSize + 4
            font.bold: true
        }
        Note {
            text: "Money only, converted at your dated rates" + (root.period === "month"
                  ? ", fixed costs included." : ".")
        }
        Line {
            visible: text !== ""
            text: root.summary && UsageText.amounts(root.summary.by_currency) !== ""
                  ? "By currency: " + UsageText.amounts(root.summary.by_currency) : ""
        }
        Line {
            visible: !!(root.summary && root.summary.unconverted && root.summary.unconverted.length)
            text: visible ? "No rate for " + root.summary.unconverted.join(", ")
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
            text: UsageText.records(m.records || 0) + ": " + (m.estimated || 0) + " estimated, "
                  + (m.reconciled || 0) + " reconciled"
        }
        Repeater {
            id: moneyRows
            model: UsageText.shownRows(root.summary, "money", root.shownDimension)
            delegate: UsageRow {
                figures: UsageText.amounts(modelData.byCurrency) + "  ·  " + UsageText.records(modelData.records)
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
            model: UsageText.shownRows(root.summary, "subscription_quota", root.shownDimension)
            delegate: UsageRow {
                figures: [UsageText.unitText(modelData.units), UsageText.records(modelData.records)]
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
            text: "GPU time and energy on this PC" + (UsageText.unitText(UsageText.unitsOf("local", l)) !== ""
                  ? ": " + UsageText.unitText(UsageText.unitsOf("local", l)) : ".")
        }
        Repeater {
            id: localRows
            model: UsageText.shownRows(root.summary, "local", root.shownDimension)
            delegate: UsageRow {
                figures: [UsageText.unitText(modelData.units),
                          UsageText.amounts(modelData.byCurrency) !== ""
                          ? "energy " + UsageText.amounts(modelData.byCurrency) : "",
                          UsageText.records(modelData.records)].filter(s => s !== "").join("  ·  ")
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
                  + (u.records ? ": " + UsageText.records(u.records)
                     + (UsageText.amounts(u.by_currency) !== "" ? ", " + UsageText.amounts(u.by_currency) : "") : ".")
        }
        Repeater {
            model: UsageText.shownRows(root.summary, "unattributed", root.shownDimension)
            delegate: UsageRow {
                figures: UsageText.amounts(modelData.byCurrency) + "  ·  " + UsageText.records(modelData.records)
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
                readonly property string own: modelData.amount + " " + modelData.currency
                readonly property string shownAs: modelData.display_amount === null
                    ? "no rate: not in the total"
                    : modelData.currency === root.summary.display_currency
                      ? "" : modelData.display_amount + " " + root.summary.display_currency
                text: modelData.name + ": " + own + (shownAs !== "" ? " (" + shownAs + ")" : "")
                color: modelData.display_amount === null ? Theme.warning : Theme.text
            }
        }
        Note {
            visible: fixedRows.count === 0
            text: "No fixed costs set."
        }
    }
}
