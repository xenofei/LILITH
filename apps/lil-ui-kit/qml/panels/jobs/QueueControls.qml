// A lane's queue controls (APP-114, CORE-160; P4a-R41): "Pause", or "Resume" while your own
// pause (`manual`) is set, named "Pause GPU work" and "Resume GPU work" (`app/queue.pause` and
// `app/queue.resume {lane}`, which clears only your pause); and "Resume held jobs (<n>)"
// whenever a restart holds any of its jobs, whatever `paused` says, which sends
// `app/queue.resume {lane, held_only: true}` and so leaves your pause as it is. They wait for
// the panel's first load and for a call of their lane that is out, so a double-click can't pause
// and then resume. Ctrl+Shift+P is the top bar's (Task 20), not bound here.
import QtQuick
import Lilith.UiKit

Flow {
    id: root

    // The JobsPanel.
    required property var panel
    required property string lane
    property var queueState: null

    readonly property bool yourPause: !!queueState && (queueState.reasons || []).indexOf("manual") >= 0
    readonly property int restartHeld: queueState ? queueState.restart_held || 0 : 0
    readonly property bool free: panel.writable && !panel.busyLanes[lane]

    spacing: Theme.spacing

    LButton {
        id: pauseButton
        requiresCore: true
        text: root.yourPause ? "Resume" : "Pause"
        Accessible.name: text + " " + root.panel.laneLabel(root.lane)
        enabled: root.free
        onClicked: root.yourPause ? root.panel.resume(root.lane) : root.panel.pause(root.lane)
    }

    LButton {
        requiresCore: true
        visible: root.restartHeld > 0
        text: "Resume held jobs (" + root.restartHeld + ")"
        enabled: root.free
        onClicked: root.panel.resumeHeld(root.lane)
    }
}
