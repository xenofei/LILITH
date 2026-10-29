// Claude Code (APP-123): each profile's 5-hour and weekly usage, reset times and "last seen"
// times come from Engine A's stream and its `/usage` checks, which arrive with Engine A in the
// M1b series. Until then the section is empty and says so; calls Claude Code makes through
// LILITH's tools are in the breakdown, under the caller "Claude Code".
import QtQuick
import Lilith.UiKit

LCard {
    id: root

    // The profiles shown: none until Engine A.
    readonly property int rowCount: 0

    title: "Claude Code"

    Text {
        width: parent.width
        text: "Nothing to show yet. Each profile's 5-hour and weekly usage and its reset times show "
              + "here once LILITH runs Claude Code itself (Engine A). Claude Code's calls to LILITH's "
              + "tools are in the breakdown, under the caller Claude Code."
        color: Theme.textMuted
        font.pixelSize: Theme.fontSize
        wrapMode: Text.Wrap
        Accessible.role: Accessible.StaticText
        Accessible.name: text
    }
}
