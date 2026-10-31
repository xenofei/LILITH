// Engine health and VRAM (ARCH-121, QA-083), the launch profile and the asset catalogue (ARCH-126,
// ARCH-065) and what changed in the engine's environment (CORE-130, CORE-166). It reads
// `engines.status` and `engines.environment` and follows `engines`, whose events each carry an
// engine's full state; an engine becoming ready reads the environment again, since that is when
// one is recorded. Nothing an engine reports is trusted text (P4a-R8).
import QtQuick
import Lilith.Core
import Lilith.UiKit
import "DiagnosticsText.js" as Words
import "../hardware/HardwareFormat.js" as Format

LCard {
    id: root

    required property var core
    // The telemetry snapshot's GPU, or null.
    property var gpu: null
    // Whether a telemetry snapshot has been read, so "no GPU" isn't said before.
    property bool gpuRead: false

    property var engines: []

    readonly property string vramText: {
        if (!gpuRead)
            return "GPU memory: " + Words.notKnown
        if (!gpu)
            return "No GPU reading"
        if (!gpu.measured)
            return "GPU memory: not measured"
        return "GPU memory " + Format.gibOf(gpu.vram_used_mib, gpu.vram_total_mib)
    }

    function shown(text, maxChars) {
        return Words.shown(core, text, maxChars)
    }

    // The head line: "ComfyUI 0.42.1 · owned · ready".
    function headLine(e) {
        const name = e.engine === "comfyui" ? "ComfyUI" : shown(e.engine, 40)
        const version = e.version ? " " + shown(e.version, 80) : " (version not known)"
        const parts = [name + version]
        if (e.class !== "none")
            parts.push(shown(e.class, 40))
        parts.push(shown(e.state, 40))
        return parts.join(" · ")
    }

    // The lines under it: {text, tone}.
    function detailLines(e) {
        const lines = []
        const add = (text, tone) => lines.push({text: text, tone: tone || ""})
        if (e.url) {
            if (e.listen === "all_interfaces")
                add("Listens on every network address: " + shown(e.url, 200), "warning")
            else if (e.listen === "loopback")
                add("Listens on this PC only: " + shown(e.url, 200))
            else
                add("Where it listens is not known: " + shown(e.url, 200), "muted")
        }
        if (e.suite_job)
            add("Running the suite's job " + shown(e.suite_job, 60))
        const outside = e.outside_jobs || {}
        if (outside.running > 0 || outside.queued > 0)
            add("Outside the suite: " + MoneyText.count(String(outside.running || 0)) + " running, "
                + MoneyText.count(String(outside.queued || 0)) + " queued")
        if (e.restart_needed)
            add("A node pack was installed: ComfyUI needs a restart.", "warning")
        if (e.inventory)
            add(MoneyText.count(String(e.inventory.models)) + " model files, read "
                + Words.when(core, e.inventory.refreshed))
        const profile = e.launch_profile
        if (profile) {
            add("Launch profile: keeps " + shown(profile.reserve_vram_gb, 20) + " GB of GPU memory free, previews "
                + shown(profile.preview_method, 40) + ", Manager " + (profile.manager ? "on" : "off")
                + ", dynamic VRAM " + (profile.dynamic_vram ? "allowed" : "off"))
            add("Launch flags: " + shown((profile.flags || []).join(" "), 400))
        } else if (e.class === "attached") {
            add("An attached ComfyUI keeps its own launch settings.", "muted")
        } else {
            add("No launch profile: ComfyUI isn't configured.", "muted")
        }
        if (e.asset_catalogue === "on")
            add("ComfyUI's asset catalogue is on", "warning")
        else if (e.asset_catalogue === "off")
            add("ComfyUI's asset catalogue is off")
        else
            add("ComfyUI's asset catalogue is unknown", "muted")
        return lines
    }

    function changeLine(c) {
        const name = c.what === "node" ? shown(c.name, 120) : c.what === "driver" ? "GPU driver" : "ComfyUI"
        if (c.change === "added")
            return name + ": added, " + shown(c.to, 120)
        if (c.change === "removed")
            return name + ": removed, was " + shown(c.from, 120)
        return name + ": " + shown(c.from, 120) + " → " + shown(c.to, 120)
    }

    function upsert(engine) {
        const next = root.engines.slice()
        const at = next.findIndex(e => e.engine === engine.engine)
        if (at >= 0)
            next[at] = engine
        else
            next.push(engine)
        root.engines = next
    }

    readonly property ToolRead statusRead: ToolRead {
        core: root.core
        tool: "engines.status"
        topic: "engines"
        onResultChanged: root.engines = result && result.engines ? result.engines : []
        onTopicEvent: data => {
            if (!data || !data.engine)
                return
            const before = root.engines.find(e => e.engine === data.engine.engine)
            root.upsert(data.engine)
            if (data.engine.state === "ready" && (!before || before.state !== "ready"))
                root.environmentRead.request()
        }
    }

    readonly property ToolRead environmentRead: ToolRead {
        core: root.core
        tool: "engines.environment"
    }

    readonly property var environment: environmentRead.result
    readonly property var changes: environment && environment.changes ? environment.changes : []
    readonly property var captures: environment && environment.captures ? environment.captures : []

    title: "Engines"

    Column {
        width: parent.width
        spacing: 2

        LineText {
            visible: text !== ""
            tone: "warning"
            text: Words.errorText(root.core, root.statusRead.error)
        }
        LineText {
            visible: root.statusRead.loaded && root.engines.length === 0
            tone: "muted"
            text: "The core manages no engine."
        }

        Repeater {
            model: root.engines
            delegate: Column {
                required property var modelData
                width: parent.width
                spacing: 2

                LineText {
                    text: root.headLine(parent.modelData)
                    font.bold: true
                }
                Repeater {
                    model: root.detailLines(parent.modelData)
                    delegate: LineText {
                        required property var modelData
                        text: modelData.text
                        tone: modelData.tone
                    }
                }
            }
        }

        LineText { text: root.vramText }

        LineText {
            visible: root.environmentRead.loaded && root.captures.length === 0
            tone: "muted"
            text: "No engine environment recorded yet."
        }
        LineText {
            visible: root.captures.length > 0
            text: {
                const at = Words.when(root.core, root.captures.length ? root.captures[0].captured : "")
                if (root.changes.length > 0)
                    return "Changed in the engine's environment, as of " + at + ". The suite doesn't restore engines."
                return "Engine environment recorded " + at + (root.captures.length > 1 ? ", the same as the one before." : ".")
            }
        }
        Repeater {
            model: root.changes
            delegate: LineText {
                required property var modelData
                text: root.changeLine(modelData)
            }
        }
    }
}
