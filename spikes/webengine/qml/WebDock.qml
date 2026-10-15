// The only file that imports QtWebEngine. The ComfyUI view, locked down per ARCH-070 / IMG-126:
//   - profile: PageDriver.viewProfile, a named persistent profile stored under --profile-dir;
//   - navigation pinned to the ComfyUI origin (navigationRequested -> PageDriver.allowNavigation);
//   - popups refused (newWindowRequested is logged, never opened);
//   - downloads cancelled in C++ (the profile's downloadRequested, see PageDriver);
//   - no web channel: the only way in is runJavaScript, driven by PageDriver through run().
import QtQuick
import QtWebEngine
import SpikeShim

WebEngineView {
    id: view
    profile: PageDriver.viewProfile

    signal loadDone(string status)

    // PageDriver -> page. The callback hands the result back by id (null if undefined).
    function run(id, script) {
        view.runJavaScript(script, function (result) {
            PageDriver.deliver(id, result === undefined ? null : result)
        })
    }

    Component.onCompleted: PageDriver.attachView(view)

    function statusName(s) {
        switch (s) {
        case WebEngineLoadingInfo.LoadStartedStatus: return "started"
        case WebEngineLoadingInfo.LoadStoppedStatus: return "stopped"
        case WebEngineLoadingInfo.LoadSucceededStatus: return "succeeded"
        case WebEngineLoadingInfo.LoadFailedStatus: return "failed"
        }
        return "unknown:" + s
    }

    onLoadingChanged: function (info) {
        const status = statusName(info.status)
        SpikeLog.log("web_load", {
            status: status,
            url: info.url.toString(),
            error_code: info.errorCode,
            error_string: info.errorString,
            is_error_page: info.isErrorPage === undefined ? null : info.isErrorPage
        })
        if (status !== "started")
            view.loadDone(status)
    }

    // Navigation pin (the QML counterpart of QWebEnginePage::acceptNavigationRequest).
    onNavigationRequested: function (request) {
        const allowed = PageDriver.allowNavigation(request.url)
        if (!allowed)
            request.reject()
        if (!allowed || request.isMainFrame)
            SpikeLog.log("navigation", {
                url: request.url.toString(),
                main_frame: request.isMainFrame,
                navigation_type: request.navigationType,
                allowed: allowed
            })
    }

    // Popups: refused (request.openIn() is never called).
    onNewWindowRequested: function (request) {
        SpikeLog.log("new_window", {
            url: request.requestedUrl.toString(),
            destination: request.destination,
            user_initiated: request.userInitiated,
            opened: false
        })
    }

    // No native dialogs from the page: beforeunload is accepted (so the navigation pin, not a
    // dialog, is what keeps the view on ComfyUI); alert/confirm/prompt are dismissed.
    onJavaScriptDialogRequested: function (request) {
        request.accepted = true
        SpikeLog.log("js_dialog", { type: request.type, message: request.message })
        if (request.type === JavaScriptDialogRequest.DialogTypeBeforeUnload)
            request.dialogAccept()
        else
            request.dialogReject()
    }

    onFileDialogRequested: function (request) {
        request.accepted = true
        SpikeLog.log("file_dialog", { mode: request.mode })
        request.dialogReject()
    }

    onRenderProcessTerminated: function (status, exitCode) {
        SpikeLog.log("web_render_process_terminated", { status: status, exit_code: exitCode })
    }
}
