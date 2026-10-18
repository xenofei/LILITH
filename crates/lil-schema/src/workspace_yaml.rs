//! The workspace file `lilith.yaml` (I2) and `project.yaml` (I4): reading the settings layers
//! and writing new files.
//!
//! Settings cascade (CORE-057, I2, I4): workspace `defaults:` → `project.yaml` → the request
//! layer. The project layer is `project.yaml`'s own top-level keys (there is no `overrides:`
//! key); `skip_stages` and `auto_approve` are written under `pipeline:` in both layers and read
//! as the cascade keys `skip_stages` and `auto_approve`.

use serde_json::{Map, Value};
use yaml_rust2::{Yaml, YamlLoader};

use crate::yaml_edit::scalar;

/// The keys the cascade reads from each layer.
pub const CASCADE_KEYS: &[&str] = &[
    "models",
    "providers",
    "qa_profile",
    "modules",
    "auto_candidate_on_save",
    "skip_stages",
    "auto_approve",
];

/// Cascade keys that both layers write under `pipeline:`.
const PIPELINE_KEYS: &[&str] = &["skip_stages", "auto_approve"];

/// What the core reads from the workspace file (`lilith.yaml`, or a legacy one).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct WorkspaceYaml {
    pub name: Option<String>,
    /// The cascade keys of `defaults:`.
    pub defaults: Map<String, Value>,
}

/// What the core reads from `project.yaml`.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ProjectYaml {
    pub name: Option<String>,
    pub slug: Option<String>,
    pub status: Option<String>,
    pub modules: Vec<String>,
    pub links: Vec<String>,
    /// The whole `pipeline:` mapping.
    pub pipeline: Map<String, Value>,
    /// The project's cascade layer, from its top-level keys (and `pipeline.skip_stages`,
    /// `pipeline.auto_approve`).
    pub overrides: Map<String, Value>,
}

/// The `progress.md` of a new project; `<name>` is replaced by the project's name.
pub const PROGRESS_TEMPLATE: &str = "# <name>\n\n## Done\n\n## Next\n\n## Open questions\n";

pub fn read_workspace_yaml(text: &str) -> Result<WorkspaceYaml, String> {
    let root = root_map(text, "the workspace file")?;
    let defaults = match root.get("defaults") {
        Some(Value::Object(d)) => cascade_layer(d),
        _ => Map::new(),
    };
    Ok(WorkspaceYaml {
        name: root.get("name").and_then(Value::as_str).map(str::to_string),
        defaults,
    })
}

/// The `hardware:` key of the workspace file: what the last detection wrote (CORE-067). A value
/// of another type is left out, as if it were missing.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct HardwareYaml {
    pub gpu: Option<String>,
    pub vram_gb: Option<u64>,
    pub ram_gb: Option<u64>,
}

/// The `hardware:` key of `text`; everything missing when it has none.
///
/// # Errors
/// When the text isn't a YAML mapping.
pub fn read_hardware(text: &str) -> Result<HardwareYaml, String> {
    let root = root_map(text, "the workspace file")?;
    let Some(Value::Object(hardware)) = root.get("hardware") else {
        return Ok(HardwareYaml::default());
    };
    let whole = |key: &str| hardware.get(key).and_then(Value::as_u64);
    Ok(HardwareYaml {
        gpu: match hardware.get("gpu") {
            Some(Value::String(s)) => Some(s.clone()),
            Some(Value::Number(n)) => Some(n.to_string()),
            _ => None,
        },
        vram_gb: whole("vram_gb"),
        ram_gb: whole("ram_gb"),
    })
}

pub fn read_project_yaml(text: &str) -> Result<ProjectYaml, String> {
    let root = root_map(text, "project.yaml")?;
    let s = |k: &str| root.get(k).and_then(Value::as_str).map(str::to_string);
    let strings = |k: &str| -> Vec<String> {
        match root.get(k) {
            Some(Value::Array(a)) => a
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect(),
            _ => Vec::new(),
        }
    };
    Ok(ProjectYaml {
        name: s("name"),
        slug: s("slug"),
        status: s("status"),
        modules: strings("modules"),
        links: strings("links"),
        pipeline: match root.get("pipeline") {
            Some(Value::Object(p)) => p.clone(),
            _ => Map::new(),
        },
        overrides: cascade_layer(&root),
    })
}

/// The cascade keys of one layer: top-level keys, and `skip_stages` / `auto_approve` from
/// `pipeline:`.
fn cascade_layer(layer: &Map<String, Value>) -> Map<String, Value> {
    let pipeline = layer.get("pipeline").and_then(Value::as_object);
    let mut out = Map::new();
    for &key in CASCADE_KEYS {
        let value = if PIPELINE_KEYS.contains(&key) {
            pipeline.and_then(|p| p.get(key))
        } else {
            layer.get(key)
        };
        if let Some(v) = value {
            out.insert(key.to_string(), v.clone());
        }
    }
    out
}

