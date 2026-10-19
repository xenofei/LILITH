//! Graph checks (CORE-018, ARCH-057, CORE-093; Appendix F "ComfyUI client"), on API-format
//! graphs (`{<node id>: {class_type, inputs}}`):
//!
//! - [`apply_node_replacements`] applies ComfyUI's `/node_replacements` locally, so the checks
//!   see the classes ComfyUI will run. The map's values are a class name, or what ComfyUI 0.41
//!   serves (`[{new_node_id, input_mapping, …}]`, the first one used, as ComfyUI does); an
//!   `input_mapping` rebuilds the node's inputs (`{new_id, old_id}` or `{new_id, set_value}`).
//!   Output remapping is not needed for the checks: the suite posts the graph as it came, and
//!   `POST /prompt` applies the replacements itself.
//! - [`model_refs`]: inputs named `*_name` or `model` whose string value ends in a model
//!   extension ([`MODEL_EXTENSIONS`], any case).
//! - [`partner_nodes`]: the class types `/object_info` marks `api_node: true`, sorted.
//! - [`missing_models`]: the references the inventory doesn't hold ([`in_inventory`]).

use serde_json::{Map, Value};

/// The model file extensions (Appendix F): the inventory's files, the references' values.
pub const MODEL_EXTENSIONS: [&str; 7] = [
    ".safetensors",
    ".gguf",
    ".sft",
    ".ckpt",
    ".pt",
    ".pth",
    ".bin",
];

/// Whether `name` ends in a model extension, ignoring case.
pub fn is_model_file(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    MODEL_EXTENSIONS.iter().any(|ext| lower.ends_with(ext))
}

/// One model reference of a graph.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelRef {
    pub node: String,
    pub input: String,
    /// The value as the graph has it (`sub/a.safetensors`).
    pub file: String,
}

/// How a replacement treats the node's inputs.
enum Inputs<'a> {
    /// The plain `{"Old": "New"}` form: the inputs stay.
    Keep,
    /// ComfyUI's form: the inputs are rebuilt from the mapping (none: no inputs), as ComfyUI
    /// rebuilds them.
    Map(Option<&'a Vec<Value>>),
}

/// The class a replacement entry names, and what it does with the inputs.
fn replacement_of(entry: &Value) -> Option<(&str, Inputs<'_>)> {
    let one = match entry {
        Value::String(class) => return Some((class.as_str(), Inputs::Keep)),
        Value::Array(list) => list.first()?,
        other => other,
    };
    let class = one.get("new_node_id")?.as_str()?;
    Some((
        class,
        Inputs::Map(one.get("input_mapping").and_then(Value::as_array)),
    ))
}

/// The class a replacement entry replaces with (its first one, as ComfyUI uses).
pub fn replacement_class(entry: &Value) -> Option<&str> {
    replacement_of(entry).map(|(class, _)| class)
}

/// Replaces every node whose `class_type` is a key of `map` (module docs), as ComfyUI does: a
/// node without `inputs` is left as it is. Returns each `(old, new)` class pair applied, in
/// node order.
pub fn apply_node_replacements(graph: &mut Value, map: &Value) -> Vec<(String, String)> {
    let mut applied = Vec::new();
    let Some(nodes) = graph.as_object_mut() else {
        return applied;
    };
    for node in nodes.values_mut() {
        let Some(old) = node.get("class_type").and_then(Value::as_str) else {
            continue;
        };
        if node.get("inputs").is_none() {
            continue;
        }
        let Some((new, inputs)) = map.get(old).and_then(replacement_of) else {
            continue;
        };
        let (old, new) = (old.to_string(), new.to_string());
        if let Inputs::Map(mapping) = inputs {
            let before = node
                .get("inputs")
                .and_then(Value::as_object)
                .cloned()
                .unwrap_or_default();
            let mut after = Map::new();
            for m in mapping.into_iter().flatten() {
                let Some(new_id) = m.get("new_id").and_then(Value::as_str) else {
                    continue;
                };
                let value = match (m.get("set_value"), m.get("old_id").and_then(Value::as_str)) {
                    (Some(v), _) => Some(v.clone()),
                    (None, Some(old_id)) => before.get(old_id).cloned(),
                    (None, None) => None,
                };
                if let Some(value) = value {
                    after.insert(new_id.to_string(), value);
                }
            }
            node["inputs"] = Value::Object(after);
        }
        node["class_type"] = Value::from(new.as_str());
        applied.push((old, new));
    }
    applied
}

/// The flags with which ComfyUI looks for models outside `<root>\models` (its
/// `cli_args.py`): with any of them the model check is skipped.
pub const OTHER_MODEL_PATH_FLAGS: [&str; 3] = [
    "--extra-model-paths-config",
    "--models-directory",
    "--base-directory",
];

