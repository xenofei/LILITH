//! Sign-off (CORE-026, CORE-027, CORE-032, CORE-051, CORE-052; I5 "Sign-off"): who signed
//! off, the manifest after a sign-off, and the automatic pick.
//!
//! [`plan_signoff`] is I5's step 3 on a copy of the manifest, plus step 2 for the chosen
//! files (their `hashes`, from each run file record's `sha256`), so the snapshot written
//! before the move (step 4) already holds them:
//! - an open run on an `approved`, `exported` or `released` asset (one kept open at an earlier
//!   sign-off) takes `CandidateRun` first, then `Signoff` (ruling Q23); `stale` is refused;
//!   `lil_schema::change_status` does both and logs one entry, from the stored status to
//!   `approved` (ruling PF-5);
//! - the version is `approved_version` bumped by the confirmed `bump`; without one (ruling
//!   Q22: omitting `bump` accepts the proposal), the
//!   manifest's `version` when it is past `approved_version` (new work set it by the bump
//!   rules), else the run's `target_version` when that is, else `approved_version` bumped by
//!   `minor`. With nothing approved yet it is the manifest's `version` (`1.0.0` when unset);
//! - `approved_version` and `version` become it, `status` goes `approved` through the status
//!   machine;
//! - the chosen files are named ([`Chosen::Files`]) or come from takes ([`Chosen::Takes`];
//!   Appendix H "Takes", P4b-R3): each take's primary, then the takes' companions, takes in
//!   ascending order, read through `lil_schema::takes_of` (the legacy rule included);
//! - the run becomes `selected`, with `selected` (the chosen names) and
//!   `signed_off: {by, date, version, via?}` (`by` is CORE-051's `approved_by`: the caller's
//!   [`signed_off_by`], or `auto` for a sign-off `auto_approve` makes, which names its rule in
//!   `via`; no tool argument sets either), plus `takes` and `companions` for a sign-off by takes;
//!   the status-log entry carries the same `by` and `via`;
//! - the other open runs for that version (targeting it or an older one, or nothing) become
//!   `rejected`, with `rejected_at_version`, so their 90 days start at this sign-off; those in
//!   `keep_runs` stay open with `pinned: true` instead (CORE-032);
//! - `hashes["v<version>/<file>"] = "sha256:<hex>"` for each chosen file;
//! - trust records go with the files to the version (ARCH-060; P4b-R28): each file the seal
//!   takes (the chosen ones and, through `from`, the run's `derived` files made from them, as
//!   the store's seal picks them) gets its run file's record at `v<version>/<name>`
//!   (`lil_schema::carry_trust`); a file made from another (`derived`, `repair` or `preview`)
//!   without a record takes its `from` file's first (`record::inherit_from`, which finds the
//!   `from` file as the seal does: by bare name among the run's files). Records are found as
//!   the seal finds names, without regard to case (`record::trust_key`). Each record under the
//!   run's `candidates/<run>/source/` goes to the same place under `v<version>/source/`, where
//!   the seal moves that folder. The run file's record stays (the run still names that path),
//!   so `app/asset.trust` without `paths` can list a candidate path whose file the seal moved.
//!   No sign-off, by anyone, clears a record.
//!
//! A chosen name with a `/` is the file's path in the asset or run folder; a bare name must
//! name exactly one file of the run (`params.invalid` otherwise), as `asset.mark` resolves a
//! file (`file_named`), and that file must be at the top of the run folder. An automatic
//! sign-off records its pick ([`auto_pick`]) as `signed_off.auto_pick` (CORE-052).
//! [`plan_signoff_with`] adds the override of an automatic pick (P4b-R38) and a
//! restore's reason and trust records (CORE-177); [`restored_run`] and [`add_restored_run`]
//! make a restore's run.

use std::collections::BTreeMap;

use lil_protocol::codes::{
    ASSET_UNKNOWN, MANIFEST_NEEDS_REPAIR, PARAMS_INVALID, STATUS_INVALID_TRANSITION, VERSION_EXISTS,
};
use lil_protocol::{CallerKind, CodeSpec, ErrorBody};
use lil_schema::{
    Actor, AutoPick, AutoPickCandidate, AutoPickReason, Bump, CandidateFile, CandidateRun, Gate,
    Manifest, Reason, StatusBy, StatusEvent, Version, Via, carry_trust, change_status, file_takes,
    hash_value, takes_of,
};
use serde_json::{Map, Value, json};

use super::record::{inherit_from, trust_key};
use super::status::status_by;
use crate::store::{name_key, path_key};

/// What a sign-off chooses: the run's files by name (`files`), or takes by number (`takes`;
/// Appendix H "Takes").
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Chosen {
    Files(Vec<String>),
    Takes(Vec<u32>),
}

/// The files a [`Chosen`] picks from `run`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChosenFiles {
    /// The names to seal, in `selected` order: for takes, the primaries in take order, then
    /// the companions.
    pub names: Vec<String>,
    /// For takes: the take numbers, ascending and each once.
    pub takes: Option<Vec<u32>>,
    /// For takes: the chosen takes' other files, take by take, in file order.
    pub companions: Vec<String>,
}

