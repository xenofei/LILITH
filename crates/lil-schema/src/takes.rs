//! Takes and the sign-off's auto pick (Appendix H "Takes" and "Manifest fields"; CORE-023,
//! CORE-052, CORE-152, CORE-176; P4b-R3, P4b-R13).
//!
//! A **take** is one output of a run: its primary file plus optional companions. Files carry
//! `take` (1-based) and one per take carries `primary: true`; the take's flag and rating live
//! on its primary file entry. A run in which no file has `take` reads by the legacy rule:
//! each `candidate`-role file is its own take, numbered in file order, and a `derived`,
//! `repair` or `preview` file joins the take of its `from` file as a companion.
//!
//! Files are named by their bare file name (the last `/` component of `path`), as `selected`
//! and `from` name them; a `from` is matched by its bare name without regard to case.

use std::collections::{BTreeMap, HashMap};

use serde_json::Value;

use crate::content_hash::lines_sha256;
use crate::manifest::{CandidateFile, CandidateRun, Obj, Reader};
use crate::marks::Flag;
use crate::version::Version;

/// One take of a run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Take {
    /// 1-based.
    pub n: u32,
    /// The primary file's name.
    pub primary: String,
    /// The names of the take's other files, in file order (its `preview` file included).
    pub companions: Vec<String>,
    /// The name of the first `preview`-role companion: the thumbnail when the primary has no
    /// renderer.
    pub preview: Option<String>,
    /// The primary file entry's `flag`.
    pub flag: Option<Flag>,
    /// The primary file entry's `rating` (0 unrated).
    pub rating: u8,
}

