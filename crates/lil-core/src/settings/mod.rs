//! The settings cascade (CORE-057, CORE-054, CORE-049; I2, I4; Appendix E "Settings cascade").
//!
//! Three layers, a plain override chain over dotted leaf keys:
//! 1. the workspace file's (`lilith.yaml`, or the legacy one) `defaults:`;
//! 2. the project's `project.yaml` top-level keys (there is no `overrides:` key);
//! 3. the request layer: the `settings` argument of a tool whose input schema declares one
//!    (in plan 2b only `test.settings`; every product tool has no request layer).
//!
//! Both file layers write `skip_stages` and `auto_approve` under `pipeline:`;
//! `lil_schema::workspace_yaml` reads them as the cascade keys of the same name. Keys that
//! aren't cascade keys (`name`, `slug`, `type`, `status`, `links`, `created`, …) are never
//! settings.
//!
//! Leaf keys: `models.<task>` and `providers.<…>` (their mappings are walked to the leaves),
//! and `qa_profile`, `auto_approve`, `skip_stages`, `auto_candidate_on_save` and `modules`,
//! which are leaves themselves, so a list replaces a whole list. A `null` leaf in a file layer
//! sets nothing (the key is inherited). A file layer's value of the wrong type is skipped with
//! a [`SchemaWarning`]; in the request layer it is `params.invalid`, and so is a suite-level
//! key ([`SUITE_LEVEL_KEYS`], CORE-057) or any other unknown key.
//!
//! The request layer takes dotted keys (`{"models.image_generate": "c"}`) or nested mappings
//! (`{"models": {"image_generate": "c"}}`), which are walked to the same leaves. Like the
//! files, it may write `skip_stages` and `auto_approve` under `pipeline`, in either form
//! (`{"pipeline.skip_stages": […]}` or `{"pipeline": {"skip_stages": […]}}`), or as the
//! plain keys; any other `pipeline.*` key is not a setting (`params.invalid`).
//!
//! `check` and `record` in `skip_stages` are dropped with a [`SchemaWarning`], whatever the
//! layer (CORE-054). `auto_candidate_on_save` defaults to `false`; nothing in plan 2b acts on
//! it (CORE-049).
//!
//! This module reads no files: [`cascade`] is a pure function over the parsed layers.

use std::collections::BTreeMap;

use lil_schema::SchemaWarning;
use lil_schema::workspace_yaml::{CASCADE_KEYS, ProjectYaml, WorkspaceYaml};
use schemars::JsonSchema;
use serde::Serialize;
use serde_json::{Map, Value};

/// Where a setting's value came from. Wire values: `workspace`, `project`, `request`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum SettingSource {
    Workspace,
    Project,
    Request,
}

/// The resolved settings of a call: each dotted leaf key's value and the layer it came from.
/// `test.settings` returns it as it is: `{values, sources}`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, JsonSchema)]
pub struct Settings {
    pub values: BTreeMap<String, Value>,
    pub sources: BTreeMap<String, SettingSource>,
}

/// Settings that are suite-level (§2.0), never in the cascade (CORE-057).
pub const SUITE_LEVEL_KEYS: &[&str] = &[
    "caps",
    "currency",
    "fixed_costs",
    "psu_efficiency",
    "power_baseline",
    "electricity_rate",
    "assistant_engine",
];

/// Stages `skip_stages` can't skip: they carry QA and the recipe (CORE-054).
pub const UNSKIPPABLE_STAGES: &[&str] = &["check", "record"];

/// Cascade keys whose mapping is walked to dotted leaves.
const MAP_KEYS: &[&str] = &["models", "providers"];
/// Cascade keys holding a list of strings.
const LIST_KEYS: &[&str] = &["auto_approve", "skip_stages", "modules"];
/// Request-layer spellings of the `pipeline:` keys.
const PIPELINE_ALIASES: &[(&str, &str)] = &[
    ("pipeline.skip_stages", "skip_stages"),
    ("pipeline.auto_approve", "auto_approve"),
];

impl Settings {
    /// The value of the dotted leaf key `key`.
    pub fn get(&self, key: &str) -> Option<&Value> {
        self.values.get(key)
    }

    /// `models.<task>`: the model (or model profile) for `task`.
    pub fn model_for(&self, task: &str) -> Option<&str> {
        self.get(&format!("models.{task}")).and_then(Value::as_str)
    }

    /// The asset types the gates may sign off on their own.
    pub fn auto_approve(&self) -> Vec<String> {
        self.strings("auto_approve")
    }

    /// The stages to skip; never `check` or `record` (CORE-054).
    pub fn skip_stages(&self) -> Vec<String> {
        self.strings("skip_stages")
    }

    /// Whether a document save creates a candidate run; `false` unless set (CORE-049).
    pub fn auto_candidate_on_save(&self) -> bool {
        self.get("auto_candidate_on_save")
            .and_then(Value::as_bool)
            .unwrap_or(false)
    }

    /// The project's enabled modules (CORE-058).
    pub fn modules(&self) -> Vec<String> {
        self.strings("modules")
    }

