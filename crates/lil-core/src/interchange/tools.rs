//! `handlers.for`, `open_in` and `ui.show` (Appendix F "Product tools"), `lilref.write`
//! (Appendix G "`.lilref` files") and `notify.claude_code` (Appendix G "Notifications"), module
//! `interchange`:
//!
//! | Tool | Effect | Arguments | `structured` |
//! |---|---|---|---|
//! | `handlers.for` | read | `{target}` | `{handlers: [{app, intents, external}]}` |
//! | `open_in` | read | `{app, target, intent: "open" \| "edit" \| "use_as_input"}` | `{running, pid, launch_command}` |
//! | `ui.show` | read | `{target}` | as `open_in` |
//! | `lilref.write` | write | `{ref, title?}` (a sealed ref with `#sha256=`) | `{path}` |
//! | `notify.claude_code` | write | `{message, title?}` (1–2,000 and at most 100 characters) | `{posted, reason?: "category_off" \| "foreground" \| "duplicate" \| "unavailable"}` |
//!
//! The intent set is closed (APP-066): any other value is `params.invalid`.

use std::path::PathBuf;
use std::sync::Arc;

use lil_protocol::{EffectClass, ErrorBody, codes};
use lil_schema::refs::AssetRef;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::{
    HandlerView, INTERCHANGE_MODULE, Intent, Interchange, NotifyOutcome, OpenResult, Target,
    handlers_for_target, write_lilref,
};
use crate::tools::{Registry, typed_tool};

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct LilrefArgs {
    /// A sealed ref carrying its content hash: `lil://<ws>/<project>/<asset>@<version>#sha256=<hex>`.
    r#ref: String,
    /// Shown by the app; at most 200 characters, no control or text-direction character.
    #[serde(default)]
    title: Option<String>,
}

#[derive(Debug, Serialize, JsonSchema)]
struct LilrefResult {
    /// The `.lilref` file, in the suite's cache: print it as a link.
    path: PathBuf,
}

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

/// The longest `notify.claude_code` message and title, in characters (Appendix G).
const MAX_MESSAGE: usize = 2000;
const MAX_TITLE: usize = 100;

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct NotifyArgs {
    /// Claude Code's notice, 1–2,000 characters.
    #[schemars(length(min = 1, max = 2000))]
    message: String,
    /// At most 100 characters.
    #[serde(default)]
    #[schemars(length(max = 100))]
    title: Option<String>,
}

#[derive(Debug, Serialize, JsonSchema)]
struct NotifyResult {
    posted: bool,
    /// Why it wasn't posted: `category_off`, `foreground`, `duplicate` or `unavailable`.
    #[serde(skip_serializing_if = "Option::is_none")]
    reason: Option<&'static str>,
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
    let ix = interchange.clone();
    reg.register(typed_tool(
        "lilref.write",
        "Writes a small .lilref file for a sealed version and returns its path, so you can give \
         the person a clickable link: print the path, and opening it shows the version in \
         LILITH (terminals only open file links). ref is a sealed ref with its content hash \
         (lil://...@<version>#sha256=<hex>, as asset.get gives it); a candidate ref or one \
         without #sha256= is ref.invalid. title, at most 200 characters, is shown with it. The \
         same ref always gets the same file.",
        EffectClass::Write,
        INTERCHANGE_MODULE,
        move |ctx, args: LilrefArgs| {
            let ix = ix.clone();
            Box::pin(async move {
                let parsed: AssetRef = args.r#ref.parse().map_err(|why| {
                    ErrorBody::new(
                        &codes::REF_INVALID,
                        format!("`{}` is not a lil:// ref: {why}", args.r#ref),
                        ctx.trace_id.clone(),
                    )
                })?;
                let env = ix.env().clone();
                let title = args.title.clone();
                let written = tokio::task::spawn_blocking(move || {
                    write_lilref(&env, &parsed, title.as_deref())
                })
                .await
                .unwrap_or_else(|e| {
                    Err(ErrorBody::new(
                        &codes::STORE_IO,
                        format!("The .lilref write stopped: {e}"),
                        "",
                    ))
                });
                let path = written.map_err(|mut e| {
                    e.trace_id.clone_from(&ctx.trace_id);
                    e
                })?;
                let summary = format!("wrote {}", path.display());
                Ok((LilrefResult { path }, summary))
            })
        },
    ));
    let ix = interchange.clone();
    reg.register(typed_tool(
        "notify.claude_code",
        "Shows Claude Code's notice as a Windows notification from LILITH (lil hook \
         notification calls it). message is 1-2,000 characters, title at most 100. Returns \
         {posted, reason?}: nothing is posted while the Claude Code and agent activity \
         category is off (category_off), a LILITH window is in front (foreground), the same \
         message came within 10 s (duplicate) or notifications can't be shown (unavailable).",
        EffectClass::Write,
        INTERCHANGE_MODULE,
        move |ctx, args: NotifyArgs| {
            let ix = ix.clone();
            Box::pin(async move {
                let chars = args.message.chars().count();
                if !(1..=MAX_MESSAGE).contains(&chars)
                    || args
                        .title
                        .as_ref()
                        .is_some_and(|t| t.chars().count() > MAX_TITLE)
                {
                    return Err(ErrorBody::new(
                        &codes::PARAMS_INVALID,
                        format!(
                            "message is 1 to {MAX_MESSAGE} characters and title at most {MAX_TITLE}"
                        ),
                        ctx.trace_id.clone(),
                    ));
                }
                let outcome = match ix.notifier() {
                    Some(n) => n.claude_code(&args.message, args.title.as_deref()).await,
                    None => NotifyOutcome::Unavailable,
                };
                let result = NotifyResult {
                    posted: outcome == NotifyOutcome::Posted,
                    reason: outcome.reason(),
                };
                let summary = match result.reason {
                    None => "posted".to_string(),
                    Some(why) => format!("not posted: {why}"),
                };
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
