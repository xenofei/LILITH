// What QA-083 asks of Diagnostics that plan 4a can't show yet, said rather than left out: provider
// health arrives with the provider slice (plan 6b, account sync in the M1b series), and each
// Claude Code profile's status with Engine A (the M1b series, APP-123).
import QtQuick
import Lilith.UiKit

LCard {
    id: root

    title: "Providers and Claude Code"

    LineText {
        tone: "muted"
        text: "Provider health arrives once LILITH connects to providers itself, and each Claude Code "
              + "profile's status once it runs Claude Code itself. Both come in a later version of LILITH. "
              + "What your accounts have used so far is in Usage & spend."
    }
}
