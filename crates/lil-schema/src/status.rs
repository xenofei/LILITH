//! The asset status machine (CORE-048; I6) and the status log (CORE-156; Appendix H "Status
//! log").
//!
//! `planned → in_progress → candidate → in_review → approved → exported → released`, with the
//! side states `stale`, `blocked` (a reason is required) and `deprecated`. Every pair the table
//! in [`transition`] doesn't list is refused (`status.invalid_transition`).

use std::fmt;
use std::str::FromStr;

use serde_json::{Map, Value};

use crate::manifest::{Manifest, Obj, Reader};

/// A manifest `status` value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Status {
    Planned,
    InProgress,
    Candidate,
    InReview,
    Approved,
    Exported,
    Released,
    Stale,
    Blocked,
    Deprecated,
}

/// What happened to the asset.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum StatusEvent {
    StartWork,
    CandidateRun,
    RequestSignoff,
    Signoff,
    Export,
    Release,
    MarkStale,
    Restore,
    /// Blocked, with the reason (`status_reason`); it must not be empty.
    Block(String),
    Unblock,
    Deprecate,
    /// `deprecated: false` (P4b-R4).
    Undeprecate,
}

/// The result of a transition: the new status, and the fields the manifest edit sets.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Transition {
    pub to: Status,
    /// `status_before_stale` to write (on `MarkStale`).
    pub set_before_stale: Option<Status>,
    /// `status_before_blocked` to write (on `Block`).
    pub set_before_blocked: Option<Status>,
    /// `status_before_deprecated` to write (on `Deprecate` from any status but `deprecated`).
    pub set_before_deprecated: Option<Status>,
    /// `status_reason` to write (on `Block`).
    pub reason: Option<String>,
}

impl Status {
    pub const ALL: [Status; 10] = [
        Status::Planned,
        Status::InProgress,
        Status::Candidate,
        Status::InReview,
        Status::Approved,
        Status::Exported,
        Status::Released,
        Status::Stale,
        Status::Blocked,
        Status::Deprecated,
    ];

    /// The wire value.
    pub fn as_str(self) -> &'static str {
        match self {
            Status::Planned => "planned",
            Status::InProgress => "in_progress",
            Status::Candidate => "candidate",
            Status::InReview => "in_review",
            Status::Approved => "approved",
            Status::Exported => "exported",
            Status::Released => "released",
            Status::Stale => "stale",
            Status::Blocked => "blocked",
            Status::Deprecated => "deprecated",
        }
    }
}

impl FromStr for Status {
    type Err = String;

    fn from_str(s: &str) -> Result<Status, String> {
        Status::ALL
            .into_iter()
            .find(|st| st.as_str() == s)
            .ok_or_else(|| format!("`{s}` is not a status"))
    }
}

impl fmt::Display for Status {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl StatusEvent {
    fn name(&self) -> &'static str {
        match self {
            StatusEvent::StartWork => "start work",
            StatusEvent::CandidateRun => "a candidate run",
            StatusEvent::RequestSignoff => "a sign-off request",
            StatusEvent::Signoff => "a sign-off",
            StatusEvent::Export => "an export",
            StatusEvent::Release => "a release",
            StatusEvent::MarkStale => "marking stale",
            StatusEvent::Restore => "a restore",
            StatusEvent::Block(_) => "blocking",
            StatusEvent::Unblock => "unblocking",
            StatusEvent::Deprecate => "deprecation",
            StatusEvent::Undeprecate => "un-deprecation",
        }
    }
}

