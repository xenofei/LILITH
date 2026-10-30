//! `usage.summary` (CORE-081–083; Appendix F "Product tools"): the usage lines of a range,
//! broken down without ever adding billing bases together.
//!
//! - **Bases.** `money` (`payg`, `prepaid_credits`, `byok`, `free_tier`), `subscription_quota`
//!   (its own units, CORE-070) and `local` (GPU-seconds and energy) are kept apart; each has at
//!   least `records`; money, subscription quota and unattributed spend, and their rows, carry
//!   `units`, the record units they add up (APP-117; `cache_ttl` is a duration, not a count,
//!   so it is never added up). Unattributed records (paid ComfyUI work submitted outside the
//!   suite) are in `unattributed` alone, never in a basis or a total (CORE-082).
//! - **Money.** A record's amount is its `cost`, else its `est_cost` (counted as `estimated`;
//!   a `reconciled` cost is counted as such). `by_currency` sums the money basis per native
//!   currency (CORE-081). `display_total` converts each record at the newest
//!   `[[money.rates]]` entry dated on or before the record's day (a currency with no such rate
//!   is listed in `unconverted` and left out), and adds the fixed costs of each month of the
//!   range when `period` is `month` (CORE-083). A record settled at its estimate
//!   (`cost_source: "estimated"`) counts as estimated too. A sum that would pass the `i64`
//!   micro-unit range leaves that addend out, and the summary says `overflow: true`.
//! - **Periods** are local calendar days or months (CORE-012). Without `from`, the range starts
//!   at the start of the current period; without `to`, it ends now.
//! - **Rows** group the records by period, basis and the `group_by` fields.

use std::collections::BTreeMap;

use lil_schema::SuiteToml;
use lil_schema::money::{Currency, Money, Rate};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

/// `usage.summary`'s `period`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Period {
    Day,
    Month,
}

/// A `group_by` field.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum GroupBy {
    Provider,
    Model,
    Project,
    Workspace,
    CallerKind,
    Tool,
    Account,
}

impl GroupBy {
    fn field(self) -> &'static str {
        match self {
            GroupBy::Provider => "provider",
            GroupBy::Model => "model",
            GroupBy::Project => "project",
            GroupBy::Workspace => "workspace",
            GroupBy::CallerKind => "caller_kind",
            GroupBy::Tool => "tool",
            GroupBy::Account => "account",
        }
    }
}

/// The `usage.summary` arguments.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SummaryQuery {
    /// RFC 3339 UTC; the range includes it. Default: the start of the current period.
    #[serde(default)]
    pub from: Option<String>,
    /// RFC 3339 UTC; the range includes it. Default: now.
    #[serde(default)]
    pub to: Option<String>,
    pub period: Period,
    #[serde(default)]
    pub group_by: Option<Vec<GroupBy>>,
}

/// One fixed recurring cost of `suite.toml` (`[[fixed_costs]]`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FixedCost {
    pub name: String,
    pub amount: Money,
    pub currency: Currency,
    pub account: Option<String>,
}

/// `[money]` and `[[fixed_costs]]` of `suite.toml`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MoneySettings {
    pub display: Currency,
    /// Rates by currency, oldest first.
    pub rates: BTreeMap<String, Vec<Rate>>,
    pub fixed_costs: Vec<FixedCost>,
}

impl Default for MoneySettings {
    fn default() -> Self {
        MoneySettings {
            display: Currency::parse("USD").expect("USD is a currency code"),
            rates: BTreeMap::new(),
            fixed_costs: Vec::new(),
        }
    }
}

/// A TOML value as the decimal text it was written as (a string or an integer; a float, whose
/// text may not be exact, is refused).
fn decimal(v: &toml::Value) -> Option<Money> {
    match v {
        toml::Value::String(s) => Money::parse_decimal(s.trim()).ok(),
        toml::Value::Integer(n) => n.checked_mul(1_000_000).map(Money::from_micros),
        _ => None,
    }
}

/// `YYYY-MM-DD`.
fn is_date(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() == 10
        && b[4] == b'-'
        && b[7] == b'-'
        && b.iter()
            .enumerate()
            .all(|(i, c)| i == 4 || i == 7 || c.is_ascii_digit())
}

