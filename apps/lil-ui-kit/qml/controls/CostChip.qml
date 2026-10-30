// The cost and time chip (APP-140, CORE-161): what an action that spends GPU time or money is
// estimated to cost, before you click: "local · ~8 s · ~2 Wh", "≈ 0.04 USD · 3 % of cap" or
// "no estimate yet" (JobText.chipText). It informs only; caps and confirmations gate as before.
//
//   CostChip { core: Core; tool: "comfy.prompt"; args: {...} }   // asks job.estimate
//   CostChip { estimate: job.estimate; caps: capsView }          // shows a given estimate
//
// With `tool` set, it asks `job.estimate {tool, args}` once, `debounceMs` (300 ms) after the last
// change of `tool` or `args`; an answer to an older call is dropped, and a failed call reads as
// no estimate. Without `tool` it shows `estimate`. `caps` is a `caps.get` result, for the share
// of the monthly cap a `money` part is; the panel that shows the chip keeps it current.
// Role StaticText, named "Estimate: <text>".
import QtQuick
import Lilith.Core
import Lilith.UiKit

Rectangle {
    id: root

    property var core: null
    property string tool
    property var args: ({})
    property var estimate: null
    property var caps: null
    property int debounceMs: 300

    // The estimate the chip shows.
    readonly property var shown: tool !== "" ? _fetched : estimate
    readonly property string text: {
        const json = JSON.stringify(shown || null)
        const share = caps ? JobText.capSharePermille(json, JSON.stringify(caps)) : -1
        return JobText.chipText(json, share)
    }

    property var _fetched: null
    property var _call: null

    implicitWidth: label.implicitWidth + 2 * Theme.spacing
    implicitHeight: label.implicitHeight + 6
    radius: height / 2
    color: Theme.surfaceRaised
    border.color: Theme.bg
    border.width: 1

    Accessible.role: Accessible.StaticText
    Accessible.name: "Estimate: " + text

    function _changed() {
        if (_call) {
            _call.cancel()
            _call = null
        }
        _fetched = null
        if (tool !== "")
            debounce.restart()
        else
            debounce.stop()
    }

    function _ask() {
        if (!core || tool === "")
            return
        _call = CoreCall.call(core, "job.estimate", {tool: tool, args: args || {}}, {
            owner: root,
            ok: result => {
                root._call = null
                root._fetched = result.structured || null
            },
            error: () => {
                root._call = null
                root._fetched = null
            }
        })
    }

    onToolChanged: _changed()
    onArgsChanged: _changed()
    onCoreChanged: _changed()
    Component.onCompleted: _changed()

    Timer {
        id: debounce
        interval: root.debounceMs
        onTriggered: root._ask()
    }

    Text {
        id: label
        anchors.centerIn: parent
        text: root.text
        color: Theme.text
        font.pixelSize: Theme.fontSize - 1
    }
}
