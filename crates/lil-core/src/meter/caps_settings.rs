//! The caps of `suite.toml` (CORE-085; Appendix F "Caps and money in `suite.toml`"):
//! `[money] display_currency` and `[[money.rates]]`, and `[caps]` `monthly`, `per_call_max` and
//! `[caps.provider_monthly]`, each a decimal string or an integer in the display currency.
//!
//! - Caps are optional and off by default: a missing key is no cap.
//! - The workspace format's `monthly_usd`, `per_call_max_usd` and `[caps.provider_monthly_usd]`
//!   are read only when the display currency is `USD` and the new key is absent.
//! - A value that doesn't parse (a float, a negative amount, more than 6 decimals, another type)
//!   is listed in [`CapsSettings::invalid`] by its cap name, and that cap counts as exceeded:
//!   caps fail closed (ARCH-056). So does a `display_currency` that isn't a currency code while
//!   a cap is set (every cap set is then invalid), and a `suite.toml` that can't be parsed
//!   (`monthly` and `per_call_max` are then invalid: what it holds can't be known).

use std::collections::BTreeMap;

use lil_schema::SuiteToml;
use lil_schema::money::{Currency, Money, Rate};

use super::summary::MoneySettings;

/// The cap of all money in a month.
pub const MONTHLY: &str = "monthly";
/// The cap of one call.
pub const PER_CALL_MAX: &str = "per_call_max";

/// `provider:<id>`, the name of a provider's monthly cap.
pub fn provider_cap(provider: &str) -> String {
    format!("provider:{provider}")
}

/// The caps and money settings of `suite.toml` (module docs).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapsSettings {
    pub display: Currency,
    /// Each currency's dated rates, oldest first.
    pub rates: Vec<(Currency, Rate)>,
    pub monthly: Option<Money>,
    pub per_call_max: Option<Money>,
    pub provider_monthly: BTreeMap<String, Money>,
    /// The caps whose value doesn't parse (`monthly`, `per_call_max`, `provider:<id>`).
    pub invalid: Vec<String>,
}

impl Default for CapsSettings {
    fn default() -> Self {
        CapsSettings {
            display: MoneySettings::default().display,
            rates: Vec::new(),
            monthly: None,
            per_call_max: None,
            provider_monthly: BTreeMap::new(),
            invalid: Vec::new(),
        }
    }
}

/// A cap amount: a non-negative decimal string or integer (a float's text may not be exact).
fn amount(v: &toml::Value) -> Option<Money> {
    let m = match v {
        toml::Value::String(s) => Money::parse_decimal(s.trim()).ok()?,
        toml::Value::Integer(n) => Money::from_micros(n.checked_mul(1_000_000)?),
        _ => return None,
    };
    (m >= Money::ZERO).then_some(m)
}

impl CapsSettings {
    /// The settings of a `suite.toml` text: the new keys, then (display currency `USD` only)
    /// the legacy ones (module docs).
    pub fn from_suite_toml(text: &str) -> CapsSettings {
        let suite = match SuiteToml::parse(text) {
            Ok(suite) => suite,
            Err(e) => {
                tracing::warn!(error = %e, "suite.toml doesn't parse; the caps fail closed");
                return CapsSettings {
                    invalid: vec![MONTHLY.to_string(), PER_CALL_MAX.to_string()],
                    ..CapsSettings::default()
                };
            }
        };
        let money = MoneySettings::from_suite_toml(&suite);
        let mut out = CapsSettings {
            display: money.display.clone(),
            rates: money
                .rates
                .iter()
                .filter_map(|(c, list)| Some((Currency::parse(c).ok()?, list)))
                .flat_map(|(c, list)| list.iter().map(move |r| (c.clone(), r.clone())))
                .collect(),
            ..CapsSettings::default()
        };
        let Some(caps) = suite.other.get("caps") else {
            return out;
        };
        let usd = out.display.as_str() == "USD";
        let invalid = |name: String, out: &mut CapsSettings| {
            tracing::warn!(cap = %name, "a cap in suite.toml doesn't parse; it counts as exceeded");
            out.invalid.push(name);
        };
        for (key, legacy, name) in [
            (MONTHLY, "monthly_usd", MONTHLY),
            (PER_CALL_MAX, "per_call_max_usd", PER_CALL_MAX),
        ] {
            let value = caps
                .get(key)
                .or_else(|| usd.then(|| caps.get(legacy)).flatten());
            let Some(value) = value else {
                continue;
            };
            match amount(value) {
                Some(m) if name == MONTHLY => out.monthly = Some(m),
                Some(m) => out.per_call_max = Some(m),
                None => invalid(name.to_string(), &mut out),
            }
        }
        let table = |key: &str| caps.get(key).and_then(toml::Value::as_table);
        let mut providers: BTreeMap<String, &toml::Value> = BTreeMap::new();
        if usd && let Some(legacy) = table("provider_monthly_usd") {
            providers.extend(legacy.iter().map(|(k, v)| (k.clone(), v)));
        }
        if let Some(new) = table("provider_monthly") {
            providers.extend(new.iter().map(|(k, v)| (k.clone(), v)));
        }
        for (key, legacy) in [("provider_monthly", false), ("provider_monthly_usd", true)] {
            if caps.get(key).is_some() && table(key).is_none() && (!legacy || usd) {
                invalid(key.to_string(), &mut out);
            }
        }
        for (provider, value) in providers {
            match amount(value) {
                Some(m) => {
                    out.provider_monthly.insert(provider, m);
                }
                None => invalid(provider_cap(&provider), &mut out),
            }
        }
        // A display currency that isn't a currency code: no cap set can be read in it.
        let display_ok = suite
            .other
            .get("money")
            .and_then(|m| m.get("display_currency"))
            .is_none_or(|c| c.as_str().is_some_and(|c| Currency::parse(c).is_ok()));
        if !display_ok {
            let mut names: Vec<String> = Vec::new();
            if out.monthly.take().is_some() {
                names.push(MONTHLY.to_string());
            }
            if out.per_call_max.take().is_some() {
                names.push(PER_CALL_MAX.to_string());
            }
            names.extend(
                std::mem::take(&mut out.provider_monthly)
                    .into_keys()
                    .map(|p| provider_cap(&p)),
            );
            for name in names {
                invalid(name, &mut out);
            }
        }
        out
    }