    /// The QA profile.
    pub fn qa_profile(&self) -> Option<&str> {
        self.get("qa_profile").and_then(Value::as_str)
    }

    fn strings(&self, key: &str) -> Vec<String> {
        self.get(key)
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default()
    }

    fn put(&mut self, key: String, value: Value, source: SettingSource) {
        self.sources.insert(key.clone(), source);
        self.values.insert(key, value);
    }
}

/// The settings of the layers `ws` (`defaults:`), `project` (`project.yaml`'s top-level keys)
/// and `request` (a tool's `settings` argument, dotted keys or nested mappings), with a
/// warning for each value a file layer gave that was skipped or dropped.
///
/// # Errors
/// The `params.invalid` message for a request key that is suite-level, unknown, or of the
/// wrong type.
pub fn cascade(
    ws: &WorkspaceYaml,
    project: Option<&ProjectYaml>,
    request: Option<&Map<String, Value>>,
) -> Result<(Settings, Vec<SchemaWarning>), String> {
    let mut settings = Settings::default();
    let mut warnings = Vec::new();
    for (layer, source) in [
        (Some(&ws.defaults), SettingSource::Workspace),
        (project.map(|p| &p.overrides), SettingSource::Project),
    ] {
        let Some(layer) = layer else { continue };
        let mut leaves = Vec::new();
        for &key in CASCADE_KEYS {
            if let Some(value) = layer.get(key) {
                leaves_of(key, value, &mut leaves);
            }
        }
        for (key, value) in leaves {
            if value.is_null() {
                continue;
            }
            match checked(&key, value, source, &mut warnings) {
                Ok(value) => settings.put(key, value, source),
                Err(why) => warnings.push(SchemaWarning {
                    pointer: pointer(source, &key),
                    message: format!("{why}; the value is ignored"),
                }),
            }
        }
    }
    if let Some(request) = request {
        let mut leaves = Vec::new();
        for (key, value) in request {
            leaves_of(key, value, &mut leaves);
        }
        // `pipeline.skip_stages` / `pipeline.auto_approve`, dotted or nested, are the cascade
        // keys of the files' `pipeline:` mapping.
        for (key, _) in &mut leaves {
            if let Some((_, to)) = PIPELINE_ALIASES.iter().find(|(alias, _)| alias == key) {
                *key = (*to).to_string();
            }
        }
        for (key, value) in leaves {
            let first = key.split('.').next().unwrap_or_default();
            if SUITE_LEVEL_KEYS.contains(&first) {
                return Err(format!(
                    "`{key}` is a suite-level setting, not one a request can set (CORE-057)"
                ));
            }
            let known = MAP_KEYS.contains(&first) || CASCADE_KEYS.contains(&key.as_str());
            if !known {
                return Err(format!(
                    "`{key}` is not a setting; the settings are models.<task>, providers.<…>, \
                     qa_profile, auto_approve, skip_stages, auto_candidate_on_save and modules"
                ));
            }
            let value = checked(&key, value, SettingSource::Request, &mut warnings)
                .map_err(|why| format!("`{key}`: {why}"))?;
            settings.put(key, value, SettingSource::Request);
        }
    }
    Ok((settings, warnings))
}

/// The dotted leaves of `key: value`: a mapping of a [`MAP_KEYS`] key (or of any key, in the
/// request layer's nested form) is walked; anything else is a leaf.
fn leaves_of(key: &str, value: &Value, out: &mut Vec<(String, Value)>) {
    match value {
        Value::Object(map) if !LIST_KEYS.contains(&key) && !is_scalar_key(key) => {
            for (sub, v) in map {
                leaves_of(&format!("{key}.{sub}"), v, out);
            }
        }
        _ => out.push((key.to_string(), value.clone())),
    }
}

fn is_scalar_key(key: &str) -> bool {
    key == "qa_profile" || key == "auto_candidate_on_save"
}

/// `value` if it has the type `key` needs; `skip_stages` without `check` and `record` (each
/// dropped with a warning).
fn checked(
    key: &str,
    value: Value,
    source: SettingSource,
    warnings: &mut Vec<SchemaWarning>,
) -> Result<Value, String> {
    let first = key.split('.').next().unwrap_or_default();
    if MAP_KEYS.contains(&first) {
        if key.len() == first.len() {
            return Err(format!("{key} must be a mapping"));
        }
        return Ok(value);
    }
    match key {
        "qa_profile" => match value {
            Value::String(_) => Ok(value),
            _ => Err("qa_profile must be a string".to_string()),
        },
        "auto_candidate_on_save" => match value {
            Value::Bool(_) => Ok(value),
            _ => Err("auto_candidate_on_save must be true or false".to_string()),
        },
        list if LIST_KEYS.contains(&list) => {
            let Value::Array(items) = value else {
                return Err(format!("{key} must be a list"));
            };
            if !items.iter().all(Value::is_string) {
                return Err(format!("{key} must be a list of strings"));
            }
            if key != "skip_stages" {
                return Ok(Value::Array(items));
            }
            let mut kept = Vec::with_capacity(items.len());
            for item in items {
                let stage = item.as_str().unwrap_or_default();
                if UNSKIPPABLE_STAGES.contains(&stage) {
                    warnings.push(SchemaWarning {
                        pointer: pointer(source, key),
                        message: format!(
                            "the stage `{stage}` can't be skipped (it carries QA and the \
                             recipe, CORE-054); it is dropped from skip_stages"
                        ),
                    });
                } else {
                    kept.push(item);
                }
            }
            Ok(Value::Array(kept))
        }
        _ => Err(format!("{key} is not a setting")),
    }
}

