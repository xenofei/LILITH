//! `doc.list_open` (Appendix F "Product tools"), module `docs`:
//!
//! | Tool | Effect | Arguments | `structured` |
//! |---|---|---|---|
//! | `doc.list_open` | read | `{}` | `{documents: [{doc_id, path, held_by}]}` |
//!
//! Documents themselves, and the `doc.*` tools that open and change them, are plan 5.

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};

use lil_protocol::{CallerKind, EffectClass, ErrorBody, codes};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::DOCS_MODULE;
use super::holds::{HoldGuard, HoldOwner, HoldView, Holds};
use crate::session::Session;
use crate::tools::{Registry, TEST_MODULE, typed_tool};
use crate::transport::ConnObserver;

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct NoArgs {}

#[derive(Debug, Serialize, JsonSchema)]
struct ListOpenResult {
    documents: Vec<HoldView>,
}

/// Registers `doc.list_open` on `reg`.
pub fn register_doc_tools(reg: &mut Registry, holds: &Arc<Holds>) {
    let holds = holds.clone();
    reg.register(typed_tool(
        "doc.list_open",
        "Lists the documents that are open in the session's workspace, and loose ones that \
         belong to none: each has a doc_id, its path and held_by, the app \
         that holds it (the first app to open a document holds it, and others open it \
         read-only), or `core` while the core is operating on a closed document. A write to a \
         held document from anyone but its holder fails with doc.held_elsewhere.",
        EffectClass::Read,
        DOCS_MODULE,
        move |ctx, _args: NoArgs| {
            let holds = holds.clone();
            Box::pin(async move {
                let workspace = ctx.session.workspace.as_ref().map(|w| w.id.as_str());
                let documents = holds.list_for(workspace);
                let summary = format!("{} open document(s)", documents.len());
                Ok((ListOpenResult { documents }, summary))
            })
        },
    ));
}

/// The test channel's way to take a hold over the wire (nothing else does before plan 5): the
/// app connections that said hello, and the guards of the holds `test.hold` took.
#[derive(Default)]
pub struct TestHolds {
    /// Session id → connection id and app name, for `ui:*` sessions.
    sessions: Mutex<HashMap<String, (u64, String)>>,
    guards: Mutex<Vec<HoldGuard>>,
}

impl ConnObserver for TestHolds {
    fn ready(&self, conn_id: u64, session: &Session) {
        if let CallerKind::Ui(app) = &session.caller {
            lock(&self.sessions).insert(session.id.clone(), (conn_id, app.clone()));
        }
    }

    fn closed(&self, conn_id: u64) {
        lock(&self.sessions).retain(|_, (conn, _)| *conn != conn_id);
    }
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct TestHoldArgs {
    doc_id: String,
    path: String,
}

/// Registers `test.hold` on `reg` (test channels only): the calling `ui:*` connection takes the
/// hold on `doc_id`, as an app opening a document will, and keeps it until it closes.
pub fn register_test_hold(reg: &mut Registry, holds: &Arc<Holds>, test: &Arc<TestHolds>) {
    let (holds, test) = (holds.clone(), test.clone());
    reg.register(typed_tool(
        "test.hold",
        "Test channels only: the calling app connection takes the hold on a document and keeps \
         it until the connection closes.",
        EffectClass::Write,
        TEST_MODULE,
        move |ctx, args: TestHoldArgs| {
            let (holds, test) = (holds.clone(), test.clone());
            Box::pin(async move {
                let owner = lock(&test.sessions).get(&ctx.session.id).cloned();
                let Some((conn, app)) = owner else {
                    return Err(ErrorBody::new(
                        &codes::PARAMS_INVALID,
                        "test.hold is for a ui:* connection",
                        ctx.trace_id.clone(),
                    ));
                };
                let guard = holds
                    .acquire(
                        &args.doc_id,
                        Path::new(&args.path),
                        HoldOwner::Connection(conn, app),
                    )
                    .map_err(|mut e| {
                        e.trace_id.clone_from(&ctx.trace_id);
                        e
                    })?;
                lock(&test.guards).push(guard);
                Ok((
                    ListOpenResult {
                        documents: holds.list(),
                    },
                    "held".to_string(),
                ))
            })
        },
    ));
}
