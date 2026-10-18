//! UTC dates for manifests, logs and run ids (Appendix E "Dates"). Pure functions over Unix
//! seconds, proleptic Gregorian calendar, no time-zone data.

/// `(year, month 1–12, day 1–31)` of a day count since 1970-01-01 (Hinnant's `civil_from_days`).
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097); // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32; // [1, 12]
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// `(year, month, day, hour, minute, second)` in UTC.
fn parts(unix_secs: i64) -> (i64, u32, u32, u32, u32, u32) {
    let days = unix_secs.div_euclid(86_400);
    let secs = unix_secs.rem_euclid(86_400) as u32;
    let (y, m, d) = civil_from_days(days);
    (y, m, d, secs / 3600, secs / 60 % 60, secs % 60)
}

/// `YYYY-MM-DD`.
pub fn date(unix_secs: i64) -> String {
    let (y, m, d, ..) = parts(unix_secs);
    format!("{y:04}-{m:02}-{d:02}")
}

/// RFC 3339 `YYYY-MM-DDTHH:MM:SSZ`.
pub fn rfc3339(unix_secs: i64) -> String {
    let (y, m, d, h, mi, s) = parts(unix_secs);
    format!("{y:04}-{m:02}-{d:02}T{h:02}:{mi:02}:{s:02}Z")
}

/// `YYYY-MM`, the month of a log file name.
pub fn year_month(unix_secs: i64) -> String {
    let (y, m, ..) = parts(unix_secs);
    format!("{y:04}-{m:02}")
}

/// `YYYYMMDDTHHMMSSZ`, the set-aside and conflict suffix.
pub fn compact(unix_secs: i64) -> String {
    let (y, m, d, h, mi, s) = parts(unix_secs);
    format!("{y:04}{m:02}{d:02}T{h:02}{mi:02}{s:02}Z")
}

/// `r_<yyyymmdd>_<hhmm>`, a run id before any collision suffix.
pub fn run_id(unix_secs: i64) -> String {
    let (y, m, d, h, mi, _) = parts(unix_secs);
    format!("r_{y:04}{m:02}{d:02}_{h:02}{mi:02}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test] // spec: CORE-039
    fn utc_dates_cover_leap_years_and_month_ends() {
        assert_eq!(date(0), "1970-01-01");
        assert_eq!(rfc3339(0), "1970-01-01T00:00:00Z");
        // 2028-02-29T12:00:00Z (a leap day).
        assert_eq!(date(1_835_438_400), "2028-02-29");
        assert_eq!(date(1_835_438_400 + 86_400), "2028-03-01");
        assert_eq!(rfc3339(946_684_799), "1999-12-31T23:59:59Z");
        assert_eq!(rfc3339(946_684_800), "2000-01-01T00:00:00Z");
        // 2000 is a leap year, 2100 is not.
        assert_eq!(date(951_782_400), "2000-02-29");
        assert_eq!(date(4_107_542_400), "2100-03-01");
        assert_eq!(date(4_107_542_400 - 86_400), "2100-02-28");
        // Month ends.
        assert_eq!(date(1_790_812_799), "2026-09-30");
        assert_eq!(date(1_790_812_800), "2026-10-01");
        assert_eq!(year_month(1_790_812_799), "2026-09");
        assert_eq!(year_month(1_790_812_800), "2026-10");
        assert_eq!(compact(946_684_799), "19991231T235959Z");
        // Before 1970.
        assert_eq!(rfc3339(-1), "1969-12-31T23:59:59Z");
    }

    #[test] // spec: ARCH-042
    fn run_ids_use_utc_minutes() {
        // 2026-10-03T14:00:59Z
        assert_eq!(run_id(1_791_036_059), "r_20261003_1400");
        assert_eq!(run_id(1_791_036_060), "r_20261003_1401");
        assert_eq!(run_id(946_684_799), "r_19991231_2359");
        assert!(crate::refs::is_run_id(&run_id(1_791_036_059)));
    }
}