/// Where a warning points: the layer and the dotted key.
fn pointer(source: SettingSource, key: &str) -> String {
    let layer = match source {
        SettingSource::Workspace => "lilith.yaml defaults",
        SettingSource::Project => "project.yaml",
        SettingSource::Request => "request settings",
    };
    format!("{layer}: {key}")
}

#[cfg(test)]
mod tests {
    use lil_schema::workspace_yaml::{read_project_yaml, read_workspace_yaml};
    use serde_json::json;

    use super::*;

    fn obj(v: Value) -> Map<String, Value> {
        match v {
            Value::Object(m) => m,
            _ => panic!("not an object"),
        }
    }

    #[test] // spec: CORE-057
    fn layers_override_dotted_leaves_and_lists_replace_lists() {
        let ws = read_workspace_yaml(
            "defaults:\n  models: {image_generate: a, vision_qa: q}\n  modules: [concept, 2d]\n  \
             pipeline:\n    auto_approve: [icon, ui]\n",
        )
        .unwrap();
        let project =
            read_project_yaml("name: X\nmodels: {image_generate: b}\nmodules: [audio]\n").unwrap();
        let request = obj(json!({"models": {"vision_qa": "r"}, "qa_profile": "strict"}));
        let (s, warnings) = cascade(&ws, Some(&project), Some(&request)).unwrap();
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(s.model_for("image_generate"), Some("b"));
        assert_eq!(s.sources["models.image_generate"], SettingSource::Project);
        assert_eq!(s.model_for("vision_qa"), Some("r"));
        assert_eq!(s.sources["models.vision_qa"], SettingSource::Request);
        assert_eq!(s.modules(), ["audio"]);
        assert_eq!(s.auto_approve(), ["icon", "ui"]);
        assert_eq!(s.sources["auto_approve"], SettingSource::Workspace);
        assert_eq!(s.qa_profile(), Some("strict"));
        assert!(!s.auto_candidate_on_save());
        assert!(!s.values.contains_key("name"));
        assert_eq!(
            serde_json::to_value(&s).unwrap()["sources"]["qa_profile"],
            "request"
        );
    }

    #[test] // spec: CORE-057, CORE-054
    fn bad_values_are_warnings_in_files_and_errors_in_requests() {
        let ws = read_workspace_yaml(
            "defaults:\n  qa_profile: 3\n  models: {image_generate: ~}\n  pipeline:\n    \
             skip_stages: [export, check, record]\n",
        )
        .unwrap();
        let (s, warnings) = cascade(&ws, None, None).unwrap();
        assert_eq!(s.qa_profile(), None);
        assert!(!s.values.contains_key("models.image_generate"));
        assert_eq!(s.skip_stages(), ["export"]);
        assert_eq!(warnings.len(), 3, "{warnings:?}");

        for (bad, why) in [
            (json!({"caps.monthly_usd": 1}), "suite-level"),
            (json!({"caps": {"monthly_usd": 1}}), "suite-level"),
            (json!({"assistant_engine": "x"}), "suite-level"),
            (json!({"vcs": "git"}), "not a setting"),
            (json!({"models": "x"}), "mapping"),
            (json!({"modules": "x"}), "list"),
            (json!({"auto_candidate_on_save": "yes"}), "true or false"),
        ] {
            let err = cascade(&ws, None, Some(&obj(bad.clone()))).unwrap_err();
            assert!(err.contains(why), "{bad}: {err}");
        }
        let (s, _) = cascade(
            &ws,
            None,
            Some(&obj(json!({"pipeline.skip_stages": ["check", "review"]}))),
        )
        .unwrap();
        assert_eq!(s.skip_stages(), ["review"]);
        assert_eq!(s.sources["skip_stages"], SettingSource::Request);
        // The nested form of the `pipeline` keys is the same setting.
        let (s, _) = cascade(
            &ws,
            None,
            Some(&obj(json!({"pipeline": {"skip_stages": ["record", "export"], "auto_approve": ["icon"]}}))),
        )
        .unwrap();
        assert_eq!(s.skip_stages(), ["export"]);
        assert_eq!(s.auto_approve(), ["icon"]);
        assert_eq!(s.sources["auto_approve"], SettingSource::Request);
        for bad in [
            json!({"pipeline": {"extra_reviews": []}}),
            json!({"pipeline.extra_reviews": []}),
            json!({"pipeline": ["skip_stages"]}),
        ] {
            let err = cascade(&ws, None, Some(&obj(bad.clone()))).unwrap_err();
            assert!(err.contains("not a setting"), "{bad}: {err}");
        }
    }
}