/// Applies `ev` to an asset in status `from`. `before_stale`, `before_blocked` and
/// `before_deprecated` are the manifest's `status_before_stale`, `status_before_blocked` and
/// `status_before_deprecated`; `log` is its status log (newest last) and `approved` whether it
/// has an `approved_version`. Only `Undeprecate` reads the last three.
///
/// | From | Event | To |
/// |---|---|---|
/// | `planned` | `StartWork` | `in_progress` |
/// | any but `blocked`, `deprecated` | `CandidateRun` | `candidate` |
/// | `in_progress`, `candidate` | `RequestSignoff` | `in_review` |
/// | `in_progress`, `candidate`, `in_review` | `Signoff` | `approved` |
/// | `approved` | `Export` | `exported` |
/// | `exported` | `Release` | `released` |
/// | `approved`, `exported`, `released` | `MarkStale` | `stale` (`status_before_stale` = from) |
/// | `stale` | `Restore` | `status_before_stale` |
/// | any but `blocked`, `deprecated` | `Block(reason)` | `blocked` (`status_before_blocked` = from) |
/// | `blocked` | `Unblock` | `status_before_blocked` |
/// | any | `Deprecate` | `deprecated` (`status_before_deprecated` = from, unless from is `deprecated`) |
/// | `deprecated` | `Undeprecate` | the fallback order below |
///
/// `Undeprecate` returns to, in order (P4b-R4): `status_before_deprecated`, unless it is
/// itself `deprecated`; else the `from` of the newest log entry whose `to` is `deprecated` and
/// whose `from` is a status other than `deprecated` (an entry for a repeated deprecation, or
/// without a `from`, is passed over); else `approved` when `approved`; else `in_progress`.
pub fn transition(
    from: Status,
    before_stale: Option<Status>,
    before_blocked: Option<Status>,
    before_deprecated: Option<Status>,
    log: &[StatusLogEntry],
    approved: bool,
    ev: &StatusEvent,
) -> Result<Transition, String> {
    use Status::*;
    let refuse = || {
        Err(format!(
            "an asset in status `{from}` can't take {}",
            ev.name()
        ))
    };
    let plain = |to: Status| Transition {
        to,
        set_before_stale: None,
        set_before_blocked: None,
        set_before_deprecated: None,
        reason: None,
    };
    let active = !matches!(from, Blocked | Deprecated);
    match ev {
        StatusEvent::StartWork if from == Planned => Ok(plain(InProgress)),
        StatusEvent::CandidateRun if active => Ok(plain(Candidate)),
        StatusEvent::RequestSignoff if matches!(from, InProgress | Candidate) => {
            Ok(plain(InReview))
        }
        StatusEvent::Signoff if matches!(from, InProgress | Candidate | InReview) => {
            Ok(plain(Approved))
        }
        StatusEvent::Export if from == Approved => Ok(plain(Exported)),
        StatusEvent::Release if from == Exported => Ok(plain(Released)),
        StatusEvent::MarkStale if matches!(from, Approved | Exported | Released) => {
            Ok(Transition {
                set_before_stale: Some(from),
                ..plain(Stale)
            })
        }
        StatusEvent::Restore if from == Stale => match before_stale {
            Some(to) => Ok(plain(to)),
            None => Err("the asset is stale but has no `status_before_stale` to restore".into()),
        },
        StatusEvent::Block(reason) if active => {
            if reason.trim().is_empty() {
                return Err("blocking an asset needs a reason".into());
            }
            Ok(Transition {
                set_before_blocked: Some(from),
                reason: Some(reason.clone()),
                ..plain(Blocked)
            })
        }
        StatusEvent::Unblock if from == Blocked => match before_blocked {
            Some(to) => Ok(plain(to)),
            None => {
                Err("the asset is blocked but has no `status_before_blocked` to return to".into())
            }
        },
        StatusEvent::Deprecate => Ok(Transition {
            set_before_deprecated: (from != Deprecated).then_some(from),
            ..plain(Deprecated)
        }),
        StatusEvent::Undeprecate if from == Deprecated => {
            let not_deprecated = |s: &Status| *s != Deprecated;
            let to = before_deprecated
                .filter(not_deprecated)
                .or_else(|| {
                    log.iter()
                        .rev()
                        .filter(|e| e.to == Deprecated)
                        .find_map(|e| e.from.filter(not_deprecated))
                })
                .unwrap_or(if approved { Approved } else { InProgress });
            Ok(plain(to))
        }
        _ => refuse(),
    }
}

/// The status an asset with an open run is in for a sign-off or a sign-off request (ruling
/// Q23): `approved`, `exported` and `released` take `CandidateRun` first (the open run, kept
/// at an earlier sign-off, is new work); every other status, `stale` included, is as it is.
///
/// # Errors
/// The status machine's refusal.
pub fn reopened(
    from: Status,
    before_stale: Option<Status>,
    before_blocked: Option<Status>,
) -> Result<Status, String> {
    match from {
        Status::Approved | Status::Exported | Status::Released => transition(
            from,
            before_stale,
            before_blocked,
            None,
            &[],
            false,
            &StatusEvent::CandidateRun,
        )
        .map(|t| t.to),
        other => Ok(other),
    }
}