/// The shortest abbreviation of each of [`OTHER_MODEL_PATH_FLAGS`] that the check takes:
/// ComfyUI's argparse accepts any unambiguous prefix (re-review N4).
const OTHER_MODEL_PATH_STEMS: [&str; 3] = ["--extra-model", "--models", "--base"];

/// Whether the command line `args` holds one of [`OTHER_MODEL_PATH_FLAGS`], whole or
/// abbreviated as argparse allows (`--base-dir`), as `--flag value` or `--flag=value`.
pub fn names_other_model_paths(args: &[String]) -> bool {
    args.iter().any(|a| {
        let name = a.split_once('=').map_or(a.as_str(), |(n, _)| n);
        OTHER_MODEL_PATH_FLAGS
            .iter()
            .zip(OTHER_MODEL_PATH_STEMS)
            .any(|(flag, stem)| name.starts_with(stem) && flag.starts_with(name))
    })
}

/// `items` joined by newlines: at most `max_items` of them, whole items only, at most
/// `max_bytes` (an item longer than that alone is cut on a character boundary).
pub fn lines_within(items: &[String], max_items: usize, max_bytes: usize) -> String {
    let mut out = String::new();
    for item in items.iter().take(max_items) {
        let extra = item.len() + usize::from(!out.is_empty());
        if out.len() + extra > max_bytes {
            if out.is_empty() {
                let mut end = max_bytes.min(item.len());
                while !item.is_char_boundary(end) {
                    end -= 1;
                }
                out.push_str(&item[..end]);
            }
            break;
        }
        if !out.is_empty() {
            out.push('\n');
        }
        out.push_str(item);
    }
    out
}
/// The model references of `graph` (module docs), sorted by node, then input.
pub fn model_refs(graph: &Value) -> Vec<ModelRef> {
    let mut refs = Vec::new();
    for (id, node) in graph.as_object().into_iter().flatten() {
        for (input, value) in node
            .get("inputs")
            .and_then(Value::as_object)
            .into_iter()
            .flatten()
        {
            let named = input == "model" || input.ends_with("_name");
            if let (true, Some(file)) = (named, value.as_str())
                && is_model_file(file)
            {
                refs.push(ModelRef {
                    node: id.clone(),
                    input: input.clone(),
                    file: file.to_string(),
                });
            }
        }
    }
    refs.sort_by(|a, b| (&a.node, &a.input).cmp(&(&b.node, &b.input)));
    refs
}

/// The class types of `graph` that `object_info` marks `api_node: true`: sorted, each once.
pub fn partner_nodes(graph: &Value, object_info: &Value) -> Vec<String> {
    let mut classes: Vec<String> = graph
        .as_object()
        .into_iter()
        .flat_map(|nodes| nodes.values())
        .filter_map(|node| node.get("class_type").and_then(Value::as_str))
        .filter(|class| object_info[*class]["api_node"] == true)
        .map(str::to_string)
        .collect();
    classes.sort();
    classes.dedup();
    classes
}

/// Whether `reference` names a file of `paths` (inventory paths below `<root>\models`, with
/// `\`): with `/` read as `\`, it equals the end of a path after a `\`, or the whole path,
/// ignoring case (Appendix F).
pub fn in_inventory(reference: &str, paths: &[String]) -> bool {
    let want = reference.replace('/', "\\").to_lowercase();
    let tail = format!("\\{want}");
    paths.iter().any(|p| {
        let p = p.to_lowercase();
        p == want || p.ends_with(&tail)
    })
}

