// The queue (ARCH-121, QA-083): how many jobs run and wait, and each lane's pause. It reads
// `jobs.list` for each status, at most one page, and follows `jobs`: a `queue` event is a lane's
// full state, and a job's change reads the counts again, at most once a second. The Jobs panel is
// where you act on them.
import QtQuick
import Lilith.Core
import Lilith.UiKit
import "DiagnosticsText.js" as Words

LCard {
    id: root

    required property var core

    readonly property int pageSize: 100

    // lane -> QueueState.
    property var lanes: ({})

    function applyLanes(states) {
        const next = Object.assign({}, lanes)
        for (const s of states || []) {
            if (s && s.lane)
                next[s.lane] = s
        }
        lanes = next
    }

    function laneLine(state) {
        const known = {"gpu-heavy": "GPU work", cpu: "CPU work"}
        const name = known[state.lane] || Words.shown(core, state.lane, 60)
        if (!state.paused && !state.held)
            return name + ": not paused"
        const reasons = (state.reasons || []).map(r => r === "manual" ? "by you"
                                                    : r === "fullscreen" ? "while fullscreen"
                                                    : "by " + Words.shown(core, r, 60))
        const last = reasons.pop()
        const parts = [state.paused
                       ? "paused" + (last ? " " + (reasons.length ? reasons.join(", ") + " and " : "") + last : "")
                       : "not paused"]
        if (state.held > 0)
            parts.push(MoneyText.count(String(state.held)) + " held")
        return name + ": " + parts.join(", ")
    }

    function countOf(read) {
        const r = read.result
        if (!r)
            return Words.notKnown
        const n = MoneyText.count(String((r.jobs || []).length))
        return r.next_cursor ? n + " or more" : n
    }

    readonly property ToolRead runningRead: ToolRead {
        core: root.core
        tool: "jobs.list"
        args: ({status: "running", limit: root.pageSize})
        topic: "jobs"
        onResultChanged: root.applyLanes(result ? result.lanes : [])
        onTopicEvent: data => {
            if (data && data.kind === "queue" && data.state) {
                root.applyLanes([data.state])
            } else if (data && data.kind === "job") {
                root.runningRead.request()
                root.queuedRead.request()
            }
        }
    }

    readonly property ToolRead queuedRead: ToolRead {
        core: root.core
        tool: "jobs.list"
        args: ({status: "queued", limit: root.pageSize})
        onResultChanged: root.applyLanes(result ? result.lanes : [])
    }

    title: "Queue"

    Column {
        width: parent.width
        spacing: 2

        LineText {
            visible: text !== ""
            tone: "warning"
            text: Words.errorText(root.core, root.runningRead.error || root.queuedRead.error)
        }
        LineText {
            text: root.countOf(root.runningRead) + " running, " + root.countOf(root.queuedRead) + " queued"
        }
        Repeater {
            // GPU work first, as the Jobs panel has it.
            model: Object.keys(root.lanes).sort((a, b) => a === b ? 0 : a === "gpu-heavy" ? -1 : b === "gpu-heavy" ? 1
                                                        : a < b ? -1 : 1)
            delegate: LineText {
                required property var modelData
                text: root.laneLine(root.lanes[modelData])
            }
        }
    }
}