    /// Whether the `provider_monthly` table itself is invalid (not a table): every provider's
    /// cap counts as exceeded then.
    pub fn providers_invalid(&self) -> bool {
        self.invalid
            .iter()
            .any(|n| n == "provider_monthly" || n == "provider_monthly_usd")
    }

    /// Whether the cap `name` is invalid.
    pub fn is_invalid(&self, name: &str) -> bool {
        self.invalid.iter().any(|n| n == name)
            || (name.starts_with("provider:") && self.providers_invalid())
    }

    /// Whether any cap would apply to a call of `provider`: a monthly or per-call cap, the
    /// provider's own cap, or an invalid one of these.
    pub fn applies_to(&self, provider: &str) -> bool {
        self.monthly.is_some()
            || self.per_call_max.is_some()
            || self.provider_monthly.contains_key(provider)
            || self.is_invalid(MONTHLY)
            || self.is_invalid(PER_CALL_MAX)
            || self.is_invalid(&provider_cap(provider))
    }

    /// The rate of `currency` on the local `date` (`YYYY-MM-DD`): the newest dated on or before
    /// it, if any. The display currency has none (it is 1).
    pub fn rate(&self, currency: &Currency, date: &str) -> Option<Rate> {
        self.rates
            .iter()
            .filter(|(c, r)| c == currency && r.date.as_str() <= date)
            .map(|(_, r)| r)
            .max_by(|a, b| a.date.cmp(&b.date))
            .cloned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn usd(m: &str) -> Money {
        Money::parse_decimal(m).unwrap()
    }

    #[test] // spec: CORE-085, ARCH-056
    fn invalid_caps_are_listed_and_legacy_keys_read_only_in_usd() {
        let s = CapsSettings::from_suite_toml(
            "[money]\ndisplay_currency = \"USD\"\n[caps]\nmonthly = \"1.5\"\nper_call_max = 0.5\n\
             [caps.provider_monthly]\na = 2\nb = \"-1\"\n",
        );
        assert_eq!(s.monthly, Some(usd("1.5")));
        assert_eq!(s.per_call_max, None);
        assert_eq!(s.provider_monthly.get("a"), Some(&usd("2")));
        assert_eq!(s.invalid, ["per_call_max", "provider:b"]);
        assert!(s.applies_to("zzz"));

        let legacy = "[caps]\nmonthly_usd = 50\n[caps.provider_monthly_usd]\na = \"3\"\n";
        let s = CapsSettings::from_suite_toml(legacy);
        assert_eq!(
            s.monthly,
            Some(usd("50")),
            "USD is the default display currency"
        );
        assert_eq!(s.provider_monthly.get("a"), Some(&usd("3")));
        let s = CapsSettings::from_suite_toml(&format!(
            "[money]\ndisplay_currency = \"EUR\"\n{legacy}"
        ));
        assert_eq!((s.monthly, s.provider_monthly.len()), (None, 0));
        assert!(!s.applies_to("a"), "no caps: they are off");
        // The new key wins over the legacy one.
        let s = CapsSettings::from_suite_toml("[caps]\nmonthly = \"1\"\nmonthly_usd = 9\n");
        assert_eq!(s.monthly, Some(usd("1")));

        let broken = CapsSettings::from_suite_toml("this is = = not toml");
        assert_eq!(broken.invalid, ["monthly", "per_call_max"]);
        let bad_display = CapsSettings::from_suite_toml(
            "[money]\ndisplay_currency = \"dollars\"\n[caps]\nmonthly = 1\n",
        );
        assert_eq!(
            (bad_display.monthly, bad_display.invalid),
            (None, vec!["monthly".to_string()])
        );
    }
}
