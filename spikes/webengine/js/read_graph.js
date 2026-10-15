// IMG-129 "Send to Worldwright": the current graph of the active workflow tab, synchronously.
// app.graph is ComfyApp's root graph (1.53.10: `get graph() { return this.rootGraphInternal }`),
// so this is the whole workflow, with its subgraph definitions, even while a subgraph is open.
(function () {
    return JSON.stringify(window.app.graph.serialize());
})
