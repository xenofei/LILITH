//! Fixes the build id suffix at compile time: `WW_BUILD_ID`, or `local` when it is unset or
//! empty. `session::BUILD_ID` is `<CARGO_PKG_VERSION>+<suffix>`.

fn main() {
    println!("cargo::rerun-if-env-changed=WW_BUILD_ID");
    let suffix = std::env::var("WW_BUILD_ID")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "local".to_string());
    // Semver build-metadata characters only, so the id is safe in every place it is printed.
    assert!(
        suffix
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-'),
        "WW_BUILD_ID may contain only ASCII letters, digits, '.' and '-'"
    );
    println!("cargo::rustc-env=WW_PROTOCOL_BUILD_SUFFIX={suffix}");
}
