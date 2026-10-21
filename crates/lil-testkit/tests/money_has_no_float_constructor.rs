//! QA-025: `Money` has no float constructor. A scan of `lil-schema`'s `money.rs`: no floating
//! point type appears in it at all, code or comment, so no conversion from one can creep in.

use std::path::Path;

#[test] // spec: QA-025, CORE-013
fn money_has_no_float_constructor() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../lil-schema/src/money.rs");
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    assert!(
        text.contains("pub struct Money(i64)"),
        "Money is not an i64 newtype"
    );
    let floats: Vec<(usize, &str)> = text
        .lines()
        .enumerate()
        .filter(|(_, line)| line.contains("f32") || line.contains("f64"))
        .map(|(n, line)| (n + 1, line.trim()))
        .collect();
    assert!(
        floats.is_empty(),
        "money.rs names a floating point type (QA-025): {floats:?}"
    );
}
