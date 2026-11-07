//! Moved and copied asset folders (CORE-154, CORE-047; Appendix H "Moved and copied folders"):
//! [`detect`], a pure function over the index's asset rows that `reconcile` calls to fill each
//! row's `repair` (and `needs_repair`).
//!
//! The folder name is authoritative. A row **agrees** when its manifest parses and its own
//! `project` and `asset_id` (each, when present) are the folder's, compared exactly as the
//! CORE-047 check does. Rows are grouped by `creation_uuid` (compared without regard to case,
//! as UUIDs are); a row without one (a legacy manifest) is never in a group, so it is never
//! `moved`, `copy` or `duplicate_uuid`.
//!
//! - **One folder** with the UUID that disagrees, by the place its manifest names (compared
//!   without regard to case: on Windows a name that differs only by case is the same folder):
//!   the folder itself, `invalid`; another **live** folder, `copy` with `other` that folder (it
//!   is no move: CORE-154's "while its old folder is gone"; ruling T8 I1); a place where the
//!   index still knows a gone asset (its manifest `missing`) with another creation UUID,
//!   `invalid`: that is another asset's place, and a redirect must never send one asset's refs
//!   to another (T8 round 2); else, a valid `<project>/<asset_id>`, `moved`, `from` that place
//!   and `to` the folder; else `invalid`. So `moved` always means no live folder is at `from`
//!   and the index knows no other asset there. Residuals (parked for the plan's final review):
//!   an original trashed through the suite leaves no row, so it isn't seen here; and the rule
//!   reads the index, so the same disk can be judged two ways. Warm, a gone row at `from`
//!   with another UUID (or none: a legacy asset) makes the folder `invalid`; after a cold
//!   rebuild no gone row exists and the same folder reads `moved`. A legacy gone row has no
//!   UUID, so it is never dropped as a move's old place and stays `missing` (ruling T8 R2).
//! - **Several folders** with one UUID: every one needs repair. Agreeing ones are
//!   `duplicate_uuid`; a disagreeing one is `copy` with `other` the original when exactly one
//!   agrees, else `duplicate_uuid` too (neither is clearly the original: you choose).
//!   `duplicate_uuid`'s `other` is the first other folder of the group, by name.
//! - **`invalid`**: any other CORE-047 `needs_repair`: a manifest that doesn't parse (or whose
//!   `project` or `asset_id` isn't a string), a manifest file not named `<asset_id>.manifest.json`
//!   in a folder whose fields agree, or a disagreeing manifest without a UUID.
//!
//! [`detect_known`] takes the **live** rows (whose manifest isn't `missing`: a folder that is
//! gone holds no UUID) and the gone rows the index still has, which only tell what asset was at
//! a place; [`detect`] is the same with no gone rows. A row not in the result needs no repair.

use std::collections::{BTreeMap, HashMap};

use lil_schema::{is_asset_id, is_slug};
use serde_json::{Value, json};

use super::AssetRow;

/// What repair an asset folder needs (Appendix H `repair.kind`); names are
/// `<project>/<asset_id>`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RepairKind {
    /// The folder's creation UUID is nowhere else and its manifest names `from`.
    Moved { from: String, to: String },
    /// A second folder with `other`'s creation UUID, whose manifest disagrees with it.
    Copy { other: String },
    /// One of several folders with one creation UUID (module docs).
    DuplicateUuid { other: String },
    /// Any other CORE-047 `needs_repair`.
    Invalid,
}