/// The bare name of a run file's `path` or a `from` value.
pub(crate) fn file_name(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

/// The takes of a run, by take number (P4b-R3, the legacy rule included). A take without a
/// primary file uses its first file; a file without `take` in a run whose other files have
/// one is in no take; the manifest's read warns of both.
pub fn takes_of(run: &CandidateRun) -> Vec<Take> {
    group(&run.files).0
}

/// The takes of a run's files, and a warning for each problem in the grouping.
pub(crate) fn group(files: &[CandidateFile]) -> (Vec<Take>, Vec<String>) {
    let mut warnings = Vec::new();
    let take = |n: u32, primary: &CandidateFile, companions: &[&CandidateFile]| Take {
        n,
        primary: file_name(&primary.path).to_string(),
        companions: companions
            .iter()
            .map(|f| file_name(&f.path).to_string())
            .collect(),
        preview: companions
            .iter()
            .find(|f| f.role == "preview")
            .map(|f| file_name(&f.path).to_string()),
        flag: primary.flag,
        rating: primary.rating,
    };

    if files.iter().any(|f| f.take.is_some()) {
        let mut by_n: BTreeMap<u32, Vec<&CandidateFile>> = BTreeMap::new();
        for f in files {
            match f.take {
                Some(n) => by_n.entry(n).or_default().push(f),
                None => warnings.push(format!(
                    "`{}` has no `take` in a run whose other files have one: it is in no take",
                    file_name(&f.path)
                )),
            }
        }
        let takes = by_n
            .into_iter()
            .map(|(n, members)| {
                let primaries: Vec<usize> =
                    (0..members.len()).filter(|&i| members[i].primary).collect();
                let p = match primaries.as_slice() {
                    [] => {
                        warnings.push(format!(
                            "take {n} has no primary file: its first file, `{}`, is used",
                            file_name(&members[0].path)
                        ));
                        0
                    }
                    [one] => *one,
                    [first, ..] => {
                        warnings.push(format!(
                            "take {n} has more than one primary file: the first, `{}`, is used",
                            file_name(&members[*first].path)
                        ));
                        *first
                    }
                };
                let companions: Vec<&CandidateFile> = members
                    .iter()
                    .enumerate()
                    .filter(|&(i, _)| i != p)
                    .map(|(_, f)| *f)
                    .collect();
                take(n, members[p], &companions)
            })
            .collect();
        return (takes, warnings);
    }

    // The legacy rule. `owner[i]` is the index into `primaries` of file i's take.
    let key = |name: &str| file_name(name).to_lowercase();
    let primaries: Vec<usize> = (0..files.len())
        .filter(|&i| files[i].role == "candidate")
        .collect();
    let mut owner: Vec<Option<usize>> = vec![None; files.len()];
    let mut by_name: HashMap<String, usize> = HashMap::new();
    for (t, &i) in primaries.iter().enumerate() {
        owner[i] = Some(t);
        by_name.entry(key(&files[i].path)).or_insert(t);
    }
    // A companion may come from another companion, listed before or after it.
    loop {
        let mut changed = false;
        for (i, f) in files.iter().enumerate() {
            if owner[i].is_some() || !matches!(f.role.as_str(), "derived" | "repair" | "preview") {
                continue;
            }
            if let Some(&t) = f.from.as_deref().and_then(|from| by_name.get(&key(from))) {
                owner[i] = Some(t);
                by_name.entry(key(&f.path)).or_insert(t);
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    let takes = primaries
        .iter()
        .enumerate()
        .map(|(t, &p)| {
            let companions: Vec<&CandidateFile> = (0..files.len())
                .filter(|&i| i != p && owner[i] == Some(t))
                .map(|i| &files[i])
                .collect();
            take(t as u32 + 1, &files[p], &companions)
        })
        .collect();
    (takes, warnings)
}

/// A take's hash (P4b-R13): the SHA-256 of the lines `<sha256>  <file name>\n` of the take's
/// files, sorted by name in byte order (the version content-hash form). `files` holds
/// `(file name, sha256 hex)` pairs, in any order.
pub fn take_sha256(files: &[(String, String)]) -> String {
    lines_sha256(
        files
            .iter()
            .map(|(name, sha)| (name.as_str(), sha.as_str()))
            .collect(),
    )
}

/// Why the auto pick chose its take: `vision_score` or `lowest_seed`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AutoPickReason {
    VisionScore,
    LowestSeed,
}

impl AutoPickReason {
    /// The wire value.
    pub fn as_str(self) -> &'static str {
        match self {
            AutoPickReason::VisionScore => "vision_score",
            AutoPickReason::LowestSeed => "lowest_seed",
        }
    }
}

/// A gate's result: `pass` or `fail`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Gate {
    Pass,
    Fail,
}

/// An `auto_pick.candidates` entry.
#[derive(Debug, Clone, PartialEq)]
pub struct AutoPickCandidate {
    pub file: String,
    /// Each gate's name and result, in the manifest's order.
    pub gates: Vec<(String, Gate)>,
    pub vision_score: Option<f64>,
    pub vision_reason: Option<String>,
}

/// `signed_off.auto_pick` (CORE-052).
#[derive(Debug, Clone, PartialEq)]
pub struct AutoPick {
    pub picked: String,
    pub reason: AutoPickReason,
    pub candidates: Vec<AutoPickCandidate>,
}

/// `signed_off.overrides_auto_pick`: the auto-picked version this sign-off replaced, and what
/// it had picked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OverridesAutoPick {
    pub version: Version,
    pub picked: String,
}