/// Every status change (CORE-048, CORE-156; P4b-R4, P4b-R41): applies `ev` to the manifest's
/// `status` through [`transition`] and, in the same edit, writes the fields the transition
/// sets (`status`, `status_before_stale`, `status_before_blocked`, `status_before_deprecated`,
/// and `status_reason` on `Block`), removes by shifting the one a return used
/// (`status_before_stale` on `Restore`, `status_before_blocked` on `Unblock`,
/// `status_before_deprecated` on `Undeprecate`), and appends one status-log entry `{from, to, by, utc, via?,
/// reason?}` ([`Manifest::push_status`] keeps the newest 50). `from` is the stored status
/// (`null` when the manifest has none, which the machine reads as `planned`).
///
/// - `Signoff` and `RequestSignoff` on an `approved`, `exported` or `released` asset with an
///   open run take [`reopened`]'s `CandidateRun` first; that inner step isn't logged, so the
///   entry goes from the stored status to the final one (ruling PF-5).
/// - `Block(note)` logs `{code: "blocked", note}`; a separate `reason` with it is refused
///   (ruling PF-22). Any other event logs `reason` as given (`{code: "input_changed", ref}`
///   when the core sets `stale`).
/// - `Deprecate` of an asset that is already `deprecated` isn't a status change: nothing is
///   written and no entry is logged.
///
/// # Errors
/// The status machine's refusal, or a `reason` given with `Block`; the manifest is unchanged.
pub fn change_status(
    m: &mut Manifest,
    ev: &StatusEvent,
    by: StatusBy,
    via: Option<Via>,
    reason: Option<Reason>,
    utc: &str,
) -> Result<Transition, String> {
    let reason = match ev {
        StatusEvent::Block(note) => {
            if reason.is_some() {
                return Err("blocking takes its reason from the event, not a separate one".into());
            }
            Some(Reason {
                code: "blocked".into(),
                r#ref: None,
                note: Some(note.clone()),
            })
        }
        _ => reason,
    };
    let view = m.view();
    let stored = |key: &str| {
        m.get(key)
            .and_then(Value::as_str)
            .and_then(|s| s.parse::<Status>().ok())
    };
    let before_stale = stored("status_before_stale");
    let before_blocked = stored("status_before_blocked");
    let before_deprecated = stored("status_before_deprecated");
    let mut from = view.status.unwrap_or(Status::Planned);
    let open_run = view
        .candidate_runs
        .iter()
        .any(|r| matches!(r.status.as_deref(), None | Some("open")) && r.trashed_utc.is_none());
    if matches!(ev, StatusEvent::Signoff | StatusEvent::RequestSignoff) && open_run {
        from = reopened(from, before_stale, before_blocked)?;
    }
    let moved = transition(
        from,
        before_stale,
        before_blocked,
        before_deprecated,
        &m.status_log(),
        view.approved_version.is_some(),
        ev,
    )?;
    if *ev == StatusEvent::Deprecate && view.status == Some(Status::Deprecated) {
        return Ok(moved);
    }
    m.set("status", Value::from(moved.to.as_str()));
    for (key, value) in [
        ("status_before_stale", moved.set_before_stale),
        ("status_before_blocked", moved.set_before_blocked),
        ("status_before_deprecated", moved.set_before_deprecated),
    ] {
        if let Some(s) = value {
            m.set(key, Value::from(s.as_str()));
        }
    }
    if let Some(why) = &moved.reason {
        m.set("status_reason", Value::from(why.as_str()));
    }
    // The key a return used is removed by shifting (Appendix E's key-order rule).
    let used = match ev {
        StatusEvent::Restore => Some("status_before_stale"),
        StatusEvent::Unblock => Some("status_before_blocked"),
        StatusEvent::Undeprecate => Some("status_before_deprecated"),
        _ => None,
    };
    if let Some(key) = used {
        m.remove(key);
    }
    m.push_status(StatusLogEntry {
        from: view.status,
        to: moved.to,
        by,
        utc: utc.to_string(),
        via,
        reason,
    });
    Ok(moved)
}

/// The most entries the status log keeps (CORE-156); beyond it the oldest drops.
pub const STATUS_LOG_MAX: usize = 50;

/// Who changed the status: `user`, `assistant`, `claude-code`, `cli`, `auto` or `core`. The
/// set is open on read, like `signed_off.by`: a value this suite doesn't know is kept,
/// unchanged, as [`StatusBy::Other`] and written back as it was. The suite never writes
/// `Other` itself.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum StatusBy {
    User,
    Assistant,
    ClaudeCode,
    Cli,
    Auto,
    Core,
    Other(String),
}

impl StatusBy {
    pub const ALL: [StatusBy; 6] = [
        StatusBy::User,
        StatusBy::Assistant,
        StatusBy::ClaudeCode,
        StatusBy::Cli,
        StatusBy::Auto,
        StatusBy::Core,
    ];

    /// The wire value, or the unknown value as read.
    pub fn as_str(&self) -> &str {
        match self {
            StatusBy::User => "user",
            StatusBy::Assistant => "assistant",
            StatusBy::ClaudeCode => "claude-code",
            StatusBy::Cli => "cli",
            StatusBy::Auto => "auto",
            StatusBy::Core => "core",
            StatusBy::Other(s) => s,
        }
    }

    /// Reads a wire value; never fails.
    pub fn from_wire(s: &str) -> StatusBy {
        StatusBy::ALL
            .into_iter()
            .find(|b| b.as_str() == s)
            .unwrap_or_else(|| StatusBy::Other(s.to_string()))
    }
}

/// `via`: what the change came through, on a status-log entry and on `signed_off`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Via {
    pub task_id: Option<String>,
    pub conversation_id: Option<String>,
    pub rule: Option<String>,
}

