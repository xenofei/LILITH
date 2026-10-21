//! The suite repo's `.claude/settings.json` keeps the agents that build the suite from changing
//! a real `suite.toml` (spec ARCH-056, CORE-008; plan 3 Global Constraints, protected files).
//!
//! Spec Appendix B.3 (verified 2026-10-18) records that `Edit(...)` path rules cover the Write
//! tool and that `Write(path)` rules are never consulted, so the file has no `Write(` rule; the
//! write-like shell commands come as a Bash and a PowerShell twin each. How the rules match is
//! `crates/lil-testkit/tests/claude_settings_patterns.rs`'s business; this test checks that
//! they are there.

use std::path::Path;

use serde_json::Value;

fn deny_rules() -> Vec<String> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.claude/settings.json");
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()));
    let settings: Value = serde_json::from_str(&text).expect(".claude/settings.json is JSON");
    settings["permissions"]["deny"]
        .as_array()
        .expect("permissions.deny is an array")
        .iter()
        .map(|r| r.as_str().expect("every rule is a string").to_string())
        .collect()
}

#[test] // spec: ARCH-056, CORE-008
fn repo_settings_deny_suite_toml_edits() {
    let deny = deny_rules();
    let has = |rule: &str| deny.iter().any(|r| r == rule);
    assert!(has("Edit(//**/LILITH*/suite.toml)"), "{deny:?}");
    for body in [
        "*>*LILITH*suite.toml*",
        "*Set-Content*LILITH*suite.toml*",
        "*Add-Content*LILITH*suite.toml*",
    ] {
        for shell in ["Bash", "PowerShell"] {
            let rule = format!("{shell}({body})");
            assert!(has(&rule), "missing {rule}: {deny:?}");
        }
    }
    assert!(
        !deny.iter().any(|r| r.starts_with("Write(")),
        "B.3: Edit rules cover the Write tool, so no Write( rule: {deny:?}"
    );
}
