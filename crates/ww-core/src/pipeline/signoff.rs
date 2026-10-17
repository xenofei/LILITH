//! Sign-off (CORE-026, CORE-027, CORE-032, CORE-051, CORE-052; I5 "Sign-off"): who signed
//! off, the manifest after a sign-off, and the automatic pick.
//!
//! [`plan_signoff`] is I5's step 3 on a copy of the manifest, plus step 2 for the chosen
//! files (their `hashes`, from each run file record's `sha256`), so the snapshot written
//! before the move (step 4) already holds them:
//! - the version is `approved_version` bumped by the confirmed `bump`; without one, the
//!   manifest's `version` when it is past `approved_version` (new work set it by the bump
//!   rules), else the run's `target_version` when that is, else `approved_version` bumped by
//!   `minor`. With nothing approved yet it is the manifest's `version` (`1.0.0` when unset);
//! - `approved_version` and `version` become it, `status` goes `approved` through the status
//!   machine;
//! - the run becomes `selected`, with `selected` (the chosen names) and
//!   `signed_off: {by, date, version}` (`by` is CORE-051's `approved_by`);
//! - the other open runs for that version (targeting it or an older one, or nothing) become
//!   `rejected`, with `rejected_at_version`, so their 90 days start at this sign-off; those in
//!   `keep_runs` stay open with `pinned: true` instead (CORE-032);
//! - `hashes["v<version>/<file>"] = "sha256:<hex>"` for each chosen file.

use std::collections::BTreeMap;

use serde_json::{Map, Value, json};
use ww_protocol::codes::{
    ASSET_UNKNOWN, MANIFEST_NEEDS_REPAIR, PARAMS_INVALID, STATUS_INVALID_TRANSITION, VERSION_EXISTS,
};
use ww_protocol::{CallerKind, CodeSpec, ErrorBody};
use ww_schema::{
    Bump, CandidateFile, Manifest, Status, StatusEvent, Version, hash_value, transition,
};

/// The `signed_off.by` value of a sign-off by `caller` (CORE-051): `user` for an app
/// (`ui:*`), `assistant`, `claude-code` (`mcp`) or `cli`.
pub fn signed_off_by(caller: &CallerKind) -> &'static str {
    match caller {
        CallerKind::Ui(_) => "user",
        CallerKind::Assistant => "assistant",
        CallerKind::Mcp => "claude-code",
        CallerKind::Cli => "cli",
    }
}

fn fail(spec: &CodeSpec, message: String) -> ErrorBody {
    ErrorBody::new(spec, message, "")
}