impl Via {
    /// The JSON form: only the keys that are set.
    pub fn to_value(&self) -> Value {
        let mut map = Map::new();
        for (key, value) in [
            ("task_id", &self.task_id),
            ("conversation_id", &self.conversation_id),
            ("rule", &self.rule),
        ] {
            if let Some(v) = value {
                map.insert(key.into(), Value::from(v.as_str()));
            }
        }
        Value::Object(map)
    }

    /// The `via` at `key` of `o`: absent or `null` is none; not an object is none with a
    /// warning.
    pub(crate) fn read(r: &mut Reader, o: Obj<'_>, key: &str) -> Option<Via> {
        let at = Reader::ptr(o, key);
        let Value::Object(map) = Reader::field(o, key)? else {
            r.warn(at, "expected an object");
            return None;
        };
        let v = Obj { map, at: &at };
        Some(Via {
            task_id: r.opt_str(v, "task_id"),
            conversation_id: r.opt_str(v, "conversation_id"),
            rule: r.opt_str(v, "rule"),
        })
    }
}

/// A status-log entry's `reason`: `{code, ref?, note?}`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reason {
    pub code: String,
    pub r#ref: Option<String>,
    pub note: Option<String>,
}

/// One status change: `{from, to, by, utc, via?, reason?}` (CORE-156). `from` is written
/// `null` when there was no status before.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatusLogEntry {
    pub from: Option<Status>,
    pub to: Status,
    pub by: StatusBy,
    pub utc: String,
    pub via: Option<Via>,
    pub reason: Option<Reason>,
}

impl StatusLogEntry {
    /// The JSON form.
    pub fn to_value(&self) -> Value {
        let mut map = Map::new();
        map.insert(
            "from".into(),
            self.from.map_or(Value::Null, |s| Value::from(s.as_str())),
        );
        map.insert("to".into(), Value::from(self.to.as_str()));
        map.insert("by".into(), Value::from(self.by.as_str()));
        map.insert("utc".into(), Value::from(self.utc.as_str()));
        if let Some(via) = &self.via {
            map.insert("via".into(), via.to_value());
        }
        if let Some(reason) = &self.reason {
            let mut m = Map::new();
            m.insert("code".into(), Value::from(reason.code.as_str()));
            if let Some(r) = &reason.r#ref {
                m.insert("ref".into(), Value::from(r.as_str()));
            }
            if let Some(n) = &reason.note {
                m.insert("note".into(), Value::from(n.as_str()));
            }
            map.insert("reason".into(), Value::Object(m));
        }
        Value::Object(map)
    }

    /// One entry, leniently: without a status `to`, a string `by` and `utc` (and a `from` that
    /// is a status or `null`) it is skipped with a warning.
    fn read(r: &mut Reader, o: Obj<'_>) -> Option<StatusLogEntry> {
        let status = |key: &str| Reader::req_str(o, key).and_then(|s| s.parse::<Status>().ok());
        let from = match Reader::field(o, "from") {
            None => Some(None),
            Some(_) => status("from").map(Some),
        };
        let by = Reader::req_str(o, "by").map(|s| StatusBy::from_wire(&s));
        let (Some(from), Some(to), Some(by), Some(utc)) =
            (from, status("to"), by, Reader::req_str(o, "utc"))
        else {
            r.skipped(
                o.at,
                "a status-log entry needs a status or null `from`, a status `to`, a string `by` \
                 and a string `utc`",
            );
            return None;
        };
        let reason = match Reader::field(o, "reason") {
            None => None,
            Some(Value::Object(m)) => {
                let at = Reader::ptr(o, "reason");
                let ro = Obj { map: m, at: &at };
                match Reader::req_str(ro, "code") {
                    Some(code) => Some(Reason {
                        code,
                        r#ref: r.opt_str(ro, "ref"),
                        note: r.opt_str(ro, "note"),
                    }),
                    None => {
                        r.warn(at, "ignored: `code` is required");
                        None
                    }
                }
            }
            Some(_) => {
                r.warn(Reader::ptr(o, "reason"), "expected an object");
                None
            }
        };
        if to == Status::Blocked && reason.is_none() {
            r.warn(
                Reader::ptr(o, "reason"),
                "a change to `blocked` needs a reason",
            );
        }
        Some(StatusLogEntry {
            from,
            to,
            by,
            utc,
            via: Via::read(r, o, "via"),
            reason,
        })
    }
}

