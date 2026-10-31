//! The `app/*` methods of protocol 1.1 (APP-043; Appendix F "`app/*` methods"): the
//! confirmation list and answer, and `app/caps.set` (CORE-008), whose result is the
//! [`CapsView`] `caps.get` returns too; and of protocol 1.2 (Appendix G "Pause GPU work"),
//! `app/queue.pause` and `app/queue.resume`, which need `queue.v1` and return a
//! [`QueueState`], and `app/desktop.set` (Appendix G "Notifications"), which needs `desktop.v1`
//! and returns the [`DesktopSettings`] `desktop.get` returns too, and `app/set_context`
//! (Appendix G "Session and listing tools"), which needs `context.v1` and sets the session's
//! project.
//!
//! Every `app/*` method is for verified `ui:*` connections only, except the read-only
//! `app/confirmations.list`, which `cli` may call too; the confirmation methods need
//! `confirmations.v1` or `caps.v1` in the session's shared capabilities, and `app/caps.set`
//! needs `caps.v1`. Params are parsed strictly: an unknown field
//! is -32602 (a caller-supplied `confirm: true` is never accepted anywhere, CORE-111).

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::outcome::Cover;

/// `app/confirmations.list`.
pub const CONFIRMATIONS_LIST: &str = "app/confirmations.list";
/// `app/confirmation.answer`.
pub const CONFIRMATION_ANSWER: &str = "app/confirmation.answer";

/// `app/caps.set`.
pub const CAPS_SET: &str = "app/caps.set";

/// The capabilities an `app/*` confirmation method needs, any one of them.
pub const CONFIRMATION_CAPABILITIES: &[&str] = &["confirmations.v1", "caps.v1"];

/// The capability `app/caps.set` needs.
pub const CAPS_CAPABILITY: &str = "caps.v1";

/// `app/queue.pause` (protocol 1.2).
pub const QUEUE_PAUSE: &str = "app/queue.pause";
/// `app/queue.resume` (protocol 1.2).
pub const QUEUE_RESUME: &str = "app/queue.resume";
/// The capability `app/queue.pause` and `app/queue.resume` need.
pub const QUEUE_CAPABILITY: &str = "queue.v1";

/// `app/desktop.set` (protocol 1.2).
pub const DESKTOP_SET: &str = "app/desktop.set";
/// The capability `app/desktop.set` needs.
pub const DESKTOP_CAPABILITY: &str = "desktop.v1";

/// `app/set_context` (protocol 1.2).
pub const SET_CONTEXT: &str = "app/set_context";
/// The capability `app/set_context` needs.
pub const CONTEXT_CAPABILITY: &str = "context.v1";

/// The most confirmations one `app/confirmations.list` returns, and its default.
pub const LIST_LIMIT_MAX: u32 = 500;
pub const LIST_LIMIT_DEFAULT: u32 = 100;

/// Which confirmations to list.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ListWhich {
    /// Waiting for an answer.
    Pending,
    /// Answered, expired or cancelled.
    History,
}

/// `app/confirmations.list` params: `{state, limit? (1–500, default 100)}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ConfirmationsListParams {
    pub state: ListWhich,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(range(min = 1, max = 500))]
    pub limit: Option<u32>,
}

impl ConfirmationsListParams {
    /// The limit to use: the given one, or 100. `Err` when it is outside 1–500.
    ///
    /// # Errors
    /// Why the limit is not acceptable.
    pub fn limit(&self) -> Result<u32, String> {
        match self.limit {
            None => Ok(LIST_LIMIT_DEFAULT),
            Some(n) if (1..=LIST_LIMIT_MAX).contains(&n) => Ok(n),
            Some(n) => Err(format!("limit {n} is not between 1 and {LIST_LIMIT_MAX}")),
        }
    }
}

