//! `handlers.for`, `open_in` and `ui.show` (Appendix F "Product tools"), module `interchange`:
//!
//! | Tool | Effect | Arguments | `structured` |
//! |---|---|---|---|
//! | `handlers.for` | read | `{target}` | `{handlers: [{app, intents, external}]}` |
//! | `open_in` | read | `{app, target, intent: "open" \| "edit" \| "use_as_input"}` | `{running, pid, launch_command}` |
//! | `ui.show` | read | `{target}` | as `open_in` |
//!
//! The intent set is closed (APP-066): any other value is `params.invalid`.

use std::sync::Arc;

use lil_protocol::{EffectClass, ErrorBody, codes};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::{
    HandlerView, INTERCHANGE_MODULE, Intent, Interchange, OpenResult, Target, handlers_for_target,
};
use crate::tools::{Registry, typed_tool};

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ForArgs {
    /// A file path, a `lil://` ref, `@candidate/<run>/<file>` or `doc:<doc_id>/…`.
    target: String,
}

#[derive(Debug, Serialize, JsonSchema)]
struct ForResult {
    handlers: Vec<HandlerView>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct OpenArgs {
    /// The app's name, as `handlers.for` lists it.
    app: String,
    target: String,
    intent: Intent,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ShowArgs {
    target: String,
}

/// Registers `handlers.for`, `open_in` and `ui.show` on `reg`.
pub fn register_interchange_tools(reg: &mut Registry, interchange: &Arc<Interchange>) {
    let ix = interchange.clone();
    reg.register(typed_tool(
        "handlers.for",
        "Lists the registered apps and programs that can open a target (a file path, a lil:// \
         ref, a candidate or a document) and the intents each handles: open, edit or \
         use_as_input. A target nothing can open has an empty list.",
        EffectClass::Read,
        INTERCHANGE_MODULE,
        move |ctx, args: ForArgs| {
            let ix = ix.clone();
            Box::pin(async move {
                let target = Target::parse(&args.target).map_err(|why| {
                    ErrorBody::new(&codes::PARAMS_INVALID, why, ctx.trace_id.clone())
                })?;
                let regs = tokio::task::spawn_blocking(move || ix.registrations())
                    .await
                    .unwrap_or_default();
                let handlers = handlers_for_target(&regs, &target);
                let summary = format!("{} handler(s)", handlers.len());
                Ok((ForResult { handlers }, summary))
            })
        },
    ));
    let ix = interchange.clone();
    reg.register(typed_tool(
        "open_in",
        "Asks an app to open a target for an intent (open, edit or use_as_input). Returns \
         {running, pid, launch_command}. A running app is told on its open_requests topic and \
         shows the content without taking focus when an agent asks. If it isn't running, \
         launch_command is {exe, args} for you to run; the core never starts an app itself. \
         An app that doesn't handle the target for that intent is handler.none.",
        EffectClass::Read,
        INTERCHANGE_MODULE,
        move |ctx, args: OpenArgs| {
            let ix = ix.clone();
            Box::pin(async move {
                let result = ix
                    .open_in(ctx, &args.app, &args.target, args.intent)
                    .await?;
                let summary = summary(&args.app, &result);
                Ok((result, summary))
            })
        },
    ));
    let ix = interchange.clone();
    reg.register(typed_tool(
        "ui.show",
        "Shows a target in the app that opens it, without taking focus when an agent asks: a \
         running app that handles it, else the first registered one. Returns \
         {running, pid, launch_command}; see open_in. No app opens it is handler.none.",
        EffectClass::Read,
        INTERCHANGE_MODULE,
        move |ctx, args: ShowArgs| {
            let ix = ix.clone();
            Box::pin(async move {
                let result = ix.ui_show(ctx, &args.target).await?;
                let summary = summary("the app", &result);
                Ok((result, summary))
            })
        },
    ));
}

fn summary(app: &str, result: &OpenResult) -> String {
    if result.running {
        format!("told {app}, which is running")
    } else {
        format!("{app} is not running; launch_command says how to start it")
    }
}
