//! `assets.query`'s paging (APP-025, APP-138; Global Constraints "`assets.query` paging").
//!
//! One keyset `SELECT … ORDER BY <sort> <dir>, project, asset_id LIMIT n+1` and one `count(*)`
//! with the same filters, in one read transaction. A missing `status` or `type` sorts and
//! compares as `''` (`COALESCE(<column>, '')`), so no row drops out of later pages through a
//! NULL comparison. Ties always break on `project`, then `asset_id`, ascending.
//!
//! The cursor is opaque: lowercase hex of the JSON `{"v":1,"q":<query hash>,"k":<keyset>}`.
//! The query hash is the first 16 hex characters of the SHA-256 of
//! `canonical_json({project, status, type, text, sort})`; `limit` isn't part of it, so the
//! page size may change between pages. A cursor never expires: it holds a position, not a
//! snapshot.

use rusqlite::types::Value as Sql;
use rusqlite::{Connection, params_from_iter};
use serde_json::{Value, json};

use super::AssetRow;
use crate::store::WsPath;

/// What `assets.query` sorts by.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SortField {
    AssetId,
    Updated,
    Status,
    Type,
}

impl SortField {
    /// `asset_id`, `updated`, `status` or `type`.
    pub fn as_str(self) -> &'static str {
        match self {
            SortField::AssetId => "asset_id",
            SortField::Updated => "updated",
            SortField::Status => "status",
            SortField::Type => "type",
        }
    }

    /// The SQL sort expression, NULL-safe.
    fn expr(self) -> &'static str {
        match self {
            SortField::AssetId => "asset_id",
            SortField::Updated => "updated",
            SortField::Status => "COALESCE(status, '')",
            SortField::Type => "COALESCE(asset_type, '')",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SortDir {
    Asc,
    Desc,
}

impl SortDir {
    /// `asc` or `desc`.
    pub fn as_str(self) -> &'static str {
        match self {
            SortDir::Asc => "asc",
            SortDir::Desc => "desc",
        }
    }
}

/// One `assets.query` page request. Every filter is optional; `project: None` means every
/// project.
#[derive(Debug, Clone, PartialEq)]
pub struct AssetQuery {
    pub project: Option<String>,
    pub status: Option<String>,
    pub asset_type: Option<String>,
    /// Words, each matched as a prefix of a word in the asset's id, project, type, status and
    /// text fields (FTS5).
    pub text: Option<String>,
    pub sort: (SortField, SortDir),
    pub limit: usize,
    /// The position after which the page starts.
    pub after: Option<Keyset>,
}

impl Default for AssetQuery {
    /// Every asset, by `asset_id` ascending, 200 per page.
    fn default() -> Self {
        AssetQuery {
            project: None,
            status: None,
            asset_type: None,
            text: None,
            sort: (SortField::AssetId, SortDir::Asc),
            limit: 200,
            after: None,
        }
    }
}

/// A keyset position: the last row's sort value (`""` for a missing status or type; a number
/// for `updated`), project and asset id.
#[derive(Debug, Clone, PartialEq)]
pub struct Keyset {
    pub sort_value: Value,
    pub project: String,
    pub asset_id: String,
}

/// One page: its rows, the position after its last row when more follow, and the exact total.
#[derive(Debug, Clone, PartialEq)]
pub struct AssetPage {
    pub rows: Vec<AssetRow>,
    pub next: Option<Keyset>,
    pub total: u64,
}

/// The first 16 hex characters of the SHA-256 of `canonical_json({project, status, type, text,
/// sort: {field, dir}})`.
pub fn query_hash(q: &AssetQuery) -> String {
    let filters = json!({
        "project": q.project,
        "status": q.status,
        "type": q.asset_type,
        "text": q.text,
        "sort": {"field": q.sort.0.as_str(), "dir": q.sort.1.as_str()},
    });
    ww_protocol::args_hash(&filters)[..16].to_string()
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes.iter().fold(String::new(), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    })
}

fn unhex(text: &str) -> Option<Vec<u8>> {
    if !text.len().is_multiple_of(2) {
        return None;
    }
    (0..text.len())
        .step_by(2)
        .map(|i| {
            let pair = text.get(i..i + 2)?;
            if pair.bytes().any(|b| b.is_ascii_uppercase()) {
                return None;
            }
            u8::from_str_radix(pair, 16).ok()
        })
        .collect()
}

