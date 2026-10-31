// One figure of the top bar (APP-006): role StaticText, `name` what a screen reader says (the text
// when it is empty). `kind` colours it: "", "ok", "warning" or "danger".
import QtQuick
import Lilith.UiKit

Text {
    property string name
    property string kind

    height: 28
    verticalAlignment: Text.AlignVCenter
    textFormat: Text.PlainText
    color: kind === "ok" ? Theme.ok : kind === "warning" ? Theme.warning
         : kind === "danger" ? Theme.danger : Theme.text
    font.pixelSize: Theme.fontSize

    Accessible.role: Accessible.StaticText
    Accessible.name: name !== "" ? name : text
}
