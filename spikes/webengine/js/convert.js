// IMG-124 hidden conversion: UI-format graph -> API-format prompt, by ComfyUI's own frontend, in
// the hidden off-the-record page. Same code path as the frontend's "Export (API)" command
// (workflowService.exportWorkflow(name, 'output') saves JSON of (await app.graphToPrompt()).output).
//   b64        base64 of the graph's UTF-8 JSON text
//   requestId  window.__wwFlags[requestId] = "pending" -> "ok" | "error:<msg>";
//              on "ok", window.__wwResults[requestId] = JSON.stringify((await app.graphToPrompt()).output)
(function (b64, requestId) {
    const flags = (window.__wwFlags = window.__wwFlags || {});
    const results = (window.__wwResults = window.__wwResults || {});
    flags[requestId] = 'pending';
    let graph;
    try {
        const bytes = Uint8Array.from(atob(b64), (c) => c.charCodeAt(0));
        graph = JSON.parse(new TextDecoder('utf-8', { fatal: true }).decode(bytes));
    } catch (e) {
        flags[requestId] = 'error:decode ' + (e && e.message ? e.message : e);
        return flags[requestId];
    }
    (async () => {
        try {
            const app = window.app;
            if (!app || typeof app.graphToPrompt !== 'function') { flags[requestId] = 'error:unreachable'; return; }
            // No view restore, no model/media scans, warnings deferred: nothing is shown in a hidden page.
            const ok = await app.loadGraphData(graph, true, false, null,
                { openSource: 'worldwright-convert', skipAssetScans: true, silentAssetErrors: true, deferWarnings: true });
            if (ok === false) { flags[requestId] = 'error:loadGraphData returned false'; return; }
            results[requestId] = JSON.stringify((await app.graphToPrompt()).output);
            flags[requestId] = 'ok';
        } catch (e) {
            flags[requestId] = 'error:' + (e && e.message ? e.message : e);
        }
    })();
    return flags[requestId];
})
