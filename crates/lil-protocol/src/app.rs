//! The `app/*` methods of protocol 1.1 (APP-043; Appendix F "`app/*` methods"): the
//! confirmation list and answer, and `app/caps.set` (CORE-008), whose result is the
//! [`CapsView`] `caps.get` returns too.
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

/// `app/confirmation.answer` params: `{confirmation_id, answer, cap?}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ConfirmationAnswerParams {
    pub confirmation_id: String,
    pub answer: Answer,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cap: Option<CapAnswer>,
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

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
