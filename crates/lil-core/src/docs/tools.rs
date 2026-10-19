//! `doc.list_open` (Appendix F "Product tools"), module `docs`:
//!
//! | Tool | Effect | Arguments | `structured` |
//! |---|---|---|---|
//! | `doc.list_open` | read | `{}` | `{documents: [{doc_id, path, held_by}]}` |
//!
//! Documents themselves, and the `doc.*` tools that open and change them, are plan 5.

use std::sync::Arc;

use lil_protocol::EffectClass;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::DOCS_MODULE;
use super::holds::{HoldView, Holds};
use crate::tools::{Registry, typed_tool};

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
        "Lists the documents that are open: each has a doc_id, its path and held_by, the app \
         that holds it (the first app to open a document holds it, and others open it \
         read-only), or `core` while the core is operating on a closed document. A write to a \
         held document from anyone but its holder fails with doc.held_elsewhere.",
        EffectClass::Read,
        DOCS_MODULE,
        move |_ctx, _args: NoArgs| {
            let holds = holds.clone();
            Box::pin(async move {
                let documents = holds.list();
                let summary = format!("{} open document(s)", documents.len());
                Ok((ListOpenResult { documents }, summary))
            })
        },
    ));
}
