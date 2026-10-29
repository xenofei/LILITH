//! Money as the apps show it (owner decision 2026-10-29), Qt-free: [`money_text`] turns the
//! core's decimal string (CORE-013) into "1,234.57 USD".
//!
//! - **Rounding.** To the currency's ISO 4217 minor unit ([`minor_unit`]: 0 for JPY, KRW and the
//!   like, 3 for BHD, KWD and the like, 2 for any other code), half up on the decimal digits
//!   themselves, away from zero for a negative amount. No binary float is ever involved.
//! - **Separators.** A comma between each group of three integer digits, a point before the
//!   decimals.
//! - **Small amounts.** A non-zero amount that rounds to zero reads "< 0.01 USD" ("< 1 JPY"),
//!   or "> -0.01 USD" when negative. Zero reads "0.00 USD".
//! - **Malformed input** (not `-?digits[.digits]`) is shown as it came, marked "(unreadable)",
//!   and never panics.
//!
//! The CSV export keeps the exact decimals; this is for the screen only.

/// The ISO 4217 minor unit of `currency`: the number of decimals it is shown with.
pub fn minor_unit(currency: &str) -> usize {
    match currency {
        "BIF" | "CLP" | "DJF" | "GNF" | "ISK" | "JPY" | "KMF" | "KRW" | "PYG" | "RWF" | "UGX"
        | "VND" | "VUV" | "XAF" | "XOF" | "XPF" => 0,
        "BHD" | "IQD" | "JOD" | "KWD" | "LYD" | "OMR" | "TND" => 3,
        "CLF" | "UYW" => 4,
        _ => 2,
    }
}

/// `amount` (the core's decimal string) in `currency`, as the apps show it (module docs).
pub fn money_text(amount: &str, currency: &str) -> String {
    let with_code = |text: String| {
        if currency.is_empty() {
            text
        } else {
            format!("{text} {currency}")
        }
    };
    let Some((negative, whole, part)) = parse(amount) else {
        return format!("{} (unreadable)", with_code(amount.to_string()));
    };
    let scale = minor_unit(currency);
    // The digits kept (whole and `scale` decimals), and the first one dropped.
    let mut kept: Vec<u8> = whole.bytes().map(|b| b - b'0').collect();
    let decimals: Vec<u8> = part.bytes().map(|b| b - b'0').collect();
    kept.extend((0..scale).map(|i| decimals.get(i).copied().unwrap_or(0)));
    if decimals.get(scale).copied().unwrap_or(0) >= 5 {
        carry(&mut kept);
    }
    let nonzero = whole.bytes().chain(part.bytes()).any(|b| b != b'0');
    if kept.iter().all(|d| *d == 0) {
        if !nonzero {
            return with_code(grouped(&[0], scale));
        }
        let mut unit = vec![0; scale + 1];
        unit[scale] = 1;
        let one = grouped(&unit, scale);
        return with_code(if negative {
            format!("> -{one}")
        } else {
            format!("< {one}")
        });
    }
    let sign = if negative { "-" } else { "" };
    with_code(format!("{sign}{}", grouped(&kept, scale)))
}

/// `(negative, whole digits, decimal digits)` of `-?[0-9]+(\.[0-9]+)?`, or `None`.
fn parse(amount: &str) -> Option<(bool, &str, &str)> {
    let (negative, rest) = match amount.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, amount),
    };
    let (whole, part) = rest.split_once('.').unwrap_or((rest, ""));
    let digits = |s: &str| s.bytes().all(|b| b.is_ascii_digit());
    let well_formed = !whole.is_empty()
        && digits(whole)
        && digits(part)
        && (part.is_empty() == !rest.contains('.'));
    well_formed.then_some((negative, whole, part))
}

/// Adds one to the last digit of `digits`, carrying (a new leading digit when it overflows).
fn carry(digits: &mut Vec<u8>) {
    for d in digits.iter_mut().rev() {
        if *d == 9 {
            *d = 0;
        } else {
            *d += 1;
            return;
        }
    }
    digits.insert(0, 1);
}

