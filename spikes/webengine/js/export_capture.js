// Test helper (criteria 3 and 5): runs one of the frontend's own export commands
// (Comfy.ExportWorkflowAPI = menu "Export (API)", Comfy.ExportWorkflow = menu "Export") exactly as
// the menu does. The frontend saves through downloadBlob(): URL.createObjectURL(blob) and an
// <a download> click. This wraps URL.createObjectURL once to keep a copy of the blob's text in
// window.__wwResults[requestId]; the download itself still goes ahead and is cancelled by the
// profile's downloadRequested handler. The filename prompt (setting Comfy.PromptFilename, on by
// default) is confirmed with Enter, as a user would.
(function (requestId, commandId) {
    const flags = (window.__wwFlags = window.__wwFlags || {});
    const results = (window.__wwResults = window.__wwResults || {});
    flags[requestId] = 'pending';
    const original = URL.createObjectURL;
    URL.createObjectURL = function (obj) {
        URL.createObjectURL = original;
        if (obj instanceof Blob) {
            obj.text().then((t) => { results[requestId] = t; flags[requestId] = 'ok'; },
                            (e) => { flags[requestId] = 'error:blob ' + e; });
        }
        return original.call(URL, obj);
    };
    setTimeout(() => { if (URL.createObjectURL !== original) URL.createObjectURL = original; }, 15000);
    const t0 = Date.now();
    const timer = setInterval(() => {
        const input = document.querySelector('[role="dialog"] input[type="text"], [role="dialog"] input:not([type])');
        if (input) {
            clearInterval(timer);
            input.dispatchEvent(new KeyboardEvent('keyup', { key: 'Enter', code: 'Enter', bubbles: true }));
        } else if (Date.now() - t0 > 8000) {
            clearInterval(timer);
        }
    }, 50);
    Promise.resolve(window.app.extensionManager.command.execute(commandId))
        .catch((e) => { flags[requestId] = 'error:' + (e && e.message ? e.message : e); });
    return 'started';
})
