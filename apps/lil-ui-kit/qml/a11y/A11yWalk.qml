// A11yWalk (APP-156): visits every item under a root (a panel, a page, the shell's own chrome)
// and lists each visible, enabled interactive item that fails the minimal accessibility rule:
//   - a non-empty accessible name and a role other than NoRole, as the accessibility layer (the
//     source UI Automation reads) exposes them (LilShim.accessibleOf);
//   - a place in the tab chain (reached by Item.nextItemInFocusChain from the root's first
//     item).
// Interactive means, judged by what an item does rather than what it is:
//   - a control: anything in the tab chain by its own activeFocusOnTab, anything with a role
//     that takes input (a button, check box, radio button, editable text, combo box, slider,
//     spin box, dial, page tab, link or menu item: AbstractButton, TextInput, Slider and the
//     like), and anything with an accessible action other than Set Focus (press, toggle,
//     increase, ...), whatever its focusPolicy (one without Qt.TabFocus is reported as out of
//     the tab chain);
//   - a pointer target: a MouseArea that takes buttons, or an item holding an enabled TapHandler
//     or DragHandler. It needs the role and name on its nearest ancestor (up to the root) with
//     an accessible role, and that ancestor must itself be a control in the tab chain: the
//     search stops at that ancestor, and a container (a Pane, a group) there doesn't count,
//     however it is named.
// Not interactive: static items and controls (Label, ProgressBar, Pane, Frame, ToolSeparator,
// ScrollView, text), and scroll bars, whose list or view the keyboard scrolls itself.
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
                if (!owner || !takesInput(owner)) {
                    const near = owner ? " (" + describe(owner, LilShim.accessibleOf(owner)) + " is a container)" : ""
                    out.push({item: item, what: label + ": takes clicks with no accessible role on it or an interactive ancestor" + near})
                    out.push({item: item, what: label + ": no accessible name on it or an interactive ancestor" + near})
                    out.push({item: item, what: label + ": not in the tab chain, nor an interactive ancestor" + near})
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
                const policy = typeof item.focusPolicy === "number" && !(item.focusPolicy & Qt.TabFocus)
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

    // The roles of items that take input themselves.
    readonly property var inputRoles: [Accessible.Button, Accessible.CheckBox, Accessible.RadioButton,
        Accessible.EditableText, Accessible.ComboBox, Accessible.Slider, Accessible.SpinBox,
        Accessible.Dial, Accessible.PageTab, Accessible.Link, Accessible.MenuItem]

    // Whether the item takes input by its role or its accessible actions. Setting the focus is
    // every focusable item's action, not a sign it does anything, and a focusable container
    // (a Pane, a group) still doesn't take input.
    function takesInput(item) {
        const a = LilShim.accessibleOf(item)
        return inputRoles.indexOf(a.role) >= 0
               || a.actions.some(name => name.replace(/\s/g, "") !== "SetFocus")
    }

    // "control", "pointer" or "" (see the header).
    function interactiveKind(item) {
        if (LilShim.accessibleOf(item).role === Accessible.ScrollBar)
            return ""
        if (item.activeFocusOnTab || takesInput(item))
            return "control"
        if (String(item).startsWith("QQuickMouseArea"))
            return item.acceptedButtons !== Qt.NoButton ? "pointer" : ""
        return hasPointerHandler(item) ? "pointer" : ""
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
