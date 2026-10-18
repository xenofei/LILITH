//! The asset status machine (CORE-048; I6).
//!
//! `planned → in_progress → candidate → in_review → approved → exported → released`, with the
//! side states `stale`, `blocked` (a reason is required) and `deprecated`. Every pair the table
//! in [`transition`] doesn't list is refused (`status.invalid_transition`).

use std::fmt;
use std::str::FromStr;

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
}

/// The result of a transition: the new status, and the fields the manifest edit sets.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Transition {
    pub to: Status,
    /// `status_before_stale` to write (on `MarkStale`).
    pub set_before_stale: Option<Status>,
    /// `status_before_blocked` to write (on `Block`).
    pub set_before_blocked: Option<Status>,
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
        }
    }
}

/// Applies `ev` to an asset in status `from`. `before_stale` and `before_blocked` are the
/// manifest's `status_before_stale` and `status_before_blocked`.
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
/// | any | `Deprecate` | `deprecated` |
pub fn transition(
    from: Status,
    before_stale: Option<Status>,
    before_blocked: Option<Status>,
    ev: &StatusEvent,
) -> Result<Transition, String> {
    use Status::*;
    let refuse = || {
        Err(format!(
            "an asset in status `{from}` can't take {}",
            ev.name()
        ))
    };
    let plain = |to: Status| {
        Ok(Transition {
            to,
            set_before_stale: None,
            set_before_blocked: None,
            reason: None,
        })
    };
    let active = !matches!(from, Blocked | Deprecated);
    match ev {
        StatusEvent::StartWork if from == Planned => plain(InProgress),
        StatusEvent::CandidateRun if active => plain(Candidate),
        StatusEvent::RequestSignoff if matches!(from, InProgress | Candidate) => plain(InReview),
        StatusEvent::Signoff if matches!(from, InProgress | Candidate | InReview) => {
            plain(Approved)
        }
        StatusEvent::Export if from == Approved => plain(Exported),
        StatusEvent::Release if from == Exported => plain(Released),
        StatusEvent::MarkStale if matches!(from, Approved | Exported | Released) => {
            Ok(Transition {
                to: Stale,
                set_before_stale: Some(from),
                set_before_blocked: None,
                reason: None,
            })
        }
        StatusEvent::Restore if from == Stale => match before_stale {
            Some(to) => plain(to),
            None => Err("the asset is stale but has no `status_before_stale` to restore".into()),
        },
        StatusEvent::Block(reason) if active => {
            if reason.trim().is_empty() {
                return Err("blocking an asset needs a reason".into());
            }
            Ok(Transition {
                to: Blocked,
                set_before_stale: None,
                set_before_blocked: Some(from),
                reason: Some(reason.clone()),
            })
        }
        StatusEvent::Unblock if from == Blocked => match before_blocked {
            Some(to) => plain(to),
            None => {
                Err("the asset is blocked but has no `status_before_blocked` to return to".into())
            }
        },
        StatusEvent::Deprecate => plain(Deprecated),
        _ => refuse(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use Status::*;
    use StatusEvent as E;

    fn to(from: Status, ev: &StatusEvent) -> Result<Status, String> {
        transition(from, None, None, ev).map(|t| t.to)
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
            let t = transition(from, None, None, &E::MarkStale).unwrap();
            assert_eq!(t.to, Stale);
            assert_eq!(t.set_before_stale, Some(from));
            assert_eq!(t.set_before_blocked, None);
            let back = transition(Stale, Some(from), None, &E::Restore).unwrap();
            assert_eq!(back.to, from);
        }
        assert!(transition(Stale, None, None, &E::Restore).is_err());

        for from in Status::ALL {
            if matches!(from, Blocked | Deprecated) {
                continue;
            }
            let t =
                transition(from, None, None, &E::Block("waiting for a palette".into())).unwrap();
            assert_eq!(t.to, Blocked);
            assert_eq!(t.set_before_blocked, Some(from));
            assert_eq!(t.reason.as_deref(), Some("waiting for a palette"));
            let back = transition(Blocked, None, Some(from), &E::Unblock).unwrap();
            assert_eq!(back.to, from);
        }
        assert!(transition(Blocked, None, None, &E::Unblock).is_err());

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
        ];
        for from in Status::ALL {
            for ev in &events {
                let got = transition(from, Some(Approved), Some(InProgress), ev);
                assert_eq!(got.is_ok(), listed(from, ev), "{from} {ev:?}");
            }
        }

        // The four refusals named in the plan.
        assert!(to(Planned, &E::Signoff).is_err());
        for ev in &events {
            if *ev != E::Deprecate {
                assert!(
                    transition(Deprecated, Some(Approved), Some(InProgress), ev).is_err(),
                    "deprecated {ev:?}"
                );
            }
        }
        assert!(to(Planned, &E::Block(String::new())).is_err());
        assert!(to(Planned, &E::Block("  ".into())).is_err());
        assert!(to(Blocked, &E::Block("again".into())).is_err());
    }
}
