//! The desktop tool (APP-146, ARCH-127; Appendix G "Notifications"), module `desktop` (an
//! always-available module, P4a-R24):
//!
//! | Tool | Effect | Arguments | `structured` |
//! |---|---|---|---|
//! | `desktop.get` | read | `{fresh?}` | `DesktopSettings` |
//!
//! The settings change only through `app/desktop.set` (APP-039), never through a tool.

use std::sync::Arc;

use lil_protocol::EffectClass;
use lil_protocol::app::{DesktopSettings, ToastsWire};
use schemars::JsonSchema;
use serde::Deserialize;

use super::registry::{Registry, typed_tool};
use crate::desktop::{DESKTOP_MODULE, DesktopSettingsStore};

const GET_DESCRIPTION: &str = "Returns the desktop settings: each notification category and \
    whether it is on, quiet_while_fullscreen, fullscreen_autopause and keep_awake_gpu, whether \
    this PC lets the core keep it awake (keep_awake_available), and the notification path: \
    toasts (ready, not_yet_shown or unavailable, with toasts_reason), the Start-menu shortcut \
    (ok, missing or wrong) and the link handlers (ok, missing or other_channel). The path is \
    served from a 10 s cache unless fresh is true. Only the LILITH app changes these settings.";

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct GetArgs {
    /// Recompute the notification path instead of serving the 10 s cache.
    #[serde(default)]
    fresh: bool,
}

/// Registers `desktop.get` on `reg`, reading `store`.
pub fn register_desktop_tools(reg: &mut Registry, store: &Arc<DesktopSettingsStore>) {
    let store = store.clone();
    reg.register(typed_tool(
        "desktop.get",
        GET_DESCRIPTION,
        EffectClass::Read,
        DESKTOP_MODULE,
        move |_ctx, args: GetArgs| {
            let store = store.clone();
            Box::pin(async move {
                let settings: DesktopSettings = store.get(args.fresh).await;
                let toasts = match settings.notification_path.toasts {
                    ToastsWire::Ready => "ready",
                    ToastsWire::NotYetShown => "not yet shown",
                    ToastsWire::Unavailable => "unavailable",
                };
                let summary = format!(
                    "toasts {toasts}; keep-awake {}",
                    if settings.keep_awake_gpu { "on" } else { "off" }
                );
                Ok((settings, summary))
            })
        },
    ));
}
