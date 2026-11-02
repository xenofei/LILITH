//! What Home's "While you were away" card lists (APP-111, P4a-R41; Appendix G "Apps"): the jobs of
//! `jobs.list {since: last_seen_at}` that finished or failed and those in `interrupted_unknown`,
//! the jobs held by a restart whatever their age, and the pending confirmations. With no
//! `last_seen_at` (the first run) it lists nothing.

use std::collections::HashSet;

use serde::Serialize;
use serde_json::Value;

use crate::format::{kind_word, parse_utc_ms};

/// One line of the card. `kind` is `finished`, `failed`, `needs_you` or `confirmation`; `title`
/// holds outside text (a kind, a project, a tool), which QML shows through `core.shownText`; `at`
/// is RFC 3339.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AwayItem {
    pub kind: &'static str,
    pub id: String,
    pub title: String,
    pub at: String,
}

/// The card's lines, newest first; `jobs` may list a job twice, and `pending` is
/// `app/confirmations.list {state: "pending"}`'s rows. A job with no project, or a kind this build
/// doesn't know, is listed all the same (P4a-R40).
pub fn away_items(jobs: &[Value], pending: &[Value], last_seen: Option<&str>) -> Vec<AwayItem> {
    let Some(since) = last_seen.and_then(since_secs) else {
        return Vec::new();
    };
    let mut seen = HashSet::new();
    let mut items: Vec<AwayItem> = jobs
        .iter()
        .filter(|job| text(job, "job_id").is_some_and(|id| seen.insert(id)))
        .filter_map(|job| job_item(job, since))
        .collect();
    items.extend(pending.iter().filter_map(confirmation_item));
    // A time that doesn't read goes last; equal times keep a steady order.
    items.sort_by(|a, b| {
        let at = |item: &AwayItem| parse_utc_ms(&item.at);
        at(b).cmp(&at(a)).then_with(|| a.id.cmp(&b.id))
    });
    items
}

/// [`away_items`] over what QML holds: `pages_json` is a JSON array of `jobs.list` results as they
/// came, `pending_json` the confirmations' rows, and `last_seen` empty when there is none. The
/// lines as a JSON array; what doesn't parse lists nothing.
pub fn away_items_json(pages_json: &str, pending_json: &str, last_seen: &str) -> Value {
    let pages: Vec<Value> = serde_json::from_str(pages_json).unwrap_or_default();
    let pending: Vec<Value> = serde_json::from_str(pending_json).unwrap_or_default();
    let jobs: Vec<Value> = pages
        .into_iter()
        .filter_map(|mut page| match page["structured"]["jobs"].take() {
            Value::Array(jobs) => Some(jobs),
            _ => None,
        })
        .flatten()
        .collect();
    serde_json::to_value(away_items(&jobs, &pending, Some(last_seen))).unwrap_or_default()
}

/// The core's own `since` (`jobs.list`, Appendix G): `updated` is kept in whole seconds, so a time
/// with a fraction counts from the second before it, and also lists the jobs of its own second. A
/// fraction of `.000` counts as one here, so no job later in that second is lost. The core takes
/// `.000` as a whole second, so Home never keeps such a time (`HomePanel.seenStamp`).
fn since_secs(last_seen: &str) -> Option<i64> {
    let ms = parse_utc_ms(last_seen)?;
    Some(ms.div_euclid(1000) - i64::from(last_seen.contains('.')))
}

fn text<'a>(row: &'a Value, key: &str) -> Option<&'a str> {
    row.get(key).and_then(Value::as_str)
}

