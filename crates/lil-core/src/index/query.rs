//! `assets.query`'s paging, filters and facets (APP-025, APP-138, CORE-152, CORE-168, CORE-170;
//! Global Constraints "`assets.query` paging"; Appendix H "`assets.query`"; P4b-R10, P4b-R11,
//! P4b-R40).
//!
//! One keyset `SELECT … ORDER BY <sort> <dir>, project, asset_id LIMIT n+1` and one `count(*)`
//! with the same filters, in one read transaction. A missing `status` or `type` sorts and
//! compares as `''` (`COALESCE(<column>, '')`), so no row drops out of later pages through a
//! NULL comparison. Ties always break on `project`, then `asset_id`, ascending.
//!
//! **Filters** combine with AND; a list filter matches any of its values (an empty list is no
//! filter). `tags` compare by their fold ([`lil_schema::fold_tag`]); `label` `none` matches an
//! unlabelled asset; `flag` matches an asset with a take so flagged in any run not trashed
//! (`take_marks` mirrors only those, P4b-R40); `duplicates` matches an asset holding a `sealed`
//! or `candidate` file whose SHA-256 another asset's `sealed` or `candidate` file has
//! (CORE-168); `deprecated` is `show` (no filter, the default), `hide` or `only` (P4b-R11).
//!
//! **Facets** (`facets: true`): the counts of `status`, `type`, `tags`, `rating`, `label` and
//! `flag`, each over the set filtered by every filter but its own (P4b-R10), by count
//! (largest first), then value; `tags` keeps the 100 largest and says whether more exist. They
//! are counted in the page's read transaction, so the page, `total` and the facets see one
//! state of the index.
//!
//! The cursor is opaque: lowercase hex of the JSON `{"v":1,"q":<query hash>,"k":<keyset>}`.
//! The query hash is the first 16 hex characters of the SHA-256 of
//! `canonical_json({project, status, type, text, sort})`, each argument in the form it was given
//! (a `status` or `type` string hashes as before plan 4b, a list as the list), plus each new
//! filter key that is present (`tags`, `rating_min`, `label`, `flag`, `collection`,
//! `duplicates`, `deprecated`), so a cursor made before those keys existed stays valid. `limit`
//! and `facets` aren't part of it: neither changes which assets match, so the page size may
//! change between pages and the next page needn't ask for facets again. A cursor never
//! expires: it holds a position, not a snapshot.

use std::cmp::Ordering;

use lil_schema::{Flag, Label, fold_tag};
use rusqlite::types::Value as Sql;
use rusqlite::{Connection, Transaction, params_from_iter};
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

    /// The SQL sort expression over `assets a`, NULL-safe.
    fn expr(self) -> &'static str {
        match self {
            SortField::AssetId => "a.asset_id",
            SortField::Updated => "a.updated",
            SortField::Status => "COALESCE(a.status, '')",
            SortField::Type => "COALESCE(a.asset_type, '')",
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

/// A `status` or `type` filter, in the form it was given: one value, or a list (any of). The
/// cursor hashes it as given.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Choice {
    One(String),
    AnyOf(Vec<String>),
}

impl Choice {
    /// The values it matches.
    pub fn values(&self) -> &[String] {
        match self {
            Choice::One(v) => std::slice::from_ref(v),
            Choice::AnyOf(v) => v,
        }
    }

    /// The form it was given in: a string or a list.
    fn wire(&self) -> Value {
        match self {
            Choice::One(v) => json!(v),
            Choice::AnyOf(v) => json!(v),
        }
    }
}

/// `deprecated`: `show` (no filter; the default), `hide` or `only` (P4b-R11).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DeprecatedFilter {
    Show,
    Hide,
    Only,
}

impl DeprecatedFilter {
    /// `show`, `hide` or `only`.
    pub fn as_str(self) -> &'static str {
        match self {
            DeprecatedFilter::Show => "show",
            DeprecatedFilter::Hide => "hide",
            DeprecatedFilter::Only => "only",
        }
    }
}

/// A collection's assets (Task 9 resolves a collection id into one of these).
#[derive(Debug, Clone, PartialEq)]
pub enum CollectionFilter {
    /// These `(project, asset_id)` names.
    Refs(Vec<(String, String)>),
    /// The assets a saved search's filters match (its sort, limit, cursor and facets don't
    /// apply).
    Rule(Box<AssetQuery>),
}

