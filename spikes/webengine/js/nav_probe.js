// Test helper (criterion 6).
//   mode "href":   location.href = url on the next tick (a renderer-initiated main-frame navigation)
//   mode "open":   window.open(url, '_blank') (a popup / new-window request)
//   mode "origin": location.origin
(function (url, mode) {
    if (mode === 'href') { setTimeout(() => { location.href = url; }, 0); return 'scheduled'; }
    if (mode === 'open') { const w = window.open(url, '_blank'); return w ? 'window' : 'null'; }
    return location.origin;
})
