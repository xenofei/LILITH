//! Shared by the property tests: the case count from `test-params.toml` `[proptest] cases`
//! (QA-019), read with a line scan because this crate has no TOML dependency.

use std::path::Path;

use proptest::test_runner::Config;

/// `[proptest] cases` of the workspace's `test-params.toml`.
pub fn cases() -> u32 {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../test-params.toml");
    let text = std::fs::read_to_string(&path).expect("read test-params.toml");
    let mut section = "";
    for line in text.lines().map(str::trim) {
        if line.starts_with('[') {
            section = line;
        } else if section == "[proptest]"
            && let Some((key, value)) = line.split_once('=')
            && key.trim() == "cases"
        {
            return value.trim().parse().expect("[proptest] cases is a number");
        }
    }
    panic!("test-params.toml has no [proptest] cases");
}

pub fn config() -> Config {
    Config::with_cases(cases())
}
