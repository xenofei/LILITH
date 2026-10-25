//! The recent errors (ARCH-121, QA-083; Appendix G "Diagnostics"): the core's last
//! [`RECENT_CAPACITY`] error outcomes in memory, grouped by code for `diagnostics.recent`.

use std::collections::{HashMap, VecDeque};

use schemars::JsonSchema;
use serde::Serialize;

use crate::telemetry::format_ts;

/// How many error outcomes are kept.
pub const RECENT_CAPACITY: usize = 1000;
/// `diagnostics.recent`'s largest and default `limit`.
pub const MAX_RECENT: u32 = 200;
pub const DEFAULT_RECENT: u32 = 50;

/// One code's errors among the kept ones. Times are RFC 3339 UTC.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct ErrorGroup {
    pub code: String,
    pub count: u32,
    pub first_at: String,
    pub last_at: String,
    pub last_message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_tool: Option<String>,
}

/// One error outcome.
#[derive(Debug, Clone)]
struct ErrorRecord {
    code: String,
    tool: Option<String>,
    message: String,
    at_ms: i64,
}

/// The newest [`RECENT_CAPACITY`] error outcomes, oldest first.
#[derive(Debug, Default)]
pub(crate) struct Ring {
    records: VecDeque<ErrorRecord>,
}

impl Ring {
    /// Keeps an error outcome, forgetting the oldest once [`RECENT_CAPACITY`] are kept.
    pub(crate) fn push(&mut self, code: &str, tool: Option<&str>, message: &str, at_ms: i64) {
        if self.records.len() == RECENT_CAPACITY {
            self.records.pop_front();
        }
        self.records.push_back(ErrorRecord {
            code: code.to_string(),
            tool: tool.map(str::to_string),
            message: message.to_string(),
            at_ms,
        });
    }

    /// One group per code, the one whose last error is newest first, at most `limit`.
    pub(crate) fn groups(&self, limit: u32) -> Vec<ErrorGroup> {
        // code → (count, first_at_ms, index of its newest record)
        let mut by_code: HashMap<&str, (u32, i64, usize)> = HashMap::new();
        for (i, r) in self.records.iter().enumerate() {
            let entry = by_code.entry(&r.code).or_insert((0, r.at_ms, i));
            entry.0 += 1;
            entry.1 = entry.1.min(r.at_ms);
            entry.2 = i;
        }
        let mut groups: Vec<(i64, usize, ErrorGroup)> = by_code
            .into_iter()
            .map(|(code, (count, first, newest))| {
                let last = &self.records[newest];
                let group = ErrorGroup {
                    code: code.to_string(),
                    count,
                    first_at: format_ts(first),
                    last_at: format_ts(last.at_ms),
                    last_message: last.message.clone(),
                    last_tool: last.tool.clone(),
                };
                (last.at_ms, newest, group)
            })
            .collect();
        groups.sort_by_key(|g| std::cmp::Reverse((g.0, g.1)));
        groups
            .into_iter()
            .take(limit as usize)
            .map(|(_, _, g)| g)
            .collect()
    }
}