/// The document's root mapping as JSON; an empty document is an empty mapping.
fn root_map(text: &str, file: &str) -> Result<Map<String, Value>, String> {
    let text = text.strip_prefix(crate::manifest::BOM).unwrap_or(text);
    let docs =
        YamlLoader::load_from_str(text).map_err(|e| format!("{file} is not valid YAML: {e}"))?;
    match docs.first() {
        None | Some(Yaml::Null) => Ok(Map::new()),
        Some(y @ Yaml::Hash(_)) => match to_json(y) {
            Value::Object(m) => Ok(m),
            _ => unreachable!("a hash converts to an object"),
        },
        Some(_) => Err(format!("{file} is not a mapping")),
    }
}

/// A YAML value as JSON. Mapping keys that aren't strings are written as YAML would show them.
fn to_json(y: &Yaml) -> Value {
    match y {
        Yaml::Real(s) => s
            .parse::<f64>()
            .ok()
            .and_then(serde_json::Number::from_f64)
            .map_or_else(|| Value::String(s.clone()), Value::Number),
        Yaml::Integer(n) => Value::Number((*n).into()),
        Yaml::String(s) => Value::String(s.clone()),
        Yaml::Boolean(b) => Value::Bool(*b),
        Yaml::Array(a) => Value::Array(a.iter().map(to_json).collect()),
        Yaml::Hash(h) => Value::Object(
            h.iter()
                .map(|(k, v)| {
                    let key = match k {
                        Yaml::String(s) | Yaml::Real(s) => s.clone(),
                        Yaml::Integer(n) => n.to_string(),
                        Yaml::Boolean(b) => b.to_string(),
                        _ => "null".to_string(),
                    };
                    (key, to_json(v))
                })
                .collect(),
        ),
        Yaml::Null | Yaml::Alias(_) | Yaml::BadValue => Value::Null,
    }
}

/// A new `lilith.yaml`.
pub fn new_workspace_yaml(name: &str, created: &str) -> String {
    format!(
        "# Workspace settings (I2). Projects inherit `defaults` unless they override them.\n\
         name: {}\n\
         created: {}\n\
         defaults:\n  models: {{}}\n",
        scalar(name),
        scalar(created)
    )
}

