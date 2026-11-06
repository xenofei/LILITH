//! The asset status machine (CORE-048; I6) and the status log (CORE-156; Appendix H "Status
//! log").
//!
//! `planned → in_progress → candidate → in_review → approved → exported → released`, with the
//! side states `stale`, `blocked` (a reason is required) and `deprecated`. Every pair the table
//! in [`transition`] doesn't list is refused (`status.invalid_transition`).

use std::fmt;
use std::str::FromStr;

use serde_json::{Map, Value};

use crate::manifest::{Obj, Reader};

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
/// `Undeprecate` returns to, in order (P4b-R4): `status_before_deprecated`; else the `from` of
/// the newest log entry whose `to` is `deprecated`; else `approved` when `approved`; else
/// `in_progress`. A candidate that is itself `deprecated`, or a newest entry without a
/// `from`, passes to the next step.
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
                        .find(|e| e.to == Deprecated)
                        .and_then(|e| e.from)
                        .filter(not_deprecated)
                })
                .unwrap_or(if approved { Approved } else { InProgress });
            Ok(plain(to))
        }
        _ => refuse(),
    }
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