/// `auto_pick` of the `signed_off` object `o`; malformed is none with a warning, and a
/// malformed candidate or gate is skipped with a warning.
pub(crate) fn read_auto_pick(r: &mut Reader, o: Obj<'_>) -> Option<AutoPick> {
    let at = Reader::ptr(o, "auto_pick");
    let map = match Reader::field(o, "auto_pick")? {
        Value::Object(m) => m,
        _ => {
            r.warn(at, "expected an object");
            return None;
        }
    };
    let a = Obj { map, at: &at };
    let reason = match Reader::req_str(a, "reason").as_deref() {
        Some("vision_score") => Some(AutoPickReason::VisionScore),
        Some("lowest_seed") => Some(AutoPickReason::LowestSeed),
        _ => None,
    };
    let (Some(picked), Some(reason)) = (Reader::req_str(a, "picked"), reason) else {
        r.warn(
            at.clone(),
            "ignored: `picked` and a `reason` of vision_score or lowest_seed are required",
        );
        return None;
    };
    let mut candidates = Vec::new();
    for (c_at, m) in r.objects(a, "candidates") {
        let c = Obj { map: m, at: &c_at };
        let Some(file) = Reader::req_str(c, "file") else {
            r.skipped(&c_at, "`file` is missing or not a string");
            continue;
        };
        let mut gates = Vec::new();
        match Reader::field(c, "gates") {
            None => {}
            Some(Value::Object(g)) => {
                for (name, value) in g {
                    match value.as_str() {
                        Some("pass") => gates.push((name.clone(), Gate::Pass)),
                        Some("fail") => gates.push((name.clone(), Gate::Fail)),
                        _ => r.skipped(
                            &format!("{}/{}", Reader::ptr(c, "gates"), crate::manifest::esc(name)),
                            "expected `pass` or `fail`",
                        ),
                    }
                }
            }
            Some(_) => r.warn(Reader::ptr(c, "gates"), "expected an object"),
        }
        let vision_score = match Reader::field(c, "vision_score") {
            None => None,
            Some(v) => {
                let n = v.as_f64();
                if n.is_none() {
                    r.warn(Reader::ptr(c, "vision_score"), "expected a number");
                }
                n
            }
        };
        candidates.push(AutoPickCandidate {
            file,
            gates,
            vision_score,
            vision_reason: r.opt_str(c, "vision_reason"),
        });
    }
    Some(AutoPick {
        picked,
        reason,
        candidates,
    })
}

