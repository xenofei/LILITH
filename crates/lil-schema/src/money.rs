//! Money (CORE-013, QA-025): an integer count of micro-units (10⁻⁶ of a currency unit) with a
//! currency. There is no float constructor: money is parsed from and written as a decimal
//! string of the exact micro-unit value (`"0.0087"`), never a JSON float.
//!
//! Plan 3 Task 8 starts the type with what the pipeline stages need ([`Money::from_micros`],
//! [`Money::parse_decimal`], [`Money::to_decimal_string`]); Task 15 completes it.

use std::fmt;

/// Micro-units per currency unit.
const MICROS_PER_UNIT: i64 = 1_000_000;
/// The decimals a [`Money`] keeps.
const DECIMALS: usize = 6;

/// An amount of money in micro-units.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct Money(i64);

impl Money {
    pub const ZERO: Money = Money(0);

    /// `m` micro-units.
    pub fn from_micros(m: i64) -> Money {
        Money(m)
    }

    /// The amount in micro-units.
    pub fn micros(self) -> i64 {
        self.0
    }

    /// Parses a decimal string: an optional `-`, digits, and at most 6 decimals after a `.`
    /// (`"2"`, `"0.0087"`, `"-1.5"`). Anything else (a `+`, an exponent, spaces, a lone `.`,
    /// more decimals, a value past the `i64` micro-unit range) is an error.
    ///
    /// # Errors
    /// Why `s` is not an amount.
    pub fn parse_decimal(s: &str) -> Result<Money, String> {
        let bad = || format!("{s:?} is not a decimal amount with at most {DECIMALS} decimals");
        let (negative, body) = match s.strip_prefix('-') {
            Some(rest) => (true, rest),
            None => (false, s),
        };
        let (whole, frac) = match body.split_once('.') {
            Some((whole, frac)) => (whole, Some(frac)),
            None => (body, None),
        };
        let digits = |t: &str| !t.is_empty() && t.bytes().all(|b| b.is_ascii_digit());
        if !digits(whole) || frac.is_some_and(|f| !digits(f) || f.len() > DECIMALS) {
            return Err(bad());
        }
        let mut micros: i64 = 0;
        for b in whole.bytes() {
            micros = micros
                .checked_mul(10)
                .and_then(|m| m.checked_add(i64::from(b - b'0')))
                .ok_or_else(bad)?;
        }
        micros = micros.checked_mul(MICROS_PER_UNIT).ok_or_else(bad)?;
        let frac = frac.unwrap_or("");
        let mut scale = MICROS_PER_UNIT;
        for b in frac.bytes() {
            scale /= 10;
            micros = micros
                .checked_add(i64::from(b - b'0') * scale)
                .ok_or_else(bad)?;
        }
        Ok(Money(if negative { -micros } else { micros }))
    }

    /// The shortest exact decimal form: `"0.0087"`, `"2"`, `"-1.5"`, `"0"`.
    pub fn to_decimal_string(self) -> String {
        let sign = if self.0 < 0 { "-" } else { "" };
        let abs = self.0.unsigned_abs();
        let unit = MICROS_PER_UNIT.unsigned_abs();
        let (whole, frac) = (abs / unit, abs % unit);
        if frac == 0 {
            return format!("{sign}{whole}");
        }
        let frac = format!("{frac:06}");
        format!("{sign}{whole}.{}", frac.trim_end_matches('0'))
    }
}

impl fmt::Display for Money {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_decimal_string())
    }
}

/// An ISO 4217 currency code: three uppercase ASCII letters (`USD`, `EUR`).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Currency(String);

impl Currency {
    /// `code` when it matches `^[A-Z]{3}$`.
    ///
    /// # Errors
    /// Why `code` is not a currency code.
    pub fn parse(code: &str) -> Result<Currency, String> {
        if code.len() == 3 && code.bytes().all(|b| b.is_ascii_uppercase()) {
            Ok(Currency(code.to_string()))
        } else {
            Err(format!(
                "{code:?} is not a currency code (three capital letters)"
            ))
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for Currency {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test] // spec: CORE-013, QA-025
    fn decimal_strings_round_trip_exactly() {
        for (text, micros) in [
            ("0", 0),
            ("2", 2_000_000),
            ("0.0087", 8_700),
            ("-1.5", -1_500_000),
            ("0.000001", 1),
            ("9223372036854.775807", i64::MAX),
        ] {
            let m = Money::parse_decimal(text).unwrap();
            assert_eq!(m.micros(), micros, "{text}");
            assert_eq!(m.to_decimal_string(), text, "{text}");
        }
        // Equal amounts in other spellings parse to the same value and print the short form.
        assert_eq!(
            Money::parse_decimal("1.50").unwrap().to_decimal_string(),
            "1.5"
        );
        assert_eq!(
            Money::parse_decimal("007").unwrap().to_decimal_string(),
            "7"
        );
        assert_eq!(
            Money::from_micros(i64::MIN).to_decimal_string(),
            "-9223372036854.775808"
        );
    }

    #[test] // spec: CORE-013, QA-025
    fn malformed_or_out_of_range_amounts_are_refused() {
        for bad in [
            "",
            "-",
            ".",
            "1.",
            ".5",
            "+1",
            "1e3",
            " 1",
            "1 ",
            "1.0000001",
            "1,5",
            "--1",
            "9223372036855",
            "NaN",
        ] {
            assert!(Money::parse_decimal(bad).is_err(), "{bad:?}");
        }
    }

    #[test] // spec: CORE-013
    fn currency_codes_are_three_capitals() {
        assert_eq!(Currency::parse("USD").unwrap().as_str(), "USD");
        for bad in ["usd", "US", "USDX", "U$D", ""] {
            assert!(Currency::parse(bad).is_err(), "{bad:?}");
        }
    }
}
