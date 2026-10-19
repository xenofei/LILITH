//! Money (CORE-013, QA-025): an integer count of micro-units (10⁻⁶ of a currency unit) with a
//! currency. There is no float constructor: money is parsed from and written as a decimal
//! string of the exact micro-unit value (`"0.0087"`), never a JSON float.
//!
//! Sums are checked ([`Money::checked_add`], [`Money::checked_sub`]): an overflow is refused,
//! never wrapped. A conversion to the display currency ([`Money::convert`]) uses a dated
//! [`Rate`] and rounds half to even (CORE-081).

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
        // The magnitude in 128 bits, so the most negative amount (whose magnitude is one past
        // `i64::MAX`) parses as exactly as it prints.
        let mut micros: i128 = 0;
        for b in whole.bytes() {
            micros = micros
                .checked_mul(10)
                .and_then(|m| m.checked_add(i128::from(b - b'0')))
                .filter(|m| *m <= i128::from(u64::MAX))
                .ok_or_else(bad)?;
        }
        micros *= i128::from(MICROS_PER_UNIT);
        let frac = frac.unwrap_or("");
        let mut scale = i128::from(MICROS_PER_UNIT);
        for b in frac.bytes() {
            scale /= 10;
            micros += i128::from(b - b'0') * scale;
        }
        let signed = if negative { -micros } else { micros };
        i64::try_from(signed).map(Money).map_err(|_| bad())
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

    /// `self + o`, or `None` past the `i64` micro-unit range.
    pub fn checked_add(self, o: Money) -> Option<Money> {
        self.0.checked_add(o.0).map(Money)
    }

    /// `self - o`, or `None` past the `i64` micro-unit range.
    pub fn checked_sub(self, o: Money) -> Option<Money> {
        self.0.checked_sub(o.0).map(Money)
    }

    /// This amount in the display currency at `r` (1 unit of this amount's currency is
    /// `r.in_display_micros` display micro-units), rounded half to even to a micro-unit. A
    /// result past the `i64` range saturates.
    pub fn convert(self, r: &Rate) -> Money {
        let product = i128::from(self.0) * i128::from(r.in_display_micros);
        let unit = i128::from(MICROS_PER_UNIT);
        let (q, rem) = (product.div_euclid(unit), product.rem_euclid(unit));
        // `q` is the floor; round up when the rest is past half, or exactly half and `q` odd.
        let up = match (2 * rem).cmp(&unit) {
            std::cmp::Ordering::Greater => true,
            std::cmp::Ordering::Equal => q % 2 != 0,
            std::cmp::Ordering::Less => false,
        };
        let rounded = if up { q + 1 } else { q };
        Money(i64::try_from(rounded).unwrap_or(if rounded < 0 { i64::MIN } else { i64::MAX }))
    }
}

/// A dated exchange rate (`suite.toml` `[[money.rates]]`, CORE-081): 1 unit of its currency is
/// `in_display_micros` micro-units of the display currency, as of `date` (`YYYY-MM-DD`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rate {
    pub in_display_micros: i64,
    pub date: String,
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
            ("-9223372036854.775808", i64::MIN),
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
            "9223372036854.775808",
            "-9223372036854.775809",
            "99999999999999999999999999",
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