/// One confirmation as the app shows it. `arguments` came from the agent: untrusted data.
/// Times are RFC 3339 UTC.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ConfirmationView {
    /// `cf_<16 hex>`.
    pub id: String,
    pub tool: String,
    /// The call's arguments (untrusted).
    pub arguments: serde_json::Value,
    pub caller_kind: String,
    /// The project the call runs in (none without a workspace): an approval is for it only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,
    /// `confirm` before `cap`.
    pub covers: Vec<Cover>,
    /// The caps the call would pass (Task 16).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cap: Option<serde_json::Value>,
    /// `pending`, `approved`, `denied`, `expired` or `cancelled`.
    pub state: String,
    /// `app` or `claude_code`, once answered.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confirmed_via: Option<String>,
    pub created: String,
    pub expires_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub answered: Option<String>,
}

/// `app/confirmations.list` result.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ConfirmationsListResult {
    /// Newest first.
    pub confirmations: Vec<ConfirmationView>,
}

/// An answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Answer {
    Approve,
    Deny,
}

/// What to do about a cap the call would pass.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum CapAction {
    AllowOnce,
    Raise,
}

impl CapAction {
    /// `allow_once` or `raise`.
    pub fn wire(self) -> &'static str {
        match self {
            CapAction::AllowOnce => "allow_once",
            CapAction::Raise => "raise",
        }
    }
}

/// A cap's new limit, a decimal string in the display currency.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RaiseTo {
    pub cap: String,
    pub amount: String,
}

/// The cap part of an answer: `{action, raise_to?}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CapAnswer {
    pub action: CapAction,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raise_to: Option<RaiseTo>,
}

/// `app/confirmation.answer` params: `{confirmation_id, answer, cap?, toast_token?}`. From
/// protocol 1.2 a `toast_token` (32 hex, with `desktop.v1`) is a deny from a confirmation
/// toast: `answer: "deny"` only, and no `cap` (Appendix G "Deny from a toast").
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ConfirmationAnswerParams {
    pub confirmation_id: String,
    pub answer: Answer,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cap: Option<CapAnswer>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub toast_token: Option<String>,
}

/// `app/confirmation.answer` result.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ConfirmationAnswerResult {
    pub confirmation: ConfirmationView,
}

/// A present field, `null` included: `Some(None)` for `null`, `Some(Some(v))` for a value (an
/// absent field is `None` through `#[serde(default)]`).
fn present<'de, D, T>(d: D) -> Result<Option<Option<T>>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(d).map(Some)
}

/// `app/caps.set` params: `{monthly?, per_call_max?, provider_monthly?: {<provider>: amount |
/// null}}`. Each amount is a decimal string in the display currency; an absent field is left
/// as it is and `null` clears the cap.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CapsSetParams {
    #[serde(
        default,
        deserialize_with = "present",
        skip_serializing_if = "Option::is_none"
    )]
    pub monthly: Option<Option<String>>,
    #[serde(
        default,
        deserialize_with = "present",
        skip_serializing_if = "Option::is_none"
    )]
    pub per_call_max: Option<Option<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_monthly: Option<std::collections::BTreeMap<String, Option<String>>>,
}

/// One cap as `caps.get` and `app/caps.set` show it. Amounts are decimal strings in the
/// display currency; `settled` and `held` are this period's.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct CapRow {
    /// `monthly`, `per_call_max` or `provider:<id>`.
    pub cap: String,
    pub limit: String,
    pub settled: String,
    pub held: String,
    /// `(settled + held) × 1000 / limit`, rounded down.
    pub ratio_permille: u64,
    /// The value in `suite.toml` doesn't parse: the cap counts as exceeded (it fails closed).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub invalid: Option<bool>,
}

/// `CapsView {display_currency, caps, period}` (Appendix F `caps.get`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct CapsView {
    pub display_currency: String,
    pub caps: Vec<CapRow>,
    /// The local calendar month, `YYYY-MM`.
    pub period: String,
}

/// A lane `app/queue.*` names (CORE-121): wire `gpu-heavy` or `cpu`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize, JsonSchema)]
pub enum QueueLane {
    #[default]
    #[serde(rename = "gpu-heavy")]
    GpuHeavy,
    #[serde(rename = "cpu")]
    Cpu,
}