impl MoneySettings {
    /// The settings `suite.toml` gives; an entry that doesn't read is left out (logged), so a
    /// typo never hides the rest.
    pub fn from_suite_toml(suite: &SuiteToml) -> MoneySettings {
        let mut out = MoneySettings::default();
        let money = suite.other.get("money");
        if let Some(code) = money.and_then(|m| m.get("display_currency")) {
            match code.as_str().map(Currency::parse) {
                Some(Ok(c)) => out.display = c,
                _ => {
                    tracing::warn!(value = %code, "[money] display_currency is not a currency code; USD stays")
                }
            }
        }
        let rates = money
            .and_then(|m| m.get("rates"))
            .and_then(toml::Value::as_array);
        for entry in rates.into_iter().flatten() {
            let currency = entry
                .get("currency")
                .and_then(toml::Value::as_str)
                .and_then(|c| Currency::parse(c).ok());
            let rate = entry.get("in_display").and_then(decimal);
            let date = entry
                .get("date")
                .and_then(toml::Value::as_str)
                .filter(|d| is_date(d));
            match (currency, rate, date) {
                (Some(c), Some(r), Some(d)) => out
                    .rates
                    .entry(c.as_str().to_string())
                    .or_default()
                    .push(Rate {
                        in_display_micros: r.micros(),
                        date: d.to_string(),
                    }),
                _ => {
                    tracing::warn!(entry = %entry, "a [[money.rates]] entry is not usable; it is left out")
                }
            }
        }
        for list in out.rates.values_mut() {
            list.sort_by(|a, b| a.date.cmp(&b.date));
        }
        let fixed = suite
            .other
            .get("fixed_costs")
            .and_then(toml::Value::as_array);
        for entry in fixed.into_iter().flatten() {
            let name = entry.get("name").and_then(toml::Value::as_str);
            let amount = entry.get("amount").and_then(decimal);
            let currency = entry
                .get("currency")
                .and_then(toml::Value::as_str)
                .and_then(|c| Currency::parse(c).ok());
            let monthly = entry
                .get("period")
                .and_then(toml::Value::as_str)
                .is_none_or(|p| p == "monthly");
            match (name, amount, currency, monthly) {
                (Some(name), Some(amount), Some(currency), true) => {
                    out.fixed_costs.push(FixedCost {
                        name: name.to_string(),
                        amount,
                        currency,
                        account: entry
                            .get("account")
                            .and_then(toml::Value::as_str)
                            .map(str::to_string),
                    });
                }
                _ => {
                    tracing::warn!(entry = %entry, "a [[fixed_costs]] entry is not usable; it is left out")
                }
            }
        }
        out
    }

    /// `m` of `currency` in the display currency on `date`: the newest rate dated on or before
    /// it; `None` when there is none.
    pub fn to_display(&self, m: Money, currency: &str, date: &str) -> Option<Money> {
        if currency == self.display.as_str() {
            return Some(m);
        }
        let rate = self
            .rates
            .get(currency)?
            .iter()
            .rev()
            .find(|r| r.date.as_str() <= date)?;
        Some(m.convert(rate))
    }
}

/// The range and the calendar a summary works in: Unix ms bounds (both included) and the local
/// offset of each instant (CORE-012).
pub struct Range<'a> {
    pub from_ms: i64,
    pub to_ms: i64,
    pub period: Period,
    pub offset_secs: &'a dyn Fn(i64) -> i32,
}

impl Range<'_> {
    /// The local `(YYYY-MM-DD, YYYY-MM)` of `ms`.
    fn local(&self, ms: i64) -> (String, String) {
        let secs = ms.div_euclid(1000) + i64::from((self.offset_secs)(ms));
        (
            lil_schema::time::date(secs),
            lil_schema::time::year_month(secs),
        )
    }

    fn bucket(&self, ms: i64) -> String {
        let (day, month) = self.local(ms);
        match self.period {
            Period::Day => day,
            Period::Month => month,
        }
    }
}

/// Sums of decimal strings or numbers, kept exact in micro-units.
#[derive(Debug, Default, Clone, Copy)]
struct Sum {
    micros: i64,
    /// A decimal string was added: the sum is written as one too.
    text: bool,
    /// An addend would have passed the `i64` micro-unit range; it was left out.
    overflow: bool,
}

impl Sum {
    fn add(&mut self, v: &Value) {
        let m = match v {
            Value::String(s) => {
                self.text = true;
                Money::parse_decimal(s).ok()
            }
            Value::Number(n) => Money::parse_decimal(&n.to_string()).ok(),
            _ => None,
        };
        if let Some(m) = m {
            match self.micros.checked_add(m.micros()) {
                Some(v) => self.micros = v,
                None => self.overflow = true,
            }
        }
    }

