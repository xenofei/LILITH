//! UTC date helpers for spec tests (QA-018, ruling R3): today's date and month, the set-aside
//! suffix pattern and RFC 3339 parsing.
//!
//! UTC, proleptic Gregorian, from the system clock. Written independently of
//! `lil-schema::time`, so a spec test never checks the suite's date code against itself.
//!
//! Tests that check "today" take [`today`] before and after the call they test and accept
//! either value, so a call that crosses midnight cannot fail them.

use std::time::{SystemTime, UNIX_EPOCH};

const DAY: i64 = 86_400;

/// Now, as whole seconds since the Unix epoch.
fn now_secs() -> i64 {
    let since = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("the system clock is before 1970");
    i64::try_from(since.as_secs()).expect("the system clock is far in the future")
}

/// Days since 1970-01-01 of a proleptic Gregorian date (Howard Hinnant's `days_from_civil`).
fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = (if y >= 0 { y } else { y - 399 }) / 400;
    let yoe = y - era * 400; // [0, 399]
    let mp = i64::from((month + 9) % 12); // March is 0
    let doy = (153 * mp + 2) / 5 + i64::from(day) - 1; // [0, 365]
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy; // [0, 146096]
    era * 146_097 + doe - 719_468
}

/// The proleptic Gregorian date of a day count since 1970-01-01 (`civil_from_days`).
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = (if z >= 0 { z } else { z - 146_096 }) / 146_097;
    let doe = z - era * 146_097; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let day = u32::try_from(doy - (153 * mp + 2) / 5 + 1).expect("a day of the month");
    let month = u32::try_from(if mp < 10 { mp + 3 } else { mp - 9 }).expect("a month");
    let year = yoe + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

fn is_leap(year: i64) -> bool {
    (year % 4 == 0 && year % 100 != 0) || year % 400 == 0
}

fn days_in_month(year: i64, month: u32) -> u32 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if is_leap(year) => 29,
        2 => 28,
        _ => 0,
    }
}

/// `YYYY-MM-DD` of the UTC day holding `secs`.
fn date_of(secs: i64) -> String {
    let (y, m, d) = civil_from_days(secs.div_euclid(DAY));
    format!("{y:04}-{m:02}-{d:02}")
}

/// Today's UTC date, `YYYY-MM-DD`.
pub fn today() -> String {
    date_of(now_secs())
}

/// This UTC month, `YYYY-MM`.
pub fn year_month() -> String {
    let mut date = today();
    date.truncate(7);
    date
}

/// A matcher for the set-aside suffix `YYYYMMDDTHHMMSSZ` (Global Constraints, "Dates"). It
/// accepts a well-formed suffix whose date is today's or yesterday's UTC date when the matcher
/// runs, so a call that crossed midnight before the check still matches.
pub fn compact_now_pattern() -> impl Fn(&str) -> bool {
    |s: &str| {
        let b = s.as_bytes();
        let digits = |r: std::ops::Range<usize>| b[r].iter().all(u8::is_ascii_digit);
        if b.len() != 16 || b[8] != b'T' || b[15] != b'Z' || !digits(0..8) || !digits(9..15) {
            return false;
        }
        let two = |i: usize| u32::from(b[i] - b'0') * 10 + u32::from(b[i + 1] - b'0');
        if two(9) > 23 || two(11) > 59 || two(13) > 59 {
            return false;
        }
        let now = now_secs();
        let compact = |secs: i64| date_of(secs).replace('-', "");
        let date = &s[..8];
        date == compact(now) || date == compact(now - DAY)
    }
}

/// Unix seconds of an RFC 3339 UTC timestamp `YYYY-MM-DDTHH:MM:SS[.fraction]Z`. The fraction
/// is dropped.
///
/// # Panics
/// On anything else: another offset, a missing `Z`, a date that does not exist, or any other
/// shape.
pub fn parse_rfc3339(s: &str) -> i64 {
    try_parse_rfc3339(s).unwrap_or_else(|| panic!("not an RFC 3339 UTC timestamp: {s:?}"))
}