/// `app/queue.pause {lane?}` and `app/queue.resume {lane?, held_only?}` params (Appendix G
/// "Pause GPU work"): `lane` defaults to `gpu-heavy`; `held_only` is for a resume only, and
/// then releases the lane's jobs held by a restart and removes no reason.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct QueueParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lane: Option<QueueLane>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub held_only: Option<bool>,
}

/// A lane's pause (Appendix G "Pause GPU work"): `paused` while it has any reason; `held`
/// counts its queued jobs in `queue_paused` for any reason, a restart's included,
/// `restart_held` those a restart holds, and `withdrawn` the prompts the current pause
/// withdrew from the engine. `reasons` is an open set of names (`manual` and `fullscreen` in
/// plan 4a); an app shows one it doesn't know by its name.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct QueueState {
    pub lane: QueueLane,
    pub paused: bool,
    pub reasons: Vec<String>,
    pub held: u32,
    pub restart_held: u32,
    pub withdrawn: u32,
}

/// `app/desktop.set {notifications?, quiet_while_fullscreen?, fullscreen_autopause?,
/// keep_awake_gpu?}` params (Appendix G "Notifications"): a field left out stays as it is;
/// `notifications` names categories by their key, only those `desktop.get` lists.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DesktopSetParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notifications: Option<std::collections::BTreeMap<String, bool>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quiet_while_fullscreen: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fullscreen_autopause: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub keep_awake_gpu: Option<bool>,
}

/// Whether the channel's toasts can be posted (Appendix G "Ready").
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ToastsWire {
    Ready,
    /// The AUMID has never shown a toast; posting is allowed (plan 4a ruling 2026-10-24).
    NotYetShown,
    Unavailable,
}

/// The AUMID shortcut's state; `wrong` covers a wrong target, AUMID or CLSID.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ShortcutWire {
    Ok,
    Missing,
    Wrong,
}

/// The channel's own scheme handler's state, the one toast URLs use; `.lilref` never changes
/// it (plan 4a ruling, 2026-10-25).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum HandlersWire {
    Ok,
    Missing,
    OtherChannel,
}

/// `notification_path {toasts, toasts_reason?, shortcut, handlers}` (Appendix G "Ready"):
/// `toasts_reason` (`disabled_for_app`, `disabled_for_user`, `disabled_by_policy`, `elevated`
/// or `no_platform`) is there only while `toasts` is `unavailable`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct NotificationPath {
    pub toasts: ToastsWire,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub toasts_reason: Option<String>,
    pub shortcut: ShortcutWire,
    pub handlers: HandlersWire,
}

/// `DesktopSettings` (Appendix G "Notifications"), what `desktop.get` and `app/desktop.set`
/// return. `notifications` holds each category this core knows, by its key; an app shows a
/// category it doesn't know by its key.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct DesktopSettings {
    pub notifications: std::collections::BTreeMap<String, bool>,
    pub quiet_while_fullscreen: bool,
    pub fullscreen_autopause: bool,
    pub keep_awake_gpu: bool,
    pub keep_awake_available: bool,
    pub notification_path: NotificationPath,
}

/// `app/set_context` params: `{project: <slug> | null}`. `null` forgets the session's project.
// `{}` (no `project`) changes nothing and answers the current context (review M3). These are
// plain comments: doc comments would change the schema hash, and the wire shape is unchanged.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SetContextParams {
    // `None`: not given; `Some(None)`: `null`; `Some(Some(slug))`: a project.
    #[serde(
        default,
        deserialize_with = "given",
        skip_serializing_if = "Option::is_none"
    )]
    // On the wire it is still an optional string or null.
    #[schemars(with = "Option<String>")]
    pub project: Option<Option<String>>,
}

/// A field that is present, `null` or not: `Some(value)`. Absent fields take the default.
fn given<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<Option<String>>, D::Error> {
    Option::<String>::deserialize(d).map(Some)
}

