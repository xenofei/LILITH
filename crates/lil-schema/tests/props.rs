//! Property tests of the manifest map (QA-044): plain `proptest` strategies over operation
//! sequences (Appendix E, QA-044 clarification). The case count comes from `test-params.toml`
//! `[proptest] cases`.

use std::path::Path;

use lil_schema::manifest::Manifest;
use proptest::prelude::*;
use serde_json::{Map, Number, Value};

fn cases() -> u32 {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../test-params.toml");
    let text = std::fs::read_to_string(&path).expect("read test-params.toml");
    let table: toml::Table = text.parse().expect("parse test-params.toml");
    let cases = table["proptest"]["cases"]
        .as_integer()
        .expect("[proptest] cases");
    u32::try_from(cases).expect("cases fits u32")
}

fn config() -> ProptestConfig {
    ProptestConfig::with_cases(cases())
}

/// JSON leaves: null, booleans, any i64 and u64, any finite f64 (normal, subnormal, zero,
/// both signs; serde_json's `float_roundtrip` makes their shortest text parse back exactly),
/// and strings.
fn leaf() -> impl Strategy<Value = Value> {
    prop_oneof![
        Just(Value::Null),
        any::<bool>().prop_map(Value::Bool),
        any::<i64>().prop_map(|n| Value::Number(n.into())),
        any::<u64>().prop_map(|n| Value::Number(n.into())),
        any::<f64>()
            .prop_filter_map("finite", Number::from_f64)
            .prop_map(Value::Number),
        "\\PC{0,12}".prop_map(Value::String),
        "[a-zÜïöé \\n\"\\\\]{0,6}".prop_map(Value::String),
    ]
}

fn key() -> impl Strategy<Value = String> {
    "[a-f]{1,2}"
}

fn json_value() -> impl Strategy<Value = Value> {
    leaf().prop_recursive(3, 24, 4, |inner| {
        prop_oneof![
            prop::collection::vec(inner.clone(), 0..4).prop_map(Value::Array),
            prop::collection::vec((key(), inner), 0..4)
                .prop_map(|kv| Value::Object(kv.into_iter().collect())),
        ]
    })
}

fn object() -> impl Strategy<Value = Map<String, Value>> {
    prop::collection::vec((key(), json_value()), 0..8).prop_map(|kv| kv.into_iter().collect())
}

#[derive(Debug, Clone)]
enum Op {
    Set(String, Value),
    Push(String, Value),
    Remove(String),
}

fn op() -> impl Strategy<Value = Op> {
    prop_oneof![
        (key(), json_value()).prop_map(|(k, v)| Op::Set(k, v)),
        (key(), json_value()).prop_map(|(k, v)| Op::Push(k, v)),
        key().prop_map(Op::Remove),
    ]
}

fn manifest_of(map: &Map<String, Value>) -> Manifest {
    Manifest::parse(&serde_json::to_string(map).unwrap())
        .expect("an object always parses")
        .0
}

fn order(m: &Manifest) -> Vec<String> {
    m.as_map().keys().cloned().collect()
}

proptest! {
    #![proptest_config(config())]

    #[test] // spec: QA-044, ARCH-076
    fn any_json_object_round_trips_by_order_and_value(map in object(), crlf in any::<bool>()) {
        let m = manifest_of(&map);
        let mut text = m.to_text();
        if crlf {
            text = text.replace('\n', "\r\n");
        }
        let (again, _) = Manifest::parse(&text).unwrap();
        // serde_json's text of a preserve_order map shows the order at every depth.
        prop_assert_eq!(
            serde_json::to_string(again.as_map()).unwrap(),
            serde_json::to_string(&map).unwrap()
        );
        prop_assert_eq!(again.to_text(), text);
    }

    #[test] // spec: QA-044, ARCH-076, CORE-148
    fn edits_never_reorder_existing_keys(map in object(), ops in prop::collection::vec(op(), 0..16)) {
        let mut m = manifest_of(&map);
        let mut model: Vec<String> = map.keys().cloned().collect();
        for op in ops {
            match op {
                Op::Set(k, v) => {
                    if !model.contains(&k) {
                        model.push(k.clone());
                    }
                    m.set(&k, v.clone());
                    prop_assert_eq!(m.get(&k), Some(&v));
                }
                Op::Push(k, v) => {
                    if !model.contains(&k) {
                        model.push(k.clone());
                    }
                    let before = m.get(&k).cloned();
                    let listable = matches!(before, None | Some(Value::Null | Value::Array(_)));
                    let pushed = m.push(&k, v.clone());
                    prop_assert_eq!(pushed.is_ok(), listable);
                    if listable {
                        let list = m.get(&k).and_then(Value::as_array).expect("a list");
                        prop_assert_eq!(list.last(), Some(&v));
                    } else {
                        // Refused: nothing changed (ruling Q5).
                        prop_assert_eq!(m.get(&k), before.as_ref());
                    }
                }
                Op::Remove(k) => {
                    let had = model.contains(&k);
                    model.retain(|x| *x != k);
                    prop_assert_eq!(m.remove(&k).is_some(), had);
                }
            }
            prop_assert_eq!(order(&m), model.clone());
        }
        // The text keeps the same order.
        let (again, _) = Manifest::parse(&m.to_text()).unwrap();
        prop_assert_eq!(order(&again), model);
    }
}
