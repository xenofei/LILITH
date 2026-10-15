//! spike_bridge: the cxx-qt side of the Qt stack spike.
//!
//! QML element `SpikeCounter` (module `SpikeBridge`):
//! - property `count: int`
//! - invokable `increment()` (QML -> Rust), which changes `count` (Rust -> QML via countChanged)
//! - signal `rendererRecreated()`, emitted from QML when the canvas renderer is (re)initialised
//!   and logged by a `Connections` handler.

// The bridge uses no cxx-qt-lib types, but cxx-qt-build's generated initializer still calls
// cxx-qt-lib's crate init, so the crate must be linked into the staticlib.
extern crate cxx_qt_lib;

#[cxx_qt::bridge]
pub mod qobject {
    extern "RustQt" {
        #[qobject]
        #[qml_element]
        #[qproperty(i32, count)]
        type SpikeCounter = super::SpikeCounterRust;

        #[qinvokable]
        fn increment(self: Pin<&mut Self>);

        #[qsignal]
        #[cxx_name = "rendererRecreated"]
        fn renderer_recreated(self: Pin<&mut Self>);
    }
}

use core::pin::Pin;

#[derive(Default)]
pub struct SpikeCounterRust {
    count: i32,
}

impl qobject::SpikeCounter {
    pub fn increment(self: Pin<&mut Self>) {
        let next = *self.count() + 1;
        self.set_count(next);
    }
}