/// `app/set_context`'s result: the session's workspace (chosen by its `core/hello`), the
/// project it has set, and the project a call naming none would use now.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SetContextResult {
    pub workspace: Option<crate::session::WorkspaceInfo>,
    pub project: Option<String>,
    #[serde(default)]
    pub effective_project: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test] // spec: ARCH-088, APP-043
    fn set_context_params_are_strict_and_the_result_serialises() {
        let p: SetContextParams = serde_json::from_value(json!({"project": "harbor"})).unwrap();
        assert_eq!(p.project, Some(Some("harbor".to_string())));
        let cleared: SetContextParams = serde_json::from_value(json!({"project": null})).unwrap();
        assert_eq!(cleared.project, Some(None));
        // Review M3: `{}` is not `null`: it changes nothing.
        let unchanged: SetContextParams = serde_json::from_value(json!({})).unwrap();
        assert_eq!(unchanged, SetContextParams::default());
        assert_eq!(unchanged.project, None);
        for bad in [json!({"project": 3}), json!({"workspace": "x"})] {
            assert!(
                serde_json::from_value::<SetContextParams>(bad.clone()).is_err(),
                "{bad}"
            );
        }
        let result = SetContextResult {
            workspace: Some(crate::session::WorkspaceInfo {
                id: "0123456789abcdef".into(),
                path: r"C:\ws".into(),
            }),
            project: None,
            effective_project: Some("sketchbook".to_string()),
        };
        assert_eq!(
            serde_json::to_value(&result).unwrap(),
            json!({"workspace": {"id": "0123456789abcdef", "path": r"C:\ws"}, "project": null,
                   "effective_project": "sketchbook"})
        );
    }

    #[test] // spec: CORE-160, APP-043
    fn queue_params_are_strict_and_the_lane_defaults_to_gpu_heavy() {
        let p: QueueParams = serde_json::from_value(json!({})).unwrap();
        assert_eq!(p.lane.unwrap_or_default(), QueueLane::GpuHeavy);
        let p: QueueParams =
            serde_json::from_value(json!({"lane": "cpu", "held_only": true})).unwrap();
        assert_eq!((p.lane, p.held_only), (Some(QueueLane::Cpu), Some(true)));
        for bad in [
            json!({"lane": "gpu-light"}),
            json!({"lane": "gpu_heavy"}),
            json!({"reason": "manual"}),
        ] {
            assert!(
                serde_json::from_value::<QueueParams>(bad.clone()).is_err(),
                "{bad}"
            );
        }
        let state = QueueState {
            lane: QueueLane::GpuHeavy,
            paused: true,
            reasons: vec!["manual".into()],
            held: 2,
            restart_held: 1,
            withdrawn: 0,
        };
        assert_eq!(
            serde_json::to_value(&state).unwrap(),
            json!({"lane": "gpu-heavy", "paused": true, "reasons": ["manual"], "held": 2,
                   "restart_held": 1, "withdrawn": 0})
        );
    }

    #[test] // spec: APP-146, APP-039
    fn desktop_set_params_are_strict_and_settings_serialise() {
        let p: DesktopSetParams = serde_json::from_value(
            json!({"notifications": {"job_finished": false}, "keep_awake_gpu": true}),
        )
        .unwrap();
        assert!(!p.notifications.as_ref().unwrap()["job_finished"]);
        assert_eq!(
            (p.keep_awake_gpu, p.fullscreen_autopause),
            (Some(true), None)
        );
        let empty: DesktopSetParams = serde_json::from_value(json!({})).unwrap();
        assert_eq!(empty, DesktopSetParams::default());
        for bad in [
            json!({"keep_awake_available": true}),
            json!({"notifications": {"job_finished": "no"}}),
            json!({"fullscreen_autopause": 1}),
        ] {
            assert!(
                serde_json::from_value::<DesktopSetParams>(bad.clone()).is_err(),
                "{bad}"
            );
        }
        let settings = DesktopSettings {
            notifications: [("job_failed".to_string(), true)].into(),
            quiet_while_fullscreen: true,
            fullscreen_autopause: false,
            keep_awake_gpu: true,
            keep_awake_available: false,
            notification_path: NotificationPath {
                toasts: ToastsWire::Unavailable,
                toasts_reason: Some("disabled_for_app".into()),
                shortcut: ShortcutWire::Wrong,
                handlers: HandlersWire::OtherChannel,
            },
        };
        assert_eq!(
            serde_json::to_value(&settings).unwrap(),
            json!({"notifications": {"job_failed": true}, "quiet_while_fullscreen": true,
                   "fullscreen_autopause": false, "keep_awake_gpu": true,
                   "keep_awake_available": false,
                   "notification_path": {"toasts": "unavailable",
                                         "toasts_reason": "disabled_for_app",
                                         "shortcut": "wrong", "handlers": "other_channel"}})
        );
        let path = NotificationPath {
            toasts: ToastsWire::NotYetShown,
            toasts_reason: None,
            shortcut: ShortcutWire::Ok,
            handlers: HandlersWire::Ok,
        };
        assert_eq!(
            serde_json::to_value(&path).unwrap(),
            json!({"toasts": "not_yet_shown", "shortcut": "ok", "handlers": "ok"})
        );
    }

    #[test] // spec: APP-043, CORE-111
    fn app_params_are_strict() {
        let list: ConfirmationsListParams =
            serde_json::from_value(json!({"state": "history"})).unwrap();
        assert_eq!(list.state, ListWhich::History);
        assert_eq!(list.limit(), Ok(100));
        for n in [0, 501] {
            let p: ConfirmationsListParams =
                serde_json::from_value(json!({"state": "pending", "limit": n})).unwrap();
            assert!(p.limit().is_err(), "{n}");
        }
        assert!(
            serde_json::from_value::<ConfirmationsListParams>(json!({"state": "all"})).is_err()
        );

        let answer: ConfirmationAnswerParams = serde_json::from_value(json!({
            "confirmation_id": "cf_1", "answer": "approve",
            "cap": {"action": "raise", "raise_to": {"cap": "monthly", "amount": "20"}}
        }))
        .unwrap();
        assert_eq!(answer.answer, Answer::Approve);
        assert_eq!(answer.cap.unwrap().action.wire(), "raise");
        assert_eq!(answer.toast_token, None);
        let deny: ConfirmationAnswerParams = serde_json::from_value(json!({
            "confirmation_id": "cf_1", "answer": "deny",
            "toast_token": "0123456789abcdef0123456789abcdef"
        }))
        .unwrap();
        assert_eq!(
            deny.toast_token.as_deref(),
            Some("0123456789abcdef0123456789abcdef")
        );
        // A caller-supplied `confirm: true` is an unknown field, never accepted (CORE-111).
        let set: CapsSetParams = serde_json::from_value(
            json!({"monthly": "5", "per_call_max": null, "provider_monthly": {"a": "1", "b": null}}),
        )
        .unwrap();
        assert_eq!(set.monthly, Some(Some("5".into())));
        assert_eq!(set.per_call_max, Some(None), "null clears");
        assert_eq!(set.provider_monthly.unwrap()["b"], None);
        let empty: CapsSetParams = serde_json::from_value(json!({})).unwrap();
        assert_eq!(empty, CapsSetParams::default(), "absent leaves it as it is");
        assert!(serde_json::from_value::<CapsSetParams>(json!({"monthly_usd": "5"})).is_err());
        for bad in [
            json!({"confirmation_id": "cf_1", "answer": "approve", "confirm": true}),
            json!({"confirmation_id": "cf_1", "answer": "yes"}),
            json!({"confirmation_id": "cf_1", "answer": "deny", "cap": {"action": "never"}}),
        ] {
            assert!(
                serde_json::from_value::<ConfirmationAnswerParams>(bad.clone()).is_err(),
                "{bad}"
            );
        }
    }
}
