// A11yWalk (APP-156): visits every item under a root (a panel, a page) and lists each visible,
// enabled interactive item that fails the minimal accessibility rule:
//   - a non-empty accessible name and a role other than NoRole, as the accessibility layer (the
//     source UI Automation reads) exposes them (LilShim.accessibleOf);
//   - a place in the tab chain (reached by Item.nextItemInFocusChain from the root's first
//     item).
// Interactive means: in the tab chain by its own setting (activeFocusOnTab, or a control's
// focusPolicy with Qt.TabFocus), or a MouseArea that takes clicks. Each panel task runs it over
// its panel:
//     A11yWalk { id: walk }
//     compare(walk.problems(panel), [])
import QtQuick
import Lilith.Shim

QtObject {
    // [{item, what}] for every failing item, `what` naming the rule; empty when all pass.
    function problems(root) {
        const chain = tabChain(root)
        const out = []
        visit(root, item => {
            if (!item.visible || !item.enabled || !isInteractive(item))
                return
            const a = LilShim.accessibleOf(item)
            const label = describe(item, a)
            if (a.role === Accessible.NoRole)
                out.push({item: item, what: label + ": no accessible role"})
            if (!a.name || a.name.trim() === "")
                out.push({item: item, what: label + ": no accessible name"})
            if (chain.indexOf(item) < 0)
                out.push({item: item, what: label + ": not in the tab chain"})
        })
        return out
    }

    // The `what` of each problem, for a readable failure message.
    function report(root) {
        return problems(root).map(p => p.what)
    }

    // The number of interactive items visited, so a test can check the walk saw them.
    function interactiveCount(root) {
        let n = 0
        visit(root, item => {
            if (item.visible && item.enabled && isInteractive(item))
                ++n
        })
        return n
    }

    function isInteractive(item) {
        if (item.activeFocusOnTab)
            return true
        if (typeof item.focusPolicy === "number" && (item.focusPolicy & Qt.TabFocus))
            return true
        return String(item).startsWith("QQuickMouseArea") && item.acceptedButtons !== Qt.NoButton
    }

    function visit(item, fn) {
        fn(item)
        for (let i = 0; i < item.children.length; ++i)
            visit(item.children[i], fn)
    }

    function isInside(item, root) {
        for (let p = item; p; p = p.parent) {
            if (p === root)
                return true
        }
        return false
    }

    // The items under `root` that Tab reaches, in order.
    function tabChain(root) {
        const out = []
        const start = root.nextItemInFocusChain(true)
        let item = start
        for (let n = 0; item && n < 5000; ++n) {
            if (isInside(item, root) && out.indexOf(item) < 0)
                out.push(item)
            item = item.nextItemInFocusChain(true)
            if (item === start)
                break
        }
        return out
    }

    function describe(item, a) {
        const kind = String(item).replace(/\(.*$/, "").replace(/_QML.*$/, "")
        return kind + (item.objectName ? " '" + item.objectName + "'" : "")
               + (a.name ? " \"" + a.name + "\"" : "")
    }
}