/// `overrides_auto_pick` of the `signed_off` object `o`; malformed is none with a warning.
pub(crate) fn read_overrides(r: &mut Reader, o: Obj<'_>) -> Option<OverridesAutoPick> {
    let at = Reader::ptr(o, "overrides_auto_pick");
    let Value::Object(map) = Reader::field(o, "overrides_auto_pick")? else {
        r.warn(at, "expected an object");
        return None;
    };
    let v = Obj { map, at: &at };
    let version = Reader::req_str(v, "version").and_then(|s| s.parse().ok());
    match (version, Reader::req_str(v, "picked")) {
        (Some(version), Some(picked)) => Some(OverridesAutoPick { version, picked }),
        _ => {
            r.warn(
                at.clone(),
                "ignored: a valid `version` and `picked` are required",
            );
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::content_hash::sha256_hex;
    use crate::manifest::{CandidateFile, CandidateRun, Manifest};
    use crate::marks::Flag;
    use crate::version::Version;
    use serde_json::json;

    const A: &str = "0eaf94256e9ad75cf113dceeb38e3c2730ae35d2888f625f6418165b41f743d3";
    const B: &str = "5cd3c91a2cdc2ee698eabfe804b94b30db24f3dccb166a26eb06718830bc6b52";

    fn file(name: &str, role: &str) -> CandidateFile {
        CandidateFile {
            path: format!("candidates/r_20261101_0900/{name}"),
            role: role.into(),
            ..Default::default()
        }
    }

    fn run(files: Vec<CandidateFile>) -> CandidateRun {
        CandidateRun {
            run_id: "r_20261101_0900".into(),
            files,
            ..Default::default()
        }
    }

    fn names(v: &[&str]) -> Vec<String> {
        v.iter().map(|n| n.to_string()).collect()
    }

    #[test] // spec: CORE-023, CORE-152
    fn legacy_runs_read_as_one_take_per_candidate() {
        let mut a = file("a.png", "candidate");
        a.flag = Some(Flag::Pick);
        a.rating = 4;
        // A repair made from a derived file, listed before it and named by its path.
        let mut big = file("a_big.png", "repair");
        big.from = Some("candidates/r_20261101_0900/a_cut.png".into());
        let mut cut = file("a_cut.png", "derived");
        cut.from = Some("a.png".into());
        let b = file("b.png", "candidate");
        let mut prev = file("b_prev.jpg", "preview");
        prev.from = Some("B.png".into());
        // No `from`: in no take.
        let stray = file("stray.png", "derived");
        let (takes, warnings) = group(&run(vec![a, big, cut, b, prev, stray]).files);
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(
            takes,
            [
                Take {
                    n: 1,
                    primary: "a.png".into(),
                    companions: names(&["a_big.png", "a_cut.png"]),
                    preview: None,
                    flag: Some(Flag::Pick),
                    rating: 4,
                },
                Take {
                    n: 2,
                    primary: "b.png".into(),
                    companions: names(&["b_prev.jpg"]),
                    preview: Some("b_prev.jpg".into()),
                    flag: None,
                    rating: 0,
                },
            ]
        );

        // The sample workspace's open run: one candidate and its cut-out.
        let crane = include_str!(
            "../../lil-spec-tests/fixtures/sample-workspace/projects/sketchbook/assets/chr_paper_crane/chr_paper_crane.manifest.json"
        );
        let (m, warnings) = Manifest::parse(crane).unwrap();
        assert!(warnings.is_empty(), "{warnings:?}");
        let v = m.view();
        let takes = takes_of(&v.candidate_runs[1]);
        assert_eq!(takes.len(), 1);
        assert_eq!(takes[0].primary, "crane_c.png");
        assert_eq!(takes[0].companions, ["crane_c_cutout.png"]);
        let takes = takes_of(&v.candidate_runs[0]);
        let primaries: Vec<&str> = takes.iter().map(|t| t.primary.as_str()).collect();
        assert_eq!(primaries, ["crane_a.png", "crane_b.png"]);
    }

    #[test] // spec: CORE-023, CORE-152
    fn explicit_takes_group_files_with_one_primary() {
        let mut x2 = file("x2.png", "candidate");
        x2.take = Some(2);
        let mut p2 = file("p2.png", "candidate");
        p2.take = Some(2);
        p2.primary = true;
        p2.flag = Some(Flag::Reject);
        p2.rating = 2;
        let mut p1 = file("p1.exr", "candidate");
        p1.take = Some(1);
        p1.primary = true;
        p1.rating = 5;
        let mut c1 = file("p1_cut.exr", "derived");
        c1.take = Some(1);
        c1.from = Some("p1.exr".into());
        let mut pv1 = file("p1_prev.png", "preview");
        pv1.take = Some(1);
        let (takes, warnings) = group(&[x2, p2, p1, c1, pv1]);
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(
            takes,
            [
                Take {
                    n: 1,
                    primary: "p1.exr".into(),
                    companions: names(&["p1_cut.exr", "p1_prev.png"]),
                    preview: Some("p1_prev.png".into()),
                    flag: None,
                    rating: 5,
                },
                Take {
                    n: 2,
                    primary: "p2.png".into(),
                    companions: names(&["x2.png"]),
                    preview: None,
                    flag: Some(Flag::Reject),
                    rating: 2,
                },
            ]
        );
    }

    #[test] // spec: CORE-023, ARCH-076
    fn a_take_without_primary_uses_its_first_file() {
        let text = json!({
            "schema_version": 1, "asset_id": "img_dock", "project": "sketchbook",
            "candidate_runs": [{"run_id": "r_20261101_0900", "files": [
                {"path": "candidates/r_20261101_0900/a.png", "role": "candidate", "take": 1},
                {"path": "candidates/r_20261101_0900/b.png", "role": "candidate", "take": 1},
                {"path": "candidates/r_20261101_0900/c.png", "role": "candidate", "take": 2,
                 "primary": true},
                {"path": "candidates/r_20261101_0900/d.png", "role": "candidate", "take": 2,
                 "primary": true},
                {"path": "candidates/r_20261101_0900/e.png", "role": "candidate"},
                {"path": "candidates/r_20261101_0900/f.png", "role": "candidate", "take": 0}
            ]}]
        })
        .to_string();
        let (m, warnings) = Manifest::parse(&text).unwrap();
        let takes = takes_of(&m.view().candidate_runs[0]);
        assert_eq!(takes.len(), 2);
        assert_eq!(takes[0].primary, "a.png");
        assert_eq!(takes[0].companions, ["b.png"]);
        // Two primaries: the first is used.
        assert_eq!(takes[1].primary, "c.png");
        assert_eq!(takes[1].companions, ["d.png"]);
        let on_files: Vec<&str> = warnings
            .iter()
            .filter(|w| w.pointer == "/candidate_runs/0/files")
            .map(|w| w.message.as_str())
            .collect();
        assert_eq!(on_files.len(), 4, "{warnings:?}");
        assert!(
            on_files.iter().any(|w| w.contains("take 1")),
            "{on_files:?}"
        );
        assert!(
            on_files.iter().any(|w| w.contains("take 2")),
            "{on_files:?}"
        );
        assert!(on_files.iter().any(|w| w.contains("e.png")), "{on_files:?}");
        assert!(
            warnings
                .iter()
                .any(|w| w.pointer == "/candidate_runs/0/files/5/take"),
            "take 0 is not a take number: {warnings:?}"
        );
    }

    #[test] // spec: CORE-176
    fn take_sha256_is_order_independent() {
        let a = ("a.png".to_string(), A.to_string());
        let b = ("B.png".to_string(), B.to_string());
        let one = take_sha256(&[a.clone(), b.clone()]);
        assert_eq!(one, take_sha256(&[b.clone(), a.clone()]));
        // `<sha256>  <file name>\n` lines sorted by name in byte order ('B' < 'a').
        assert_eq!(
            one,
            sha256_hex(format!("{B}  B.png\n{A}  a.png\n").as_bytes())
        );
        assert_ne!(one, take_sha256(std::slice::from_ref(&a)));
        assert_ne!(one, take_sha256(&[(a.0, B.into()), (b.0, A.into())]));
    }

    #[test] // spec: CORE-051, CORE-052
    fn signed_off_reads_takes_companions_and_auto_pick() {
        let text = json!({
            "schema_version": 1, "asset_id": "img_dock", "project": "sketchbook",
            "candidate_runs": [{"run_id": "r_20261101_0900", "status": "selected",
                "signed_off": {"by": "auto", "date": "2026-11-01", "version": "1.1.0",
                    "via": {"rule": "auto_approve"}, "takes": [2], "companions": ["b_prev.jpg"],
                    "auto_pick": {"picked": "b.png", "reason": "lowest_seed", "candidates": [
                        {"file": "a.png", "gates": {"size": "fail"}},
                        {"file": "b.png", "gates": {"size": "pass", "alpha": "pass"},
                         "vision_score": 0.75, "vision_reason": "cleanest"}
                    ]},
                    "overrides_auto_pick": {"version": "1.0.0", "picked": "a.png"}}}]
        })
        .to_string();
        let (m, warnings) = Manifest::parse(&text).unwrap();
        assert!(warnings.is_empty(), "{warnings:?}");
        let s = m.view().candidate_runs[0].signed_off.clone().unwrap();
        assert_eq!(s.by, "auto");
        assert_eq!(s.via.unwrap().rule.as_deref(), Some("auto_approve"));
        assert_eq!(s.takes, [2]);
        assert_eq!(s.companions, ["b_prev.jpg"]);
        assert_eq!(
            s.auto_pick,
            Some(AutoPick {
                picked: "b.png".into(),
                reason: AutoPickReason::LowestSeed,
                candidates: vec![
                    AutoPickCandidate {
                        file: "a.png".into(),
                        gates: vec![("size".into(), Gate::Fail)],
                        vision_score: None,
                        vision_reason: None,
                    },
                    AutoPickCandidate {
                        file: "b.png".into(),
                        gates: vec![("size".into(), Gate::Pass), ("alpha".into(), Gate::Pass)],
                        vision_score: Some(0.75),
                        vision_reason: Some("cleanest".into()),
                    },
                ],
            })
        );
        assert_eq!(
            s.overrides_auto_pick,
            Some(OverridesAutoPick {
                version: Version::new(1, 0, 0),
                picked: "a.png".into(),
            })
        );
    }
}