fn try_parse_rfc3339(s: &str) -> Option<i64> {
    let b = s.as_bytes();
    if b.len() < 20 || *b.last()? != b'Z' {
        return None;
    }
    let num = |r: std::ops::Range<usize>| -> Option<u32> {
        let part = s.get(r)?;
        if !part.bytes().all(|c| c.is_ascii_digit()) {
            return None;
        }
        part.parse().ok()
    };
    let shape = b[4] == b'-' && b[7] == b'-' && b[10] == b'T' && b[13] == b':' && b[16] == b':';
    if !shape {
        return None;
    }
    // After the seconds: `Z`, or `.` with at least one digit and then `Z`.
    let rest = &b[19..b.len() - 1];
    match rest.split_first() {
        None => {}
        Some((b'.', digits)) if !digits.is_empty() && digits.iter().all(u8::is_ascii_digit) => {}
        Some(_) => return None,
    }
    let year = i64::from(num(0..4)?);
    let (month, day) = (num(5..7)?, num(8..10)?);
    let (hour, minute, second) = (num(11..13)?, num(14..16)?, num(17..19)?);
    if !(1..=12).contains(&month)
        || day == 0
        || day > days_in_month(year, month)
        || hour > 23
        || minute > 59
        || second > 59
    {
        return None;
    }
    Some(
        days_from_civil(year, month, day) * DAY
            + i64::from(hour) * 3600
            + i64::from(minute) * 60
            + i64::from(second),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test] // spec: QA-031
    fn utc_helpers_agree_with_each_other() {
        let before = today();
        let month = year_month();
        let after = today();
        assert!(
            before.starts_with(&month) || after.starts_with(&month),
            "{before} / {after} / {month}"
        );
        assert_eq!(month.len(), 7, "{month}");
        assert_eq!(before.len(), 10, "{before}");

        assert_eq!(
            parse_rfc3339("2028-02-29T00:00:00Z") - parse_rfc3339("2028-01-30T00:00:00Z"),
            30 * 86_400
        );
        // Known values, worked out independently of this module's arithmetic.
        assert_eq!(parse_rfc3339("1970-01-01T00:00:00Z"), 0);
        assert_eq!(parse_rfc3339("2000-03-01T00:00:00Z"), 951_868_800);
        assert_eq!(parse_rfc3339("1999-12-31T23:59:59Z"), 946_684_799);
        assert_eq!(parse_rfc3339("2026-10-16T12:00:00.250Z"), 1_792_152_000);
        // `today()` is the date of now: its midnight parses and lies within a day of now.
        let midnight = parse_rfc3339(&format!("{}T00:00:00Z", today()));
        let now = now_secs();
        assert!(
            midnight <= now && now - midnight < 2 * 86_400,
            "{midnight} {now}"
        );

        let matches = compact_now_pattern();
        let suffix = format!("{}T120000Z", today().replace('-', ""));
        assert!(matches(&suffix), "{suffix}");
        assert!(!matches("20261016T1200Z"));
        assert!(!matches(&suffix.replace('T', "t")));
        assert!(!matches(&format!("{}T250000Z", today().replace('-', ""))));
        assert!(!matches("19991231T120000Z"));
    }

    #[test] // spec: QA-031
    fn parse_rfc3339_refuses_other_shapes() {
        for bad in [
            "2026-10-16T12:00:00+00:00",
            "2026-10-16 12:00:00Z",
            "2026-10-16T12:00:00",
            "2026-02-29T00:00:00Z",
            "2026-13-01T00:00:00Z",
            "2026-10-16T24:00:00Z",
            "2026-10-16T12:00:00.Z",
            "20261016T120000Z",
        ] {
            assert!(try_parse_rfc3339(bad).is_none(), "{bad}");
        }
        let panicked = std::panic::catch_unwind(|| parse_rfc3339("2026-10-16"));
        assert!(panicked.is_err());
    }

    #[test] // spec: QA-031
    fn civil_round_trips_across_four_centuries() {
        for days in (-200_000_i64..200_000).step_by(37) {
            let (y, m, d) = civil_from_days(days);
            assert!(d >= 1 && d <= days_in_month(y, m), "{days}: {y}-{m}-{d}");
            assert_eq!(days_from_civil(y, m, d), days, "{y}-{m}-{d}");
        }
    }
}