/// The files `chosen` picks from `run` (P4b-R3). Names are passed through as given; take
/// numbers are sorted and deduplicated. The errors carry an empty trace id.
///
/// # Errors
/// `params.invalid` for no take or take 0 (takes are 1-based); `asset.unknown` for a take the
/// run doesn't have.
pub fn chosen_files(run: &CandidateRun, chosen: &Chosen) -> Result<ChosenFiles, ErrorBody> {
    let wanted = match chosen {
        Chosen::Files(names) => {
            return Ok(ChosenFiles {
                names: names.clone(),
                takes: None,
                companions: Vec::new(),
            });
        }
        Chosen::Takes(takes) => takes,
    };
    if wanted.is_empty() {
        return Err(fail(
            &PARAMS_INVALID,
            "A sign-off by takes chooses at least one take of the run.".to_string(),
        ));
    }
    if wanted.contains(&0) {
        return Err(fail(
            &PARAMS_INVALID,
            "Takes are numbered from 1.".to_string(),
        ));
    }
    let mut numbers = wanted.clone();
    numbers.sort_unstable();
    numbers.dedup();
    let takes = takes_of(run);
    let mut names = Vec::new();
    let mut companions = Vec::new();
    for n in &numbers {
        let Some(take) = takes.iter().find(|t| t.n == *n) else {
            return Err(fail(
                &ASSET_UNKNOWN,
                format!("The run `{}` has no take {n}.", run.run_id),
            ));
        };
        names.push(take.primary.clone());
        companions.extend(take.companions.iter().cloned());
    }
    names.extend(companions.iter().cloned());
    Ok(ChosenFiles {
        names,
        takes: Some(numbers),
        companions,
    })
}

/// The `signed_off.by` value of a sign-off by `caller` (CORE-051): the status log's `by` for
/// that caller ([`status_by`], the one table): `user` for an app (`ui:*`), `assistant`,
/// `claude-code` (`mcp`) or `cli`.
pub fn signed_off_by(caller: &CallerKind) -> String {
    status_by(caller).as_str().to_string()
}

/// Who a sign-off is by (CORE-051; Appendix H "Manifest fields"): `signed_off.by` and its
/// optional `via`, which its status-log entry carries too. Only its constructors make one, so
/// no caller can name `auto` itself (ruling 4).
#[derive(Debug, Clone, PartialEq)]
pub struct SignedOffBy {
    /// `user`, `assistant`, `claude-code`, `cli` or `auto`.
    by: String,
    via: Option<Via>,
    /// An automatic sign-off's pick and why ([`auto_pick`]), recorded as
    /// `signed_off.auto_pick` (CORE-052).
    auto_pick: Option<AutoPick>,
}

impl SignedOffBy {
    /// A sign-off by `caller` ([`signed_off_by`]), with no `via`: every sign-off tool's.
    pub fn caller(caller: &CallerKind) -> SignedOffBy {
        SignedOffBy {
            by: signed_off_by(caller),
            via: None,
            auto_pick: None,
        }
    }

    /// The same sign-off, recording the automatic pick that chose its file (CORE-052).
    #[cfg(test)]
    pub(crate) fn with_auto_pick(mut self, pick: AutoPick) -> SignedOffBy {
        self.auto_pick = Some(pick);
        self
    }

    /// A sign-off `auto_approve` makes under `rule`: `by: auto`, `via: {rule}`. No tool argument
    /// asks for it, and until the `auto_approve` caller exists only this crate's tests make one
    /// (ruling 4).
    #[cfg(test)]
    pub(crate) fn auto(rule: &str) -> SignedOffBy {
        SignedOffBy {
            by: StatusBy::Auto.as_str().to_string(),
            via: Some(Via {
                task_id: None,
                conversation_id: None,
                rule: Some(rule.to_string()),
            }),
            auto_pick: None,
        }
    }
}

/// What a sign-off records beyond its files (Appendix H): the automatic pick it overrides
/// (`asset.signoff {overrides}`, P4b-R38) or the version a restore copied (CORE-177).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SignoffRecord {
    /// The version an automatic sign-off made, whose pick this sign-off overrides.
    pub overrides: Option<Version>,
    /// The version a restore as new version copied into the run.
    pub restored_from: Option<Version>,
}

fn fail(spec: &CodeSpec, message: String) -> ErrorBody {
    ErrorBody::new(spec, message, "")
}

/// The bare name a run file record or a `from` value names.
fn bare_name(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

/// The manifest after signing off the files `chosen` picks from run `run_id` (module docs),
/// and the version it seals, signed off `by` (`signed_off.by`, and `via` when it has one).
/// The errors carry an empty trace id.
///
/// # Errors
/// `asset.unknown` for a run the manifest doesn't have, a run that isn't open, a chosen name
/// that isn't one of the run's files, a take the run doesn't have, or a `keep_runs` id the
/// manifest doesn't have; `params.invalid` for no chosen file or take;
/// `manifest.needs_repair` for a chosen file record
/// without a `sha256` (the caller hashes a pre-2b run's files first) or a `hashes` that isn't
/// an object; `version.exists` when a run was already signed off as the version;
/// `status.invalid_transition` when the asset's status can't take a sign-off.
pub fn plan_signoff(
    m: &Manifest,
    run_id: &str,
    chosen: &Chosen,
    bump: Option<Bump>,
    keep_runs: &[String],
    by: &SignedOffBy,
    now_utc: i64,
) -> Result<(Manifest, Version), ErrorBody> {
    plan_signoff_with(
        m,
        run_id,
        chosen,
        bump,
        keep_runs,
        by,
        &SignoffRecord::default(),
        now_utc,
    )
}

/// The run folder of `run`, lower-cased, ending in `/`.
fn run_dir_of(run: &CandidateRun) -> String {
    let mut run_dir = run
        .path
        .clone()
        .unwrap_or_else(|| format!("candidates/{}/", run.run_id))
        .to_lowercase();
    if !run_dir.ends_with('/') {
        run_dir.push('/');
    }
    run_dir
}

/// Whether the record `f` of run `run` is the file `given` names (ruling 4; `asset.mark` and
/// sign-off resolve a file this way), compared without regard to case: a name with a `/` (or
/// `\`) is the file's path in the asset folder or in the run folder; a bare name is the file's
/// bare name (which more than one file may have).
pub(crate) fn file_named(run: &CandidateRun, f: &CandidateFile, given: &str) -> bool {
    let given = given.replace('\\', "/").to_lowercase();
    let path = f.path.to_lowercase();
    if given.contains('/') {
        path == given || path.strip_prefix(&run_dir_of(run)) == Some(given.as_str())
    } else {
        bare_name(&path) == given
    }
}

/// The record of run `run` that `name` names to sign off ([`file_named`]): a bare name must be
/// one file's, and the file must be at the top of the run folder, where a seal takes it.
///
/// # Errors
/// `asset.unknown` when no file matches; `params.invalid` for a bare name more than one file
/// has, or a file in a folder inside the run folder.
fn named_file<'r>(run: &'r CandidateRun, name: &str) -> Result<&'r CandidateFile, ErrorBody> {
    let hits: Vec<&CandidateFile> = run
        .files
        .iter()
        .filter(|f| file_named(run, f, name))
        .collect();
    match hits.as_slice() {
        [one]
            if one
                .path
                .to_lowercase()
                .strip_prefix(&run_dir_of(run))
                .is_some_and(|inside| inside.contains('/')) =>
        {
            Err(fail(
                &PARAMS_INVALID,
                format!(
                    "`{name}` is in a folder inside the run `{}`: only files at the top of the \
                     run folder can be signed off.",
                    run.run_id
                ),
            ))
        }
        [one] => Ok(one),
        [] => Err(fail(
            &ASSET_UNKNOWN,
            format!("The run `{}` has no file `{name}`.", run.run_id),
        )),
        _ => Err(fail(
            &PARAMS_INVALID,
            format!(
                "`{name}` names more than one file of the run `{}`: name the file's path in the \
                 run folder.",
                run.run_id
            ),
        )),
    }
}