impl RepairKind {
    /// `moved`, `copy`, `duplicate_uuid` or `invalid`.
    pub fn as_str(&self) -> &'static str {
        match self {
            RepairKind::Moved { .. } => "moved",
            RepairKind::Copy { .. } => "copy",
            RepairKind::DuplicateUuid { .. } => "duplicate_uuid",
            RepairKind::Invalid => "invalid",
        }
    }

    /// The wire form, `{kind, from?, to?, other?}`.
    pub fn to_json(&self) -> Value {
        match self {
            RepairKind::Moved { from, to } => json!({"kind": "moved", "from": from, "to": to}),
            RepairKind::Copy { other } => json!({"kind": "copy", "other": other}),
            RepairKind::DuplicateUuid { other } => {
                json!({"kind": "duplicate_uuid", "other": other})
            }
            RepairKind::Invalid => json!({"kind": "invalid"}),
        }
    }

    /// The wire form read back; `None` for anything else.
    pub fn from_json(v: &Value) -> Option<RepairKind> {
        let text = |key: &str| v.get(key).and_then(Value::as_str).map(str::to_string);
        Some(match v.get("kind")?.as_str()? {
            "moved" => RepairKind::Moved {
                from: text("from")?,
                to: text("to")?,
            },
            "copy" => RepairKind::Copy {
                other: text("other")?,
            },
            "duplicate_uuid" => RepairKind::DuplicateUuid {
                other: text("other")?,
            },
            "invalid" => RepairKind::Invalid,
            _ => return None,
        })
    }
}

/// `(project, asset_id)` of the folder.
fn folder(row: &AssetRow) -> (String, String) {
    (row.project.clone(), row.asset_id.clone())
}

fn name(n: &(String, String)) -> String {
    format!("{}/{}", n.0, n.1)
}

/// The place the manifest names: its own `project` and `asset_id`, the folder's for one that is
/// absent. `None` when the manifest doesn't parse.
fn place(row: &AssetRow) -> Option<(String, String)> {
    row.manifest_parses.then(|| {
        (
            row.manifest_project
                .clone()
                .unwrap_or_else(|| row.project.clone()),
            row.manifest_asset_id
                .clone()
                .unwrap_or_else(|| row.asset_id.clone()),
        )
    })
}

/// The manifest parses and names its folder (exactly, as the CORE-047 check compares).
pub fn agrees(row: &AssetRow) -> bool {
    place(row).is_some_and(|p| p == folder(row))
}

/// The manifest file is `<asset_id>.manifest.json` (without regard to case).
fn canonical_file(row: &AssetRow) -> bool {
    row.manifest
        .file_name()
        .eq_ignore_ascii_case(&format!("{}.manifest.json", row.asset_id))
}

/// The CORE-047 check alone: the manifest doesn't parse, disagrees with its folder, or isn't
/// named for it.
pub fn needs_repair_alone(row: &AssetRow) -> bool {
    !agrees(row) || !canonical_file(row)
}

/// The row's creation UUID as grouped: lowercase; `None` for none, an empty one, or a manifest
/// that doesn't parse.
pub fn uuid_key(row: &AssetRow) -> Option<String> {
    if !row.manifest_parses {
        return None;
    }
    row.creation_uuid
        .as_deref()
        .filter(|u| !u.is_empty())
        .map(str::to_ascii_lowercase)
}

/// The repair each live asset folder of `rows` needs (module docs), by `(project, asset_id)`;
/// a folder that needs none isn't listed. The index knows no gone folder here.
pub fn detect(rows: &[AssetRow]) -> HashMap<(String, String), RepairKind> {
    detect_known(rows, &[])
}