/// One `assets.query` page request. Every filter is optional; `project: None` means every
/// project. `None` is "not given", which the cursor tells apart from a given value.
#[derive(Debug, Clone, PartialEq)]
pub struct AssetQuery {
    pub project: Option<String>,
    pub status: Option<Choice>,
    pub asset_type: Option<Choice>,
    /// Words, each matched as a prefix of a word in the asset's id, project, type, status and
    /// text fields (FTS5).
    pub text: Option<String>,
    /// Any of these tags, compared by their fold.
    pub tags: Option<Vec<String>>,
    /// 0–5.
    pub rating_min: Option<u8>,
    /// Any of these labels; `None` is "no label".
    pub label: Option<Vec<Option<Label>>>,
    /// An asset with a take so flagged in any run not trashed.
    pub flag: Option<Vec<Flag>>,
    pub collection: Option<CollectionFilter>,
    /// `Some(true)`: assets sharing bytes with another asset.
    pub duplicates: Option<bool>,
    /// `None` is `show`.
    pub deprecated: Option<DeprecatedFilter>,
    pub sort: (SortField, SortDir),
    pub limit: usize,
    /// The position after which the page starts.
    pub after: Option<Keyset>,
    /// Count the facets too.
    pub facets: bool,
}

impl Default for AssetQuery {
    /// Every asset, by `asset_id` ascending, 200 per page, without facets.
    fn default() -> Self {
        AssetQuery {
            project: None,
            status: None,
            asset_type: None,
            text: None,
            tags: None,
            rating_min: None,
            label: None,
            flag: None,
            collection: None,
            duplicates: None,
            deprecated: None,
            sort: (SortField::AssetId, SortDir::Asc),
            limit: 200,
            after: None,
            facets: false,
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

/// One value of a facet and how many assets have it; `value` is `null` for none.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FacetCount {
    pub value: Value,
    pub count: u64,
}

/// The disjunctive facet counts of a query (P4b-R10).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Facets {
    pub status: Vec<FacetCount>,
    pub asset_type: Vec<FacetCount>,
    /// The 100 largest counts, ties by the tag's fold; each value is one spelling of the tag.
    pub tags: Vec<FacetCount>,
    /// More tags than `tags` lists.
    pub tags_more: bool,
    pub rating: Vec<FacetCount>,
    pub label: Vec<FacetCount>,
    /// `pick`, `reject`, and `null` for an asset with neither.
    pub flag: Vec<FacetCount>,
}

/// The most tags the `tags` facet lists.
pub const TAGS_FACET_MAX: usize = 100;

/// One page: its rows, the position after its last row when more follow, the exact total, and
/// the facets when they were asked for.
#[derive(Debug, Clone, PartialEq)]
pub struct AssetPage {
    pub rows: Vec<AssetRow>,
    pub next: Option<Keyset>,
    pub total: u64,
    pub facets: Option<Facets>,
}

/// The filters of `q` in the form they were given, without `sort`: `{project, status, type,
/// text}` always (`null` when not given), and each other filter key that is present.
fn filter_json(q: &AssetQuery) -> serde_json::Map<String, Value> {
    let mut f = serde_json::Map::new();
    f.insert("project".into(), json!(q.project));
    f.insert(
        "status".into(),
        q.status.as_ref().map_or(Value::Null, Choice::wire),
    );
    f.insert(
        "type".into(),
        q.asset_type.as_ref().map_or(Value::Null, Choice::wire),
    );
    f.insert("text".into(), json!(q.text));
    if let Some(tags) = &q.tags {
        f.insert("tags".into(), json!(tags));
    }
    if let Some(r) = q.rating_min {
        f.insert("rating_min".into(), json!(r));
    }
    if let Some(labels) = &q.label {
        let labels: Vec<&str> = labels
            .iter()
            .map(|l| l.map_or("none", Label::as_str))
            .collect();
        f.insert("label".into(), json!(labels));
    }
    if let Some(flags) = &q.flag {
        let flags: Vec<&str> = flags.iter().map(|f| f.as_str()).collect();
        f.insert("flag".into(), json!(flags));
    }
    if let Some(c) = &q.collection {
        let c = match c {
            CollectionFilter::Refs(refs) => {
                let refs: Vec<String> = refs.iter().map(|(p, a)| format!("{p}/{a}")).collect();
                json!({ "refs": refs })
            }
            CollectionFilter::Rule(rule) => json!({ "rule": Value::Object(filter_json(rule)) }),
        };
        f.insert("collection".into(), c);
    }
    if let Some(d) = q.duplicates {
        f.insert("duplicates".into(), json!(d));
    }
    if let Some(d) = q.deprecated {
        f.insert("deprecated".into(), json!(d.as_str()));
    }
    f
}

/// The first 16 hex characters of the SHA-256 of `canonical_json` of the filters as given
/// ([`filter_json`]) and `sort: {field, dir}`.
pub fn query_hash(q: &AssetQuery) -> String {
    let mut filters = filter_json(q);
    filters.insert(
        "sort".into(),
        json!({"field": q.sort.0.as_str(), "dir": q.sort.1.as_str()}),
    );
    lil_protocol::args_hash(&Value::Object(filters))[..16].to_string()
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

/// The columns [`asset_row`] reads, in order, over `assets a` (the tags as a JSON list, in the
/// manifest's order).
pub(super) const ASSET_COLUMNS: &str = "a.project, a.asset_id, a.asset_type, a.status, \
    a.version, a.approved_version, a.manifest, a.creation_uuid, a.updated, a.needs_repair, \
    a.rating, a.label, a.runs_waiting, a.external, a.working_doc, a.display, a.last_change, \
    a.repair, (SELECT json_group_array(t.tag ORDER BY t.pos) FROM asset_tags t \
    WHERE t.project = a.project AND t.asset_id = a.asset_id), a.manifest_project, \
    a.manifest_asset_id, a.manifest_parses";

/// A JSON column: `None` for NULL or text that isn't JSON.
fn json_column(text: Option<String>) -> Option<Value> {
    text.and_then(|t| serde_json::from_str(&t).ok())
}

/// A row of [`ASSET_COLUMNS`].
pub(super) fn asset_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<AssetRow> {
    let tags: Option<String> = r.get(18)?;
    let tags: Vec<String> = tags
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default();
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
        tags,
        rating: u8::try_from(r.get::<_, i64>(10)?).unwrap_or(0),
        label: r.get(11)?,
        runs_waiting: u32::try_from(r.get::<_, i64>(12)?).unwrap_or(0),
        external: r.get::<_, i64>(13)? != 0,
        working_doc: r.get::<_, i64>(14)? != 0,
        display: json_column(r.get(15)?),
        last_change: json_column(r.get(16)?),
        repair: json_column(r.get(17)?),
        manifest_project: r.get(19)?,
        manifest_asset_id: r.get(20)?,
        manifest_parses: r.get::<_, i64>(21)? != 0,
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

/// Which filter a condition comes from: a facet leaves out its own (P4b-R10).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Key {
    Status,
    Type,
    Tags,
    Rating,
    Label,
    Flag,
    /// Every filter no facet is about.
    Other,
}

/// One WHERE condition over `assets a` and its arguments.
#[derive(Debug, Clone)]
struct Cond {
    key: Key,
    sql: String,
    args: Vec<Sql>,
}

impl Cond {
    fn other(sql: &str) -> Cond {
        Cond {
            key: Key::Other,
            sql: sql.to_string(),
            args: Vec::new(),
        }
    }
}

/// `?, ?, …` for `n` arguments.
fn placeholders(n: usize) -> String {
    vec!["?"; n].join(", ")
}

fn texts<'a>(values: impl IntoIterator<Item = &'a str>) -> Vec<Sql> {
    values
        .into_iter()
        .map(|v| Sql::Text(v.to_string()))
        .collect()
}

/// An `IN` condition on `column` over `values`; no condition for an empty list.
fn any_of(key: Key, column: &str, values: &[String]) -> Option<Cond> {
    (!values.is_empty()).then(|| Cond {
        key,
        sql: format!("{column} IN ({})", placeholders(values.len())),
        args: texts(values.iter().map(String::as_str)),
    })
}

/// Whether the `files` row `t` is a deliverable for `duplicates` (CORE-168): a sealed or
/// candidate file, not `missing`, not empty, and not under a version's or a run's `source/`
/// folder (`v<ver>/source/…`, `candidates/<run>/source/…`, compared without regard to case).
fn deliverable(t: &str) -> String {
    // The path inside the asset folder (`v1.0.0/source/x.json`), then after its first and its
    // second component.
    let inside = format!(
        "substr({t}.path, length('projects/' || {t}.project || '/assets/' || {t}.asset_id \
         || '/') + 1)"
    );
    let after_one = format!("substr({inside}, instr({inside}, '/') + 1)");
    let after_two = format!("substr({after_one}, instr({after_one}, '/') + 1)");
    format!(
        "{t}.kind IN ('sealed', 'candidate') AND {t}.state <> 'missing' AND {t}.size > 0 \
         AND NOT ({t}.kind = 'sealed' AND {after_one} LIKE 'source/%') \
         AND NOT ({t}.kind = 'candidate' AND {after_two} LIKE 'source/%')"
    )
}

/// `duplicates`: the asset holds a deliverable ([`deliverable`]) whose SHA-256 another asset's
/// deliverable has, through `files_asset` and `files_sha`.
fn duplicates() -> String {
    format!(
        "EXISTS (SELECT 1 FROM files f WHERE f.project = a.project \
         AND f.asset_id = a.asset_id AND {} AND f.sha256 IS NOT NULL \
         AND EXISTS (SELECT 1 FROM files g WHERE g.sha256 = f.sha256 AND {} \
         AND (g.project IS NOT f.project OR g.asset_id IS NOT f.asset_id)))",
        deliverable("f"),
        deliverable("g")
    )
}

/// The conditions of `q`'s filters (not its keyset).
fn conditions(q: &AssetQuery) -> Vec<Cond> {
    let mut c: Vec<Cond> = Vec::new();
    if let Some(project) = &q.project {
        c.push(Cond {
            key: Key::Other,
            sql: "a.project = ?".into(),
            args: vec![Sql::Text(project.clone())],
        });
    }
    if let Some(status) = &q.status {
        c.extend(any_of(Key::Status, "a.status", status.values()));
    }
    if let Some(kind) = &q.asset_type {
        c.extend(any_of(Key::Type, "a.asset_type", kind.values()));
    }
    match q.text.as_deref().and_then(fts_query) {
        None => {}
        Some(Ok(text)) => c.push(Cond {
            key: Key::Other,
            sql: "a.id IN (SELECT rowid FROM assets_fts WHERE assets_fts MATCH ?)".into(),
            args: vec![Sql::Text(text)],
        }),
        Some(Err(())) => c.push(Cond::other("0")),
    }
    if let Some(tags) = q.tags.as_ref().filter(|t| !t.is_empty()) {
        let folds: Vec<String> = tags.iter().map(|t| fold_tag(t.trim())).collect();
        c.push(Cond {
            key: Key::Tags,
            sql: format!(
                "(a.project, a.asset_id) IN (SELECT project, asset_id FROM asset_tags \
                 WHERE tag_fold IN ({}))",
                placeholders(folds.len())
            ),
            args: texts(folds.iter().map(String::as_str)),
        });
    }
    if let Some(r) = q.rating_min {
        c.push(Cond {
            key: Key::Rating,
            sql: "a.rating >= ?".into(),
            args: vec![Sql::Integer(i64::from(r))],
        });
    }
    if let Some(labels) = q.label.as_ref().filter(|l| !l.is_empty()) {
        let named: Vec<&str> = labels.iter().flatten().map(|l| l.as_str()).collect();
        let mut parts = Vec::new();
        if !named.is_empty() {
            parts.push(format!("a.label IN ({})", placeholders(named.len())));
        }
        if labels.iter().any(Option::is_none) {
            parts.push("a.label IS NULL".into());
        }
        c.push(Cond {
            key: Key::Label,
            sql: format!("({})", parts.join(" OR ")),
            args: texts(named),
        });
    }
    if let Some(flags) = q.flag.as_ref().filter(|f| !f.is_empty()) {
        c.push(Cond {
            key: Key::Flag,
            sql: format!(
                "(a.project, a.asset_id) IN (SELECT project, asset_id FROM take_marks \
                 WHERE flag IN ({}))",
                placeholders(flags.len())
            ),
            args: texts(flags.iter().map(|f| f.as_str())),
        });
    }
    match &q.collection {
        None => {}
        Some(CollectionFilter::Refs(refs)) if refs.is_empty() => c.push(Cond::other("0")),
        Some(CollectionFilter::Refs(refs)) => c.push(Cond {
            key: Key::Other,
            sql: format!(
                "(a.project, a.asset_id) IN (VALUES {})",
                vec!["(?, ?)"; refs.len()].join(", ")
            ),
            args: refs
                .iter()
                .flat_map(|(p, a)| [Sql::Text(p.clone()), Sql::Text(a.clone())])
                .collect(),
        }),
        Some(CollectionFilter::Rule(rule)) => {
            let inner = conditions(rule);
            if !inner.is_empty() {
                let sql: Vec<&str> = inner.iter().map(|i| i.sql.as_str()).collect();
                c.push(Cond {
                    key: Key::Other,
                    sql: format!("({})", sql.join(" AND ")),
                    args: inner.iter().flat_map(|i| i.args.iter().cloned()).collect(),
                });
            }
        }
    }
    if q.duplicates == Some(true) {
        c.push(Cond::other(&duplicates()));
    }
    match q.deprecated {
        None | Some(DeprecatedFilter::Show) => {}
        Some(DeprecatedFilter::Hide) => {
            c.push(Cond::other("COALESCE(a.status, '') <> 'deprecated'"));
        }
        Some(DeprecatedFilter::Only) => c.push(Cond::other("a.status = 'deprecated'")),
    }
    c
}

/// The WHERE clause (with a leading space) and arguments of every condition but `skip`'s.
fn clause(conds: &[Cond], skip: Option<Key>) -> (String, Vec<Sql>) {
    let kept: Vec<&Cond> = conds.iter().filter(|c| Some(c.key) != skip).collect();
    if kept.is_empty() {
        return (String::new(), Vec::new());
    }
    let sql: Vec<&str> = kept.iter().map(|c| c.sql.as_str()).collect();
    let args = kept.iter().flat_map(|c| c.args.iter().cloned()).collect();
    (format!(" WHERE {}", sql.join(" AND ")), args)
}

/// A facet value from a column: text, a whole number, or `null`.
fn facet_value(v: Sql) -> Value {
    match v {
        Sql::Text(s) => json!(s),
        Sql::Integer(n) => json!(n),
        _ => Value::Null,
    }
}

fn count(n: i64) -> u64 {
    u64::try_from(n).unwrap_or(0)
}

/// `SELECT <column>, count(*) … GROUP BY <column>`, by count (largest first), then value.
fn grouped(
    tx: &Transaction<'_>,
    conds: &[Cond],
    key: Key,
    column: &str,
) -> rusqlite::Result<Vec<FacetCount>> {
    let (w, args) = clause(conds, Some(key));
    let mut stmt = tx.prepare_cached(&format!(
        "SELECT {column}, count(*) AS c FROM assets a{w} GROUP BY {column} \
         ORDER BY c DESC, {column} ASC"
    ))?;
    stmt.query_map(params_from_iter(args.iter()), |r| {
        Ok(FacetCount {
            value: facet_value(r.get(0)?),
            count: count(r.get(1)?),
        })
    })?
    .collect()
}

/// The tags facet: the [`TAGS_FACET_MAX`] largest counts by fold, each shown as its smallest
/// spelling, ties by that value (Appendix H), and whether more exist.
fn tags_facet(tx: &Transaction<'_>, conds: &[Cond]) -> rusqlite::Result<(Vec<FacetCount>, bool)> {
    let (w, mut args) = clause(conds, Some(Key::Tags));
    // With no other filter the tags alone answer, from the (tag_fold, tag) index.
    let sql = if w.is_empty() {
        "SELECT min(tag) AS v, count(*) AS c FROM asset_tags GROUP BY tag_fold \
         ORDER BY c DESC, v ASC LIMIT ?"
            .to_string()
    } else {
        format!(
            "SELECT min(t.tag) AS v, count(*) AS c FROM asset_tags t JOIN assets a \
             ON a.project = t.project AND a.asset_id = t.asset_id{w} GROUP BY t.tag_fold \
             ORDER BY c DESC, v ASC LIMIT ?"
        )
    };
    args.push(Sql::Integer(
        i64::try_from(TAGS_FACET_MAX + 1).unwrap_or(i64::MAX),
    ));
    let mut stmt = tx.prepare_cached(&sql)?;
    let mut tags: Vec<FacetCount> = stmt
        .query_map(params_from_iter(args.iter()), |r| {
            Ok(FacetCount {
                value: json!(r.get::<_, String>(0)?),
                count: count(r.get(1)?),
            })
        })?
        .collect::<rusqlite::Result<_>>()?;
    let more = tags.len() > TAGS_FACET_MAX;
    tags.truncate(TAGS_FACET_MAX);
    Ok((tags, more))
}

/// The flag facet: assets with a `pick` take, with a `reject` take, and with neither (`null`),
/// each tested by `take_marks`' key (a row-value `IN` in a result column is far slower here,
/// `docs/spikes/12-browse-core.md`). Zero counts aren't listed.
fn flag_facet(tx: &Transaction<'_>, conds: &[Cond]) -> rusqlite::Result<Vec<FacetCount>> {
    let (w, args) = clause(conds, Some(Key::Flag));
    let has = |flag: Flag| {
        format!(
            "EXISTS (SELECT 1 FROM take_marks k WHERE k.project = a.project \
             AND k.asset_id = a.asset_id AND k.flag = '{}')",
            flag.as_str()
        )
    };
    let (pick, reject, neither): (Option<i64>, Option<i64>, Option<i64>) = tx
        .prepare_cached(&format!(
            "SELECT sum(p), sum(j), sum(NOT p AND NOT j) FROM \
             (SELECT {} AS p, {} AS j FROM assets a{w})",
            has(Flag::Pick),
            has(Flag::Reject)
        ))?
        .query_row(params_from_iter(args.iter()), |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?))
        })?;
    let mut flags: Vec<FacetCount> = [
        (json!(Flag::Pick.as_str()), pick),
        (json!(Flag::Reject.as_str()), reject),
        (Value::Null, neither),
    ]
    .into_iter()
    .filter_map(|(value, n)| {
        let n = count(n.unwrap_or(0));
        (n > 0).then_some(FacetCount { value, count: n })
    })
    .collect();
    flags.sort_by(|a, b| {
        b.count
            .cmp(&a.count)
            .then_with(|| by_value(&a.value, &b.value))
    });
    Ok(flags)
}