/// The automatic sign-off an `overrides` names (P4b-R38): its run and what it picked.
///
/// # Errors
/// `params.invalid` when no run's sign-off made `version` or that sign-off's `by` isn't
/// `auto`, and when that automatic pick was overridden already (one override per automatic
/// version: the overriding sign-off replaced the run's record, so its run is no longer the
/// automatic one).
fn overridden(
    runs: &[CandidateRun],
    version: Version,
) -> Result<(&CandidateRun, String), ErrorBody> {
    let auto = runs.iter().find_map(|r| {
        let s = r.signed_off.as_ref()?;
        (s.version == version && s.by == StatusBy::Auto.as_str()).then_some((r, s))
    });
    let by_override = runs.iter().find_map(|r| {
        let s = r.signed_off.as_ref()?;
        let o = s.overrides_auto_pick.as_ref()?;
        (o.version == version).then_some(s.version)
    });
    if let (None, Some(by)) = (auto, by_override) {
        return Err(fail(
            &PARAMS_INVALID,
            format!(
                "The automatic pick of {version} was overridden already, by version {by}: an \
                 automatic version is overridden once."
            ),
        ));
    }
    let Some((run, signed)) = auto else {
        return Err(fail(
            &PARAMS_INVALID,
            format!(
                "overrides names {version}, which is not a version an automatic sign-off made."
            ),
        ));
    };
    let picked = signed
        .auto_pick
        .as_ref()
        .map(|p| p.picked.clone())
        .or_else(|| run.selected.first().cloned())
        .unwrap_or_default();
    Ok((run, picked))
}

