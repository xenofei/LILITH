//! The list models' Qt-free state (APP-025): [`PageState`] pages a list tool through `cursor`
//! and `next_cursor`, and [`Rows`] holds the rows, upserted by id from the topic's events.
//!
//! - **Paging.** Every list tool the models page (`jobs.list`, `assets.query`) takes `limit` and
//!   `cursor` and answers `next_cursor`, `null` on the last page. One page is asked at a time.
//! - **Rows.** A row is a JSON object identified by the string (or number) under its id key. An
//!   upsert of a new id inserts it first, since the lists are newest first and an event's new row
//!   is the newest; an upsert of a known id updates it in place, unless the row it carries is
//!   older than the one held: both have an `updated` time (RFC 3339 UTC, Appendix F) and the
//!   carried one is earlier. Events carry full state but can arrive after a page that already
//!   holds a newer state (calls and events reach the app on separate paths), so such an event is
//!   ignored.

use std::cmp::Ordering;
use std::collections::HashMap;

use serde_json::Value;

/// Where paging stands (module docs).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PageState {
    /// The cursor of the next page; `None` for the first page or after the last.
    cursor: Option<String>,
    /// The last page is in.
    done: bool,
    /// A page has been asked for and isn't in.
    in_flight: bool,
}

impl PageState {
    pub fn new() -> PageState {
        PageState::default()
    }

    /// Whether another page can be asked for now: the last one isn't in, and none is asked.
    pub fn can_fetch_more(&self) -> bool {
        !self.done && !self.in_flight
    }

    /// The arguments of the next page: `base` (an object; anything else counts as `{}`) with
    /// `limit` set to `page` and `cursor` to the previous page's `next_cursor`, once there is one.
    /// `None` when [`PageState::can_fetch_more`] is false; else the page counts as asked.
    pub fn next_args(&mut self, base: &Value, page: u32) -> Option<Value> {
        if !self.can_fetch_more() {
            return None;
        }
        self.in_flight = true;
        let mut args = base.as_object().cloned().unwrap_or_default();
        args.insert("limit".to_string(), Value::from(page));
        match &self.cursor {
            Some(cursor) => args.insert("cursor".to_string(), Value::from(cursor.as_str())),
            None => args.remove("cursor"),
        };
        Some(Value::Object(args))
    }

    /// The page asked for is in, with its `next_cursor`.
    pub fn on_page(&mut self, next_cursor: Option<String>) {
        self.in_flight = false;
        self.done = next_cursor.is_none();
        self.cursor = next_cursor;
    }

    /// The page asked for failed: it can be asked for again.
    pub fn on_failed(&mut self) {
        self.in_flight = false;
    }

    /// Back to the first page, with nothing asked.
    pub fn reset(&mut self) {
        *self = PageState::default();
    }
}

/// What an upsert or a removal did, for the model's signals.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Change {
    Inserted(usize),
    Updated(usize),
    Removed(usize),
}

/// The rows (module docs).
#[derive(Debug, Clone, Default)]
pub struct Rows {
    rows: Vec<Value>,
    /// Each row's index by id.
    index: HashMap<String, usize>,
}

impl Rows {
    pub fn new() -> Rows {
        Rows::default()
    }

    pub fn len(&self) -> usize {
        self.rows.len()
    }

    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    pub fn get(&self, row: usize) -> Option<&Value> {
        self.rows.get(row)
    }

    /// Inserts `row` first when its id is new, else updates it in place; `None` when it has no
    /// id under `id_key`, or when it is older than the row held (module docs).
    pub fn upsert(&mut self, row: Value, id_key: &str) -> Option<Change> {
        let id = row_id(&row, id_key)?;
        match self.index.get(&id) {
            Some(&at) => {
                if is_older(&row, &self.rows[at]) {
                    return None;
                }
                self.rows[at] = row;
                Some(Change::Updated(at))
            }
            None => {
                self.rows.insert(0, row);
                self.reindex(id_key);
                Some(Change::Inserted(0))
            }
        }
    }

    /// Removes the row with id `id`.
    pub fn remove(&mut self, id: &str) -> Option<Change> {
        let at = self.index.remove(id)?;
        self.rows.remove(at);
        for i in self.index.values_mut() {
            if *i > at {
                *i -= 1;
            }
        }
        Some(Change::Removed(at))
    }

