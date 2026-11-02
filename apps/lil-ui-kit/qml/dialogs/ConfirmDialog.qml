// The shared dialog (APP-044, APP-115, P4a-R10): one confirmation, what it asks and its answer. The
// panel shows it in a modal popup and the confirm window in its list. Deny (or a cap's Cancel) has the
// focus first, and Approve is never a default button: Approve, Allow once and Raise take only a real
// pointer click, and the note next to them says so. Once it isn't pending it says what became of it,
// with no buttons. A confirmation that covers a cap answers through the cap prompt (APP-045).
//
// What the owner reads is what a click approves. The action sits in a view with a real scroll bar, and
// the buttons that approve stay off until all of it has been on screen, with the reason next to them.
// One too long to show in full (lil_qt::confirm_text) can't be approved here at all; Deny always works.
// When anything that is shown changes, the gate re-arms (`shownKey`).
import QtQuick
import QtQuick.Controls.Basic as QQC
import Lilith.Core
import Lilith.UiKit
import "ConfirmText.js" as Words

Column {
    id: root

    required property var core
    required property string confirmationId
    // The ConfirmationView, or null when the list doesn't hold it.
    property var view: null
    // The list has loaded: a null view means it is unknown, and an answer may go out.
    property bool loaded: true
    property var pointerSource: ConfirmInput
    // A notification's Deny went out for it.
    property bool fromToast: false
    // How tall the action may be before it scrolls.
    property real cardMaxHeight: 360

    // The core's row after an answer given here.
    signal answered(var confirmation)

    readonly property bool pending: !!view && view.state === "pending"
    readonly property bool isCap: Words.coversCap(view)
    readonly property string shownKey: Words.fingerprint(view)
    // Every line of the action has been drawn in the view, from the top down with no gap: a jump
    // past part of it counts for nothing.
    readonly property bool seenAll: !scroller.overflows || scroller.seenTo >= scroller.contentHeight - 1
    readonly property int unseenPercent: seenAll ? 0
        : Math.max(1, Math.round(100 * (scroller.contentHeight - scroller.seenTo) / scroller.contentHeight))
    readonly property bool tooLong: !!view && (!view.shown || view.shown.over)
    readonly property bool approvable: pending && seenAll && !tooLong
    readonly property string blockedWhy: tooLong
        ? "This request is too long to show in full, so it can't be approved here. Deny works."
        : seenAll ? ""
        : scroller.contentY > scroller.seenTo
          ? "You jumped past part of the request. Scroll back up to where you stopped: " + unseenPercent + "% hasn't been on screen yet."
          : "Scroll through all of the request: " + unseenPercent + "% hasn't been on screen yet. This turns on once every line has been."
    // AnswerBar's.
    property bool bannerBehind: false
    readonly property alias header: header
    readonly property alias card: card
    readonly property alias scroller: scroller
    readonly property alias bar: bar
    readonly property alias denyButton: denyButton
    readonly property alias approveButton: approveButton
    readonly property alias capPrompt: capPromptLoader.item
    readonly property alias stateLine: stateLine
    readonly property alias moreBelow: moreBelow
    readonly property alias blockedLine: blockedLine

    spacing: Theme.spacing

    function focusDeny() {
        if (isCap && capPromptLoader.item)
            capPromptLoader.item.focusCancel()
        else
            denyButton.forceActiveFocus(Qt.TabFocusReason)
    }

    // The bar that answers here: an answer can go.
    readonly property bool live: isCap && capPromptLoader.item ? capPromptLoader.item.bar.live : bar.live
    property bool _wasLive: false
    // It could answer, couldn't for a while, and can again.
    signal liveAgain()
    onLiveChanged: {
        if (live && _wasLive)
            liveAgain()
        if (live)
            _wasLive = true
    }

    // Qt gives the keyboard back to a control that had it when that control comes back on. This is for
    // when nothing has it by then (the panel's Close held it, and has gone): Deny, never Approve, and
    // only while this window has the keyboard and nothing in the dialog holds it.
    function focusDenyIfLost() {
        const window = root.Window.window
        if (!window || !window.active)
            return
        for (let item = window.activeFocusItem; item; item = item.parent) {
            if (item === root)
                return
        }
        focusDeny()
    }

    onShownKeyChanged: scroller.startAgain()
    // The dialog moved in its column: the buttons with it.
    onYChanged: {
        bar.restartClock()
        if (capPromptLoader.item)
            capPromptLoader.item.bar.restartClock()
    }

    // Who asks and for what stay in sight whatever is scrolled.
    Column {
        id: header
        width: parent.width
        spacing: Theme.spacing / 2
        visible: !!root.view

        Text {
            width: parent.width
            text: card.actionTitle
            textFormat: Text.PlainText
            color: Theme.text
            font.pixelSize: Theme.fontSize + 1
            font.bold: true
            wrapMode: Text.WrapAnywhere
            Accessible.role: Accessible.StaticText
            Accessible.name: text
        }

        Text {
            width: parent.width
            text: card.factsText
            textFormat: Text.PlainText
            color: Theme.textMuted
            font.pixelSize: Theme.fontSize
            wrapMode: Text.Wrap
            Accessible.role: Accessible.StaticText
            Accessible.name: text
        }
    }

    Flickable {
        id: scroller
        readonly property bool overflows: contentHeight > height + 1
        readonly property bool atEnd: contentY + height >= contentHeight - 1
        // How far down every line has been drawn in the view, without a gap.
        property real seenTo: 0
        property real _lastHeight: 0

        function note() {
            if (contentY <= seenTo)
                seenTo = Math.max(seenTo, contentY + height)
        }

        // Nothing of what is shown now counts as seen but what the view holds at its top.
        function startAgain() {
            contentY = 0
            seenTo = height
        }

        width: parent.width
        height: Math.min(card.implicitHeight, root.cardMaxHeight)
        contentWidth: width
        contentHeight: card.implicitHeight
        clip: true
        boundsBehavior: Flickable.StopAtBounds
        interactive: overflows
        onContentYChanged: note()
        // A shorter view or a new width lays the lines out differently from what was covered.
        onWidthChanged: startAgain()
        onHeightChanged: {
            if (height < _lastHeight)
                startAgain()
            else
                note()
            _lastHeight = height
        }

        QQC.ScrollBar.vertical: QQC.ScrollBar {
            policy: scroller.overflows ? QQC.ScrollBar.AlwaysOn : QQC.ScrollBar.AlwaysOff
        }

        ConfirmationCard {
            id: card
            // Room for the scroll bar whether it shows or not: the card's height follows its width.
            width: scroller.width - 16
            height: implicitHeight
            core: root.core
            confirmationId: root.confirmationId
            view: root.view
            factsOutside: true
        }
    }

    // One line for as long as it scrolls, so the buttons below it stay where they are.
    Text {
        id: moreBelow
        width: parent.width
        visible: scroller.overflows
        text: scroller.contentY <= 0 ? "More below."
            : scroller.atEnd ? "More above."
            : "More above and below."
        textFormat: Text.PlainText
        color: Theme.warning
        font.pixelSize: Theme.fontSize
        Accessible.role: Accessible.StaticText
        Accessible.name: text
    }

    Text {
        id: stateLine
        width: parent.width
        visible: text !== ""
        text: !root.view ? (root.loaded ? "LILITH doesn't know this confirmation: it may have ended long ago, or belong to a workspace that isn't open." : "")
            : root.pending ? (card.ranOut ? "This confirmation has run out of time." : "")
            : Words.outcome(root.view, root.fromToast)
        textFormat: Text.PlainText
        color: Theme.text
        font.pixelSize: Theme.fontSize
        wrapMode: Text.Wrap
        Accessible.role: Accessible.StaticText
        Accessible.name: text
    }

    AnswerBar {
        id: bar
        width: parent.width
        visible: root.pending && !root.isCap
        core: root.core
        confirmationId: root.confirmationId
        shownKey: root.shownKey
        open: root.pending && root.loaded && !card.ranOut
        bannerBehind: root.bannerBehind
        onAnswered: confirmation => root.answered(confirmation)

        DenyButton {
            id: denyButton
            text: "Deny"
            requiresCore: true
            enabled: bar.buttonsOn
            onActivated: kind => bar.deny(kind, denyButton)
        }

        ApproveButton {
            id: approveButton
            text: "Approve"
            targetKey: root.shownKey
            pointerSource: root.pointerSource
            requiresCore: true
            enabled: bar.buttonsOn && root.approvable
            onClicked: bar.approve(text)
            onRefused: name => bar.refuse(name, text)
        }
    }

    Text {
        id: blockedLine
        width: parent.width
        visible: bar.visible && text !== ""
        text: root.blockedWhy
        textFormat: Text.PlainText
        color: Theme.warning
        font.pixelSize: Theme.fontSize
        wrapMode: Text.Wrap
        Accessible.role: Accessible.StaticText
        Accessible.name: text
    }

    Text {
        width: parent.width
        visible: bar.visible
        text: "Approve needs a click from your mouse or touchpad. Deny works from the keyboard."
        textFormat: Text.PlainText
        color: Theme.textMuted
        font.pixelSize: Theme.fontSize
        wrapMode: Text.Wrap
    }

    Loader {
        id: capPromptLoader
        width: parent.width
        active: root.pending && root.isCap
        sourceComponent: CapPrompt {
            width: capPromptLoader.width
            core: root.core
            confirmationId: root.confirmationId
            cap: root.view ? root.view.cap : []
            pointerSource: root.pointerSource
            open: !card.ranOut && root.loaded
            bannerBehind: root.bannerBehind
            followTopic: false
            shownKey: root.shownKey
            alsoApproves: Words.coversConfirm(root.view) ? ConfirmInput.shownLine(String(root.view.tool), 60) : ""
            approvable: root.approvable
            blockedWhy: root.blockedWhy
            onAnswered: confirmation => root.answered(confirmation)
        }
    }
}
