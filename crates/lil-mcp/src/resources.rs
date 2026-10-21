//! The MCP resources (AI-115; Appendix F "MCP lowering"): `lil://context/project`,
//! `lil://jobs/open` and `lil://assets/recent`, each read through the core's tools and given
//! as JSON text. A tool's error result is given as its JSON too, so a reader sees why.

use serde_json::{Map, Value, json};

use crate::core_link::CoreLink;
use lil_protocol::{Outcome, ToolOutput};

/// `project.context` for the session's project.
pub const PROJECT: &str = "lil://context/project";
/// The `queued` and `running` jobs, newest first.
pub const JOBS_OPEN: &str = "lil://jobs/open";
/// The 20 assets updated last, newest first.
pub const ASSETS_RECENT: &str = "lil://assets/recent";

/// How many assets `lil://assets/recent` holds.
const RECENT_ASSETS: u64 = 20;

/// `(uri, name, description)` of each resource.
pub const RESOURCES: [(&str, &str, &str); 3] = [
    (
        PROJECT,
        "Project context",
        "The current project: its settings and where each comes from, its links, and the tools \
         suggested for it.",
    ),
    (
        JOBS_OPEN,
        "Open jobs",
        "The queued and running jobs, newest first.",
    ),
    (
        ASSETS_RECENT,
        "Recent assets",
        "The 20 assets updated most recently, newest first.",
    ),
];

/// The JSON text of the resource at `uri`; `None` for a URI that is not one of [`RESOURCES`].
pub async fn read(link: &CoreLink, uri: &str) -> Option<String> {
    let value = match uri {
        PROJECT => structured(call(link, "project.context", json!({})).await),
        ASSETS_RECENT => structured(
            call(
                link,
                "assets.query",
                json!({"sort": {"field": "updated", "dir": "desc"}, "limit": RECENT_ASSETS}),
            )
            .await,
        ),
        JOBS_OPEN => open_jobs(link).await,
        _ => return None,
    };
    Some(value.to_string())
}

/// `jobs.list` takes one `status`, so the `queued` and the `running` jobs are fetched
/// separately and merged by `created`, newest first. Within a second the queued come first:
/// a queued job was most often submitted after the running one it waits behind.
async fn open_jobs(link: &CoreLink) -> Value {
    let mut jobs = Vec::new();
    for status in ["queued", "running"] {
        match call(link, "jobs.list", json!({ "status": status })).await {
            Outcome::Ok(out) => {
                jobs.extend(
                    out.structured["jobs"]
                        .as_array()
                        .cloned()
                        .unwrap_or_default(),
                );
            }
            other => return structured(other),
        }
    }
    // A stable sort keeps each list's own order (newest first) for equal times.
    jobs.sort_by(|a, b| {
        let created = |j: &Value| j["created"].as_str().unwrap_or_default().to_string();
        created(b).cmp(&created(a))
    });
    json!({ "jobs": jobs })
}

async fn call(link: &CoreLink, tool: &str, args: Value) -> Outcome<ToolOutput> {
    let args: Map<String, Value> = match args {
        Value::Object(map) => map,
        _ => Map::new(),
    };
    link.call(tool, args, lil_client::mint_id("ik")).await.0
}

/// An `ok` result's `structured`; any other outcome as its JSON.
fn structured(outcome: Outcome<ToolOutput>) -> Value {
    match outcome {
        Outcome::Ok(out) => out.structured,
        other => serde_json::to_value(&other).unwrap_or(Value::Null),
    }
}