/// The opaque cursor of the position `k` in query `q`.
pub fn encode_cursor(q: &AssetQuery, k: &Keyset) -> String {
    let cursor = json!({
        "v": 1,
        "q": query_hash(q),
        "k": {"s": k.sort_value, "p": k.project, "a": k.asset_id},
    });
    hex(cursor.to_string().as_bytes())
}

/// The position a cursor of query `q` holds.
///
/// # Errors
/// The `params.invalid` message: the cursor doesn't decode, or belongs to another query
/// (other filters or another sort).
pub fn decode_cursor(q: &AssetQuery, cursor: &str) -> Result<Keyset, String> {
    let invalid = || {
        "the cursor is not a cursor of this query (its filters and sort must be the same); start again without one".to_string()
    };
    let bytes = unhex(cursor).ok_or_else(invalid)?;
    let value: Value = serde_json::from_slice(&bytes).map_err(|_| invalid())?;
    if value["v"] != json!(1) || value["q"] != json!(query_hash(q)) {
        return Err(invalid());
    }
    let k = &value["k"];
    let sort_value = k["s"].clone();
    let typed = match q.sort.0 {
        SortField::Updated => sort_value.is_i64(),
        _ => sort_value.is_string(),
    };
    let (Some(project), Some(asset_id)) = (k["p"].as_str(), k["a"].as_str()) else {
        return Err(invalid());
    };
    if !typed {
        return Err(invalid());
    }
    Ok(Keyset {
        sort_value,
        project: project.to_string(),
        asset_id: asset_id.to_string(),
    })
}

/// The columns [`asset_row`] reads, in order.
pub(super) const ASSET_COLUMNS: &str = "project, asset_id, asset_type, status, version, \
    approved_version, manifest, creation_uuid, updated, needs_repair";

/// A row of [`ASSET_COLUMNS`].
pub(super) fn asset_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<AssetRow> {
    Ok(AssetRow {
        project: r.get(0)?,
        asset_id: r.get(1)?,
        asset_type: r.get(2)?,
        status: r.get(3)?,
        version: r.get(4)?,
        approved_version: r.get(5)?,
        manifest: WsPath::from_disk(r.get(6)?),
        creation_uuid: r.get(7)?,
        updated: r.get(8)?,
        needs_repair: r.get::<_, i64>(9)? != 0,
    })
}

/// How `text` filters: no filter for blank text; otherwise the FTS5 query of its words, each a
/// quoted prefix (`"word"*`), all of them required. A word with no letter or digit can match
/// no token, so text made only of such words matches nothing (`Err(())`).
fn fts_query(text: &str) -> Option<Result<String, ()>> {
    if text.trim().is_empty() {
        return None;
    }
    let words: Vec<String> = text
        .split_whitespace()
        .filter(|w| w.chars().any(char::is_alphanumeric))
        .map(|w| format!("\"{}\"*", w.replace('"', "\"\"")))
        .collect();
    Some(if words.is_empty() {
        Err(())
    } else {
        Ok(words.join(" "))
    })
}

fn sort_value(row: &AssetRow, field: SortField) -> Value {
    match field {
        SortField::AssetId => json!(row.asset_id),
        SortField::Updated => json!(row.updated),
        SortField::Status => json!(row.status.clone().unwrap_or_default()),
        SortField::Type => json!(row.asset_type.clone().unwrap_or_default()),
    }
}

fn sql_value(v: &Value) -> Sql {
    match v {
        Value::Number(n) => n.as_i64().map_or(Sql::Null, Sql::Integer),
        Value::String(s) => Sql::Text(s.clone()),
        _ => Sql::Null,
    }
}