/// [`plan_signoff`] with what the sign-off records beyond its files (module docs):
/// - `record.overrides` (P4b-R38): the version's sign-off must be by `auto`, and the run must
///   be the one that made it, which is admitted though closed, for files it hasn't selected;
///   `signed_off.overrides_auto_pick` gets `{version, picked}`;
/// - `record.restored_from` (CORE-177): the status-log entry gets the reason
///   `{code: "restored", ref: "<asset_id>@<version>"}`. The trust records of the run's files
///   go to the new version's paths as every sign-off carries them (module docs).
///
/// # Errors
/// As [`plan_signoff`]; `params.invalid` for an `overrides` that names no automatic sign-off,
/// one whose run isn't `run_id` while `run_id` is open, or a chosen file the overridden run
/// selected already. A closed run that isn't the overridden one is `asset.unknown`, as today.
#[allow(clippy::too_many_arguments)]
pub fn plan_signoff_with(
    m: &Manifest,
    run_id: &str,
    chosen: &Chosen,
    bump: Option<Bump>,
    keep_runs: &[String],
    by: &SignedOffBy,
    record: &SignoffRecord,
    now_utc: i64,
) -> Result<(Manifest, Version), ErrorBody> {
    let view = m.view();
    let same = |a: &str, b: &str| a.eq_ignore_ascii_case(b);
    let overrides = record
        .overrides
        .map(|v| overridden(&view.candidate_runs, v).map(|(r, picked)| (v, r, picked)))
        .transpose()?;
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
    let open = matches!(run.status.as_deref(), None | Some("open")) && run.trashed_utc.is_none();
    // P4b-R38: the run of the overridden automatic sign-off is admitted though closed.
    let admitted = overrides
        .as_ref()
        .is_some_and(|(_, r, _)| same(&r.run_id, &run_id) && r.trashed_utc.is_none());
    if !open && !admitted {
        return Err(fail(
            &ASSET_UNKNOWN,
            format!(
                "The run `{run_id}` is {}, not open: only an open run can be signed off.",
                run.status.as_deref().unwrap_or("trashed")
            ),
        ));
    }
    if let (Some((v, r, _)), false) = (&overrides, admitted) {
        return Err(fail(
            &PARAMS_INVALID,
            format!(
                "overrides {v} signs off a file of the run that made it (`{}`), not of `{run_id}`.",
                r.run_id
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
    let picked = chosen_files(run, chosen)?;
    let mut chosen: Vec<(&str, &CandidateFile)> = Vec::new();
    for name in &picked.names {
        let record = named_file(run, name)?;
        if admitted
            && run
                .selected
                .iter()
                .any(|s| same(s, bare_name(&record.path)))
        {
            return Err(fail(
                &PARAMS_INVALID,
                format!(
                    "`{name}` is what the automatic sign-off chose already: an override signs \
                     off another take of its run."
                ),
            ));
        }
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

    // Ruling Q22: omitting `bump` accepts the proposal (CORE-027's proposed-then-confirmed);
    // `version.exists` is only for a real collision.
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
    // Ruling Q23: an open run on an approved asset is new work, so it can be signed off; the
    // status log gets one entry for the sign-off (ruling PF-5).
    let mut out = m.clone();
    if admitted {
        // The overridden run counts as open for the sign-off: the status machine takes it as
        // new work on the approved asset (ruling Q23), and it is `selected` again below.
        for r in out.runs_mut() {
            if r.get("run_id")
                .and_then(Value::as_str)
                .is_some_and(|id| same(id, &run_id))
            {
                r.insert("status".into(), Value::from("open"));
            }
        }
    }
    let reason = record.restored_from.map(|from| Reason {
        code: "restored".to_string(),
        r#ref: Some(format!("{}@{from}", view.asset_id)),
        note: None,
    });
    change_status(
        &mut out,
        &StatusEvent::Signoff,
        StatusBy::from_wire(&by.by),
        by.via.clone(),
        reason,
        &lil_schema::time::rfc3339(now_utc),
    )
    .map_err(|why| fail(&STATUS_INVALID_TRANSITION, why))?;
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
    let sealed: Vec<&CandidateFile> = chosen.iter().map(|(_, record)| *record).collect();
    carry_to_version(&mut out, &run.run_id, &run.files, sealed, version);

    let names: Vec<Value> = chosen.iter().map(|(n, _)| Value::from(*n)).collect();
    let date = lil_schema::time::date(now_utc);
    let mut signed_off = json!({"by": by.by, "date": date, "version": version.to_string()});
    if let Some(via) = &by.via {
        signed_off["via"] = via.to_value();
    }
    if let Some(takes) = &picked.takes {
        signed_off["takes"] = json!(takes);
        signed_off["companions"] = json!(picked.companions);
    }
    if let Some(pick) = &by.auto_pick {
        signed_off["auto_pick"] = auto_pick_value(pick);
    }
    if let Some((version, _, picked)) = &overrides {
        signed_off["overrides_auto_pick"] =
            json!({"version": version.to_string(), "picked": picked});
    }
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
            r.insert("signed_off".into(), signed_off.clone());
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

/// Trust records at the seal (module docs): `sealed` (the chosen records of run `run_id`,
/// whose records are `files`) and, transitively, the `derived` records made from them, matched
/// by name as the store's seal matches them ([`name_key`]); then the run's `source/` records.
fn carry_to_version<'a>(
    m: &mut Manifest,
    run_id: &str,
    files: &'a [CandidateFile],
    mut sealed: Vec<&'a CandidateFile>,
    version: Version,
) {
    let has = |list: &[&CandidateFile], name: &str| {
        let key = name_key(name);
        list.iter().any(|f| name_key(&f.path) == key)
    };
    loop {
        let more: Vec<&CandidateFile> = files
            .iter()
            .filter(|f| f.role == "derived" && !has(&sealed, &f.path))
            .filter(|f| f.from.as_deref().is_some_and(|from| has(&sealed, from)))
            .collect();
        if more.is_empty() {
            break;
        }
        sealed.extend(more);
    }
    for file in sealed {
        inherit_from(m, files, file);
        if let Some(key) = trust_key(m, &file.path) {
            carry_trust(m, &key, &format!("v{version}/{}", bare_name(&file.path)));
        }
    }
    // The seal moves the run's `source/` folder to `v<version>/source/`.
    let source = path_key(&format!("candidates/{run_id}/source/"));
    let keys: Vec<String> = match m.get("trust") {
        Some(Value::Object(trust)) => trust
            .keys()
            .filter(|k| path_key(k).starts_with(&source))
            .cloned()
            .collect(),
        _ => Vec::new(),
    };
    let skip = source.chars().count();
    for key in keys {
        let rest: String = key.chars().skip(skip).collect();
        carry_trust(m, &key, &format!("v{version}/source/{rest}"));
    }
}

/// CORE-052's automatic pick among `files` (a run's records), and why. `gates` gives each
/// file's technical gate results (by bare name); a file passes when every one is `pass`. Of
/// the passing files: the best vision-review score when a vision review (`vision`: by bare
/// name, the score and the review's reason) scored any of them (ties: the lower seed, then the
/// name), with reason `vision_score`; otherwise the lowest seed (files without a seed last,
/// ties by name), with reason `lowest_seed`. Every file is a candidate in the record, in file
/// order, with its gates and, when the review scored it, its score and reason. `None` when no
/// file passes.
pub fn auto_pick(
    files: &[CandidateFile],
    vision: Option<&BTreeMap<String, (f64, String)>>,
    gates: &dyn Fn(&str) -> Vec<(String, Gate)>,
) -> Option<AutoPick> {
    let review = |f: &CandidateFile| vision.and_then(|v| v.get(bare_name(&f.path)));
    let candidates: Vec<AutoPickCandidate> = files
        .iter()
        .map(|f| AutoPickCandidate {
            file: bare_name(&f.path).to_string(),
            gates: gates(bare_name(&f.path)),
            vision_score: review(f).map(|(s, _)| *s),
            vision_reason: review(f).map(|(_, r)| r.clone()),
        })
        .collect();
    let passing: Vec<&CandidateFile> = files
        .iter()
        .zip(&candidates)
        .filter(|(_, c)| c.gates.iter().all(|(_, g)| *g == Gate::Pass))
        .map(|(f, _)| f)
        .collect();
    let seed_then_name = |a: &&CandidateFile, b: &&CandidateFile| {
        let seed = |f: &CandidateFile| f.seed.unwrap_or(i64::MAX);
        seed(a)
            .cmp(&seed(b))
            .then_with(|| bare_name(&a.path).cmp(bare_name(&b.path)))
    };
    let scored: Vec<(&CandidateFile, f64)> = passing
        .iter()
        .filter_map(|f| review(f).map(|(s, _)| (*f, *s)))
        .filter(|(_, s)| s.is_finite())
        .collect();
    let (best, reason) = if scored.is_empty() {
        (
            passing.into_iter().min_by(seed_then_name),
            AutoPickReason::LowestSeed,
        )
    } else {
        let best = scored
            .into_iter()
            .min_by(|(a, sa), (b, sb)| sb.total_cmp(sa).then_with(|| seed_then_name(a, b)))
            .map(|(f, _)| f);
        (best, AutoPickReason::VisionScore)
    };
    best.map(|f| AutoPick {
        picked: bare_name(&f.path).to_string(),
        reason,
        candidates,
    })
}

/// `signed_off.auto_pick` as Appendix H writes it: `{picked, reason, candidates: [{file,
/// gates: {<name>: "pass" | "fail"}, vision_score?, vision_reason?}]}`.
fn auto_pick_value(pick: &AutoPick) -> Value {
    let candidates: Vec<Value> = pick
        .candidates
        .iter()
        .map(|c| {
            let gates: Map<String, Value> = c
                .gates
                .iter()
                .map(|(name, gate)| {
                    let result = match gate {
                        Gate::Pass => "pass",
                        Gate::Fail => "fail",
                    };
                    (name.clone(), Value::from(result))
                })
                .collect();
            let mut v = json!({"file": c.file, "gates": gates});
            if let Some(score) = c.vision_score {
                v["vision_score"] = json!(score);
            }
            if let Some(reason) = &c.vision_reason {
                v["vision_reason"] = json!(reason);
            }
            v
        })
        .collect();
    json!({"picked": pick.picked, "reason": pick.reason.as_str(), "candidates": candidates})
}

/// The paths in the manifest's `trust` map under the folder `prefix` (ending in `/`), found
/// as the seal finds names ([`path_key`]), in map order, each with the rest of its path.
fn trust_paths(m: &Manifest, prefix: &str) -> Vec<(String, String)> {
    let prefix = path_key(prefix);
    let skip = prefix.chars().count();
    match m.get("trust") {
        Some(Value::Object(trust)) => trust
            .keys()
            .filter(|k| path_key(k).starts_with(&prefix))
            .map(|k| (k.clone(), k.chars().skip(skip).collect()))
            .collect(),
        _ => Vec::new(),
    }
}

/// A restore's new run (CORE-177) and what its sign-off chooses.
#[derive(Debug, Clone, PartialEq)]
pub struct RestoredRun {
    /// The `candidate_runs` entry: `open`, with `restored_from`, the caller's `actor`, and a
    /// record for each top-level file of the version (`source/**` files have none, as in any
    /// run).
    pub run: Map<String, Value>,
    /// The old sign-off's takes when it named takes, else the files it selected; for a version
    /// without that sign-off (a legacy version), every file.
    pub chosen: Chosen,
}

/// The run a restore of `restored_from` stages its copies into (CORE-177): `files` are the
/// version's files as (path inside the version folder, bare SHA-256), the snapshot left out;
/// `snapshot` is the version's `manifest.snapshot.json` (`None` for a legacy version). Each
/// top-level file's record is `candidates/<run_id>/<name>` with the old run's `role`, `seed`
/// and `from` for that name (`candidate` without one), its take number and primary as the old
/// run numbered its takes (`take` and `primary` written explicitly, so the old sign-off's take
/// numbers name the same takes), and the copy's `sha256`. `target_version` is left `null` for
/// the caller to set.
pub fn restored_run(
    snapshot: Option<&Manifest>,
    restored_from: Version,
    run_id: &str,
    files: &[(String, String)],
    actor: &Actor,
    now_utc: i64,
) -> RestoredRun {
    let old = snapshot.and_then(|s| {
        s.view().candidate_runs.into_iter().find(|r| {
            r.signed_off
                .as_ref()
                .is_some_and(|o| o.version == restored_from)
        })
    });
    // Each old record with its take as the old run numbered it (P4b-R3: a run without `take`
    // fields numbers its takes by file order) and whether it is that take's primary. The
    // restored run holds only the version's files, so it gets these explicitly: numbered
    // afresh, the old sign-off's take numbers would name other takes or none.
    let numbered: Vec<(&CandidateFile, Option<u32>, bool)> = old
        .as_ref()
        .map(|r| {
            let takes = takes_of(r);
            r.files
                .iter()
                .zip(file_takes(r))
                .map(|(f, n)| {
                    let primary = n.is_some_and(|n| {
                        takes
                            .iter()
                            .any(|t| t.n == n && same_name(&f.path, &t.primary))
                    });
                    (f, n, primary)
                })
                .collect()
        })
        .unwrap_or_default();
    let was = |name: &str| {
        numbered
            .iter()
            .find(|(f, _, _)| same_name(&f.path, name))
            .copied()
    };
    let names: Vec<&str> = files
        .iter()
        .map(|(inside, _)| inside.as_str())
        .filter(|inside| !inside.contains('/'))
        .collect();
    let mut records = Vec::with_capacity(names.len());
    for (name, sha256) in files.iter().filter(|(inside, _)| !inside.contains('/')) {
        let mut rec = Map::new();
        rec.insert("path".into(), json!(format!("candidates/{run_id}/{name}")));
        let old_record = was(name);
        rec.insert(
            "role".into(),
            json!(old_record.map_or("candidate", |(f, _, _)| f.role.as_str())),
        );
        rec.insert("sha256".into(), json!(sha256));
        if let Some((f, take, primary)) = old_record {
            if let Some(seed) = f.seed {
                rec.insert("seed".into(), json!(seed));
            }
            if let Some(from) = &f.from {
                rec.insert("from".into(), json!(bare_name(from)));
            }
            if let Some(take) = take {
                rec.insert("take".into(), json!(take));
            }
            if primary {
                rec.insert("primary".into(), Value::Bool(true));
            }
        }
        rec.insert("actor".into(), actor.to_value());
        records.push(Value::Object(rec));
    }
    // Takes only when every file keeps a take: a file in none would stay behind unsealed.
    let all_taken = names
        .iter()
        .all(|n| was(n).is_some_and(|(_, take, _)| take.is_some()));
    let signed = old.as_ref().and_then(|r| r.signed_off.as_ref());
    let every = || Chosen::Files(names.iter().map(|n| n.to_string()).collect());
    let chosen = match (old.as_ref(), signed) {
        (Some(_), Some(s)) if !s.takes.is_empty() && all_taken => Chosen::Takes(s.takes.clone()),
        (Some(r), Some(_)) => {
            let selected: Vec<String> = r
                .selected
                .iter()
                .filter(|s| names.iter().any(|n| n.eq_ignore_ascii_case(s)))
                .cloned()
                .collect();
            if selected.is_empty() {
                every()
            } else {
                Chosen::Files(selected)
            }
        }
        _ => every(),
    };
    let run = json!({
        "run_id": run_id,
        "path": format!("candidates/{run_id}/"),
        "status": "open",
        "created": lil_schema::time::rfc3339(now_utc),
        "target_version": null,
        "files": records,
        "selected": null,
        "signed_off": null,
        "pinned": false,
        "actor": actor.to_value(),
        "restored_from": restored_from.to_string(),
    });
    let Value::Object(run) = run else {
        unreachable!("the run is an object");
    };
    RestoredRun { run, chosen }
}

/// Adds a restore's run (from [`restored_run`], `target_version` set) to the manifest, with no
/// status change (the sign-off that follows makes the one entry), and carries the trust record
/// of each file of `v<restored_from>/` to the same path in the run folder
/// (`lil_schema::carry_trust`), where the sign-off finds it and carries it on to the new
/// version as every sign-off does.
///
/// # Errors
/// A message when the manifest has a run with that id already or `candidate_runs` isn't a list.
pub fn add_restored_run(
    m: &mut Manifest,
    run: &Map<String, Value>,
    restored_from: Version,
) -> Result<(), String> {
    let id = run
        .get("run_id")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if m.view()
        .candidate_runs
        .iter()
        .any(|r| r.run_id.eq_ignore_ascii_case(id))
    {
        return Err(format!("the manifest has a run `{id}` already"));
    }
    m.push("candidate_runs", Value::Object(run.clone()))
        .map_err(|e| format!("{e:?}"))?;
    for (path, rest) in trust_paths(m, &format!("v{restored_from}/")) {
        carry_trust(m, &path, &format!("candidates/{id}/{rest}"));
    }
    Ok(())
}

/// Whether a run file record's `path` has the bare name `name` (compared without regard to
/// case).
fn same_name(path: &str, name: &str) -> bool {
    bare_name(path).eq_ignore_ascii_case(name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use lil_schema::Status;

    fn files(names: &[&str]) -> Chosen {
        Chosen::Files(names.iter().map(|n| n.to_string()).collect())
    }

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
    fn auto_approve_signoff_is_by_auto_with_via_rule() {
        // The sign-off path with `auto` and its rule, as the `auto_approve` caller will call it.
        let (signed, version) = plan_signoff(
            &manifest(),
            "r_1",
            &files(&["a.png"]),
            None,
            &[],
            &SignedOffBy::auto("auto_approve:image"),
            86_400,
        )
        .unwrap();
        let run = &signed.as_map()["candidate_runs"][0];
        assert_eq!(run["signed_off"]["by"], "auto", "{run}");
        assert_eq!(
            run["signed_off"]["via"],
            json!({"rule": "auto_approve:image"}),
            "{run}"
        );
        assert_eq!(run["signed_off"]["version"], version.to_string());
        let newest = signed.as_map()["status_log"]
            .as_array()
            .and_then(|log| log.last())
            .cloned()
            .unwrap();
        assert_eq!(newest["by"], "auto", "{newest}");
        assert_eq!(
            newest["via"],
            json!({"rule": "auto_approve:image"}),
            "{newest}"
        );
        // A caller's sign-off has no `via`.
        let (signed, _) = plan_signoff(
            &manifest(),
            "r_1",
            &files(&["a.png"]),
            None,
            &[],
            &SignedOffBy::caller(&CallerKind::Mcp),
            86_400,
        )
        .unwrap();
        let signed_off = &signed.as_map()["candidate_runs"][0]["signed_off"];
        assert_eq!(signed_off["by"], "claude-code");
        assert!(signed_off.get("via").is_none(), "{signed_off}");
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
            &files(&["A.PNG"]),
            None,
            &["r_3".to_string()],
            &SignedOffBy::caller(&CallerKind::Cli),
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
            &files(&["b.png"]),
            Some(Bump::Major),
            &[],
            &SignedOffBy::caller(&CallerKind::Cli),
            0,
        )
        .unwrap();
        assert_eq!(v, Version::new(2, 0, 0));
        let (_, v) = plan_signoff(
            &m,
            "r_2",
            &files(&["b.png"]),
            None,
            &[],
            &SignedOffBy::caller(&CallerKind::Cli),
            0,
        )
        .unwrap();
        assert_eq!(v, Version::new(1, 1, 0));
    }

    #[test] // spec: CORE-032, CORE-048 (ruling Q23)
    fn a_kept_run_is_signable_after_approval_but_not_when_stale() {
        let (signed, _) = plan_signoff(
            &manifest(),
            "r_1",
            &files(&["a.png"]),
            None,
            &["r_2".into()],
            &SignedOffBy::caller(&CallerKind::Cli),
            0,
        )
        .unwrap();
        let (again, v) = plan_signoff(
            &signed,
            "r_2",
            &files(&["b.png"]),
            None,
            &[],
            &SignedOffBy::caller(&CallerKind::Cli),
            0,
        )
        .unwrap();
        assert_eq!(v, Version::new(1, 1, 0));
        assert_eq!(again.view().status, Some(Status::Approved));
        let mut stale = signed.clone();
        stale.set("status", json!("stale"));
        stale.set("status_before_stale", json!("approved"));
        let e = plan_signoff(
            &stale,
            "r_2",
            &files(&["b.png"]),
            None,
            &[],
            &SignedOffBy::caller(&CallerKind::Cli),
            0,
        )
        .unwrap_err();
        assert_eq!(e.code, "status.invalid_transition");
    }

    #[test] // spec: CORE-026
    fn refusals() {
        let m = manifest();
        let code = |r: Result<(Manifest, Version), ErrorBody>| r.unwrap_err().code;
        assert_eq!(
            code(plan_signoff(
                &m,
                "r_9",
                &files(&["a.png"]),
                None,
                &[],
                &SignedOffBy::caller(&CallerKind::Cli),
                0
            )),
            "asset.unknown"
        );
        assert_eq!(
            code(plan_signoff(
                &m,
                "r_1",
                &files(&["x.png"]),
                None,
                &[],
                &SignedOffBy::caller(&CallerKind::Cli),
                0
            )),
            "asset.unknown"
        );
        assert_eq!(
            code(plan_signoff(
                &m,
                "r_1",
                &files(&[]),
                None,
                &[],
                &SignedOffBy::caller(&CallerKind::Cli),
                0
            )),
            "params.invalid"
        );
        assert_eq!(
            code(plan_signoff(
                &m,
                "r_1",
                &files(&["a.png"]),
                None,
                &["r_9".into()],
                &SignedOffBy::caller(&CallerKind::Cli),
                0
            )),
            "asset.unknown"
        );
        let (signed, _) = plan_signoff(
            &m,
            "r_1",
            &files(&["a.png"]),
            None,
            &[],
            &SignedOffBy::caller(&CallerKind::Cli),
            0,
        )
        .unwrap();
        // r_1 is selected now, r_2 rejected.
        assert_eq!(
            code(plan_signoff(
                &signed,
                "r_1",
                &files(&["a.png"]),
                None,
                &[],
                &SignedOffBy::caller(&CallerKind::Cli),
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
                &files(&["a.png"]),
                None,
                &[],
                &SignedOffBy::caller(&CallerKind::Cli),
                0
            )),
            "status.invalid_transition"
        );
    }

    /// `r_1` with `a.png` at the top of its folder and in `sub/`.
    fn two_named_a() -> Manifest {
        let mut m = manifest();
        m.runs_mut()[0].insert(
            "files".into(),
            json!([
                {"path": "candidates/r_1/a.png", "role": "candidate", "sha256": "a".repeat(64)},
                {"path": "candidates/r_1/sub/a.png", "role": "candidate", "sha256": "c".repeat(64)},
            ]),
        );
        m
    }

    #[test] // spec: CORE-026 (controller ruling 4)
    fn an_ambiguous_bare_name_is_params_invalid_and_a_path_chooses() {
        let m = two_named_a();
        let cli = SignedOffBy::caller(&CallerKind::Cli);
        let e = plan_signoff(&m, "r_1", &files(&["a.png"]), None, &[], &cli, 0).unwrap_err();
        assert_eq!(e.code, "params.invalid", "{e:?}");
        // A path names one file, but a file in a folder inside the run folder is never sealed.
        for name in ["sub/a.png", r"sub\a.png", "candidates/r_1/sub/a.png"] {
            let e = plan_signoff(&m, "r_1", &files(&[name]), None, &[], &cli, 0).unwrap_err();
            assert_eq!(e.code, "params.invalid", "{name}: {e:?}");
            assert!(e.message.contains("top of the run folder"), "{name}: {e:?}");
        }
        // The top-level file's path in the asset folder chooses it.
        let (signed, _) = plan_signoff(
            &m,
            "r_1",
            &files(&["candidates/r_1/a.png"]),
            None,
            &[],
            &cli,
            0,
        )
        .unwrap();
        assert_eq!(
            signed.view().hashes.get("v1.0.0/a.png").map(String::as_str),
            Some("a".repeat(64).as_str())
        );
        let e = plan_signoff(&m, "r_1", &files(&["other/a.png"]), None, &[], &cli, 0).unwrap_err();
        assert_eq!(e.code, "asset.unknown", "{e:?}");
    }

    fn pick() -> AutoPick {
        AutoPick {
            picked: "a.png".to_string(),
            reason: AutoPickReason::VisionScore,
            candidates: vec![AutoPickCandidate {
                file: "a.png".to_string(),
                gates: vec![("size".to_string(), Gate::Pass)],
                vision_score: Some(0.5),
                vision_reason: Some("clean".to_string()),
            }],
        }
    }

    #[test] // spec: CORE-052
    fn an_automatic_signoff_records_its_pick() {
        let by = SignedOffBy::auto("auto_approve:image").with_auto_pick(pick());
        let (signed, _) =
            plan_signoff(&manifest(), "r_1", &files(&["a.png"]), None, &[], &by, 0).unwrap();
        let run = &signed.as_map()["candidate_runs"][0];
        assert_eq!(
            run["signed_off"]["auto_pick"],
            json!({"picked": "a.png", "reason": "vision_score", "candidates": [
                {"file": "a.png", "gates": {"size": "pass"}, "vision_score": 0.5,
                 "vision_reason": "clean"}]}),
            "{run}"
        );
        // It reads back as the same pick.
        let view = signed.view();
        assert_eq!(
            view.candidate_runs[0]
                .signed_off
                .as_ref()
                .and_then(|s| s.auto_pick.clone()),
            Some(pick())
        );
        // A caller's sign-off records none.
        let (signed, _) = plan_signoff(
            &manifest(),
            "r_1",
            &files(&["a.png"]),
            None,
            &[],
            &SignedOffBy::caller(&CallerKind::Cli),
            0,
        )
        .unwrap();
        let signed_off = &signed.as_map()["candidate_runs"][0]["signed_off"];
        assert!(signed_off.get("auto_pick").is_none(), "{signed_off}");
    }

    #[test] // spec: CORE-052 (P4b-R38)
    fn an_override_reopens_only_the_automatic_run() {
        // `r_1` holds a.png and b.png; the automatic sign-off chose a.png as 1.0.0.
        let mut m = manifest();
        m.runs_mut()[0].insert(
            "files".into(),
            json!([
                {"path": "candidates/r_1/a.png", "role": "candidate", "sha256": "a".repeat(64)},
                {"path": "candidates/r_1/b.png", "role": "candidate", "sha256": "b".repeat(64)},
            ]),
        );
        let by = SignedOffBy::auto("auto_approve:image").with_auto_pick(pick());
        let (auto, _) = plan_signoff(&m, "r_1", &files(&["a.png"]), None, &[], &by, 0).unwrap();
        let cli = SignedOffBy::caller(&CallerKind::Cli);
        let record = SignoffRecord {
            overrides: Some(Version::new(1, 0, 0)),
            restored_from: None,
        };
        let plan = |chosen: &[&str], run: &str, record: &SignoffRecord| {
            plan_signoff_with(&auto, run, &files(chosen), None, &[], &cli, record, 0)
        };
        let (signed, v) = plan(&["b.png"], "r_1", &record).unwrap();
        assert_eq!(v, Version::new(1, 1, 0));
        let run = &signed.view().candidate_runs[0];
        assert_eq!(run.status.as_deref(), Some("selected"));
        assert_eq!(run.selected, ["b.png"]);
        let s = run.signed_off.as_ref().unwrap();
        assert_eq!((s.by.as_str(), s.version), ("cli", v));
        assert_eq!(
            s.overrides_auto_pick,
            Some(lil_schema::OverridesAutoPick {
                version: Version::new(1, 0, 0),
                picked: "a.png".to_string()
            })
        );
        let log = signed.status_log();
        let newest = log.last().unwrap();
        assert_eq!(
            (newest.from, newest.to),
            (
                Some(lil_schema::Status::Approved),
                lil_schema::Status::Approved
            )
        );
        // The file it chose already; the closed run without `overrides`; another run with it.
        assert_eq!(
            plan(&["a.png"], "r_1", &record).unwrap_err().code,
            "params.invalid"
        );
        assert_eq!(
            plan(&["b.png"], "r_1", &SignoffRecord::default())
                .unwrap_err()
                .code,
            "asset.unknown"
        );
        // r_2 was rejected by the automatic sign-off: still refused as today.
        assert_eq!(
            plan(&["b.png"], "r_2", &record).unwrap_err().code,
            "asset.unknown"
        );
        // A version no automatic sign-off made.
        let not_auto = SignoffRecord {
            overrides: Some(Version::new(2, 0, 0)),
            restored_from: None,
        };
        assert_eq!(
            plan(&["b.png"], "r_1", &not_auto).unwrap_err().code,
            "params.invalid"
        );
    }

    #[test] // spec: CORE-177
    fn a_restore_logs_restored_and_carries_its_trust_records() {
        // The restore's open run `r_9`, holding a copy of 1.0.0's a.png with its trust record.
        let (mut m, _) = plan_signoff(
            &manifest(),
            "r_1",
            &files(&["a.png"]),
            None,
            &[],
            &SignedOffBy::caller(&CallerKind::Cli),
            0,
        )
        .unwrap();
        let untrusted = lil_schema::TrustRecord {
            state: lil_schema::TrustState::Untrusted,
            sources: Vec::new(),
            marked_trusted: None,
        };
        m.set_trust("v1.0.0/a.png", Some(&untrusted));
        let files_of = [("a.png".to_string(), "a".repeat(64))];
        let actor = lil_schema::Actor {
            kind: lil_schema::ActorKind::Cli,
            task_id: None,
            conversation_id: None,
            session_id: None,
        };
        let snapshot = m.clone();
        let restored = restored_run(
            Some(&snapshot),
            Version::new(1, 0, 0),
            "r_9",
            &files_of,
            &actor,
            0,
        );
        assert_eq!(restored.chosen, files(&["a.png"]));
        add_restored_run(&mut m, &restored.run, Version::new(1, 0, 0)).unwrap();
        assert_eq!(m.trust("candidates/r_9/a.png"), Some(untrusted.clone()));
        let entries = m.status_log().len();
        let record = SignoffRecord {
            overrides: None,
            restored_from: Some(Version::new(1, 0, 0)),
        };
        let (signed, v) = plan_signoff_with(
            &m,
            "r_9",
            &restored.chosen,
            Some(Bump::Minor),
            &[],
            &SignedOffBy::caller(&CallerKind::Cli),
            &record,
            0,
        )
        .unwrap();
        assert_eq!(v, Version::new(1, 1, 0));
        let log = signed.status_log();
        assert_eq!(log.len(), entries + 1);
        let reason = log.last().unwrap().reason.clone().unwrap();
        assert_eq!(
            (reason.code.as_str(), reason.r#ref.as_deref()),
            ("restored", Some("prop_kite@1.0.0"))
        );
        assert_eq!(signed.trust("v1.1.0/a.png"), Some(untrusted.clone()));
        assert_eq!(signed.trust("v1.0.0/a.png"), Some(untrusted.clone()));
        // The run's record stays, as with every sign-off.
        assert_eq!(signed.trust("candidates/r_9/a.png"), Some(untrusted));
        assert_eq!(
            signed.view().candidate_runs[3].restored_from,
            Some(Version::new(1, 0, 0))
        );
        // A second run of the same id is refused.
        assert!(add_restored_run(&mut m, &restored.run, Version::new(1, 0, 0)).is_err());
    }
}