    fn value(self) -> Value {
        if !self.text && self.micros % 1_000_000 == 0 {
            json!(self.micros / 1_000_000)
        } else {
            Value::String(Money::from_micros(self.micros).to_decimal_string())
        }
    }
}

/// Money per currency. An addend that would pass the `i64` micro-unit range is left out and
/// marks the sum (`overflow`), so a wrong total is never shown as exact.
#[derive(Debug, Default, Clone)]
struct ByCurrency {
    totals: BTreeMap<String, Money>,
    overflow: bool,
}

/// `*total += m`, or `false` (`total` unchanged) past the `i64` micro-unit range.
fn add_to(total: &mut Money, m: Money) -> bool {
    match total.checked_add(m) {
        Some(sum) => {
            *total = sum;
            true
        }
        None => false,
    }
}

impl ByCurrency {
    fn add(&mut self, currency: &str, m: Money) {
        let total = self
            .totals
            .entry(currency.to_string())
            .or_insert(Money::ZERO);
        if !add_to(total, m) {
            self.overflow = true;
        }
    }

    fn value(&self) -> Value {
        Value::Object(
            self.totals
                .iter()
                .map(|(c, m)| (c.clone(), Value::String(m.to_decimal_string())))
                .collect(),
        )
    }
}

/// What a basis (or a row) adds up.
#[derive(Debug, Default, Clone)]
struct Tally {
    records: u64,
    estimated: u64,
    reconciled: u64,
    money: ByCurrency,
    energy_cost: ByCurrency,
    units: BTreeMap<String, Sum>,
}

impl Tally {
    /// Whether one of its sums passed the micro-unit range.
    fn overflowed(&self) -> bool {
        self.money.overflow || self.energy_cost.overflow || self.units.values().any(|s| s.overflow)
    }

    fn value(&self, basis: &str) -> Value {
        let mut v = Map::new();
        v.insert("records".into(), json!(self.records));
        match basis {
            "money" | "unattributed" => {
                v.insert("estimated".into(), json!(self.estimated));
                v.insert("reconciled".into(), json!(self.reconciled));
                v.insert("by_currency".into(), self.money.value());
                v.insert("units".into(), self.units_value());
            }
            "local" => {
                for (k, s) in &self.units {
                    v.insert(k.clone(), s.value());
                }
                v.insert("energy_cost".into(), self.energy_cost.value());
            }
            _ => {
                v.insert("units".into(), self.units_value());
            }
        }
        Value::Object(v)
    }

    /// `{"<unit>": sum}` of the record units added up.
    fn units_value(&self) -> Value {
        Value::Object(
            self.units
                .iter()
                .map(|(k, s)| (k.clone(), s.value()))
                .collect(),
        )
    }
}

/// The record units a tally adds up: every unit but `cache_ttl`, a duration.
fn counted_units() -> impl Iterator<Item = &'static &'static str> {
    super::record::UNITS.iter().filter(|u| **u != "cache_ttl")
}

/// The basis a record counts under.
fn basis_of(line: &Map<String, Value>) -> &'static str {
    if line.get("attribution").and_then(Value::as_str) == Some("unattributed") {
        return "unattributed";
    }
    match line.get("billing_basis").and_then(Value::as_str) {
        Some("subscription_quota") => "subscription_quota",
        Some("local") => "local",
        _ => "money",
    }
}

/// The amount of a money record and its currency: its `cost`, else its `est_cost`, and whether
/// that is an estimate: a record without a cost, or one settled at its estimate
/// (`cost_source: "estimated"`, as stage 8 settles an inline call).
fn amount_of(line: &Map<String, Value>) -> Option<(Money, String, bool)> {
    let currency = line.get("currency").and_then(Value::as_str)?.to_string();
    let parse = |k: &str| {
        line.get(k)
            .and_then(Value::as_str)
            .and_then(|s| Money::parse_decimal(s).ok())
    };
    let estimated = line.get("cost_source").and_then(Value::as_str) == Some("estimated");
    match parse("cost") {
        Some(m) => Some((m, currency, estimated)),
        None => parse("est_cost").map(|m| (m, currency, true)),
    }
}

