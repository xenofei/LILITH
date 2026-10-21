//! Money (CORE-013, QA-025): decimal strings parse to the exact micro-unit value and print back
//! in the shortest exact form, and a conversion at a dated rate rounds half to even. The case
//! count comes from `test-params.toml` `[proptest] cases`.

use std::path::Path;

use lil_schema::money::{Money, Rate};
use proptest::prelude::*;

fn cases() -> u32 {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../test-params.toml");
    let text = std::fs::read_to_string(&path).expect("read test-params.toml");
    let table: toml::Table = text.parse().expect("parse test-params.toml");
    let cases = table["proptest"]["cases"]
        .as_integer()
        .expect("[proptest] cases");
    u32::try_from(cases).expect("cases fits u32")
}

/// The shortest exact text of `micros`, built by hand: whole units, then the six decimals with
/// trailing zeros (and a bare point) removed.
fn reference_text(micros: i64) -> String {
    let sign = if micros < 0 { "-" } else { "" };
    let abs = micros.unsigned_abs();
    let frac = format!("{:06}", abs % 1_000_000);
    let frac = frac.trim_end_matches('0');
    if frac.is_empty() {
        format!("{sign}{}", abs / 1_000_000)
    } else {
        format!("{sign}{}.{frac}", abs / 1_000_000)
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(cases()))]

    #[test] // spec: CORE-013, QA-025
    fn money_parses_exactly_and_round_trips(
        micros in prop_oneof![
            any::<i64>(),
            -10_000_000_i64..10_000_000,
            Just(i64::MIN),
            Just(i64::MAX),
        ],
        zeros in 0_usize..3,
    ) {
        let m = Money::from_micros(micros);
        let text = m.to_decimal_string();
        prop_assert_eq!(&text, &reference_text(micros));
        prop_assert_eq!(Money::parse_decimal(&text), Ok(m));
        // Trailing zeros (within six decimals) are another spelling of the same amount.
        let decimals = text.split_once('.').map_or(0, |(_, f)| f.len());
        if zeros > 0 && decimals + zeros <= 6 {
            let padded = if decimals == 0 {
                format!("{text}.{}", "0".repeat(zeros))
            } else {
                format!("{text}{}", "0".repeat(zeros))
            };
            prop_assert_eq!(Money::parse_decimal(&padded), Ok(m));
        }
        // Sums are exact, and overflow is refused rather than wrapped.
        let half = Money::from_micros(micros / 2);
        let rest = Money::from_micros(micros - micros / 2);
        prop_assert_eq!(half.checked_add(rest), Some(m));
        prop_assert_eq!(m.checked_sub(rest), Some(half));
        if micros > 0 {
            prop_assert_eq!(Money::from_micros(i64::MAX).checked_add(m), None);
        }
    }
}

#[test] // spec: CORE-081, CORE-013
fn convert_rounds_half_even() {
    let rate = |in_display: &str| Rate {
        in_display_micros: Money::parse_decimal(in_display).unwrap().micros(),
        date: "2026-01-01".to_string(),
    };
    let cases = [
        // 2 EUR at 1.1: exact.
        ("2", "1.1", "2.2"),
        // 0.000001 × 0.5 = 0.0000005: a tie, rounded to the even 0.
        ("0.000001", "0.5", "0"),
        // 0.000003 × 0.5 = 0.0000015: a tie, rounded to the even 0.000002.
        ("0.000003", "0.5", "0.000002"),
        // 0.000005 × 0.5 = 0.0000025: a tie, rounded to the even 0.000002.
        ("0.000005", "0.5", "0.000002"),
        // 0.000001 × 0.6 = 0.0000006: above the tie, up.
        ("0.000001", "0.6", "0.000001"),
        // Negative amounts (a refund) are symmetric: -0.0000025 → -0.000002.
        ("-0.000005", "0.5", "-0.000002"),
        ("-0.000003", "0.5", "-0.000002"),
        // Large amounts don't overflow on the way.
        ("5000000000000", "1.5", "7500000000000"),
    ];
    for (amount, at, want) in cases {
        let got = Money::parse_decimal(amount).unwrap().convert(&rate(at));
        assert_eq!(got.to_decimal_string(), want, "{amount} × {at}");
    }
}