    /// Adds a page's rows after the rows held; a row whose id is held already updates it in
    /// place instead (a live insert came first). The rows appended, for the model's signals.
    pub fn append_page(&mut self, page: Vec<Value>, id_key: &str) -> std::ops::Range<usize> {
        let start = self.rows.len();
        for row in page {
            match row_id(&row, id_key) {
                Some(id) if self.index.contains_key(&id) => {
                    // Held already: in place, as an event would be (not a new row).
                    let _ = self.upsert(row, id_key);
                }
                id => {
                    if let Some(id) = id {
                        self.index.insert(id, self.rows.len());
                    }
                    self.rows.push(row);
                }
            }
        }
        start..self.rows.len()
    }

    /// Replaces every row with a first page's.
    pub fn replace(&mut self, page: Vec<Value>, id_key: &str) {
        self.rows.clear();
        self.index.clear();
        let _ = self.append_page(page, id_key);
    }

    fn reindex(&mut self, id_key: &str) {
        self.index = self
            .rows
            .iter()
            .enumerate()
            .filter_map(|(at, row)| Some((row_id(row, id_key)?, at)))
            .collect();
    }
}

/// The id of `row` under `id_key`: a string, or a number as text.
pub fn row_id(row: &Value, id_key: &str) -> Option<String> {
    match row.get(id_key)? {
        Value::String(id) => Some(id.clone()),
        Value::Number(id) => Some(id.to_string()),
        _ => None,
    }
}

/// Whether `row` carries an earlier `updated` time than `held`.
fn is_older(row: &Value, held: &Value) -> bool {
    let time = |v: &Value| v.get("updated").and_then(Value::as_str).map(str::to_string);
    match (time(row), time(held)) {
        (Some(a), Some(b)) => compare_utc(&a, &b) == Some(Ordering::Less),
        _ => false,
    }
}

/// Orders two RFC 3339 UTC times (`2026-10-27T12:00:00.5Z`); `None` when either isn't one.
fn compare_utc(a: &str, b: &str) -> Option<Ordering> {
    let (a_secs, a_frac) = utc_parts(a)?;
    let (b_secs, b_frac) = utc_parts(b)?;
    // The fractions as written, padded with zeros to the same length, compare as numbers.
    let width = a_frac.len().max(b_frac.len());
    let pad = |f: &str| format!("{f:0<width$}");
    Some(
        a_secs
            .cmp(b_secs)
            .then_with(|| pad(a_frac).cmp(&pad(b_frac))),
    )
}

