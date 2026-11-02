// How many confirm dialogs and cap prompts are open in this process (P4a-R10). The shell's app-wide
// shortcuts fire under a modal popup, so they read this and do nothing while one is open.
pragma Singleton
import QtQml

QtObject {
    property int count: 0
}
