// CoreCall's watch on a call's owner (CoreCall.js): made as the owner's child, it is destroyed
// with the owner and then calls `gone`, which cancels the call.
import QtQml

QtObject {
    property var gone: null

    Component.onDestruction: if (gone) gone()
}
