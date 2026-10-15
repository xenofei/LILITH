//! `suite.toml`, the workspace registry in the suite's roaming data folder (ARCH-090).
//!
//! Plan 2 reads two keys: `default_workspace = '<path>'` and `[[workspaces]] path = '<path>'`.
//! Every other key, at the top level or inside a `[[workspaces]]` entry, is kept in `other`,
//! so a parse followed by a write loses nothing.

use serde::{Deserialize, Serialize};

/// The whole file. Unknown keys are preserved in `other`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SuiteToml {
    /// The workspace the core serves.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_workspace: Option<String>,
    /// Every registered workspace.
    #[serde(default)]
    pub workspaces: Vec<WorkspaceEntry>,
    #[serde(flatten)]
    pub other: toml::Table,
}

/// One `[[workspaces]]` entry. Unknown keys are preserved in `other`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorkspaceEntry {
    pub path: String,
    #[serde(flatten)]
    pub other: toml::Table,
}

impl SuiteToml {
    /// Parses the text of a `suite.toml`.
    pub fn parse(text: &str) -> Result<SuiteToml, toml::de::Error> {
        toml::from_str(text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEXT: &str = r#"
default_workspace = 'C:\work\Ünïcödé space'
theme = "dark"

[[workspaces]]
path = 'C:\work\Ünïcödé space'
pinned = true

[[workspaces]]
path = 'D:\other'

[future]
answer = 42
"#;

    #[test] // spec: ARCH-090
    fn suite_toml_round_trips_unknown_keys() {
        let suite = SuiteToml::parse(TEXT).expect("parse");
        assert_eq!(
            suite.default_workspace.as_deref(),
            Some(r"C:\work\Ünïcödé space")
        );
        let paths: Vec<&str> = suite.workspaces.iter().map(|w| w.path.as_str()).collect();
        assert_eq!(paths, [r"C:\work\Ünïcödé space", r"D:\other"]);
        assert_eq!(
            suite.workspaces[0].other["pinned"],
            toml::Value::Boolean(true)
        );
        assert!(suite.workspaces[1].other.is_empty());
        assert_eq!(suite.other["theme"].as_str(), Some("dark"));
        assert_eq!(suite.other["future"]["answer"].as_integer(), Some(42));
        assert!(!suite.other.contains_key("default_workspace"));
        assert!(!suite.other.contains_key("workspaces"));

        // Written back and read again, nothing is lost or changed.
        let written = toml::to_string(&suite).expect("serialise");
        assert_eq!(SuiteToml::parse(&written).expect("reparse"), suite);
        let original: toml::Table = TEXT.parse().unwrap();
        let rewritten: toml::Table = written.parse().unwrap();
        assert_eq!(rewritten, original, "{written}");
    }

    #[test] // spec: ARCH-090
    fn empty_suite_toml_has_no_workspaces() {
        let suite = SuiteToml::parse("").expect("parse");
        assert_eq!(suite.default_workspace, None);
        assert!(suite.workspaces.is_empty());
        assert!(suite.other.is_empty());
        assert!(SuiteToml::parse("[[workspaces]]\npinned = true\n").is_err());
        assert!(SuiteToml::parse("default_workspace = 3\n").is_err());
    }
}
