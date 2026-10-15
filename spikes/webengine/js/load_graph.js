// IMG-127 "Open in ComfyUI": loads a UI-format graph into a NEW workflow tab of ComfyUI's own
// frontend (comfyui-frontend-package 1.53.10).
//   b64        base64 of the graph's UTF-8 JSON text, decoded here in the page
//   requestId  key of the result flag: window.__wwFlags[requestId] = "pending" -> "ok" | "error:<msg>"
// Call shape used (ComfyApp.loadGraphData in 1.53.10):
//   app.loadGraphData(graphData, clean = true, restore_view = true, workflow = null, options = {})
//   workflow null (or a name not already open) -> workflowService.afterLoadNewGraph ->
//   workflowStore.createNewTemporary(...) + openWorkflow: a new temporary tab; the tabs already
//   open are kept. Returns false if the graph could not be configured.
(function (b64, requestId) {
    const flags = (window.__wwFlags = window.__wwFlags || {});
    flags[requestId] = 'pending';
    let graph;
    try {
        const bytes = Uint8Array.from(atob(b64), (c) => c.charCodeAt(0));
        graph = JSON.parse(new TextDecoder('utf-8', { fatal: true }).decode(bytes));
    } catch (e) {
        flags[requestId] = 'error:decode ' + (e && e.message ? e.message : e);
        return flags[requestId];
    }
    const ready = () => !!(window.app && typeof window.app.loadGraphData === 'function' && window.app.canvas &&
                           window.app.extensionManager && window.app.extensionManager.workflow);
    (async () => {
        const t0 = Date.now();
        while (!ready()) {
            // Not a (booted) ComfyUI page, e.g. Chromium's error page when ComfyUI is down.
            if (Date.now() - t0 > 2000) { flags[requestId] = 'error:unreachable'; return; }
            await new Promise((r) => setTimeout(r, 50));
        }
        try {
            const ok = await window.app.loadGraphData(graph, true, true, null, { openSource: 'worldwright' });
            flags[requestId] = ok === false ? 'error:loadGraphData returned false' : 'ok';
        } catch (e) {
            flags[requestId] = 'error:' + (e && e.message ? e.message : e);
        }
    })();
    return flags[requestId];
})
