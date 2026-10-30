// A lane's header in the Jobs panel (APP-114, CORE-160): its name, its pause and the reasons for
// it (an unknown reason by its name), how many jobs it holds, "Waiting for GPU memory" while a
// queued job of it waits for VRAM, ComfyUI's outside jobs (the GPU lane), and its QueueControls
// (for a lane this build knows; another lane shows by its name, P4a-R40). Role Grouping, named "<lane>: <what it says>"; `text` is all it says, one line each.
import QtQuick
import Lilith.UiKit

Rectangle {
    id: root

    // The JobsPanel.
    required property var panel
    required property string lane
    // Its QueueState, or null before the first read.
    property var queueState: null

    readonly property bool waitingForVram: panel.vramLanes.indexOf(lane) >= 0
    readonly property string outsideText: {
        if (lane !== "gpu-heavy")
            return ""
        const o = panel.outsideJobs || {}
        const running = o.running || 0, queued = o.queued || 0
        const jobs = n => n === 1 ? "1 outside job" : n + " outside jobs"
        if (running > 0 && queued > 0)
            return "ComfyUI: " + jobs(running) + " running, " + queued + " queued"
        if (running > 0)
            return "ComfyUI: " + jobs(running) + " running"
        if (queued > 0)
            return "ComfyUI: " + jobs(queued) + " queued"
        return ""
    }
    readonly property var lines: {
        const out = []
        if (queueState && queueState.paused)
            out.push(panel.pausedLabel(queueState.reasons))
        if (queueState && queueState.held > 0)
            out.push(queueState.held === 1 ? "1 job held" : queueState.held + " jobs held")
        if (waitingForVram)
            out.push("Waiting for GPU memory")
        if (outsideText !== "")
            out.push(outsideText)
        return out
    }
    readonly property string text: [panel.laneLabel(lane)].concat(lines).join("\n")

    implicitHeight: column.implicitHeight + 2 * Theme.spacing
    color: Theme.bg
    radius: Theme.radius
    border.color: Theme.surfaceRaised
    border.width: 1

    Accessible.role: Accessible.Grouping
    Accessible.name: panel.laneLabel(lane) + (lines.length ? ": " + lines.join(". ") : "")

    Column {
        id: column
        x: Theme.spacing
        y: Theme.spacing
        width: parent.width - 2 * Theme.spacing
        spacing: 4

        Text {
            width: parent.width
            textFormat: Text.PlainText
            text: root.panel.laneLabel(root.lane)
            color: Theme.text
            font.pixelSize: Theme.fontSize + 1
            font.bold: true
            elide: Text.ElideRight
        }

        Repeater {
            model: root.lines
            Text {
                required property string modelData
                width: column.width
                textFormat: Text.PlainText
                text: modelData
                color: modelData.startsWith("Paused") || modelData.startsWith("Waiting") ? Theme.warning : Theme.textMuted
                font.pixelSize: Theme.fontSize
                wrapMode: Text.Wrap
            }
        }

        // `app/queue.*` names only the lanes this build knows.
        QueueControls {
            visible: root.panel.knownLanes.indexOf(root.lane) >= 0
            width: parent.width
            panel: root.panel
            lane: root.lane
            queueState: root.queueState
        }
    }
}
