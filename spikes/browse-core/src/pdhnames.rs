//! PDH GPU instance names, e.g. `pid_<pid>_luid_0x<hi>_0x<lo>_phys_<n>_eng_<n>_engtype_<type>`.
//! Pure: what S0.1 records (the grammar with placeholders) and how the busiest engine is found.

use std::collections::BTreeMap;

/// The PDH form of an adapter LUID: `luid_0x%08X_0x%08X` (high part, then low part).
pub fn luid_tag(high: i32, low: u32) -> String {
    format!("luid_0x{:08X}_0x{low:08X}", high as u32)
}

/// Whether instance `name` belongs to the adapter with PDH LUID tag `tag` (case-insensitive).
pub fn is_on(name: &str, tag: &str) -> bool {
    name.to_ascii_lowercase()
        .contains(&tag.to_ascii_lowercase())
}

fn is_digits(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit())
}

fn is_hex32(s: &str) -> bool {
    s.len() == 10
        && (s.starts_with("0x") || s.starts_with("0X"))
        && s[2..].bytes().all(|b| b.is_ascii_hexdigit())
}

/// `name` with its identifying numbers replaced: `pid_<pid>`, `luid_<luid>` (both parts),
/// `phys_<n>`, `eng_<n>`. Engine type names (`3D`, `Copy`, `Compute_0`, …) are kept.
pub fn generalise(name: &str) -> String {
    let t: Vec<&str> = name.split('_').collect();
    let mut out: Vec<String> = Vec::new();
    let mut i = 0;
    while i < t.len() {
        let tok = t[i];
        let next = t.get(i + 1).copied();
        match tok.to_ascii_lowercase().as_str() {
            "pid" if next.is_some_and(is_digits) => {
                out.push("pid_<pid>".into());
                i += 2;
            }
            "luid" if next.is_some_and(is_hex32) && t.get(i + 2).copied().is_some_and(is_hex32) => {
                out.push("luid_<luid>".into());
                i += 3;
            }
            "phys" if next.is_some_and(is_digits) => {
                out.push("phys_<n>".into());
                i += 2;
            }
            "eng" if next.is_some_and(is_digits) => {
                out.push("eng_<n>".into());
                i += 2;
            }
            "engtype" => {
                out.push(t[i..].join("_"));
                break;
            }
            _ => {
                out.push(if is_digits(tok) {
                    "<n>".into()
                } else {
                    tok.to_string()
                });
                i += 1;
            }
        }
    }
    out.join("_")
}

/// The value after `key_` in `name` (`pid`, `phys`, `eng`), when it is a number.
pub fn number_after(name: &str, key: &str) -> Option<u64> {
    let t: Vec<&str> = name.split('_').collect();
    t.windows(2)
        .find(|w| w[0].eq_ignore_ascii_case(key) && is_digits(w[1]))
        .and_then(|w| w[1].parse().ok())
}

/// The engine type: everything after `engtype_`.
pub fn engine_type(name: &str) -> Option<String> {
    let lower = name.to_ascii_lowercase();
    lower
        .find("engtype_")
        .map(|at| name[at + "engtype_".len()..].to_string())
}

/// Per engine type, its busiest engine's sum over processes: instances are grouped by
/// (`phys`, `eng`, type), summed over every `pid`, and each type keeps its largest engine.
/// Only instances of `tag`'s adapter count.
pub fn busiest_per_type(items: &[(String, f64)], tag: &str) -> BTreeMap<String, f64> {
    let mut engines: BTreeMap<(u64, u64, String), f64> = BTreeMap::new();
    for (name, v) in items {
        if !is_on(name, tag) {
            continue;
        }
        let (Some(ty), Some(eng)) = (engine_type(name), number_after(name, "eng")) else {
            continue;
        };
        let phys = number_after(name, "phys").unwrap_or(0);
        *engines.entry((phys, eng, ty)).or_insert(0.0) += v;
    }
    let mut per_type: BTreeMap<String, f64> = BTreeMap::new();
    for ((_, _, ty), sum) in engines {
        let e = per_type.entry(ty).or_insert(0.0);
        if sum > *e {
            *e = sum;
        }
    }
    per_type
}

/// The busiest type and its value, from [`busiest_per_type`].
pub fn busiest(per_type: &BTreeMap<String, f64>) -> Option<(String, f64)> {
    per_type
        .iter()
        .max_by(|a, b| a.1.total_cmp(b.1))
        .map(|(k, v)| (k.clone(), *v))
}

#[cfg(test)]
mod tests {
    use super::*;

    const TAG: &str = "luid_0x00000000_0x0000D1C2";

    #[test]
    fn luid_tag_is_high_then_low_uppercase() {
        assert_eq!(luid_tag(0, 0xd1c2), TAG);
        assert_eq!(luid_tag(-1, 1), "luid_0xFFFFFFFF_0x00000001");
    }

    #[test]
    fn names_are_generalised() {
        assert_eq!(
            generalise("pid_1234_luid_0x00000000_0x0000D1C2_phys_0_eng_3_engtype_Compute_0"),
            "pid_<pid>_luid_<luid>_phys_<n>_eng_<n>_engtype_Compute_0"
        );
        assert_eq!(
            generalise("luid_0x00000000_0x0000D1C2_phys_0"),
            "luid_<luid>_phys_<n>"
        );
        assert_eq!(generalise("_Total"), "_Total");
    }

    #[test]
    fn busiest_engine_sums_processes_then_takes_the_largest_engine() {
        let n =
            |pid: u32, eng: u32, ty: &str| format!("pid_{pid}_{TAG}_phys_0_eng_{eng}_engtype_{ty}");
        let items = vec![
            (n(10, 0, "3D"), 30.0),
            (n(11, 0, "3D"), 25.0),
            (n(10, 1, "Compute_0"), 40.0),
            (n(11, 2, "Compute_0"), 10.0),
            (n(12, 4, "Copy"), 5.0),
            // another adapter: ignored
            (
                "pid_10_luid_0x00000000_0x00000001_phys_0_eng_0_engtype_3D".to_string(),
                99.0,
            ),
        ];
        let per = busiest_per_type(&items, TAG);
        assert_eq!(per["3D"], 55.0);
        assert_eq!(per["Compute_0"], 40.0);
        assert_eq!(per["Copy"], 5.0);
        assert_eq!(busiest(&per), Some(("3D".to_string(), 55.0)));
        assert_eq!(number_after(&n(77, 1, "3D"), "pid"), Some(77));
    }
}