fn job_item(job: &Value, since: i64) -> Option<AwayItem> {
    let status = text(job, "status")?;
    let later = || {
        // A row that came from `{since}` with a time that doesn't read isn't hidden.
        text(job, "updated")
            .and_then(parse_utc_ms)
            .is_none_or(|ms| ms.div_euclid(1000) > since)
    };
    let held = job
        .get("pause_reasons")
        .and_then(Value::as_array)
        .is_some_and(|reasons| reasons.iter().any(|r| r == "restart"));
    let (kind, note) = match status {
        "succeeded" if later() => ("finished", None),
        "failed" if later() => ("failed", None),
        "running" if text(job, "substatus") == Some("interrupted_unknown") && later() => {
            ("needs_you", Some("Outcome unknown"))
        }
        "queued" if held => ("needs_you", Some("Held after a restart")),
        _ => return None,
    };
    let what = text(job, "kind").unwrap_or("Job");
    let what = kind_word(what).unwrap_or(what);
    let project = text(job, "project").unwrap_or("no project");
    let mut title = format!("{what} · {project}");
    if let Some(note) = note {
        title.push_str(&format!(" · {note}"));
    }
    Some(AwayItem {
        kind,
        id: text(job, "job_id")?.to_string(),
        title,
        at: text(job, "finished")
            .or_else(|| text(job, "updated"))
            .unwrap_or_default()
            .to_string(),
    })
}