/// One page of `q`, and the exact total, in one read transaction.
pub(super) fn page(conn: &mut Connection, q: &AssetQuery) -> rusqlite::Result<AssetPage> {
    let mut filters: Vec<&str> = Vec::new();
    let mut args: Vec<Sql> = Vec::new();
    if let Some(project) = &q.project {
        filters.push("project = ?");
        args.push(Sql::Text(project.clone()));
    }
    if let Some(status) = &q.status {
        filters.push("status = ?");
        args.push(Sql::Text(status.clone()));
    }
    if let Some(kind) = &q.asset_type {
        filters.push("asset_type = ?");
        args.push(Sql::Text(kind.clone()));
    }
    match q.text.as_deref().and_then(fts_query) {
        None => {}
        Some(Ok(text)) => {
            filters.push("id IN (SELECT rowid FROM assets_fts WHERE assets_fts MATCH ?)");
            args.push(Sql::Text(text));
        }
        Some(Err(())) => filters.push("0"),
    }
    let (field, dir) = q.sort;
    let s = field.expr();
    let keyset: String;
    let mut page_filters = filters.clone();
    let mut page_args = args.clone();
    if let Some(after) = &q.after {
        let cmp = match dir {
            SortDir::Asc => ">",
            SortDir::Desc => "<",
        };
        keyset = format!(
            "({s} {cmp} ? OR ({s} = ? AND (project > ? OR (project = ? AND asset_id > ?))))"
        );
        page_filters.push(&keyset);
        let v = sql_value(&after.sort_value);
        page_args.extend([
            v.clone(),
            v,
            Sql::Text(after.project.clone()),
            Sql::Text(after.project.clone()),
            Sql::Text(after.asset_id.clone()),
        ]);
    }
    let clause = |f: &[&str]| {
        if f.is_empty() {
            String::new()
        } else {
            format!(" WHERE {}", f.join(" AND "))
        }
    };
    let limit = q.limit.max(1);
    page_args.push(Sql::Integer(i64::try_from(limit + 1).unwrap_or(i64::MAX)));
    let order = match dir {
        SortDir::Asc => "ASC",
        SortDir::Desc => "DESC",
    };

    let tx = conn.transaction()?;
    let mut rows: Vec<AssetRow> = {
        let mut stmt = tx.prepare(&format!(
            "SELECT {ASSET_COLUMNS} FROM assets{} ORDER BY {s} {order}, project ASC, asset_id ASC LIMIT ?",
            clause(&page_filters)
        ))?;
        stmt.query_map(params_from_iter(page_args.iter()), asset_row)?
            .collect::<rusqlite::Result<_>>()?
    };
    let total: i64 = tx.query_row(
        &format!("SELECT count(*) FROM assets{}", clause(&filters)),
        params_from_iter(args.iter()),
        |r| r.get(0),
    )?;
    tx.commit()?;

    let next = if rows.len() > limit {
        rows.truncate(limit);
        rows.last().map(|last| Keyset {
            sort_value: sort_value(last, field),
            project: last.project.clone(),
            asset_id: last.asset_id.clone(),
        })
    } else {
        None
    };
    Ok(AssetPage {
        rows,
        next,
        total: u64::try_from(total).unwrap_or(0),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test] // spec: APP-025
    fn fts_words_are_quoted_prefixes() {
        assert_eq!(fts_query("crane"), Some(Ok("\"crane\"*".into())));
        assert_eq!(fts_query(" a  \"b "), Some(Ok("\"a\"* \"\"\"b\"*".into())));
        assert_eq!(fts_query("a - b"), Some(Ok("\"a\"* \"b\"*".into())));
        assert_eq!(
            fts_query("- \""),
            Some(Err(())),
            "nothing a token can match"
        );
        assert_eq!(fts_query("   "), None);
    }

    #[test] // spec: APP-025
    fn cursor_hex_rejects_odd_and_uppercase() {
        assert_eq!(unhex("0aff"), Some(vec![10, 255]));
        assert_eq!(unhex("0AFF"), None);
        assert_eq!(unhex("0af"), None);
        assert_eq!(unhex("zz"), None);
        let q = AssetQuery::default();
        let k = Keyset {
            sort_value: json!("a"),
            project: "p".into(),
            asset_id: "a".into(),
        };
        let c = encode_cursor(&q, &k);
        assert_eq!(decode_cursor(&q, &c), Ok(k));
        // A sort value of the wrong type is refused.
        let mut by_date = q.clone();
        by_date.sort.0 = SortField::Updated;
        let forged = hex(
            json!({"v": 1, "q": query_hash(&by_date), "k": {"s": "x", "p": "p", "a": "a"}})
                .to_string()
                .as_bytes(),
        );
        assert!(decode_cursor(&by_date, &forged).is_err());
    }
}
