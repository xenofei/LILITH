//! The suite-held queue's choice of the next job of a lane (CORE-123, CORE-124).
//!
//! The candidates are the lane's queued jobs. The best class present wins (`interactive` >
//! `user` > `agent`); within it a job requeued at the head (a preempted one, CORE-125) comes
//! first, then submission order. When the lane last ran a job of a `group` (a model profile),
//! the oldest job of the class with that group may go first, overtaking the older jobs of its
//! class, unless one of them has been overtaken [`MAX_OVERTAKES`] times already; then the
//! class's first job goes.

use super::{MAX_OVERTAKES, Priority};

/// A queued job, as the choice sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate<'a> {
    pub id: &'a str,
    pub priority: Priority,
    /// Requeued at the head of its class.
    pub head: bool,
    /// Submission order.
    pub order: u64,
    pub group: Option<&'a str>,
    /// How often a grouped job went before it.
    pub overtaken: u32,
}

/// The job to start, and the older jobs of its class it goes before.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pick<'a> {
    pub id: &'a str,
    pub overtakes: Vec<&'a str>,
}

/// The next job among `candidates`, when the lane's last job had `lane_group` (module docs).
pub fn pick<'a>(candidates: &[Candidate<'a>], lane_group: Option<&str>) -> Option<Pick<'a>> {
    let best = candidates.iter().map(|c| c.priority).min()?;
    let mut class: Vec<&Candidate<'a>> = candidates.iter().filter(|c| c.priority == best).collect();
    class.sort_by_key(|c| (!c.head, c.order));
    let first = Pick {
        id: class[0].id,
        overtakes: Vec::new(),
    };
    let Some(group) = lane_group else {
        return Some(first);
    };
    let Some(at) = class.iter().position(|c| c.group == Some(group)) else {
        return Some(first);
    };
    let older = &class[..at];
    if older.iter().any(|c| c.overtaken >= MAX_OVERTAKES) {
        return Some(first);
    }
    Some(Pick {
        id: class[at].id,
        overtakes: older.iter().map(|c| c.id).collect(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn c<'a>(id: &'a str, priority: Priority, order: u64, group: Option<&'a str>) -> Candidate<'a> {
        Candidate {
            id,
            priority,
            head: false,
            order,
            group,
            overtaken: 0,
        }
    }

    #[test] // spec: CORE-124
    fn best_class_first_then_submission_order() {
        let jobs = [
            c("a1", Priority::Agent, 1, None),
            c("u1", Priority::User, 2, None),
            c("a2", Priority::Agent, 3, None),
            c("u2", Priority::User, 4, None),
        ];
        assert_eq!(pick(&jobs, None).unwrap().id, "u1");
        assert_eq!(pick(&jobs[2..], None).unwrap().id, "u2");
        assert_eq!(
            pick(&[jobs[0].clone(), jobs[2].clone()], None).unwrap().id,
            "a1"
        );
        assert_eq!(pick(&[], None), None);
        // A job requeued at the head of its class goes before older ones of the class.
        let mut head = c("a9", Priority::Agent, 9, None);
        head.head = true;
        assert_eq!(pick(&[jobs[0].clone(), head], None).unwrap().id, "a9");
    }

    #[test] // spec: CORE-124
    fn a_grouped_job_overtakes_only_within_its_class_and_within_the_limit() {
        let mut jobs = vec![
            c("x", Priority::Agent, 1, Some("b")),
            c("y", Priority::Agent, 2, None),
            c("g", Priority::Agent, 3, Some("a")),
        ];
        let p = pick(&jobs, Some("a")).unwrap();
        assert_eq!(p.id, "g");
        assert_eq!(p.overtakes, ["x", "y"]);
        // Another group, or none, keeps submission order.
        assert_eq!(pick(&jobs, Some("c")).unwrap().id, "x");
        assert_eq!(pick(&jobs, None).unwrap().id, "x");
        // Never past a job of a better class.
        jobs.push(c("u", Priority::User, 4, None));
        assert_eq!(pick(&jobs, Some("a")).unwrap().id, "u");
        jobs.pop();
        // Not past a job overtaken 8 times already.
        jobs[1].overtaken = MAX_OVERTAKES;
        let p = pick(&jobs, Some("a")).unwrap();
        assert_eq!((p.id, p.overtakes.len()), ("x", 0));
    }
}