/// A pending confirmation, in the words of its toast (Appendix G "Copy").
fn confirmation_item(row: &Value) -> Option<AwayItem> {
    if text(row, "state") != Some("pending") {
        return None;
    }
    let covers = |what: &str| {
        row.get("covers")
            .and_then(Value::as_array)
            .is_some_and(|c| c.iter().any(|v| v == what))
    };
    let cap = row
        .get("cap")
        .and_then(|c| c.get(0))
        .and_then(|hit| text(hit, "cap"))
        .unwrap_or("spending");
    let title = if covers("confirm") {
        let caller = text(row, "caller_kind").unwrap_or_default();
        let who = match caller {
            "mcp" => "Claude Code",
            "assistant" => "The assistant",
            "cli" => "A script (lil)",
            k if k.starts_with("ui:") => "LILITH",
            other => other,
        };
        let tool = text(row, "tool").unwrap_or_default();
        let over = if covers("cap") {
            format!(", over your {cap} cap")
        } else {
            String::new()
        };
        format!("{who} wants to run {tool}{over}")
    } else {
        format!("A call would go over your {cap} cap")
    };
    Some(AwayItem {
        kind: "confirmation",
        id: text(row, "id")?.to_string(),
        title,
        at: text(row, "created").unwrap_or_default().to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const SEEN: &str = "2026-10-30T12:00:00Z";

    fn job(id: &str, status: &str, updated: &str) -> Value {
        json!({"job_id": id, "kind": "comfy.prompt", "status": status, "substatus": null,
               "project": "sketchbook", "updated": updated})
    }

    fn pending(id: &str, created: &str) -> Value {
        json!({"id": id, "tool": "comfy.run", "caller_kind": "mcp", "covers": ["confirm"],
               "state": "pending", "created": created, "arguments": {}})
    }

    fn ids(items: &[AwayItem]) -> Vec<&str> {
        items.iter().map(|i| i.id.as_str()).collect()
    }

    #[test] // spec: APP-111
    fn away_selects_finished_failed_unknown_and_pending_since_last_seen() {
        let mut unknown = job("j_unknown", "running", "2026-10-30T13:00:03Z");
        unknown["substatus"] = json!("interrupted_unknown");
        let mut old_unknown = job("j_old_unknown", "running", "2026-10-30T11:00:00Z");
        old_unknown["substatus"] = json!("interrupted_unknown");
        let jobs = [
            job("j_done", "succeeded", "2026-10-30T13:00:05Z"),
            job("j_failed", "failed", "2026-10-30T13:00:04Z"),
            unknown,
            old_unknown,
            // Not what the card is for.
            job("j_cancelled", "cancelled", "2026-10-30T13:00:02Z"),
            job("j_running", "running", "2026-10-30T13:00:01Z"),
            job("j_queued", "queued", "2026-10-30T13:00:01Z"),
            // Seen already: not later than the time, whole seconds apart or not.
            job("j_before", "succeeded", "2026-10-30T11:59:59Z"),
            job("j_same", "failed", SEEN),
        ];
        // A confirmation waits for you however old it is: "since" is about the jobs.
        let waiting = [
            pending("cf_new", "2026-10-30T13:00:00Z"),
            pending("cf_old", "2026-10-29T08:00:00Z"),
        ];
        let items = away_items(&jobs, &waiting, Some(SEEN));
        let kinds: Vec<(&str, &str)> = items.iter().map(|i| (i.id.as_str(), i.kind)).collect();
        assert_eq!(
            kinds,
            [
                ("j_done", "finished"),
                ("j_failed", "failed"),
                ("j_unknown", "needs_you"),
                ("cf_new", "confirmation"),
                ("cf_old", "confirmation"),
            ]
        );
        assert_eq!(items[0].title, "ComfyUI prompt · sketchbook");
        assert_eq!(items[0].at, "2026-10-30T13:00:05Z");
        assert_eq!(
            items[2].title,
            "ComfyUI prompt · sketchbook · Outcome unknown"
        );
        assert_eq!(items[3].title, "Claude Code wants to run comfy.run");
        assert_eq!(items[3].at, "2026-10-30T13:00:00Z");
    }

    #[test] // spec: APP-111
    fn away_keeps_the_core_s_rule_for_a_time_with_a_fraction() {
        // `updated` is in whole seconds, so a `since` with a fraction also lists the jobs of its
        // own second (Appendix G): none is missed, and one seen may show again.
        let jobs = [
            job("j_same_second", "succeeded", "2026-10-30T12:00:05Z"),
            job("j_before", "succeeded", "2026-10-30T12:00:04Z"),
        ];
        let items = away_items(&jobs, &[], Some("2026-10-30T12:00:05.200Z"));
        assert_eq!(ids(&items), ["j_same_second"]);
        // A fraction of exactly .000 is one too: the job later in that second isn't lost.
        let items = away_items(&jobs, &[], Some("2026-10-30T12:00:05.000Z"));
        assert_eq!(ids(&items), ["j_same_second"]);
        // With no fraction, as the core's whole-second `since`: only later seconds.
        let items = away_items(&jobs, &[], Some("2026-10-30T12:00:05Z"));
        assert_eq!(ids(&items), [] as [&str; 0]);
        // The same time with an offset reads the same.
        let items = away_items(&jobs, &[], Some("2026-10-30T14:00:05.200+02:00"));
        assert_eq!(ids(&items), ["j_same_second"]);
    }

    #[test] // spec: APP-111
    fn away_is_empty_without_a_last_seen_time() {
        let mut held = job("j_held", "queued", "2026-10-30T13:00:00Z");
        held["pause_reasons"] = json!(["restart"]);
        let jobs = [job("j_done", "succeeded", "2026-10-30T13:00:05Z"), held];
        let waiting = [pending("cf_new", "2026-10-30T13:00:00Z")];
        assert_eq!(away_items(&jobs, &waiting, None), []);
    }

    #[test] // spec: APP-111
    fn away_orders_newest_first() {
        let mut finished_earlier = job("j_a", "succeeded", "2026-10-30T15:00:00Z");
        finished_earlier["finished"] = json!("2026-10-30T13:00:00Z");
        let jobs = [
            finished_earlier,
            job("j_b", "failed", "2026-10-30T14:00:00Z"),
            // An offset that puts it last, though its text sorts first.
            job("j_c", "failed", "2026-10-30T18:30:00+06:00"),
        ];
        let waiting = [pending("cf_a", "2026-10-30T13:30:00Z")];
        let items = away_items(&jobs, &waiting, Some(SEEN));
        assert_eq!(ids(&items), ["j_b", "cf_a", "j_a", "j_c"]);
        // A job's time is when it finished, once it has.
        assert_eq!(items[2].at, "2026-10-30T13:00:00Z");
    }

    #[test] // spec: P4a-R41
    fn away_lists_jobs_held_by_a_restart() {
        let mut held = job("j_held", "queued", "2026-10-01T09:00:00Z");
        held["substatus"] = json!("queue_paused");
        held["pause_reasons"] = json!(["restart"]);
        let mut paused = job("j_paused", "queued", "2026-10-30T13:00:00Z");
        paused["substatus"] = json!("queue_paused");
        paused["pause_reasons"] = json!(["manual"]);
        let items = away_items(&[held, paused], &[], Some(SEEN));
        assert_eq!(ids(&items), ["j_held"], "whatever its age, and only it");
        assert_eq!(items[0].kind, "needs_you");
        assert_eq!(
            items[0].title,
            "ComfyUI prompt · sketchbook · Held after a restart"
        );
    }

    #[test] // spec: APP-111
    fn away_lists_a_job_once_and_shows_what_it_does_not_know() {
        let mut held = job("j_held", "queued", "2026-10-30T13:00:00Z");
        held["pause_reasons"] = json!(["restart"]);
        // Read from `{since}` and from `{status: "queued"}`.
        let again = held.clone();
        let no_project = json!({"job_id": "j_loose", "kind": "future.kind", "status": "failed",
                                "updated": "2026-10-30T13:00:01Z"});
        let no_time = json!({"job_id": "j_odd", "kind": "comfy.prompt", "status": "failed"});
        let no_id = json!({"kind": "comfy.prompt", "status": "failed",
                           "updated": "2026-10-30T13:00:01Z"});
        let items = away_items(&[held, again, no_project, no_time, no_id], &[], Some(SEEN));
        assert_eq!(ids(&items), ["j_loose", "j_held", "j_odd"]);
        assert_eq!(items[0].title, "future.kind · no project");
    }

    #[test] // spec: APP-111
    fn away_words_a_confirmation_as_its_toast_does() {
        let row = |caller: &str, covers: Value, cap: Value| {
            json!({"id": "cf_1", "tool": "comfy.run", "caller_kind": caller, "covers": covers,
                   "cap": cap, "state": "pending", "created": "2026-10-30T13:00:00Z"})
        };
        let title = |row: Value| away_items(&[], &[row], Some(SEEN))[0].title.clone();
        let cap = json!([{"cap": "monthly"}]);
        assert_eq!(
            title(row("cli", json!(["confirm"]), Value::Null)),
            "A script (lil) wants to run comfy.run"
        );
        assert_eq!(
            title(row("assistant", json!(["confirm", "cap"]), cap.clone())),
            "The assistant wants to run comfy.run, over your monthly cap"
        );
        assert_eq!(
            title(row("mcp", json!(["cap"]), cap)),
            "A call would go over your monthly cap"
        );
        // A row that isn't pending any more isn't waiting.
        let mut answered = pending("cf_2", "2026-10-30T13:00:00Z");
        answered["state"] = json!("approved");
        assert_eq!(away_items(&[], &[answered], Some(SEEN)), []);
    }

    #[test] // spec: APP-111
    fn away_of_pages_reads_the_results_as_they_came() {
        let pages = json!([
            {"structured": {"jobs": [job("j_done", "succeeded", "2026-10-30T13:00:05Z")]}},
            {"structured": {"jobs": []}},
            {"nothing": true},
        ])
        .to_string();
        let waiting = json!([pending("cf_new", "2026-10-30T13:00:00Z")]).to_string();
        let items = away_items_json(&pages, &waiting, SEEN);
        assert_eq!(items[0]["id"], "j_done");
        assert_eq!(items[0]["kind"], "finished");
        assert_eq!(items[1]["id"], "cf_new");
        // No time, or one that doesn't read: as on the first run.
        assert_eq!(away_items_json(&pages, &waiting, ""), json!([]));
        assert_eq!(away_items_json(&pages, &waiting, "yesterday"), json!([]));
        // What doesn't parse lists nothing, rather than failing the card.
        assert_eq!(
            away_items_json("{nope", "[]", SEEN),
            json!([]),
            "no pages, no jobs"
        );
    }
}
