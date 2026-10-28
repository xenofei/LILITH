// A11yWalk (APP-156): visits every item under a root (a panel, a page, the shell's own chrome)
// and lists each visible, enabled interactive item that fails the minimal accessibility rule:
//   - a non-empty accessible name and a role other than NoRole, as the accessibility layer (the
//     source UI Automation reads) exposes them (LilShim.accessibleOf);
//   - a place in the tab chain (reached by Item.nextItemInFocusChain from the root's first
//     item).
// Interactive means:
//   - a control: every Qt Quick Control and template (Control, AbstractButton, TextField, ...;
//     known by their `background`), whatever its focusPolicy (one without Qt.TabFocus is
//     reported as out of the tab chain), anything in the tab
//     chain by its own activeFocusOnTab, and anything with an accessible action (press, toggle,
//     increase, ...);
//   - a pointer target: a MouseArea that takes buttons, or an item holding an enabled TapHandler
//     or DragHandler. It is judged by its nearest ancestor (up to the root) with an accessible
//     role: that ancestor needs the name and the place in the tab chain; with no such ancestor,
//     the pointer target itself fails all three.
// `exempt(item)`, when given, skips an item and what it holds (each caller says why). Each panel
// task runs it over its panel:
//     A11yWalk { id: walk }
//     compare(walk.report(panel), [])
import QtQuick
import Lilith.Shim

QtObject {
    // [{item, what}] for every failing item, `what` naming the rule; empty when all pass.
    function problems(root, exempt) {
        const chain = tabChain(root)
        const out = []
        visit(root, exempt, item => {
            if (!item.visible || !item.enabled)
                return
            const kind = interactiveKind(item)
            if (!kind)
                return
            const own = LilShim.accessibleOf(item)
            const label = describe(item, own)
            if (kind === "pointer") {
                const owner = accessibleAncestor(item, root)
                if (!owner) {
                    out.push({item: item, what: label + ": takes clicks with no accessible role on it or an ancestor"})
                    out.push({item: item, what: label + ": no accessible name on it or an ancestor"})
                    out.push({item: item, what: label + ": not in the tab chain, nor an accessible ancestor"})
                    return
                }
                const a = LilShim.accessibleOf(owner)
                if (!a.name || a.name.trim() === "")
                    out.push({item: item, what: label + ": no accessible name on " + describe(owner, a)})
                if (chain.indexOf(owner) < 0)
                    out.push({item: item, what: label + ": not in the tab chain, nor " + describe(owner, a)})
                return
            }
            if (own.role === Accessible.NoRole)
                out.push({item: item, what: label + ": no accessible role"})
            if (!own.name || own.name.trim() === "")
                out.push({item: item, what: label + ": no accessible name"})
            if (chain.indexOf(item) < 0) {
                const policy = isControl(item) && !(item.focusPolicy & Qt.TabFocus)
                out.push({item: item, what: label + ": not in the tab chain"
                                            + (policy ? " (its focusPolicy lacks Qt.TabFocus)" : "")})
            }
        })
        return out
    }

    // The `what` of each problem, for a readable failure message.
    function report(root, exempt) {
        return problems(root, exempt).map(p => p.what)
    }

    // The number of interactive items visited, so a test can check the walk saw them.
    function interactiveCount(root, exempt) {
        let n = 0
        visit(root, exempt, item => {
            if (item.visible && item.enabled && interactiveKind(item))
                ++n
        })
        return n
    }

    // "control", "pointer" or "" (see the header).
    function interactiveKind(item) {
        if (isControl(item) || item.activeFocusOnTab)
            return "control"
        if (String(item).startsWith("QQuickMouseArea"))
            return item.acceptedButtons !== Qt.NoButton ? "pointer" : ""
        if (hasPointerHandler(item))
            return "pointer"
        const a = LilShim.accessibleOf(item)
        // Setting the focus is every focusable item's action, not a sign it does anything.
        return a.actions.some(name => name.replace(/\s/g, "") !== "SetFocus") ? "control" : ""
    }

    // A Qt Quick Control or template: every item has a focusPolicy since Qt 6.7, but only these
    // have a background.
    function isControl(item) {
        return item.background !== undefined
    }

    function hasPointerHandler(item) {
        const data = item.data || []
        for (let i = 0; i < data.length; ++i) {
            const s = String(data[i])
            if ((s.startsWith("QQuickTapHandler") || s.startsWith("QQuickDragHandler")) && data[i].enabled)
                return true
        }
        return false
    }

    function accessibleAncestor(item, root) {
        for (let p = item.parent; p; p = p.parent) {
            if (LilShim.accessibleOf(p).role !== Accessible.NoRole && !String(p).startsWith("QQuickRootItem"))
                return p
            if (p === root)
                break
        }
        return null
    }

    function visit(item, exempt, fn) {
        if (exempt && exempt(item))
            return
        fn(item)
        for (let i = 0; i < item.children.length; ++i)
            visit(item.children[i], exempt, fn)
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
               + (a && a.name ? " \"" + a.name + "\"" : "")
    }
}