/// The summary of `lines` (usage records, `machine` and `seq` removed) whose `ts` lies in
/// `range` (module docs).
pub fn summarize(
    lines: &[Map<String, Value>],
    range: &Range<'_>,
    settings: &MoneySettings,
    group_by: &[GroupBy],
) -> Value {
    let mut bases: BTreeMap<&str, Tally> = BTreeMap::new();
    for b in ["money", "subscription_quota", "local", "unattributed"] {
        bases.insert(b, Tally::default());
    }
    let mut rows: BTreeMap<Vec<String>, (Map<String, Value>, Tally)> = BTreeMap::new();
    let mut display = Money::ZERO;
    let mut overflow = false;
    let mut unconverted: Vec<String> = Vec::new();
    for line in lines {
        let Some(ms) = line
            .get("ts")
            .and_then(Value::as_str)
            .and_then(crate::telemetry::parse_ts)
        else {
            continue;
        };
        if ms < range.from_ms || ms > range.to_ms {
            continue;
        }
        let basis = basis_of(line);
        let bucket = range.bucket(ms);
        let mut key = vec![bucket.clone(), basis.to_string()];
        let mut head = Map::new();
        head.insert("period".into(), json!(bucket));
        head.insert("basis".into(), json!(basis));
        for g in group_by {
            let v = line.get(g.field()).cloned().unwrap_or(Value::Null);
            key.push(v.to_string());
            head.insert(g.field().into(), v);
        }
        let row = &mut rows
            .entry(key)
            .or_insert_with(|| (head, Tally::default()))
            .1;
        let tally = bases.get_mut(basis).expect("every basis has a tally");
        for t in [tally, row] {
            t.records += 1;
            match basis {
                "money" | "unattributed" => {
                    if let Some((m, currency, estimated)) = amount_of(line) {
                        t.money.add(&currency, m);
                        t.estimated += u64::from(estimated);
                    }
                    if line.get("cost_source").and_then(Value::as_str) == Some("reconciled") {
                        t.reconciled += 1;
                    }
                    for unit in counted_units() {
                        if let Some(v) = line.get(*unit) {
                            t.units.entry((*unit).to_string()).or_default().add(v);
                        }
                    }
                }
                "local" => {
                    for unit in ["gpu_seconds", "energy_wh_gross", "energy_wh_above_idle"] {
                        if let Some(v) = line.get(unit) {
                            t.units.entry(unit.to_string()).or_default().add(v);
                        }
                    }
                    let energy = line
                        .get("energy_cost")
                        .and_then(Value::as_str)
                        .and_then(|s| Money::parse_decimal(s).ok());
                    let currency = line.get("currency").and_then(Value::as_str);
                    if let (Some(m), Some(c)) = (energy, currency) {
                        t.energy_cost.add(c, m);
                    }
                }
                _ => {
                    for unit in counted_units() {
                        if let Some(v) = line.get(*unit) {
                            t.units.entry((*unit).to_string()).or_default().add(v);
                        }
                    }
                }
            }
        }
        if basis == "money"
            && let Some((m, currency, _)) = amount_of(line)
        {
            let (day, _) = range.local(ms);
            match settings.to_display(m, &currency, &day) {
                Some(d) => overflow |= !add_to(&mut display, d),
                None => {
                    if !unconverted.contains(&currency) {
                        unconverted.push(currency);
                    }
                }
            }
        }
    }
    // Fixed costs: once per local month of the range (CORE-083), in monthly summaries only.
    let mut fixed = Vec::new();
    if range.period == Period::Month {
        let months = months_between(&range.local(range.from_ms).1, &range.local(range.to_ms).1);
        let (to_day, _) = range.local(range.to_ms);
        for f in &settings.fixed_costs {
            let shown = settings.to_display(f.amount, f.currency.as_str(), &to_day);
            match shown {
                Some(d) => {
                    for _ in 0..months {
                        overflow |= !add_to(&mut display, d);
                    }
                }
                None => {
                    let c = f.currency.as_str().to_string();
                    if !unconverted.contains(&c) {
                        unconverted.push(c);
                    }
                }
            }
            fixed.push(json!({
                "name": f.name,
                "amount": f.amount.to_decimal_string(),
                "currency": f.currency.as_str(),
                "display_amount": shown.map(|d| Value::String(d.to_decimal_string())),
            }));
        }
    }
    overflow |= bases.values().any(Tally::overflowed) || rows.values().any(|(_, t)| t.overflowed());
    let money = &bases["money"];
    let mut out = Map::new();
    out.insert(
        "bases".into(),
        json!({
            "money": money.value("money"),
            "subscription_quota": bases["subscription_quota"].value("subscription_quota"),
            "local": bases["local"].value("local"),
        }),
    );
    out.insert("by_currency".into(), money.money.value());
    out.insert("display_currency".into(), json!(settings.display.as_str()));
    out.insert("display_total".into(), json!(display.to_decimal_string()));
    out.insert("fixed_costs".into(), Value::Array(fixed));
    out.insert(
        "unattributed".into(),
        bases["unattributed"].value("unattributed"),
    );
    if overflow {
        // A total passed the micro-unit range: the totals shown leave those addends out.
        out.insert("overflow".into(), Value::Bool(true));
    }
    if !unconverted.is_empty() {
        unconverted.sort();
        out.insert("unconverted".into(), json!(unconverted));
    }
    out.insert(
        "rows".into(),
        Value::Array(
            rows.into_values()
                .map(|(mut head, t)| {
                    let basis = head
                        .get("basis")
                        .and_then(Value::as_str)
                        .unwrap_or("money")
                        .to_string();
                    if let Value::Object(fields) = t.value(&basis) {
                        head.extend(fields);
                    }
                    Value::Object(head)
                })
                .collect(),
        ),
    );
    Value::Object(out)
}

