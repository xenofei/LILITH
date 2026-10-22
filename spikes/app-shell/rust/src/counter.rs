//! `SpikeCounter`: `count` and `increment()` (QML -> Rust -> QML).

#[cxx_qt::bridge]
pub mod qobject {
    extern "RustQt" {
        #[qobject]
        #[qml_element]
        #[qproperty(i32, count)]
        type SpikeCounter = super::SpikeCounterRust;

        #[qinvokable]
        fn increment(self: Pin<&mut Self>);
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