/// [`detect`], with the gone folders the index still knows (`gone`, their manifests `missing`):
/// a lone folder naming a place where one of them held another creation UUID is `invalid`, not
/// `moved` (module docs).
pub fn detect_known(rows: &[AssetRow], gone: &[AssetRow]) -> HashMap<(String, String), RepairKind> {
    // The UUIDs of the gone folders, by their name without regard to case.
    let mut held: HashMap<(String, String), Vec<Option<String>>> = HashMap::new();
    for g in gone {
        let f = folder(g);
        held.entry((f.0.to_ascii_lowercase(), f.1.to_ascii_lowercase()))
            .or_default()
            .push(uuid_key(g));
    }
    let mut groups: BTreeMap<String, Vec<&AssetRow>> = BTreeMap::new();
    for row in rows {
        if let Some(uuid) = uuid_key(row) {
            groups.entry(uuid).or_default().push(row);
        }
    }
    // The live folders, by their name without regard to case.
    let live: HashMap<(String, String), (String, String)> = rows
        .iter()
        .map(|r| {
            let f = folder(r);
            ((f.0.to_ascii_lowercase(), f.1.to_ascii_lowercase()), f)
        })
        .collect();
    let mut out = HashMap::new();
    for row in rows {
        let kind = match uuid_key(row) {
            None => needs_repair_alone(row).then_some(RepairKind::Invalid),
            Some(uuid) => {
                let mut group = groups[&uuid].clone();
                group.sort_by_key(|r| folder(r));
                group.dedup_by_key(|r| folder(r));
                if group.len() == 1 {
                    alone(row, &live, &held)
                } else {
                    Some(in_group(row, &group))
                }
            }
        };
        if let Some(kind) = kind {
            out.insert(folder(row), kind);
        }
    }
    out
}

/// A folder whose UUID is nowhere else; `live` maps each live folder's lowercased name to it,
/// `held` each gone folder's to the UUIDs the index has there.
fn alone(
    row: &AssetRow,
    live: &HashMap<(String, String), (String, String)>,
    held: &HashMap<(String, String), Vec<Option<String>>>,
) -> Option<RepairKind> {
    if agrees(row) {
        return (!canonical_file(row)).then_some(RepairKind::Invalid);
    }
    let here = folder(row);
    let from = place(row)?;
    let same_folder = from.0.eq_ignore_ascii_case(&here.0) && from.1.eq_ignore_ascii_case(&here.1);
    if same_folder {
        return Some(RepairKind::Invalid);
    }
    let lowered = (from.0.to_ascii_lowercase(), from.1.to_ascii_lowercase());
    if let Some(original) = live.get(&lowered) {
        return Some(RepairKind::Copy {
            other: name(original),
        });
    }
    let mine = uuid_key(row);
    if held
        .get(&lowered)
        .is_some_and(|uuids| uuids.iter().any(|u| *u != mine))
    {
        return Some(RepairKind::Invalid);
    }
    Some(if is_slug(&from.0) && is_asset_id(&from.1) {
        RepairKind::Moved {
            from: name(&from),
            to: name(&here),
        }
    } else {
        RepairKind::Invalid
    })
}

