//! Pause GPU work's reasons (CORE-160; Appendix G "Pause GPU work"; P4a-R5).
//!
//! A lane's pause is a set of named reasons, each with a fixed scope: a lane dispatches a
//! queued job only while no reason whose scope covers the job's class is set. Plan 4a has
//! `manual` (`app/queue.pause`) and `fullscreen` (the auto-pause, Task 9), both
//! [`PauseScope::All`]; `agent_only` is reserved for a later plan's reasons. The job-level
//! [`RESTART_REASON`] (P4a-R41) holds only the jobs a restart found, never a lane. The reasons
//! live in memory: a core restart ends a pause.

use super::Priority;

/// The reason a restart holds a job with (P4a-R41), listed in its `pause_reasons`.
pub const RESTART_REASON: &str = "restart";

/// A lane's pause reason: wire `manual`, `fullscreen`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PauseReason {
    /// Your own pause (`app/queue.pause`).
    Manual,
    /// The fullscreen auto-pause.
    Fullscreen,
}

impl PauseReason {
    pub fn wire(self) -> &'static str {
        match self {
            PauseReason::Manual => "manual",
            PauseReason::Fullscreen => "fullscreen",
        }
    }

    /// The classes it holds (both of plan 4a's hold every class).
    pub fn scope(self) -> PauseScope {
        match self {
            PauseReason::Manual | PauseReason::Fullscreen => PauseScope::All,
        }
    }
}

/// Which classes a reason holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PauseScope {
    /// Every class.
    All,
    /// Only `agent`-class jobs (a later plan's reasons).
    AgentOnly,
}

impl PauseScope {
    /// Whether a reason of this scope holds a job of class `p`.
    pub fn covers(self, p: Priority) -> bool {
        match self {
            PauseScope::All => true,
            PauseScope::AgentOnly => p == Priority::Agent,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test] // spec: CORE-160
    fn a_scope_covers_only_its_classes() {
        for p in [Priority::Interactive, Priority::User, Priority::Agent] {
            assert!(PauseScope::All.covers(p), "{p:?}");
            assert_eq!(
                PauseScope::AgentOnly.covers(p),
                p == Priority::Agent,
                "{p:?}"
            );
        }
        for r in [PauseReason::Manual, PauseReason::Fullscreen] {
            assert_eq!(r.scope(), PauseScope::All, "{r:?}");
        }
        assert_eq!(PauseReason::Manual.wire(), "manual");
        assert_eq!(PauseReason::Fullscreen.wire(), "fullscreen");
        assert_eq!(RESTART_REASON, "restart");
    }
}