/// A new `project.yaml`: name, slug, type, `status: active`, `links: []`, created and one
/// comment line, so status changes and new links edit keys that already exist.
pub fn new_project_yaml(slug: &str, name: &str, kind: &str, created: &str) -> String {
    format!(
        "# Project settings (I4). Keys left out inherit the workspace defaults.\n\
         name: {}\n\
         slug: {}\n\
         type: {}\n\
         status: active\n\
         links: []\n\
         created: {}\n",
        scalar(name),
        scalar(slug),
        scalar(kind),
        scalar(created)
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::yaml_edit::{YamlEdit, apply};
    use serde_json::json;

    const PROJECT: &str = "\
name: Ember Knight
slug: ember-knight
type: game
status: active           # active | paused | archived
models: {image_generate: b}
modules: [direction, style, concept]
qa_profile: strict
pipeline:
  skip_stages: [export]
  extra_reviews: []
  auto_approve: [icon]
links: [lib:pal_ember, proj:sketchbook]
created: 2026-10-12
";

    #[test] // spec: CORE-057
    fn project_layer_is_the_top_level_keys() {
        let p = read_project_yaml(PROJECT).unwrap();
        assert_eq!(p.name.as_deref(), Some("Ember Knight"));
        assert_eq!(p.slug.as_deref(), Some("ember-knight"));
        assert_eq!(p.status.as_deref(), Some("active"));
        assert_eq!(p.modules, ["direction", "style", "concept"]);
        assert_eq!(p.links, ["lib:pal_ember", "proj:sketchbook"]);
        assert_eq!(p.pipeline["extra_reviews"], json!([]));
        let o = &p.overrides;
        assert_eq!(o["models"], json!({"image_generate": "b"}));
        assert_eq!(o["modules"], json!(["direction", "style", "concept"]));
        assert_eq!(o["qa_profile"], json!("strict"));
        assert_eq!(o["skip_stages"], json!(["export"]));
        assert_eq!(o["auto_approve"], json!(["icon"]));
        for not in [
            "name",
            "links",
            "created",
            "pipeline",
            "extra_reviews",
            "status",
        ] {
            assert!(!o.contains_key(not), "{not}");
        }
        assert_eq!(o.len(), 5);

        let ws = "\
# workspace
name: Sample workspace
defaults:
  vcs: git+lfs
  models: {image_generate: a}
  auto_candidate_on_save: true
  pipeline:
    auto_approve: [ui_sprite]
hardware: {gpu: none}
";
        let w = read_workspace_yaml(ws).unwrap();
        assert_eq!(w.name.as_deref(), Some("Sample workspace"));
        assert_eq!(w.defaults["models"], json!({"image_generate": "a"}));
        assert_eq!(w.defaults["auto_candidate_on_save"], json!(true));
        assert_eq!(w.defaults["auto_approve"], json!(["ui_sprite"]));
        assert!(!w.defaults.contains_key("vcs"));
        assert!(!w.defaults.contains_key("pipeline"));
        assert_eq!(w.defaults.len(), 3);

        // A byte-order mark is skipped.
        let with_bom = read_project_yaml(&format!("\u{feff}{PROJECT}")).unwrap();
        assert_eq!(with_bom, p);
        assert_eq!(read_workspace_yaml(&format!("\u{feff}{ws}")).unwrap(), w);

        // Empty files read as empty layers; text that isn't a mapping is an error.
        assert_eq!(read_project_yaml("").unwrap(), ProjectYaml::default());
        assert_eq!(
            read_workspace_yaml("# only a comment\n").unwrap(),
            WorkspaceYaml::default()
        );
        assert!(read_project_yaml("- a\n- b\n").is_err());
        assert!(read_project_yaml("a: [1\n").is_err());
        // Wrong types are skipped, not errors.
        let odd =
            read_project_yaml("name: 3\nmodules: x\nlinks: [a, 2, b]\npipeline: 1\n").unwrap();
        assert_eq!(odd.name, None);
        assert!(odd.modules.is_empty());
        assert_eq!(odd.links, ["a", "b"]);
        assert!(odd.pipeline.is_empty());
    }

    #[test] // spec: CORE-039, CORE-057
    fn new_project_yaml_has_status_links_and_a_comment() {
        let text = new_project_yaml("sketchbook", "Sketchbook", "media", "2026-10-01");
        let p = read_project_yaml(&text).unwrap();
        assert_eq!(p.slug.as_deref(), Some("sketchbook"));
        assert_eq!(p.name.as_deref(), Some("Sketchbook"));
        assert_eq!(p.status.as_deref(), Some("active"));
        assert!(p.links.is_empty());
        assert!(text.lines().any(|l| l == "status: active"), "{text}");
        assert!(text.lines().any(|l| l == "links: []"), "{text}");
        assert!(text.lines().any(|l| l == "type: media"), "{text}");
        assert!(text.lines().any(|l| l == "created: 2026-10-01"), "{text}");
        assert_eq!(text.lines().filter(|l| l.starts_with('#')).count(), 1);
        assert!(!text.contains('\r'));

        let edited = apply(
            &text,
            &[
                YamlEdit::SetScalar {
                    path: vec!["status".into()],
                    value: "paused".into(),
                },
                YamlEdit::AppendToList {
                    path: vec!["links".into()],
                    item: "proj:harbor".into(),
                },
            ],
        )
        .unwrap();
        let p = read_project_yaml(&edited).unwrap();
        assert_eq!(p.status.as_deref(), Some("paused"));
        assert_eq!(p.links, ["proj:harbor"]);
        assert_eq!(edited.lines().count(), text.lines().count());

        // A name that needs quoting is quoted.
        let odd = new_project_yaml("x", "Ünïcödé space: yes", "custom", "2026-10-01");
        assert_eq!(
            read_project_yaml(&odd).unwrap().name.as_deref(),
            Some("Ünïcödé space: yes")
        );

        // The fixture project was written by this function.
        let fixture = include_str!(
            "../../lil-spec-tests/fixtures/sample-workspace/projects/sketchbook/project.yaml"
        );
        assert_eq!(fixture, text);

        let ws = new_workspace_yaml("Ünïcödé space", "2026-10-01");
        let w = read_workspace_yaml(&ws).unwrap();
        assert_eq!(w.name.as_deref(), Some("Ünïcödé space"));
        assert_eq!(w.defaults["models"], json!({}));
        assert!(ws.lines().any(|l| l.starts_with('#')));
        assert!(ws.lines().any(|l| l == "created: 2026-10-01"));

        assert_eq!(
            PROGRESS_TEMPLATE.replace("<name>", "Sketchbook"),
            include_str!(
                "../../lil-spec-tests/fixtures/sample-workspace/projects/sketchbook/progress.md"
            )
        );
    }

    #[test] // spec: CORE-067
    fn hardware_reads_the_three_values_and_skips_other_types() {
        let h = read_hardware("name: x\nhardware:\n  gpu: RTX 5090\n  vram_gb: 32\n  ram_gb: 64\n")
            .unwrap();
        assert_eq!(
            h,
            HardwareYaml {
                gpu: Some("RTX 5090".into()),
                vram_gb: Some(32),
                ram_gb: Some(64)
            }
        );
        let flow = read_hardware("hardware: {gpu: none}\n").unwrap();
        assert_eq!((flow.gpu.as_deref(), flow.vram_gb), (Some("none"), None));
        assert_eq!(read_hardware("name: x\n").unwrap(), HardwareYaml::default());
        let odd = read_hardware("hardware: {gpu: [a], vram_gb: lots, ram_gb: -1}\n").unwrap();
        assert_eq!(odd, HardwareYaml::default());
        assert!(read_hardware("- a\n").is_err());
    }
}