/// A folder of a group of two or more (sorted by name, one entry per folder).
fn in_group(row: &AssetRow, group: &[&AssetRow]) -> RepairKind {
    let here = folder(row);
    let agreeing: Vec<&&AssetRow> = group.iter().filter(|r| agrees(r)).collect();
    if !agrees(row)
        && let [original] = agreeing.as_slice()
    {
        return RepairKind::Copy {
            other: name(&folder(original)),
        };
    }
    let other = group
        .iter()
        .map(|r| folder(r))
        .find(|f| *f != here)
        .expect("a group of two or more has another folder");
    RepairKind::DuplicateUuid {
        other: name(&other),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::WsPath;

    fn row(folder: &str, manifest: (&str, &str), uuid: Option<&str>) -> AssetRow {
        let (p, a) = folder.split_once('/').unwrap();
        AssetRow {
            project: p.into(),
            asset_id: a.into(),
            asset_type: None,
            status: None,
            version: None,
            approved_version: None,
            manifest: WsPath::from_disk(format!("projects/{p}/assets/{a}/{a}.manifest.json")),
            creation_uuid: uuid.map(str::to_string),
            updated: 0,
            needs_repair: false,
            tags: Vec::new(),
            rating: 0,
            label: None,
            runs_waiting: 0,
            external: false,
            working_doc: false,
            display: None,
            last_change: None,
            repair: None,
            manifest_project: Some(manifest.0.into()),
            manifest_asset_id: Some(manifest.1.into()),
            manifest_parses: true,
        }
    }

    fn key(n: &str) -> (String, String) {
        let (p, a) = n.split_once('/').unwrap();
        (p.into(), a.into())
    }

    #[test] // spec: CORE-154, CORE-047
    fn kinds_follow_appendix_h() {
        // Moved: alone, disagreeing.
        let got = detect(&[row("s/b", ("s", "a"), Some("U1"))]);
        assert_eq!(
            got[&key("s/b")],
            RepairKind::Moved {
                from: "s/a".into(),
                to: "s/b".into()
            }
        );
        // A copy beside its original; the UUID compared without regard to case.
        let got = detect(&[
            row("s/a", ("s", "a"), Some("u1")),
            row("s/c", ("s", "a"), Some("U1")),
        ]);
        assert_eq!(
            got[&key("s/c")],
            RepairKind::Copy {
                other: "s/a".into()
            }
        );
        assert_eq!(
            got[&key("s/a")],
            RepairKind::DuplicateUuid {
                other: "s/c".into()
            }
        );
        // Both disagree (a move and a copy at once): both duplicate_uuid.
        let got = detect(&[
            row("s/b", ("s", "a"), Some("u1")),
            row("s/c", ("s", "a"), Some("u1")),
        ]);
        assert_eq!(got[&key("s/b")].as_str(), "duplicate_uuid");
        assert_eq!(got[&key("s/c")].as_str(), "duplicate_uuid");
        // Alone with its UUID but naming a live folder (any UUID, any case): a copy of it.
        let got = detect(&[
            row("s/Orig", ("s", "x"), Some("u2")),
            row("s/c", ("s", "orig"), Some("u1")),
        ]);
        assert_eq!(
            got[&key("s/c")],
            RepairKind::Copy {
                other: "s/Orig".into()
            }
        );
        // A place where the index knows a gone asset: the same UUID is a move; another UUID,
        // or none, is another asset's place, so invalid.
        let mover = row("s/b", ("s", "a"), Some("u1"));
        let gone_same = row("s/a", ("s", "a"), Some("U1"));
        let gone_other = row("s/A", ("s", "a"), Some("u9"));
        let gone_legacy = row("s/a", ("s", "a"), None);
        assert_eq!(
            detect_known(std::slice::from_ref(&mover), &[gone_same])[&key("s/b")].as_str(),
            "moved"
        );
        for g in [gone_other, gone_legacy] {
            assert_eq!(
                detect_known(std::slice::from_ref(&mover), &[g])[&key("s/b")],
                RepairKind::Invalid
            );
        }
        // Legacy: never moved; a disagreeing one is invalid, an agreeing one fine.
        let got = detect(&[row("s/b", ("s", "a"), None), row("s/a", ("s", "a"), None)]);
        assert_eq!(got[&key("s/b")], RepairKind::Invalid);
        assert!(!got.contains_key(&key("s/a")));
        // A case-only difference names the folder itself: invalid, not moved.
        let got = detect(&[row("s/Prop", ("s", "prop"), Some("u1"))]);
        assert_eq!(got[&key("s/Prop")], RepairKind::Invalid);
        // A place that isn't a name: invalid.
        let got = detect(&[row("s/b", ("s", "Not A Name"), Some("u1"))]);
        assert_eq!(got[&key("s/b")], RepairKind::Invalid);
        // An agreeing manifest under another file name: invalid.
        let mut r = row("s/a", ("s", "a"), Some("u1"));
        r.manifest = WsPath::from_disk("projects/s/assets/a/old.manifest.json".to_string());
        assert_eq!(detect(&[r])[&key("s/a")], RepairKind::Invalid);
        // Wire form round trip.
        for k in [
            RepairKind::Moved {
                from: "s/a".into(),
                to: "s/b".into(),
            },
            RepairKind::Copy {
                other: "s/a".into(),
            },
            RepairKind::DuplicateUuid {
                other: "s/a".into(),
            },
            RepairKind::Invalid,
        ] {
            assert_eq!(RepairKind::from_json(&k.to_json()), Some(k));
        }
    }
}