/// `null` first, then strings in order.
fn by_value(a: &Value, b: &Value) -> Ordering {
    match (a.as_str(), b.as_str()) {
        (None, None) => Ordering::Equal,
        (None, Some(_)) => Ordering::Less,
        (Some(_), None) => Ordering::Greater,
        (Some(a), Some(b)) => a.cmp(b),
    }
}

/// The six facets of `conds` (P4b-R10).
fn facets(tx: &Transaction<'_>, conds: &[Cond]) -> rusqlite::Result<Facets> {
    let (tags, tags_more) = tags_facet(tx, conds)?;
    Ok(Facets {
        status: grouped(tx, conds, Key::Status, "a.status")?,
        asset_type: grouped(tx, conds, Key::Type, "a.asset_type")?,
        tags,
        tags_more,
        rating: grouped(tx, conds, Key::Rating, "a.rating")?,
        label: grouped(tx, conds, Key::Label, "a.label")?,
        flag: flag_facet(tx, conds)?,
    })
}

/// One page of `q`, the exact total and, when asked, the facets, in one read transaction.
pub(super) fn page(conn: &mut Connection, q: &AssetQuery) -> rusqlite::Result<AssetPage> {
    let conds = conditions(q);
    let (field, dir) = q.sort;
    let s = field.expr();
    let mut page_conds = conds.clone();
    if let Some(after) = &q.after {
        let cmp = match dir {
            SortDir::Asc => ">",
            SortDir::Desc => "<",
        };
        let v = sql_value(&after.sort_value);
        page_conds.push(Cond {
            key: Key::Other,
            sql: format!(
                "({s} {cmp} ? OR ({s} = ? AND (a.project > ? OR (a.project = ? \
                 AND a.asset_id > ?))))"
            ),
            args: vec![
                v.clone(),
                v,
                Sql::Text(after.project.clone()),
                Sql::Text(after.project.clone()),
                Sql::Text(after.asset_id.clone()),
            ],
        });
    }
    let (page_where, mut page_args) = clause(&page_conds, None);
    let (all_where, all_args) = clause(&conds, None);
    let limit = q.limit.max(1);
    page_args.push(Sql::Integer(i64::try_from(limit + 1).unwrap_or(i64::MAX)));
    let order = match dir {
        SortDir::Asc => "ASC",
        SortDir::Desc => "DESC",
    };

    let tx = conn.transaction()?;
    let mut rows: Vec<AssetRow> = {
        let mut stmt = tx.prepare_cached(&format!(
            "SELECT {ASSET_COLUMNS} FROM assets a{page_where} \
             ORDER BY {s} {order}, a.project ASC, a.asset_id ASC LIMIT ?"
        ))?;
        stmt.query_map(params_from_iter(page_args.iter()), asset_row)?
            .collect::<rusqlite::Result<_>>()?
    };
    let total: i64 = tx
        .prepare_cached(&format!("SELECT count(*) FROM assets a{all_where}"))?
        .query_row(params_from_iter(all_args.iter()), |r| r.get(0))?;
    let facets = if q.facets {
        Some(facets(&tx, &conds)?)
    } else {
        None
    };
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
        total: count(total),
        facets,
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
