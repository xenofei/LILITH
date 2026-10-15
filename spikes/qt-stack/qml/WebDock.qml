// The only file that imports QtWebEngine (APP-137). Main.qml's Loader creates it the first time
// the Web dock is shown, so the WebEngine DLLs stay out of the process until then.
import QtQuick
import QtWebEngine
import SpikeShim

WebEngineView {
    id: view
    url: "http://127.0.0.1:8188"

    function statusName(s) {
        switch (s) {
        case WebEngineLoadingInfo.LoadStartedStatus: return "started"
        case WebEngineLoadingInfo.LoadStoppedStatus: return "stopped"
        case WebEngineLoadingInfo.LoadSucceededStatus: return "succeeded"
        case WebEngineLoadingInfo.LoadFailedStatus: return "failed"
        }
        return "unknown:" + s
    }

    onLoadingChanged: function(info) {
        const status = statusName(info.status)
        SpikeLog.log("web_load", {
            status: status,
            url: info.url.toString(),
            error_code: info.errorCode,
            error_domain: info.errorDomain,
            error_string: info.errorString,
            is_error_page: info.isErrorPage === undefined ? null : info.isErrorPage
        })
        if (status === "succeeded" || status === "failed")
            probeTimer.restart()
    }

    onRenderProcessTerminated: function(status, exitCode) {
        SpikeLog.log("web_render_process_terminated", { status: status, exit_code: exitCode })
    }

    // A few seconds after a load finishes, record what the page shows.
    Timer {
        id: probeTimer
        interval: 6000
        onTriggered: {
            // Logged even when the page (e.g. Chromium's error page) does not run our script.
            SpikeLog.log("web_title", { title: view.title, url: view.url.toString(), loading: view.loading })
            view.runJavaScript(
            "({ title: document.title, url: location.href," +
            "   graph: !!document.querySelector('#graph-canvas, canvas.graph-canvas, .graph-canvas-container canvas, canvas')," +
            "   canvases: document.querySelectorAll('canvas').length," +
            "   text: (document.body ? document.body.innerText : '').slice(0, 200) })",
            function(r) {
                SpikeLog.log("web_probe", {
                    title: view.title,
                    url: view.url.toString(),
                    js_title: r ? r.title : null,
                    js_url: r ? r.url : null,
                    has_graph_canvas: r ? r.graph : false,
                    canvas_count: r ? r.canvases : 0,
                    text_head: r ? r.text : null,
                    js_result: r !== undefined && r !== null
                })
            })
        }
    }
}