/// The months from `from` to `to` (`YYYY-MM`), both counted; at least 1.
fn months_between(from: &str, to: &str) -> u32 {
    let parse = |s: &str| -> Option<i64> {
        let (y, m) = s.split_once('-')?;
        Some(y.parse::<i64>().ok()? * 12 + m.parse::<i64>().ok()? - 1)
    };
    match (parse(from), parse(to)) {
        (Some(a), Some(b)) if b >= a => u32::try_from(b - a + 1).unwrap_or(1),
        _ => 1,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test] // spec: CORE-083
    fn months_are_counted_inclusively() {
        assert_eq!(months_between("2026-10", "2026-10"), 1);
        assert_eq!(months_between("2026-11", "2027-01"), 3);
        assert_eq!(months_between("2026-11", "2026-10"), 1);
    }

    #[test] // spec: CORE-082
    fn summary_money_and_unattributed_carry_their_units() {
        let line = |provider: &str, model: &str, attribution: &str, units: Value| {
            let mut l = json!({
                "ts": "2027-01-15T12:00:00.000Z", "provider": provider, "model": model,
                "billing_basis": "payg", "attribution": attribution, "currency": "USD", "cost": "1",
            });
            l.as_object_mut()
                .unwrap()
                .extend(units.as_object().unwrap().clone());
            l.as_object().unwrap().clone()
        };
        let lines = [
            line(
                "p",
                "m1",
                "suite",
                json!({"input_tokens": 1200, "output_tokens": 300, "cache_ttl": "5m"}),
            ),
            line(
                "p",
                "m1",
                "suite",
                json!({"input_tokens": 800, "images": 2}),
            ),
            line("p", "m2", "suite", json!({"seconds": "1.5"})),
            line("comfy", "m3", "unattributed", json!({"images": 4})),
        ];
        let utc = |_: i64| 0;
        let range = Range {
            from_ms: 0,
            to_ms: i64::MAX,
            period: Period::Month,
            offset_secs: &utc,
        };
        let s = summarize(&lines, &range, &MoneySettings::default(), &[GroupBy::Model]);
        assert_eq!(
            s["bases"]["money"]["units"],
            json!({"input_tokens": 2000, "output_tokens": 300, "images": 2, "seconds": "1.5"}),
            "{s}"
        );
        assert_eq!(s["unattributed"]["units"], json!({"images": 4}), "{s}");
        assert!(
            s["bases"]["money"]["units"].get("cache_ttl").is_none(),
            "a TTL is never added up"
        );
        let row = |model: &str| {
            s["rows"]
                .as_array()
                .unwrap()
                .iter()
                .find(|r| r["model"] == model)
                .cloned()
                .unwrap()
        };
        assert_eq!(
            row("m1")["units"],
            json!({"input_tokens": 2000, "output_tokens": 300, "images": 2})
        );
        assert_eq!(
            row("m1")["by_currency"],
            json!({"USD": "2"}),
            "money stays as it was"
        );
        assert_eq!(row("m3")["basis"], "unattributed");
        assert_eq!(row("m3")["units"], json!({"images": 4}));
    }
}
