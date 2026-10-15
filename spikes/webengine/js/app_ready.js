// True once ComfyUI's frontend has booted (GraphView sets window.app after app.setup()) and has an
// active workflow tab.
(function () {
    const a = window.app;
    return !!(a && typeof a.loadGraphData === 'function' && a.canvas && a.extensionManager &&
              a.extensionManager.workflow && a.extensionManager.workflow.activeWorkflow);
})
