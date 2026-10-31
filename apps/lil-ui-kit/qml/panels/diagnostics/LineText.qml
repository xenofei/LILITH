// One line of a Diagnostics card: plain text, wrapped, read out as it is. What comes from outside
// reaches it through Core.shownText (P4a-R8).
import QtQuick
import Lilith.UiKit

Text {
    // "muted" or "warning" for a line that isn't the card's plain kind.
    property string tone

    width: parent ? parent.width : implicitWidth
    textFormat: Text.PlainText
    color: tone === "muted" ? Theme.textMuted : tone === "warning" ? Theme.warning : Theme.text
    font.pixelSize: Theme.fontSize
    wrapMode: Text.Wrap

    Accessible.role: Accessible.StaticText
    Accessible.name: text
}
