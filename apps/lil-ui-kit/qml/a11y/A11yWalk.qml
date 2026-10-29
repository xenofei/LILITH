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
//
// Lists (for the Jobs, Backups and Diagnostics panels and any other list): a keyboard-navigable
// ListView or GridView is one tab stop, and the arrow keys and Enter work inside it. Make the
// view itself the tab stop (activeFocusOnTab, keyNavigationEnabled left on) with an
// Accessible role (List, Table or Tree) and name. Its rows then aren't tab stops: each delegate
// needs a row role (ListItem, Row, Cell or TreeItem) and its own name ("Job 3, running"), and
// its pointer input (a MouseArea, a TapHandler, a press action) sits on the row or under it. A
// row with no name, or with another role, is reported; so is a view whose rows take clicks but
// that isn't a tab stop, or that has key navigation off. Controls of their own inside a row (a
// button), and the view's header and footer, aren't rows: they follow the usual rule.
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
            // A row of a keyboard-navigable list, or a click on or under one: judged by the row.
            const lr = listRow(item, root, chain)
            if (lr && (item === lr.row || kind === "pointer")) {
                const r = LilShim.accessibleOf(lr.row)
                const row = describe(lr.row, r)
                if (rowRoles.indexOf(r.role) < 0)
                    out.push({item: item, what: label + ": " + row + " of " + describe(lr.view, LilShim.accessibleOf(lr.view))
                                                + " needs a row role (ListItem, Row, Cell or TreeItem)"})
                if (!r.name || r.name.trim() === "")
                    out.push({item: item, what: label + ": no accessible name on " + row})
                return
            }
            if (kind === "pointer") {
                const owner = accessibleAncestor(item, root)
                if (!owner || !takesInput(owner)) {
                    const near = owner ? " (" + describe(owner, LilShim.accessibleOf(owner)) + " is a container)" : ""
                    out.push({item: item, what: label + ": takes clicks with no accessible role on it or an interactive ancestor" + near})
                    out.push({item: item, what: label + ": no accessible name on it or an interactive ancestor" + near})
                    out.push({item: item, what: label + ": not in the tab chain, nor an interactive ancestor" + near
                                                + ": make it or its accessible owner focusable (activeFocusOnTab)"})
                    return
                }
                const a = LilShim.accessibleOf(owner)
                if (!a.name || a.name.trim() === "")
                    out.push({item: item, what: label + ": no accessible name on " + describe(owner, a)})
                if (chain.indexOf(owner) < 0)
                    out.push({item: item, what: label + ": not in the tab chain, nor " + describe(owner, a)
                                                + ": make it or its accessible owner focusable (activeFocusOnTab)"})
                return
            }
            if (own.role === Accessible.NoRole)
                out.push({item: item, what: label + ": no accessible role"})
            if (!own.name || own.name.trim() === "")
                out.push({item: item, what: label + ": no accessible name"})
            if (chain.indexOf(item) < 0) {
                // The focus policy is the cause only for a control that takes input and isn't a
                // tab stop by its own setting; any other item is told to become focusable.
                const policy = typeof item.focusPolicy === "number" && !item.activeFocusOnTab
                               && takesInput(item) && !(item.focusPolicy & Qt.TabFocus)
                out.push({item: item, what: label + ": not in the tab chain"
                                            + (policy ? " (its focusPolicy lacks Qt.TabFocus)"
                                                      : ": make it or its accessible owner focusable (activeFocusOnTab)")})
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

    // The roles a list's row takes, and the roles of a list that is one tab stop.
    readonly property var rowRoles: [Accessible.ListItem, Accessible.Row, Accessible.Cell, Accessible.TreeItem]
    readonly property var listRoles: [Accessible.List, Accessible.Table, Accessible.Tree]

    // {view, row} when `item` is, or sits under, a delegate of a keyboard-navigable ListView or
    // GridView inside `root`: the view is a tab stop in `chain` with key navigation on and a list
    // role and name. Else null, also for the view's header and footer, which aren't rows.
    function listRow(item, root, chain) {
        for (let child = item, p = item.parent; p; child = p, p = p.parent) {
            const view = p.parent
            if (view && view.contentItem === p && view.keyNavigationEnabled !== undefined) {
                if (child === view.headerItem || child === view.footerItem)
                    return null
                const a = LilShim.accessibleOf(view)
                const navigable = view.activeFocusOnTab && view.keyNavigationEnabled
                                  && chain.indexOf(view) >= 0 && listRoles.indexOf(a.role) >= 0
                                  && a.name && a.name.trim() !== ""
                return navigable ? {view: view, row: child} : null
            }
            if (p === root)
                break
        }
        return null
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
