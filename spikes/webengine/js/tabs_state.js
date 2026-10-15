// Test helper (criterion 1): the open workflow tabs, the active one, and a hash of each tab's
// node/link shape (ids, types, modes, links, subgraphs) taken from its stored state.
(function () {
    const ws = window.app.extensionManager.workflow;
    const fnv = (s) => {
        let h = 0x811c9dc5;
        for (let i = 0; i < s.length; i++) { h ^= s.charCodeAt(i); h = Math.imul(h, 0x01000193) >>> 0; }
        return h.toString(16);
    };
    const shape = (st) => st ? fnv(JSON.stringify({
        nodes: (st.nodes || []).map((n) => [n.id, n.type, n.mode]),
        links: st.links || [],
        subgraphs: ((st.definitions && st.definitions.subgraphs) || [])
            .map((s) => [s.id, (s.nodes || []).map((n) => [n.id, n.type, n.mode]), s.links || []])
    })) : null;
    return JSON.stringify({
        active: ws.activeWorkflow ? ws.activeWorkflow.path : null,
        open: ws.openWorkflows.map((w) => ({
            path: w.path, temporary: w.isTemporary, modified: w.isModified,
            shape_hash: shape(w.activeState),
            nodes: w.activeState && w.activeState.nodes ? w.activeState.nodes.length : null
        })),
        href: location.href
    });
})