/// The manifest's `status_log`, newest last.
pub(crate) fn read_status_log(r: &mut Reader, root: Obj<'_>) -> Vec<StatusLogEntry> {
    let mut out = Vec::new();
    for (at, m) in r.objects(root, "status_log") {
        if let Some(e) = StatusLogEntry::read(r, Obj { map: m, at: &at }) {
            out.push(e);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use Status::*;
    use StatusEvent as E;

    /// `transition` with no `status_before_deprecated`, no log and no approved version.
    fn tr(
        from: Status,
        before_stale: Option<Status>,
        before_blocked: Option<Status>,
        ev: &StatusEvent,
    ) -> Result<Transition, String> {
        transition(from, before_stale, before_blocked, None, &[], false, ev)
    }

    fn to(from: Status, ev: &StatusEvent) -> Result<Status, String> {
        tr(from, None, None, ev).map(|t| t.to)
    }

    #[test] // spec: CORE-048
    fn status_transitions_follow_the_table() {
        for s in Status::ALL {
            assert_eq!(s.as_str().parse::<Status>(), Ok(s));
        }

        assert_eq!(to(Planned, &E::StartWork), Ok(InProgress));
        for from in Status::ALL {
            let want_candidate = !matches!(from, Blocked | Deprecated);
            assert_eq!(
                to(from, &E::CandidateRun).is_ok(),
                want_candidate,
                "{from} candidate run"
            );
            if want_candidate {
                assert_eq!(to(from, &E::CandidateRun), Ok(Candidate));
            }
            // Deprecate is allowed from anywhere.
            assert_eq!(to(from, &E::Deprecate), Ok(Deprecated), "{from}");
        }
        for from in [InProgress, Candidate] {
            assert_eq!(to(from, &E::RequestSignoff), Ok(InReview));
        }
        for from in [InProgress, Candidate, InReview] {
            assert_eq!(to(from, &E::Signoff), Ok(Approved));
        }
        assert_eq!(to(Approved, &E::Export), Ok(Exported));
        assert_eq!(to(Exported, &E::Release), Ok(Released));
        for from in [Approved, Exported, Released] {
            let t = tr(from, None, None, &E::MarkStale).unwrap();
            assert_eq!(t.to, Stale);
            assert_eq!(t.set_before_stale, Some(from));
            assert_eq!(t.set_before_blocked, None);
            let back = tr(Stale, Some(from), None, &E::Restore).unwrap();
            assert_eq!(back.to, from);
        }
        assert!(tr(Stale, None, None, &E::Restore).is_err());

        for from in Status::ALL {
            if matches!(from, Blocked | Deprecated) {
                continue;
            }
            let t = tr(from, None, None, &E::Block("waiting for a palette".into())).unwrap();
            assert_eq!(t.to, Blocked);
            assert_eq!(t.set_before_blocked, Some(from));
            assert_eq!(t.reason.as_deref(), Some("waiting for a palette"));
            let back = tr(Blocked, None, Some(from), &E::Unblock).unwrap();
            assert_eq!(back.to, from);
        }
        assert!(tr(Blocked, None, None, &E::Unblock).is_err());

        // Every pair the table doesn't list is refused.
        let listed = |from: Status, ev: &StatusEvent| -> bool {
            match ev {
                E::StartWork => from == Planned,
                E::CandidateRun | E::Block(_) => !matches!(from, Blocked | Deprecated),
                E::RequestSignoff => matches!(from, InProgress | Candidate),
                E::Signoff => matches!(from, InProgress | Candidate | InReview),
                E::Export => from == Approved,
                E::Release => from == Exported,
                E::MarkStale => matches!(from, Approved | Exported | Released),
                E::Restore => from == Stale,
                E::Unblock => from == Blocked,
                E::Deprecate => true,
                E::Undeprecate => from == Deprecated,
            }
        };
        let events = [
            E::StartWork,
            E::CandidateRun,
            E::RequestSignoff,
            E::Signoff,
            E::Export,
            E::Release,
            E::MarkStale,
            E::Restore,
            E::Block("why".into()),
            E::Unblock,
            E::Deprecate,
            E::Undeprecate,
        ];
        for from in Status::ALL {
            for ev in &events {
                let got = tr(from, Some(Approved), Some(InProgress), ev);
                assert_eq!(got.is_ok(), listed(from, ev), "{from} {ev:?}");
            }
        }

        // The four refusals named in the plan.
        assert!(to(Planned, &E::Signoff).is_err());
        for ev in &events {
            if !matches!(ev, E::Deprecate | E::Undeprecate) {
                assert!(
                    tr(Deprecated, Some(Approved), Some(InProgress), ev).is_err(),
                    "deprecated {ev:?}"
                );
            }
        }
        assert!(to(Planned, &E::Block(String::new())).is_err());
        assert!(to(Planned, &E::Block("  ".into())).is_err());
        assert!(to(Blocked, &E::Block("again".into())).is_err());
    }

    fn entry(from: Option<Status>, to: Status) -> StatusLogEntry {
        StatusLogEntry {
            from,
            to,
            by: StatusBy::User,
            utc: "2026-11-01T00:00:00Z".into(),
            via: None,
            reason: None,
        }
    }

    #[test] // spec: CORE-170, CORE-156
    fn undeprecate_follows_the_fallback_order() {
        let undeprecate = |before: Option<Status>, log: &[StatusLogEntry], approved: bool| {
            transition(
                Deprecated,
                None,
                None,
                before,
                log,
                approved,
                &E::Undeprecate,
            )
            .map(|t| t.to)
        };
        // Deprecating records where the asset was.
        let t = tr(InReview, None, None, &E::Deprecate).unwrap();
        assert_eq!(t.to, Deprecated);
        assert_eq!(t.set_before_deprecated, Some(InReview));
        assert_eq!(
            tr(Deprecated, None, None, &E::Deprecate)
                .unwrap()
                .set_before_deprecated,
            None,
            "deprecating again keeps the first record"
        );
        for ev in [E::MarkStale, E::Signoff, E::Block("why".into())] {
            let from = if ev == E::MarkStale {
                Approved
            } else {
                InReview
            };
            assert_eq!(
                tr(from, None, None, &ev).unwrap().set_before_deprecated,
                None
            );
        }

        let log = [
            entry(Some(Candidate), Deprecated),
            entry(Some(Deprecated), InReview),
            entry(Some(InReview), Deprecated),
            entry(Some(Planned), InProgress),
        ];
        // 1. `status_before_deprecated` comes first.
        assert_eq!(undeprecate(Some(Exported), &log, true), Ok(Exported));
        // 2. Else the `from` of the newest log entry whose `to` is `deprecated`.
        assert_eq!(undeprecate(None, &log, true), Ok(InReview));
        assert_eq!(undeprecate(Some(Deprecated), &log, false), Ok(InReview));
        // 3. Else `approved` when the asset has an `approved_version` (also when the newest
        //    such entry has no `from`).
        let no_deprecation = [entry(Some(Planned), InProgress)];
        assert_eq!(undeprecate(None, &no_deprecation, true), Ok(Approved));
        assert_eq!(
            undeprecate(None, &[entry(None, Deprecated)], true),
            Ok(Approved)
        );
        // A repeated deprecation (or an entry without a `from`) is passed over for the older
        // entry that recorded the real status.
        let twice = [
            entry(Some(InReview), Deprecated),
            entry(Some(Deprecated), Deprecated),
        ];
        assert_eq!(undeprecate(None, &twice, false), Ok(InReview));
        let no_from = [entry(Some(Exported), Deprecated), entry(None, Deprecated)];
        assert_eq!(undeprecate(None, &no_from, true), Ok(Exported));
        // 4. Else `in_progress`.
        assert_eq!(undeprecate(None, &[], false), Ok(InProgress));
        assert_eq!(undeprecate(None, &no_deprecation, false), Ok(InProgress));
        // Only a deprecated asset can be un-deprecated.
        for from in Status::ALL {
            let got = transition(
                from,
                None,
                None,
                Some(Approved),
                &log,
                true,
                &E::Undeprecate,
            );
            assert_eq!(got.is_ok(), from == Deprecated, "{from}");
        }
    }

    fn approved_manifest() -> crate::manifest::Manifest {
        let mut m = crate::manifest::Manifest::new(
            "sketchbook",
            "img_dock",
            "image",
            "0b8e5c1a-2f3d-4e6a-8b9c-0d1e2f3a4b5c",
        );
        m.set("status", serde_json::json!("approved"));
        m.set("approved_version", serde_json::json!("1.0.0"));
        m
    }

    /// One `change_status` call: its result, and the one entry it appended.
    fn change(
        m: &mut crate::manifest::Manifest,
        ev: &StatusEvent,
        reason: Option<Reason>,
    ) -> StatusLogEntry {
        let before = m.status_log().len();
        change_status(m, ev, StatusBy::Cli, None, reason, "2026-11-05T08:00:00Z")
            .unwrap_or_else(|e| panic!("{ev:?}: {e}"));
        let log = m.status_log();
        assert_eq!(log.len(), before + 1, "{ev:?} appends one entry");
        log.last().cloned().unwrap()
    }

    #[test] // spec: CORE-156, CORE-048
    fn change_status_covers_the_events_with_no_tool() {
        use serde_json::json;
        let mut m = approved_manifest();

        let e = change(&mut m, &E::Export, None);
        assert_eq!((e.from, e.to), (Some(Approved), Exported));
        assert_eq!(e.by, StatusBy::Cli);
        assert_eq!(e.utc, "2026-11-05T08:00:00Z");
        let e = change(&mut m, &E::Release, None);
        assert_eq!((e.from, e.to), (Some(Exported), Released));

        // Restore (from stale) has no tool; it returns to `status_before_stale`.
        let input = Reason {
            code: "input_changed".into(),
            r#ref: Some("prop_kite@1.0.0".into()),
            note: None,
        };
        let e = change(&mut m, &E::MarkStale, Some(input.clone()));
        assert_eq!((e.from, e.to), (Some(Released), Stale));
        assert_eq!(e.reason, Some(input));
        assert_eq!(m.get("status_before_stale"), Some(&json!("released")));
        let e = change(&mut m, &E::Restore, None);
        assert_eq!((e.from, e.to, e.reason), (Some(Stale), Released, None));
        assert_eq!(m.get("status_before_stale"), None, "removed by the restore");

        let e = change(&mut m, &E::Block("waiting for a palette".into()), None);
        assert_eq!((e.from, e.to), (Some(Released), Blocked));
        assert_eq!(
            e.reason,
            Some(Reason {
                code: "blocked".into(),
                r#ref: None,
                note: Some("waiting for a palette".into()),
            })
        );
        assert_eq!(
            m.get("status_reason"),
            Some(&json!("waiting for a palette"))
        );
        assert_eq!(m.get("status_before_blocked"), Some(&json!("released")));
        let e = change(&mut m, &E::Unblock, None);
        assert_eq!((e.from, e.to), (Some(Blocked), Released));
        assert_eq!(m.view().status, Some(Released));
        assert_eq!(
            m.get("status_before_blocked"),
            None,
            "removed by the unblock"
        );

        // Deprecating a deprecated asset again is no status change (no entry, nothing written).
        change(&mut m, &E::Deprecate, None);
        let text = m.to_text();
        let again = change_status(&mut m, &E::Deprecate, StatusBy::Cli, None, None, "x");
        assert_eq!(again.map(|t| t.to), Ok(Deprecated));
        assert_eq!(m.to_text(), text);
        assert_eq!(m.get("status_before_deprecated"), Some(&json!("released")));
        let e = change(&mut m, &E::Undeprecate, None);
        assert_eq!((e.from, e.to), (Some(Deprecated), Released));
        assert_eq!(
            m.get("status_before_deprecated"),
            None,
            "removed by the return"
        );
        // Each removal shifted the keys after it: the other keys keep their order.
        let keys: Vec<&String> = m.as_map().keys().collect();
        let fresh = approved_manifest();
        let mut want: Vec<&String> = fresh.as_map().keys().collect();
        let (reason, log) = ("status_reason".to_string(), "status_log".to_string());
        want.extend([&log, &reason]);
        assert_eq!(keys, want);

        // A manifest without a status logs `from: null`.
        m.remove("status");
        let e = change(&mut m, &E::StartWork, None);
        assert_eq!((e.from, e.to), (None, InProgress));
        assert_eq!(
            m.get("status_log").unwrap().as_array().unwrap()[8]["from"],
            json!(null)
        );
    }

    #[test] // spec: CORE-048, CORE-156 (ruling PF-22)
    fn change_status_refuses_block_without_a_reason() {
        let mut m = approved_manifest();
        let text = m.to_text();
        let block = |m: &mut crate::manifest::Manifest, ev: &StatusEvent, r: Option<Reason>| {
            change_status(m, ev, StatusBy::User, None, r, "2026-11-05T08:00:00Z")
        };
        assert!(block(&mut m, &E::Block(String::new()), None).is_err());
        assert!(block(&mut m, &E::Block(" \t".into()), None).is_err());
        let separate = Reason {
            code: "blocked".into(),
            r#ref: None,
            note: Some("a second reason".into()),
        };
        assert!(block(&mut m, &E::Block("why".into()), Some(separate)).is_err());
        // A refused transition writes nothing either.
        assert!(block(&mut m, &E::StartWork, None).is_err());
        assert_eq!(m.to_text(), text, "a refusal leaves the manifest as it was");
        assert!(m.get("status_log").is_none());
    }

    #[test] // spec: CORE-053, CORE-048 (ruling Q23)
    fn reopened_takes_candidate_run_only_after_approval() {
        assert_eq!(reopened(Exported, None, None), Ok(Candidate));
        assert_eq!(reopened(InReview, None, None), Ok(InReview));
        for from in [Approved, Exported, Released] {
            assert_eq!(reopened(from, None, None), Ok(Candidate), "{from}");
        }
        assert_eq!(reopened(Stale, Some(Approved), None), Ok(Stale));
    }

    #[test] // spec: CORE-156, CORE-053 (rulings Q23, PF-5)
    fn change_status_logs_one_entry_for_a_reopened_signoff() {
        use serde_json::json;
        let mut m = approved_manifest();
        m.push(
            "candidate_runs",
            json!({"run_id": "r_2", "status": "open", "files": []}),
        )
        .unwrap();
        let e = change(&mut m, &E::RequestSignoff, None);
        assert_eq!((e.from, e.to), (Some(Approved), InReview));
        m.set("status", json!("approved"));
        let e = change(&mut m, &E::Signoff, None);
        assert_eq!((e.from, e.to), (Some(Approved), Approved));
        // Without an open run, an approved asset can't be signed off again.
        m.set("candidate_runs", json!([]));
        let refused = change_status(&mut m, &E::Signoff, StatusBy::Cli, None, None, "x");
        assert!(refused.is_err());
        assert_eq!(m.status_log().len(), 2);
    }

    #[test] // spec: CORE-156, CORE-051
    fn auto_by_round_trips() {
        use crate::manifest::Manifest;
        use serde_json::json;

        for by in StatusBy::ALL {
            assert_eq!(StatusBy::from_wire(by.as_str()), by);
        }
        let wire: Vec<&str> = StatusBy::ALL.iter().map(|b| b.as_str()).collect();
        assert_eq!(
            wire,
            ["user", "assistant", "claude-code", "cli", "auto", "core"]
        );
        // `claude_code` is the actor kind's spelling, not the status log's.
        assert_eq!(
            StatusBy::from_wire("claude_code"),
            StatusBy::Other("claude_code".into())
        );

        let mut m = Manifest::new(
            "sketchbook",
            "img_dock",
            "image",
            "0b8e5c1a-2f3d-4e6a-8b9c-0d1e2f3a4b5c",
        );
        let auto = StatusLogEntry {
            from: Some(InReview),
            to: Approved,
            by: StatusBy::Auto,
            utc: "2026-11-01T09:00:00Z".into(),
            via: Some(Via {
                task_id: None,
                conversation_id: None,
                rule: Some("auto_approve".into()),
            }),
            reason: None,
        };
        let restored = StatusLogEntry {
            from: Some(Approved),
            to: Approved,
            by: StatusBy::Core,
            utc: "2026-11-01T10:00:00Z".into(),
            via: None,
            reason: Some(Reason {
                code: "restored".into(),
                r#ref: Some("img_dock@1.0.0".into()),
                note: None,
            }),
        };
        m.push_status(auto.clone());
        m.push_status(restored.clone());
        assert_eq!(
            m.get("status_log").unwrap()[0],
            json!({"from": "in_review", "to": "approved", "by": "auto",
                   "utc": "2026-11-01T09:00:00Z", "via": {"rule": "auto_approve"}})
        );
        let (again, warnings) = Manifest::parse(&m.to_text()).unwrap();
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(again.status_log(), [auto, restored]);

        // A blocked entry without a reason is read, with a warning; one without a string `by`
        // is skipped, with a warning.
        let text = json!({"schema_version": 1, "asset_id": "a", "project": "p",
        "status_log": [
            {"from": null, "to": "planned", "by": 7, "utc": "2026-11-01T00:00:00Z"},
            {"from": "planned", "to": "blocked", "by": "user", "utc": "2026-11-01T00:00:00Z"}
        ]})
        .to_string();
        let (m, warnings) = Manifest::parse(&text).unwrap();
        assert_eq!(m.status_log().len(), 1);
        let pointers: Vec<&str> = warnings.iter().map(|w| w.pointer.as_str()).collect();
        assert!(pointers.contains(&"/status_log/0"), "{pointers:?}");
        assert!(pointers.contains(&"/status_log/1/reason"), "{pointers:?}");
    }

    #[test] // spec: CORE-156, CORE-148
    fn status_log_keeps_an_unknown_by_unchanged() {
        use crate::manifest::Manifest;
        use serde_json::json;

        assert_eq!(
            StatusBy::from_wire("night_shift"),
            StatusBy::Other("night_shift".into())
        );
        assert_eq!(
            StatusBy::Other("night_shift".into()).as_str(),
            "night_shift"
        );

        let robot = json!({"from": "approved", "to": "deprecated", "by": "night_shift",
                           "utc": "2026-11-01T00:00:00Z", "x_extra": 1});
        let text = json!({"schema_version": 1, "asset_id": "a", "project": "p",
                          "status_log": [robot.clone()]})
        .to_string();
        let (mut m, warnings) = Manifest::parse(&text).unwrap();
        assert!(warnings.is_empty(), "{warnings:?}");
        // The entry stays in the view, so the newest deprecation is still found (P4b-R4).
        let log = m.status_log();
        assert_eq!(log.len(), 1);
        assert_eq!(log[0].by, StatusBy::Other("night_shift".into()));
        let back = transition(Deprecated, None, None, None, &log, true, &E::Undeprecate);
        assert_eq!(back.map(|t| t.to), Ok(Approved));
        // Written back unchanged, also through the view's own form.
        assert_eq!(log[0].to_value()["by"], json!("night_shift"));
        m.push_status(entry(Some(Approved), Deprecated));
        assert_eq!(m.get("status_log").unwrap()[0], robot);
        let (again, _) = Manifest::parse(&m.to_text()).unwrap();
        assert_eq!(
            again.status_log()[0].by,
            StatusBy::Other("night_shift".into())
        );
    }
}