/// `YYYY-MM-DDTHH:MM:SS` and the fraction's digits (maybe none) of a `Z` time.
fn utc_parts(t: &str) -> Option<(&str, &str)> {
    let t = t.strip_suffix('Z')?;
    let (secs, frac) = t.split_once('.').unwrap_or((t, ""));
    let shape = secs.len() == 19
        && secs.bytes().enumerate().all(|(i, c)| match i {
            4 | 7 => c == b'-',
            10 => c == b'T',
            13 | 16 => c == b':',
            _ => c.is_ascii_digit(),
        });
    (shape && frac.bytes().all(|c| c.is_ascii_digit())).then_some((secs, frac))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test] // spec: APP-025
    fn page_state_fetches_until_the_cursor_is_null() {
        let mut p = PageState::new();
        let base = json!({"status": "running"});
        assert!(p.can_fetch_more());
        let first = p.next_args(&base, 2).expect("the first page");
        assert_eq!(first, json!({"status": "running", "limit": 2}));
        assert!(!p.can_fetch_more(), "one page at a time");
        assert_eq!(p.next_args(&base, 2), None);

        p.on_page(Some("c1".to_string()));
        assert!(p.can_fetch_more());
        let second = p.next_args(&base, 2).unwrap();
        assert_eq!(
            second,
            json!({"status": "running", "limit": 2, "cursor": "c1"})
        );
        // A failed page is asked for again, with the same cursor.
        p.on_failed();
        assert_eq!(p.next_args(&base, 2).unwrap(), second);

        p.on_page(None);
        assert!(!p.can_fetch_more(), "the last page is in");
        assert_eq!(p.next_args(&base, 2), None);

        // A base that isn't an object counts as {}; a reset starts again.
        p.reset();
        assert_eq!(p.next_args(&json!(null), 5).unwrap(), json!({"limit": 5}));
    }

    #[test] // spec: APP-025
    fn rows_upsert_by_id_updates_in_place() {
        let mut rows = Rows::new();
        rows.replace(
            vec![
                json!({"job_id": "j2", "status": "queued"}),
                json!({"job_id": "j1", "status": "running"}),
            ],
            "job_id",
        );
        assert_eq!(
            rows.upsert(json!({"job_id": "j1", "status": "succeeded"}), "job_id"),
            Some(Change::Updated(1))
        );
        assert_eq!(rows.len(), 2);
        assert_eq!(rows.get(1).unwrap()["status"], "succeeded");
        // A new id goes first; the others keep their ids.
        assert_eq!(
            rows.upsert(json!({"job_id": "j3", "status": "queued"}), "job_id"),
            Some(Change::Inserted(0))
        );
        assert_eq!(
            rows.upsert(json!({"job_id": "j1", "status": "failed"}), "job_id"),
            Some(Change::Updated(2))
        );
        // A row without an id changes nothing.
        assert_eq!(rows.upsert(json!({"status": "queued"}), "job_id"), None);
        assert_eq!(rows.len(), 3);

        // A page's row already held (inserted live) is updated, not appended twice.
        let added = rows.append_page(
            vec![
                json!({"job_id": "j3", "status": "running"}),
                json!({"job_id": "j0"}),
            ],
            "job_id",
        );
        assert_eq!(added, 3..4);
        assert_eq!(rows.get(0).unwrap()["status"], "running");
        assert_eq!(
            row_id(rows.get(3).unwrap(), "job_id").as_deref(),
            Some("j0")
        );

        assert_eq!(rows.remove("j2"), Some(Change::Removed(1)));
        assert_eq!(rows.remove("j2"), None);
        assert_eq!(
            rows.upsert(json!({"job_id": "j0", "status": "queued"}), "job_id"),
            Some(Change::Updated(2)),
            "the index follows a removal"
        );
        assert_eq!(row_id(&json!({"id": 7}), "id").as_deref(), Some("7"));
    }

    #[test] // spec: APP-025
    fn rows_ignore_events_older_than_the_row() {
        let mut rows = Rows::new();
        rows.replace(
            vec![json!({"job_id": "j1", "status": "succeeded", "updated": "2026-10-27T12:00:05.25Z"})],
            "job_id",
        );
        let older =
            json!({"job_id": "j1", "status": "running", "updated": "2026-10-27T12:00:05.2Z"});
        assert_eq!(rows.upsert(older, "job_id"), None);
        assert_eq!(rows.get(0).unwrap()["status"], "succeeded");
        // The same time is the same state: applied (full state, idempotent).
        let same =
            json!({"job_id": "j1", "status": "succeeded", "updated": "2026-10-27T12:00:05.250Z"});
        assert_eq!(rows.upsert(same, "job_id"), Some(Change::Updated(0)));
        let newer = json!({"job_id": "j1", "status": "failed", "updated": "2026-10-27T12:00:06Z"});
        assert_eq!(rows.upsert(newer, "job_id"), Some(Change::Updated(0)));
        assert_eq!(rows.get(0).unwrap()["status"], "failed");
        // A time that isn't RFC 3339 UTC can't be ordered: the event applies.
        let odd = json!({"job_id": "j1", "status": "queued", "updated": "yesterday"});
        assert_eq!(rows.upsert(odd, "job_id"), Some(Change::Updated(0)));

        assert_eq!(
            compare_utc("2026-10-27T12:00:05Z", "2026-10-27T12:00:05.000Z"),
            Some(Ordering::Equal)
        );
        assert_eq!(
            compare_utc("2026-10-27T12:00:05.9Z", "2026-10-27T12:00:06Z"),
            Some(Ordering::Less)
        );
        assert_eq!(
            compare_utc("2026-10-27T12:00:05+01:00", "2026-10-27T12:00:06Z"),
            None
        );
    }
}
