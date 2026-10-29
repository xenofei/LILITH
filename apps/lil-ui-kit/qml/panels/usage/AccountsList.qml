// The accounts (APP-122, APP-124): each provider account this month's usage names, from one
// `usage.summary` grouped by provider and account. An account is its opaque id (a key id, or a
// profile id for Claude Code): the suite has no account labels yet, so the id is what shows.
// No provider's account API is read until plan 6b, so every account is "tracked only": what
// LILITH's own usage records say, never the provider's balances or limits. Its Texts are plain
// text, and the provider and account names go through `core.shownText` (P4a-R8).
pragma ComponentBehavior: Bound
import QtQuick
import Lilith.UiKit
import "UsageText.js" as UsageText

LCard {
    id: root

    required property var core
    property var summary: null
    property var error: null
    property var _call: null
    property bool _due: false

    // [{provider, account, shown, bases, status}], in the summary's order.
    readonly property var entries: {
        const out = []
        for (const r of (summary && summary.rows) || []) {
            if (r.account === null || r.account === undefined || r.account === "")
                continue
            const found = out.find(e => e.provider === r.provider && e.account === r.account)
            if (found) {
                if (found.bases.indexOf(r.basis) < 0)
                    found.bases.push(r.basis)
                continue
            }
            out.push({provider: r.provider, account: r.account, shown: UsageText.shown(core, UsageText.label("account", r.account)),
                      bases: [r.basis], status: "tracked only"})
        }
        return out
    }

    title: "Accounts"

    // Reads the month's accounts again; a read still out is let finish, then one more follows.
    function read() {
        if (_call) {
            _due = true
            return
        }
        _call = CoreCall.call(core, "usage.summary", {period: "month", group_by: ["provider", "account"]}, {
            owner: root,
            ok: r => {
                root._call = null
                root.summary = r.structured
                root.error = null
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

    function basisWord(basis) {
        return {money: "money", subscription_quota: "subscription quota", local: "local work",
                unattributed: "unattributed"}[basis] || basis
    }

    Column {
        width: parent.width
        spacing: 4

        Text {
            width: parent.width
            text: "Account sync comes later: these are tracked only, from LILITH's own usage records."
            color: Theme.textMuted
            font.pixelSize: Theme.fontSize - 1
            textFormat: Text.PlainText
            wrapMode: Text.Wrap
            Accessible.role: Accessible.StaticText
            Accessible.name: text
        }

        Text {
            width: parent.width
            visible: root.entries.length === 0
            text: root.error ? "Couldn't read the accounts" + (root.error.message ? ": " + UsageText.shown(root.core, root.error.message, 300) : ".")
                : root.summary ? "No accounts in this month's usage yet." : "Reading the accounts…"
            color: root.error ? Theme.danger : Theme.textMuted
            font.pixelSize: Theme.fontSize
            textFormat: Text.PlainText
            wrapMode: Text.Wrap
            Accessible.role: Accessible.StaticText
            Accessible.name: text
        }

        Repeater {
            model: root.entries
            delegate: Item {
                id: entry

                required property var modelData

                width: parent.width
                height: Math.max(entryText.implicitHeight, badge.implicitHeight) + 2

                Accessible.role: Accessible.StaticText
                Accessible.name: entryText.text + ", " + modelData.status

                Text {
                    id: entryText
                    width: parent.width - badge.width - Theme.spacing
                    anchors.verticalCenter: parent.verticalCenter
                    text: UsageText.shown(root.core, entry.modelData.provider) + " · " + entry.modelData.shown + " · "
                          + UsageText.shown(root.core, entry.modelData.bases.map(root.basisWord).join(", "))
                    textFormat: Text.PlainText
                    color: Theme.text
                    font.pixelSize: Theme.fontSize
                    elide: Text.ElideRight
                }

                LBadge {
                    id: badge
                    anchors.right: parent.right
                    anchors.verticalCenter: parent.verticalCenter
                    text: entry.modelData.status
                }
            }
        }
    }
}
