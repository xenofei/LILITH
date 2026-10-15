use cxx_qt_build::CxxQtBuilder;

fn main() {
    // No QML module: the bridge only exposes the CPU reference compositor to C++
    // (generated header: canvas_bridge/src/cpu_composite.cxx.h).
    CxxQtBuilder::new().file("src/cpu_composite.rs").build();
}