/// The bare name a run file record or a `from` value names.
fn bare_name(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

/// The manifest after signing off run `run_id`'s `files` (module docs), and the version it
/// seals. The errors carry an empty trace id.
///
/// # Errors
/// `asset.unknown` for a run the manifest doesn't have, a run that isn't open, a chosen name
/// that isn't one of the run's files, or a `keep_runs` id the manifest doesn't have;
/// `params.invalid` for no chosen file; `manifest.needs_repair` for a chosen file record
/// without a `sha256` (the caller hashes a pre-2b run's files first) or a `hashes` that isn't
/// an object; `version.exists` when a run was already signed off as the version;
/// `status.invalid_transition` when the asset's status can't take a sign-off.
pub fn plan_signoff(
    m: &Manifest,
    run_id: &str,
    files: &[String],
    bump: Option<Bump>,
    keep_runs: &[String],
    by: &str,
    now_utc: i64,
) -> Result<(Manifest, Version), ErrorBody> {
    let view = m.view();
    let same = |a: &str, b: &str| a.eq_ignore_ascii_case(b);
    let Some(run) = view.candidate_runs.iter().find(|r| same(&r.run_id, run_id)) else {
        return Err(fail(
            &ASSET_UNKNOWN,
            format!(
                "The asset `{}` has no candidate run `{run_id}`; asset.get lists its runs.",
                view.asset_id
            ),
        ));
    };
    let run_id = run.run_id.clone();
    if !matches!(run.status.as_deref(), None | Some("open")) || run.trashed_utc.is_some() {
        return Err(fail(
            &ASSET_UNKNOWN,
            format!(
                "The run `{run_id}` is {}, not open: only an open run can be signed off.",
                run.status.as_deref().unwrap_or("trashed")
            ),
        ));
    }
    for keep in keep_runs {
        if !view.candidate_runs.iter().any(|r| same(&r.run_id, keep)) {
            return Err(fail(
                &ASSET_UNKNOWN,
                format!("keep_runs names `{keep}`, which is not a run of this asset."),
            ));
        }
    }
    let mut chosen: Vec<(&str, &CandidateFile)> = Vec::new();
    for name in files {
        let Some(record) = run.files.iter().find(|f| same(bare_name(&f.path), name)) else {
            return Err(fail(
                &ASSET_UNKNOWN,
                format!("The run `{run_id}` has no file `{name}`."),
            ));
        };
        if !chosen.iter().any(|(n, _)| same(n, bare_name(&record.path))) {
            chosen.push((bare_name(&record.path), record));
        }
    }
    if chosen.is_empty() {
        return Err(fail(
            &PARAMS_INVALID,
            "A sign-off chooses at least one file of the run.".to_string(),
        ));
    }

    let version = match (view.approved_version, bump) {
        (None, _) => view.version.unwrap_or(Version::new(1, 0, 0)),
        (Some(approved), Some(b)) => approved.bump(b),
        (Some(approved), None) => match (view.version, run.target_version) {
            (Some(v), _) if v > approved => v,
            (_, Some(t)) if t > approved => t,
            _ => approved.bump(Bump::Minor),
        },
    };
    let taken = view.approved_version == Some(version)
        || view
            .candidate_runs
            .iter()
            .any(|r| r.signed_off.as_ref().is_some_and(|s| s.version == version));
    if taken {
        return Err(fail(
            &VERSION_EXISTS,
            format!(
                "Version {version} of `{}` is signed off already; confirm another bump.",
                view.asset_id
            ),
        ));
    }
    let parse = |key: &str| {
        m.get(key)
            .and_then(Value::as_str)
            .and_then(|s| s.parse::<Status>().ok())
    };
    let moved = transition(
        view.status.unwrap_or(Status::Planned),
        parse("status_before_stale"),
        parse("status_before_blocked"),
        &StatusEvent::Signoff,
    )
    .map_err(|why| fail(&STATUS_INVALID_TRANSITION, why))?;

    let mut out = m.clone();
    let needs_repair = |why: String| fail(&MANIFEST_NEEDS_REPAIR, why);
    let mut hashes: Map<String, Value> = match out.get("hashes") {
        None | Some(Value::Null) => Map::new(),
        Some(Value::Object(h)) => h.clone(),
        Some(_) => {
            return Err(needs_repair(
                "The manifest's `hashes` is not an object; repair it first.".to_string(),
            ));
        }
    };
    for (name, record) in &chosen {
        let Some(sha256) = &record.sha256 else {
            return Err(needs_repair(format!(
                "The run file `{name}` has no recorded sha256."
            )));
        };
        hashes.insert(
            format!("v{version}/{name}"),
            Value::from(hash_value(sha256)),
        );
    }
    out.set("hashes", Value::Object(hashes));
    out.set("version", Value::from(version.to_string()));
    out.set("approved_version", Value::from(version.to_string()));
    out.set("status", Value::from(moved.to.as_str()));

    let names: Vec<Value> = chosen.iter().map(|(n, _)| Value::from(*n)).collect();
    let date = ww_schema::time::date(now_utc);
    let keep = |id: &str| keep_runs.iter().any(|k| same(k, id));
    for r in out.runs_mut() {
        let id = r
            .get("run_id")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        if same(&id, &run_id) {
            r.insert("status".into(), Value::from("selected"));
            r.insert("selected".into(), Value::Array(names.clone()));
            r.insert(
                "signed_off".into(),
                json!({"by": by, "date": date, "version": version.to_string()}),
            );
            continue;
        }
        let open = matches!(r.get("status").and_then(Value::as_str), None | Some("open"))
            && r.get("trashed_utc").is_none_or(Value::is_null);
        let target = r
            .get("target_version")
            .and_then(Value::as_str)
            .and_then(|t| t.parse::<Version>().ok());
        if !open || target.is_some_and(|t| t > version) {
            continue;
        }
        if keep(&id) {
            r.insert("pinned".into(), Value::Bool(true));
        } else {
            r.insert("status".into(), Value::from("rejected"));
            r.insert(
                "rejected_at_version".into(),
                Value::from(version.to_string()),
            );
        }
    }
    Ok((out, version))
}

/// CORE-052's automatic pick among `files` (a run's records): of those that pass every
/// technical gate (`passes`, by bare name), the best vision-review score when a vision review
/// scored any of them (ties: the lower seed, then the name); otherwise the lowest seed (files
/// without a seed last, ties by name). `None` when no file passes.
pub fn auto_pick(
    files: &[CandidateFile],
    vision: Option<&BTreeMap<String, f64>>,
    passes: &dyn Fn(&str) -> bool,
) -> Option<String> {
    let passing: Vec<&CandidateFile> = files
        .iter()
        .filter(|f| passes(bare_name(&f.path)))
        .collect();
    let seed_then_name = |a: &&CandidateFile, b: &&CandidateFile| {
        let seed = |f: &CandidateFile| f.seed.unwrap_or(i64::MAX);
        seed(a)
            .cmp(&seed(b))
            .then_with(|| bare_name(&a.path).cmp(bare_name(&b.path)))
    };
    let score = |f: &CandidateFile| vision.and_then(|v| v.get(bare_name(&f.path)).copied());
    let scored: Vec<(&CandidateFile, f64)> = passing
        .iter()
        .filter_map(|f| score(f).map(|s| (*f, s)))
        .filter(|(_, s)| s.is_finite())
        .collect();
    let best = if scored.is_empty() {
        passing.into_iter().min_by(seed_then_name)
    } else {
        scored
            .into_iter()
            .min_by(|(a, sa), (b, sb)| sb.total_cmp(sa).then_with(|| seed_then_name(a, b)))
            .map(|(f, _)| f)
    };
    best.map(|f| bare_name(&f.path).to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest() -> Manifest {
        let text = json!({
            "asset_id": "prop_kite", "project": "sketchbook", "version": "1.0.0",
            "approved_version": null, "status": "candidate",
            "candidate_runs": [
                {"run_id": "r_1", "status": "open", "target_version": "1.0.0",
                 "files": [{"path": "candidates/r_1/a.png", "role": "candidate", "sha256": "a".repeat(64)}],
                 "selected": null, "signed_off": null},
                {"run_id": "r_2", "status": "open", "target_version": "1.0.0",
                 "files": [{"path": "candidates/r_2/b.png", "role": "candidate", "sha256": "b".repeat(64)}]},
                {"run_id": "r_3", "status": "open", "target_version": "1.0.0", "files": []},
            ],
            "hashes": {},
        });
        Manifest::parse(&text.to_string()).unwrap().0
    }

    #[test] // spec: CORE-051
    fn by_follows_the_caller() {
        assert_eq!(signed_off_by(&CallerKind::Ui("studio".into())), "user");
        assert_eq!(signed_off_by(&CallerKind::Assistant), "assistant");
        assert_eq!(signed_off_by(&CallerKind::Mcp), "claude-code");
        assert_eq!(signed_off_by(&CallerKind::Cli), "cli");
    }

    #[test] // spec: CORE-026, CORE-032
    fn signoff_selects_rejects_and_keeps() {
        let (m, v) = plan_signoff(
            &manifest(),
            "r_1",
            &["A.PNG".to_string()],
            None,
            &["r_3".to_string()],
            "cli",
            86_400,
        )
        .unwrap();
        assert_eq!(v, Version::new(1, 0, 0));
        let view = m.view();
        assert_eq!(view.approved_version, Some(v));
        assert_eq!(view.status, Some(Status::Approved));
        assert_eq!(
            view.hashes.get("v1.0.0/a.png").map(String::as_str),
            Some("a".repeat(64).as_str())
        );
        assert_eq!(
            m.get("hashes").unwrap()["v1.0.0/a.png"],
            json!(format!("sha256:{}", "a".repeat(64)))
        );
        let runs = &view.candidate_runs;
        assert_eq!(runs[0].status.as_deref(), Some("selected"));
        assert_eq!(runs[0].selected, ["a.png"]);
        let signed = runs[0].signed_off.as_ref().unwrap();
        assert_eq!(
            (signed.by.as_str(), signed.date.as_str()),
            ("cli", "1970-01-02")
        );
        assert_eq!(runs[1].status.as_deref(), Some("rejected"));
        assert_eq!(runs[1].rejected_at_version, Some(v));
        assert_eq!(runs[2].status.as_deref(), Some("open"));
        assert!(runs[2].pinned);
    }

    #[test] // spec: CORE-027
    fn the_confirmed_bump_wins() {
        let mut m = manifest();
        m.set("approved_version", json!("1.0.0"));
        m.set("version", json!("1.1.0"));
        let (_, v) = plan_signoff(
            &m,
            "r_2",
            &["b.png".into()],
            Some(Bump::Major),
            &[],
            "cli",
            0,
        )
        .unwrap();
        assert_eq!(v, Version::new(2, 0, 0));
        let (_, v) = plan_signoff(&m, "r_2", &["b.png".into()], None, &[], "cli", 0).unwrap();
        assert_eq!(v, Version::new(1, 1, 0));
    }

    #[test] // spec: CORE-026
    fn refusals() {
        let m = manifest();
        let code = |r: Result<(Manifest, Version), ErrorBody>| r.unwrap_err().code;
        assert_eq!(
            code(plan_signoff(
                &m,
                "r_9",
                &["a.png".into()],
                None,
                &[],
                "cli",
                0
            )),
            "asset.unknown"
        );
        assert_eq!(
            code(plan_signoff(
                &m,
                "r_1",
                &["x.png".into()],
                None,
                &[],
                "cli",
                0
            )),
            "asset.unknown"
        );
        assert_eq!(
            code(plan_signoff(&m, "r_1", &[], None, &[], "cli", 0)),
            "params.invalid"
        );
        assert_eq!(
            code(plan_signoff(
                &m,
                "r_1",
                &["a.png".into()],
                None,
                &["r_9".into()],
                "cli",
                0
            )),
            "asset.unknown"
        );
        let (signed, _) = plan_signoff(&m, "r_1", &["a.png".into()], None, &[], "cli", 0).unwrap();
        // r_1 is selected now, r_2 rejected.
        assert_eq!(
            code(plan_signoff(
                &signed,
                "r_1",
                &["a.png".into()],
                None,
                &[],
                "cli",
                0
            )),
            "asset.unknown"
        );
        let mut blocked = m.clone();
        blocked.set("status", json!("blocked"));
        assert_eq!(
            code(plan_signoff(
                &blocked,
                "r_1",
                &["a.png".into()],
                None,
                &[],
                "cli",
                0
            )),
            "status.invalid_transition"
        );
    }
}
