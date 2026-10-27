#pragma once
// C++ helpers of the Core bridge (src/qt/core_object.rs): JSON text as a QVariant, and a Core
// made outside a QML engine (the e2e tests').

#include <QtCore/QVariant>

#include <memory>

#include "rust/cxx.h"

class Core;

namespace lil_qt {

// An object is a QVariantMap, an array a QVariantList; null or text that isn't JSON is an
// invalid QVariant, which QML reads as null.
QVariant jsonVariant(rust::Str json);

// A new Core with no parent.
std::unique_ptr<Core> newCore();

} // namespace lil_qt
