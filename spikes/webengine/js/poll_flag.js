// window.__wwFlags[name], or "" if unset.
(function (name) {
    const flags = window.__wwFlags;
    return flags && typeof flags[name] === 'string' ? flags[name] : '';
})
