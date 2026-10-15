// WebEngine spike main window: a KDDockWidgets DockingArea with one dock, Web (qml/WebDock.qml,
// created by a Loader). With --phase, a check sequence drives PageDriver and quits:
//   hidden     10 conversions in the hidden page (the view stays on about:blank), plus a navigation test
//   visible    loadGraph / readGraph / Export (API) per fixture, one plain Export, navigation tests
//   down       loadGraph with ComfyUI stopped
//   firstboot  one hidden conversion against a scratch ComfyUI (own user directory, port 8189)
// PageDriver's calls block in a nested event loop, so the sequences read top to bottom.
import QtQuick
import QtQuick.Controls
import com.kdab.dockwidgets 2.0 as KDDW
import SpikeShim

ApplicationWindow {
    id: root
    visible: true
    width: 1400
    height: 900
    title: "WebEngine spike"

    // Set from main.cpp.
    property string phase: ""
    property string viewUrl: "http://127.0.0.1:8188"
    property string fixturesDir: ""
    property string outDir: ""
    property int conversions: 10
    property int quitAfterMs: -1
    property string screenshotPath: ""

    readonly property var fixtureNames: ["zimage_t2i", "subgraph_muted"]
    property var view: webLoader.item
    property string viewLoadStatus: ""
    property bool quitting: false

    function elapsed() { return SpikeLog.elapsedMs() }

    function quitNow(reason) {
        if (quitting)
            return
        quitting = true
        if (screenshotPath !== "")
            SpikeLog.grabWindow(root, screenshotPath)
        SpikeLog.log("quit_requested", { reason: reason, download_attempts: PageDriver.downloadAttempts })
        Qt.quit()
    }

    // The fixture text with the IMG-128 stamp added before loading.
    function stamped(name) {
        const text = PageDriver.readTextFile(fixturesDir + "/" + name + ".ui.json")
        const g = JSON.parse(text)
        g.extra = g.extra || {}
        g.extra.worldwright = {
            source_ref: "spikes/webengine/fixtures/" + name + ".ui.json",
            base_hash: "sha256:" + PageDriver.sha256Hex(text)
        }
        return JSON.stringify(g)
    }

    function canonical(v) {
        if (Array.isArray(v))
            return "[" + v.map(canonical).join(",") + "]"
        if (v !== null && typeof v === "object")
            return "{" + Object.keys(v).sort().map(k => JSON.stringify(k) + ":" + canonical(v[k])).join(",") + "}"
        return JSON.stringify(v)
    }

    function isDone(flag) { return flag === "ok" || flag.indexOf("error:") === 0 }

    function waitViewReady(timeoutMs) {
        const t0 = elapsed()
        while (elapsed() - t0 < timeoutMs) {
            if (PageDriver.runInView("app_ready.js", [], 1000) === true)
                return elapsed() - t0
            PageDriver.sleep(250)
        }
        return -1
    }

    function tabs() {
        const s = PageDriver.runInView("tabs_state.js", [], 3000)
        return s ? JSON.parse(s) : null
    }

    function exportOnce(name, command, path) {
        const before = PageDriver.downloadAttempts
        const text = PageDriver.exportFromView(command, 10000)
        PageDriver.waitForDownloadAttempts(before + 1, 5000)
        PageDriver.sleep(300)
        if (text !== "")
            PageDriver.writeTextFile(path, text)
        SpikeLog.log("export", {
            fixture: name, command: command, flag: text !== "" ? "ok" : "no blob captured",
            bytes: text.length, attempts_before: before, attempts_after: PageDriver.downloadAttempts
        })
    }

    // ---- visible: criteria 1, 2, 3 (Export (API) side), 5, 6 ------------------------------
    function runVisible() {
        const readyMs = waitViewReady(90000)
        SpikeLog.log("view_ready", { ms: readyMs, url: view.url.toString() })
        if (readyMs < 0)
            return quitNow("view not ready")
        PageDriver.sleep(1500) // let the frontend settle its initial tab
        SpikeLog.log("tabs_initial", { tabs: tabs() })

        // Criterion 1, negative probe: loadGraph with arguments that are not plain base64 must be
        // refused by PageDriver ("argument rejected", nothing sent) and leave the page unchanged.
        const hostile = [
            { name: "quote", value: 'QUJD"+alert(1)+"' },
            { name: "backslash", value: "QUJD\\" },
            { name: "angle_bracket", value: "<script>QUJD" },
            { name: "raw_json", value: '{"nodes":[],"links":[]}' },
            { name: "trailing_newline", value: "QUJD\n" }
        ]
        const probeTabsBefore = tabs()
        const cases = []
        for (const h of hostile)
            cases.push({ name: h.name, request_id: PageDriver.loadGraph(h.value), chars: h.value.length })
        PageDriver.sleep(800)
        for (const c of cases)
            c.flag_after = PageDriver.pollFlag(c.request_id)
        const probeTabsAfter = tabs()
        SpikeLog.log("hostile_probe", {
            cases: cases, tabs_before: probeTabsBefore, tabs_after: probeTabsAfter,
            tabs_unchanged: JSON.stringify(probeTabsBefore) === JSON.stringify(probeTabsAfter)
        })

        for (const name of fixtureNames) {
            const text = stamped(name)
            PageDriver.writeTextFile(outDir + "/" + name + ".stamped.json", text)
            const before = tabs()
            const b64 = PageDriver.utf8ToBase64(text)
            const t0 = elapsed()
            const id = PageDriver.loadGraph(b64)
            let flag = ""
            while (elapsed() - t0 < 5000) {
                flag = PageDriver.pollFlag(id)
                if (isDone(flag))
                    break
                PageDriver.sleep(25)
            }
            const flagMs = elapsed() - t0
            PageDriver.sleep(300)
            const after = tabs()
            const graph = PageDriver.readGraph()
            PageDriver.writeTextFile(outDir + "/" + name + ".readgraph.json", graph)
            SpikeLog.log("load_graph", {
                fixture: name, request_id: id, flag: flag, flag_ms: flagMs,
                base64_chars: b64.length,
                tabs_before: before, tabs_after: after, readgraph_chars: graph.length
            })
            exportOnce(name, "Comfy.ExportWorkflowAPI", outDir + "/" + name + ".export_api.json")
        }
        exportOnce("subgraph_muted", "Comfy.ExportWorkflow", outDir + "/subgraph_muted.export_ui.json")
        SpikeLog.log("download_attempts_final", { download_attempts: PageDriver.downloadAttempts })
        SpikeLog.log("tabs_final", { tabs: tabs() })
        // Informational (criterion 4 context): a hidden conversion while the user's ComfyUI view is
        // open must not change that view's tabs.
        const tabsBeforeHidden = tabs()
        const hiddenOut = PageDriver.convertHidden(PageDriver.utf8ToBase64(stamped("zimage_t2i")))
        PageDriver.sleep(500)
        const tabsAfterHidden = tabs()
        SpikeLog.log("hidden_while_visible", {
            ok: hiddenOut !== "", tabs_before: tabsBeforeHidden, tabs_after: tabsAfterHidden,
            view_tabs_unchanged: JSON.stringify(tabsBeforeHidden) === JSON.stringify(tabsAfterHidden)
        })

        // Criterion 6: off-origin navigation and a popup.
        const urlBefore = view.url.toString()
        const probe = PageDriver.runInView("nav_probe.js", ["https://example.com/", "href"], 2000)
        PageDriver.sleep(2500)
        const originAfter = PageDriver.runInView("nav_probe.js", ["", "origin"], 2000)
        const alive = PageDriver.runInView("app_ready.js", [], 2000) === true
        const openResult = PageDriver.runInView("nav_probe.js", ["https://example.com/", "open"], 2000)
        PageDriver.sleep(1500)
        SpikeLog.log("nav_test", {
            target: "https://example.com/", probe: probe, view_url_before: urlBefore,
            view_url_after: view.url.toString(), page_origin_after: originAfter,
            app_alive_after: alive, window_open_result: openResult
        })
        quitNow("visible done")
    }

    // ---- hidden: criteria 3 (hidden side), 4, 6 (hidden page) -----------------------------
    function runHidden() {
        const b64 = {}
        for (const n of fixtureNames)
            b64[n] = PageDriver.utf8ToBase64(stamped(n))
        for (let i = 0; i < conversions; ++i) {
            const name = fixtureNames[i % fixtureNames.length]
            const t0 = elapsed()
            const out = PageDriver.convertHidden(b64[name])
            const ms = elapsed() - t0
            let canon = ""
            try { canon = canonical(JSON.parse(out)) } catch (e) { canon = "" }
            if (i < fixtureNames.length)
                PageDriver.writeTextFile(outDir + "/" + name + ".hidden.api.json", out)
            SpikeLog.log("convert_hidden", {
                index: i, fixture: name, ms: ms, ok: out !== "", chars: out.length,
                canonical_sha256: canon === "" ? "" : PageDriver.sha256Hex(canon)
            })
        }
        SpikeLog.log("nav_test_hidden", PageDriver.hiddenNavigationTest("https://example.com/", 3000))
        quitNow("hidden done")
    }

    // ---- down: criterion 7 -------------------------------------------------------------------
    // ---- firstboot: one hidden conversion against a scratch ComfyUI (own --user-directory) -----
    // Shows what the frontend's first boot writes to a never-opened ComfyUI user folder.
    function runFirstboot() {
        const out = PageDriver.convertHidden(PageDriver.utf8ToBase64(stamped("zimage_t2i")))
        PageDriver.sleep(2000) // let debounced settings writes (500 ms) land
        SpikeLog.log("firstboot_convert", { ok: out !== "", chars: out.length, origin: PageDriver.origin.toString() })
        quitNow("firstboot done")
    }

    function runDown() {
        const t0 = elapsed()
        while (viewLoadStatus === "" && elapsed() - t0 < 20000)
            PageDriver.sleep(100)
        PageDriver.sleep(500)
        const b64 = PageDriver.utf8ToBase64(stamped("zimage_t2i"))
        const c0 = elapsed()
        const id = PageDriver.loadGraph(b64)
        let flag = ""
        while (elapsed() - c0 < 5000) {
            flag = PageDriver.pollFlag(id)
            if (isDone(flag))
                break
            PageDriver.sleep(50)
        }
        const waited = elapsed() - c0
        if (!isDone(flag))
            SpikeLog.log("load_graph_timeout", {
                request_id: id, waited_ms: waited,
                message: "loadGraph " + id + ": no result flag from the page after 5 s; ComfyUI unreachable?"
            })
        SpikeLog.log("load_graph_down", { request_id: id, flag: flag, waited_ms: waited, view_load_status: viewLoadStatus })
        PageDriver.sleep(3000) // keep the event loop observable (heartbeats) after the failure
        quitNow("down done")
    }

    KDDW.DockingArea {
        id: dockingArea
        anchors.fill: parent
        uniqueName: "WebSpikeLayout"

        KDDW.DockWidget {
            id: webDock
            uniqueName: "web"
            title: "ComfyUI"

            Item {
                anchors.fill: parent
                Loader {
                    id: webLoader
                    anchors.fill: parent
                    source: "WebDock.qml"
                    onLoaded: {
                        SpikeLog.log("webengine_loaded", { view_url: root.viewUrl })
                        item.url = root.viewUrl
                    }
                    onStatusChanged: if (status === Loader.Error)
                        SpikeLog.log("error", { what: "WebDock.qml failed to load" })
                }
                Connections {
                    target: webLoader.item
                    function onLoadDone(status) { root.viewLoadStatus = status }
                }
            }
        }

        Component.onCompleted: addDockWidget(webDock, KDDW.KDDockWidgets.Location_OnLeft)
    }

    Timer {
        interval: 1000
        running: root.phase !== ""
        onTriggered: {
            SpikeLog.log("phase_start", { phase: root.phase })
            if (root.phase === "visible") root.runVisible()
            else if (root.phase === "hidden") root.runHidden()
            else if (root.phase === "down") root.runDown()
            else if (root.phase === "firstboot") root.runFirstboot()
            else root.quitNow("unknown phase " + root.phase)
        }
    }

    Timer {
        interval: Math.max(0, root.quitAfterMs)
        running: root.quitAfterMs >= 0
        onTriggered: root.quitNow("quit-after")
    }
}
