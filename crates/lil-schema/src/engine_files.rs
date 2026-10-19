//! Files the core reads about engines and model files (Appendix F "ComfyUI client"): the
//! workspace's `registry/models.yaml` (I9), whose `files[].sha256` name model files by hash,
//! and a Python package's `pyproject.toml` version (a ComfyUI custom node's).

use serde_json::Value;
use yaml_rust2::{Yaml, YamlLoader};

use crate::manifest::bare_sha256;
use crate::workspace_yaml::to_json;

/// One entry of `registry/models.yaml` as the model inventory reads it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegistryModel {
    pub id: String,
    pub name: Option<String>,
    /// The bare lowercase hex of each `files[].sha256` that is a SHA-256.
    pub sha256: Vec<String>,
}

/// The entries of `registry/models.yaml` (a YAML sequence; an empty document has none). An
/// entry without a string `id` is left out; a `sha256` that isn't `<64 hex>` or
/// `sha256:<64 hex>` (either case) is left out.
///
/// # Errors
/// When the text isn't YAML, or its root isn't a sequence.
pub fn read_models_yaml(text: &str) -> Result<Vec<RegistryModel>, String> {
    let text = text.strip_prefix(crate::manifest::BOM).unwrap_or(text);
    let docs = YamlLoader::load_from_str(text)
        .map_err(|e| format!("registry/models.yaml is not valid YAML: {e}"))?;
    let entries = match docs.first() {
        None | Some(Yaml::Null) => return Ok(Vec::new()),
        Some(y @ Yaml::Array(_)) => to_json(y),
        Some(_) => return Err("registry/models.yaml is not a list".to_string()),
    };
    let text_of = |v: Option<&Value>| v.and_then(Value::as_str).map(str::to_string);
    Ok(entries
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|entry| {
            let id = text_of(entry.get("id"))?;
            let sha256 = entry
                .get("files")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(|f| f.get("sha256").and_then(Value::as_str))
                .filter_map(|h| bare_sha256(&h.to_ascii_lowercase()).map(str::to_string))
                .collect();
            Some(RegistryModel {
                id,
                name: text_of(entry.get("name")),
                sha256,
            })
        })
        .collect())
}

/// `[project] version` of a `pyproject.toml`, when it is a string; `None` for text that isn't
/// TOML or has none.
pub fn pyproject_version(text: &str) -> Option<String> {
    let table: toml::Table = text.parse().ok()?;
    table
        .get("project")?
        .get("version")?
        .as_str()
        .map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn models_yaml_entries_keep_their_sha256_files() {
        let hash = "AB".repeat(32);
        let text = format!(
            "- id: zimage-turbo\n  name: Z-Image Turbo\n  files:\n    - {{path: a, sha256: \"sha256:{hash}\"}}\n    - {{path: b, sha256: \"…\"}}\n- name: no id\n- id: bare\n"
        );
        let models = read_models_yaml(&text).unwrap();
        assert_eq!(
            models,
            [
                RegistryModel {
                    id: "zimage-turbo".into(),
                    name: Some("Z-Image Turbo".into()),
                    sha256: vec![hash.to_ascii_lowercase()],
                },
                RegistryModel {
                    id: "bare".into(),
                    name: None,
                    sha256: Vec::new(),
                },
            ]
        );
        assert_eq!(read_models_yaml(""), Ok(Vec::new()));
        assert!(read_models_yaml("a: 1").is_err());
    }

    #[test]
    fn pyproject_version_is_the_project_tables() {
        assert_eq!(
            pyproject_version("[project]\nname = \"x\"\nversion = \"1.2.3\"\n").as_deref(),
            Some("1.2.3")
        );
        assert_eq!(pyproject_version("[tool.x]\nversion = \"9\"\n"), None);
        assert_eq!(pyproject_version("not toml ["), None);
    }
}
