// The telemetry summary (ARCH-121, APP-134): the hardware now, from `telemetry.snapshot` and the
// `telemetry` topic. Each `sample` event carries the whole snapshot and replaces it, unless it is
// older than the one shown. The lines are fixed items bound to the snapshot, updated in place and
// never rebuilt. The Hardware panel has the rest. A figure that isn't measured says so.
import QtQuick
import Lilith.UiKit
import "DiagnosticsText.js" as Words
import "../hardware/HardwareFormat.js" as Format

LCard {
    id: root

    required property var core

    property var snapshot: null

    readonly property var gpu: snapshot ? snapshot.gpu || null : null
    readonly property bool read: snapshot !== null

    function apply(next) {
        if (next && (!snapshot || !(next.ts < snapshot.ts)))
            snapshot = next
    }

    readonly property ToolRead snapshotRead: ToolRead {
        core: root.core
        tool: "telemetry.snapshot"
        topic: "telemetry"
        onResultChanged: root.apply(result)
        onTopicEvent: data => {
            if (data && data.kind === "sample")
                root.apply(data.snapshot)
        }
    }

    title: "Telemetry"

    Column {
        width: parent.width
        spacing: 2

        LineText {
            visible: text !== ""
            tone: "warning"
            text: Words.errorText(root.core, root.snapshotRead.error)
        }
        LineText {
            visible: root.read
            text: {
                const g = root.gpu
                if (!g)
                    return "No GPU reading"
                const name = Words.shown(root.core, g.name, 80)
                return g.measured ? name + ": " + Format.percent(g.util_pct) + " load, " + g.temp_c + " °C"
                                  : name + ": not measured"
            }
        }
        LineText {
            visible: root.read
            text: {
                const cpu = root.snapshot ? root.snapshot.cpu : null
                return cpu ? "CPU " + Format.percent(cpu.util_permille / 10) + " load" : "CPU: " + Words.notKnown
            }
        }
        LineText {
            visible: root.read
            text: {
                const ram = root.snapshot ? root.snapshot.ram : null
                return ram ? "RAM " + Format.gibOf(ram.used_mib, ram.total_mib) : "RAM: " + Words.notKnown
            }
        }
        LineText {
            visible: root.read
            text: {
                const mw = root.snapshot ? root.snapshot.system_power_mw : 0
                return mw > 0 ? "Whole system " + Format.watts(mw) + ", estimated" : "Whole system: unknown"
            }
        }
        LineText {
            visible: root.read
            tone: text.startsWith("No ") ? "" : "warning"
            text: {
                const n = root.snapshot ? (root.snapshot.alerts || []).length : 0
                return n === 0 ? "No hardware alerts" : n === 1 ? "1 hardware alert" : n + " hardware alerts"
            }
        }
    }
}
