// Returns and removes window.__wwResults[id] (null if absent).
(function (id) {
    const results = window.__wwResults;
    if (!results || !(id in results)) return null;
    const r = results[id];
    delete results[id];
    return r;
})
