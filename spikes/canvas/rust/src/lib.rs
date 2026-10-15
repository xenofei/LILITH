//! canvas_bridge: the Rust side of the canvas compositing spike (Task 3, IMG-036).
//!
//! `cpu_composite` is the CPU reference compositor. It implements the same formulas as
//! `shaders/composite.comp` and `shaders/downsample.comp`, and is called from canvas.exe to build
//! the golden reference the GPU read-back is compared with.

// cxx-qt-build's generated initializer calls cxx-qt-lib's crate init even though the bridge uses
// no cxx-qt-lib type, so the crate must be linked into the staticlib (Task 2 trap).
extern crate cxx_qt_lib;

pub mod cpu_composite;