/// `digits` with their last `scale` as decimals: the integer part without leading zeros (at
/// least one digit), grouped by threes with commas.
fn grouped(digits: &[u8], scale: usize) -> String {
    let split = digits.len().saturating_sub(scale);
    let (whole, part) = digits.split_at(split);
    let first = whole.iter().position(|d| *d != 0).unwrap_or(whole.len());
    let whole: &[u8] = if first == whole.len() {
        &[0]
    } else {
        &whole[first..]
    };
    let mut out = String::new();
    for (i, d) in whole.iter().enumerate() {
        if i > 0 && (whole.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(char::from(b'0' + d));
    }
    if scale > 0 {
        out.push('.');
        // A `digits` shorter than `scale` (only the zero case) is padded.
        for _ in part.len()..scale {
            out.push('0');
        }
        out.extend(part.iter().map(|d| char::from(b'0' + d)));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test] // spec: APP-120
    fn money_text_rounds_half_up_on_the_decimal() {
        assert_eq!(money_text("1234.565", "USD"), "1,234.57 USD");
        assert_eq!(money_text("1234.5649", "USD"), "1,234.56 USD");
        assert_eq!(money_text("0.125", "EUR"), "0.13 EUR");
        assert_eq!(
            money_text("2.995", "USD"),
            "3.00 USD",
            "a carry into the integer part"
        );
        assert_eq!(
            money_text("999.995", "USD"),
            "1,000.00 USD",
            "a carry that adds a digit"
        );
        assert_eq!(
            money_text("9.4", "USD"),
            "9.40 USD",
            "padded to the minor unit"
        );
        assert_eq!(money_text("5", "USD"), "5.00 USD");
        assert_eq!(money_text("007.1", "USD"), "7.10 USD", "leading zeros go");
    }

    #[test] // spec: APP-120
    fn money_text_groups_thousands() {
        assert_eq!(money_text("1234567.8", "USD"), "1,234,567.80 USD");
        assert_eq!(money_text("123", "USD"), "123.00 USD");
        assert_eq!(money_text("1000", "USD"), "1,000.00 USD");
        assert_eq!(
            money_text("123456789012345678901234567890.5", "USD"),
            "123,456,789,012,345,678,901,234,567,890.50 USD",
            "beyond any float, exactly"
        );
    }

    #[test] // spec: APP-120
    fn money_text_marks_amounts_under_one_minor_unit() {
        assert_eq!(money_text("0.0049", "USD"), "< 0.01 USD");
        assert_eq!(
            money_text("0.005", "USD"),
            "0.01 USD",
            "half a cent rounds up"
        );
        assert_eq!(money_text("0.4", "JPY"), "< 1 JPY");
        assert_eq!(money_text("0.0004", "KWD"), "< 0.001 KWD");
        assert_eq!(money_text("-0.004", "USD"), "> -0.01 USD");
        assert_eq!(money_text("0", "USD"), "0.00 USD");
        assert_eq!(money_text("0.000", "USD"), "0.00 USD");
        assert_eq!(money_text("-0", "USD"), "0.00 USD", "no negative zero");
    }

    #[test] // spec: APP-120
    fn money_text_shows_negatives() {
        assert_eq!(money_text("-1.5", "USD"), "-1.50 USD");
        assert_eq!(
            money_text("-1234.565", "EUR"),
            "-1,234.57 EUR",
            "half away from zero"
        );
        assert_eq!(money_text("-0.005", "USD"), "-0.01 USD");
    }

    #[test] // spec: APP-120
    fn money_text_uses_each_currency_s_minor_unit() {
        assert_eq!(money_text("1234.5", "JPY"), "1,235 JPY");
        assert_eq!(money_text("1234.4", "JPY"), "1,234 JPY");
        assert_eq!(money_text("1.2345", "KWD"), "1.235 KWD");
        assert_eq!(money_text("3", "BHD"), "3.000 BHD");
        assert_eq!(money_text("1.2", "XYZ"), "1.20 XYZ", "an unknown code: 2");
        assert_eq!(minor_unit("JPY"), 0);
        assert_eq!(minor_unit("KRW"), 0);
        assert_eq!(minor_unit("KWD"), 3);
        assert_eq!(minor_unit("GBP"), 2);
        assert_eq!(
            money_text("1.2", ""),
            "1.20",
            "no currency, no trailing space"
        );
    }

    #[test] // spec: APP-120
    fn money_text_shows_malformed_input_as_it_came_marked() {
        for raw in [
            "1.2.3", "", "-", "1.", ".5", "+1", "1e3", "1,000", " 1", "abc", "١٢", "--1",
        ] {
            assert_eq!(
                money_text(raw, "USD"),
                format!("{raw} USD (unreadable)"),
                "{raw:?}"
            );
        }
    }
}
