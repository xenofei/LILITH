//! Canonical JSON and argument hashes (CORE-017: idempotency compares argument hashes).

use serde_json::Value;
use sha2::{Digest, Sha256};

/// `v` as compact JSON with every object's keys sorted (byte-wise, recursively) and no
/// whitespace. Independent of whether `serde_json` keeps insertion order.
pub fn canonical_json(v: &Value) -> String {
    let mut out = String::new();
    write_canonical(v, &mut out);
    out
}

fn write_canonical(v: &Value, out: &mut String) {
    match v {
        Value::Object(map) => {
            let mut entries: Vec<(&String, &Value)> = map.iter().collect();
            entries.sort_unstable_by(|a, b| a.0.cmp(b.0));
            out.push('{');
            for (i, (k, val)) in entries.into_iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                out.push_str(&Value::String(k.clone()).to_string());
                out.push(':');
                write_canonical(val, out);
            }
            out.push('}');
        }
        Value::Array(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_canonical(item, out);
            }
            out.push(']');
        }
        // Scalars: serde_json's compact form.
        scalar => out.push_str(&scalar.to_string()),
    }
}

/// 64 lowercase hex characters: SHA-256 of [`canonical_json`].
pub fn args_hash(v: &Value) -> String {
    sha256_hex(canonical_json(v).as_bytes())
}

pub(crate) fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test] // spec: CORE-017
    fn canonical_json_sorts_keys_recursively() {
        assert_eq!(
            canonical_json(&json!({"b": 1, "a": {"d": 1, "c": 2}})),
            r#"{"a":{"c":2,"d":1},"b":1}"#
        );
        assert_eq!(
            canonical_json(&json!([{"z": null, "y": [true, 1.5, "Ünïcödé space"]}, "x\n"])),
            r#"[{"y":[true,1.5,"Ünïcödé space"],"z":null},"x\n"]"#
        );

        let h = args_hash(&json!({"b": 1, "a": {"d": 1, "c": 2}}));
        assert_eq!(h.len(), 64);
        assert!(
            h.bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        );
        assert_eq!(h, args_hash(&json!({"a": {"c": 2, "d": 1}, "b": 1})));
        assert_ne!(h, args_hash(&json!({"a": {"c": 2, "d": 1}, "b": 2})));
        // SHA-256 of the two bytes `{}`.
        assert_eq!(
            args_hash(&json!({})),
            "44136fa355b3678a1146ad16f7e8649e94fb4fc21fe77e8310c060f61caaff8a"
        );
    }
}