/// The files of `refs` that the inventory `paths` doesn't hold: sorted, each once.
pub fn missing_models(refs: &[ModelRef], paths: &[String]) -> Vec<String> {
    let mut missing: Vec<String> = refs
        .iter()
        .filter(|r| !in_inventory(&r.file, paths))
        .map(|r| r.file.clone())
        .collect();
    missing.sort();
    missing.dedup();
    missing
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test] // spec: CORE-018
    fn node_replacements_apply_locally_and_model_refs_are_extracted() {
        let mut graph = json!({
            "1": {"class_type": "OldLoader", "inputs": {"ckpt_name": "a.safetensors"}},
            "2": {"class_type": "KSampler", "inputs": {"model": ["1", 0]}},
        });
        let applied =
            apply_node_replacements(&mut graph, &json!({"OldLoader": "CheckpointLoaderSimple"}));
        assert_eq!(
            applied,
            [(
                "OldLoader".to_string(),
                "CheckpointLoaderSimple".to_string()
            )]
        );
        assert_eq!(graph["1"]["class_type"], "CheckpointLoaderSimple");
        assert_eq!(graph["2"]["class_type"], "KSampler");

        let graph = json!({
            "1": {"class_type": "CheckpointLoaderSimple", "inputs": {"ckpt_name": "a.safetensors"}},
            "2": {"class_type": "LoraLoader", "inputs": {"lora_name": "b.safetensors", "strength": 1}},
            "3": {"class_type": "CLIPTextEncode", "inputs": {"text": "c.safetensors"}},
            "4": {"class_type": "UnetLoaderGGUF", "inputs": {"model": "d.gguf"}},
        });
        let files: Vec<String> = model_refs(&graph).into_iter().map(|r| r.file).collect();
        assert_eq!(files, ["a.safetensors", "b.safetensors", "d.gguf"]);
    }

    #[test] // spec: CORE-018
    fn comfyuis_replacement_entries_map_inputs() {
        let mut graph =
            json!({"7": {"class_type": "Old", "inputs": {"ckpt": "x.ckpt", "drop": 1}}});
        let map = json!({"Old": [
            {"old_node_id": "Old", "new_node_id": "New",
             "input_mapping": [{"new_id": "ckpt_name", "old_id": "ckpt"},
                               {"new_id": "mode", "set_value": "fast"}]},
            {"old_node_id": "Old", "new_node_id": "Other"}
        ]});
        assert_eq!(
            apply_node_replacements(&mut graph, &map),
            [("Old".to_string(), "New".to_string())]
        );
        assert_eq!(
            graph["7"],
            json!({"class_type": "New", "inputs": {"ckpt_name": "x.ckpt", "mode": "fast"}})
        );
    }
    #[test] // spec: CORE-018
    fn replacements_without_a_mapping_or_inputs_follow_comfyui() {
        let mut graph = json!({
            "1": {"class_type": "Old", "inputs": {"ckpt": "x.ckpt"}},
            "2": {"class_type": "Old"},
        });
        let map = json!({"Old": [{"old_node_id": "Old", "new_node_id": "New"}]});
        assert_eq!(apply_node_replacements(&mut graph, &map).len(), 1);
        assert_eq!(graph["1"], json!({"class_type": "New", "inputs": {}}));
        assert_eq!(
            graph["2"],
            json!({"class_type": "Old"}),
            "no inputs: left alone"
        );
    }

    #[test] // spec: CORE-018
    fn other_model_path_flags_are_found_in_either_form() {
        let args = |a: &[&str]| a.iter().map(|s| (*s).to_string()).collect::<Vec<_>>();
        assert!(names_other_model_paths(&args(&[
            "main.py",
            "--base-directory",
            "D:\\c"
        ])));
        assert!(names_other_model_paths(&args(&[
            "--extra-model-paths-config=x.yaml"
        ])));
        assert!(names_other_model_paths(&args(&["--models-directory", "m"])));
        assert!(!names_other_model_paths(&args(&[
            "--base-directoryx",
            "--port",
            "8188"
        ])));
        // Abbreviated, as argparse accepts (re-review N4).
        for abbreviated in ["--base-dir", "--models-dir=m", "--extra-model", "--base"] {
            assert!(
                names_other_model_paths(&args(&[abbreviated])),
                "{abbreviated}"
            );
        }
        assert!(!names_other_model_paths(&args(&[
            "--ba", "--model", "--extra"
        ])));
    }

    #[test] // spec: ARCH-060
    fn missing_model_detail_keeps_whole_lines_within_its_cap() {
        let items: Vec<String> = (0..30).map(|i| format!("m{i:02}.safetensors")).collect();
        assert_eq!(lines_within(&items, 20, 2048).lines().count(), 20);
        let big = vec!["a".repeat(1500), "b".repeat(1500)];
        assert_eq!(lines_within(&big, 20, 2048), "a".repeat(1500));
        assert_eq!(lines_within(&["é".repeat(3000)], 20, 2048).len(), 2048);
    }

    #[test] // spec: ARCH-057, CORE-093
    fn partner_nodes_come_from_object_info_api_node() {
        let graph = json!({
            "1": {"class_type": "OpenAIImageNode", "inputs": {}},
            "2": {"class_type": "KSampler", "inputs": {}},
            "3": {"class_type": "OpenAIImageNode", "inputs": {}},
        });
        let info = json!({"OpenAIImageNode": {"api_node": true}, "KSampler": {}});
        assert_eq!(partner_nodes(&graph, &info), ["OpenAIImageNode"]);
    }

    #[test] // spec: CORE-018
    fn references_match_the_end_of_an_inventory_path() {
        let paths = vec![r"checkpoints\sub\a.safetensors".to_string()];
        for found in [
            "sub/a.safetensors",
            "A.SAFETENSORS",
            r"checkpoints\sub\a.safetensors",
        ] {
            assert!(in_inventory(found, &paths), "{found}");
        }
        for missing in ["b/a.safetensors", "ub/a.safetensors", "gone.safetensors"] {
            assert!(!in_inventory(missing, &paths), "{missing}");
        }
    }
}
