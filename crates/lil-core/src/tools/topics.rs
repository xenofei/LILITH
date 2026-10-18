//! The snapshot providers of the live topics (ARCH-044, ARCH-032; Appendix E "Topics"):
//! [`AssetSnapshots`] for `asset:lil://<workspace id>/<project>/<asset_id>`, whose snapshot is
//! the `asset.get` result (`pipeline::asset_detail`), and [`ProjectSnapshots`] for
//! `project:<slug>`, whose snapshot is the `project.status` result
//! (`projects::project_status`). They are in `tools`, not `pipeline` (CORE-010).
//!
//! Both serve the workspace this core serves, the default one, opened through the hub (a
//! workspace that is already open is shared). A topic of another workspace, a name that doesn't
//! parse, or a read that fails gets `{"error": <error result>}` as its snapshot, so the
//! subscriber always learns the state it can't see. The providers hold the hub weakly: the
//! bus holds the providers, and the hub holds the bus.

use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Weak};

use lil_protocol::ErrorBody;
use lil_protocol::codes::{CORE_DRAINING, PARAMS_INVALID, WORKSPACE_REQUIRED};
use lil_schema::SCHEME;
use serde_json::{Value, json};

use crate::events::SnapshotProvider;
use crate::pipeline::asset_detail;
use crate::projects::project_status;
use crate::workspace::{OpenWorkspace, WorkspaceHub};

/// `{"error": e}`.
fn error_value(e: &ErrorBody) -> Value {
    json!({ "error": e })
}

/// The open workspace `ws_id` (`None`: the default one), when it is this core's default.
async fn served(
    hub: &Weak<WorkspaceHub>,
    ws_id: Option<&str>,
) -> Result<Arc<OpenWorkspace>, ErrorBody> {
    let Some(hub) = hub.upgrade() else {
        return Err(ErrorBody::new(
            &CORE_DRAINING,
            "The core is shutting down.",
            "",
        ));
    };
    let Some(default) = hub.default_ref() else {
        return Err(ErrorBody::new(
            &WORKSPACE_REQUIRED,
            "This core serves no workspace; set a default workspace first.",
            "",
        ));
    };
    if let Some(id) = ws_id
        && id != default.id
    {
        return Err(ErrorBody::new(
            &WORKSPACE_REQUIRED,
            format!("The workspace {id} is not the one this core serves."),
            "",
        ));
    }
    hub.open(&default).await
}

/// The `asset:` topics' snapshots (module docs).
#[derive(Debug, Clone)]
pub struct AssetSnapshots {
    hub: Weak<WorkspaceHub>,
}

impl AssetSnapshots {
    pub fn new(hub: &Arc<WorkspaceHub>) -> AssetSnapshots {
        AssetSnapshots {
            hub: Arc::downgrade(hub),
        }
    }

    async fn take(&self, topic: &str) -> Result<Value, ErrorBody> {
        let parsed = topic
            .strip_prefix("asset:")
            .and_then(|t| t.strip_prefix(SCHEME))
            .map(|rest| rest.split('/').collect::<Vec<_>>());
        let Some([ws, project, asset_id]) = parsed.as_deref() else {
            return Err(ErrorBody::new(
                &PARAMS_INVALID,
                format!("{topic:?} is not asset:lil://<workspace>/<project>/<asset_id>"),
                "",
            ));
        };
        let open = served(&self.hub, Some(ws)).await?;
        open.index
            .ready_result()
            .await
            .map_err(|e| e.to_error(""))?;
        let detail = asset_detail(&open.store, &open.index, &open.ws.id, project, asset_id).await?;
        Ok(serde_json::to_value(detail).expect("an asset detail serialises"))
    }
}

impl SnapshotProvider for AssetSnapshots {
    fn snapshot<'a>(&'a self, topic: &'a str) -> Pin<Box<dyn Future<Output = Value> + Send + 'a>> {
        Box::pin(async move {
            self.take(topic).await.unwrap_or_else(|e| {
                tracing::info!(topic, code = %e.code, "an asset snapshot is an error");
                error_value(&e)
            })
        })
    }
}

/// The `project:` topics' snapshots (module docs).
#[derive(Debug, Clone)]
pub struct ProjectSnapshots {
    hub: Weak<WorkspaceHub>,
}

impl ProjectSnapshots {
    pub fn new(hub: &Arc<WorkspaceHub>) -> ProjectSnapshots {
        ProjectSnapshots {
            hub: Arc::downgrade(hub),
        }
    }

    async fn take(&self, topic: &str) -> Result<Value, ErrorBody> {
        let slug = topic.strip_prefix("project:").unwrap_or(topic);
        let open = served(&self.hub, None).await?;
        let status = project_status(&open, slug).await?;
        Ok(serde_json::to_value(status).expect("a project status serialises"))
    }
}

impl SnapshotProvider for ProjectSnapshots {
    fn snapshot<'a>(&'a self, topic: &'a str) -> Pin<Box<dyn Future<Output = Value> + Send + 'a>> {
        Box::pin(async move {
            self.take(topic).await.unwrap_or_else(|e| {
                tracing::info!(topic, code = %e.code, "a project snapshot is an error");
                error_value(&e)
            })
        })
    }
}

/// Registers both providers on `hub`'s bus `events` (`asset:` and `project:`).
pub fn register_snapshots(events: &crate::events::EventBus, hub: &Arc<WorkspaceHub>) {
    events.register_snapshot("asset:", Arc::new(AssetSnapshots::new(hub)));
    events.register_snapshot("project:", Arc::new(ProjectSnapshots::new(hub)));
}
