//! The Hardware panel's series and energy periods (APP-129, APP-131; P4a-R17), Qt-free.
//!
//! - [`resolution_for`]: the `telemetry.series` resolution of a chart window: `1s` up to an
//!   hour, `1m` beyond (the panel's longest window is 24 hours).
//! - [`downsample`]: a series cut to at most one point per pixel, each keeping its pixel's least
//!   min and greatest max, so a one-second spike survives a 24-hour window.
//! - [`SeriesBuffer`]: one chart's points: a `telemetry.series` answer, then the live samples of
//!   the `telemetry` topic, merged into the window's resolution step and cut to the window. The
//!   window ends at the newest sample of any chart (`set_end`), so a series that stopped leaves
//!   the right of its chart empty and says it is stale; a hole in a series breaks its line.
//! - [`energy_periods`]: the energy table's session, day, month and year, by the local calendar.

/// One point of a `telemetry.series` answer: `(Unix ms, min, average, max)`.
pub type SeriesPoint = (i64, f64, f64, f64);

/// The window the Hardware panel opens on, in seconds (APP-129).
pub const LIVE_WINDOW_S: u64 = 15 * 60;
/// The longest window, in seconds.
pub const LONGEST_WINDOW_S: u64 = 24 * 3600;

const DAY_S: i64 = 86_400;

/// The `telemetry.series` resolution of a chart window of `window_s` seconds: `1s` up to an
/// hour, else `1m`.
pub fn resolution_for(window_s: u64) -> &'static str {
    if window_s <= 3600 { "1s" } else { "1m" }
}

/// The step of [`resolution_for`] in ms.
fn step_ms(window_s: u64) -> i64 {
    if resolution_for(window_s) == "1s" {
        1000
    } else {
        60_000
    }
}

/// `points` (oldest first) cut to at most `width_px` points: their time span split into
/// `width_px` equal pixels, each non-empty one giving `(its first point's time, the least min,
/// the greatest max)`. With no more points than pixels, each point as it is.
pub fn downsample(points: &[SeriesPoint], width_px: usize) -> Vec<(i64, f64, f64)> {
    if width_px == 0 {
        return Vec::new();
    }
    if points.len() <= width_px {
        return points
            .iter()
            .map(|&(ts, min, _, max)| (ts, min, max))
            .collect();
    }
    let t0 = i128::from(points[0].0);
    let span = (i128::from(points[points.len() - 1].0) - t0 + 1).max(1);
    let width = width_px as i128;
    let mut out: Vec<(i64, f64, f64)> = Vec::with_capacity(width_px);
    let mut current: Option<i128> = None;
    for &(ts, min, _, max) in points {
        let pixel = ((i128::from(ts) - t0).max(0) * width / span).min(width - 1);
        match out.last_mut() {
            Some(last) if current == Some(pixel) => {
                last.1 = last.1.min(min);
                last.2 = last.2.max(max);
            }
            _ => {
                out.push((ts, min, max));
                current = Some(pixel);
            }
        }
    }
    out
}

/// One chart's points, oldest first, for a window of `window_s` seconds that ends at the newest
/// point or at the end set, whichever is later.
#[derive(Debug, Clone, PartialEq)]
pub struct SeriesBuffer {
    window_s: u64,
    points: Vec<SeriesPoint>,
    end_ms: Option<i64>,
}

impl SeriesBuffer {
    pub fn new(window_s: u64) -> SeriesBuffer {
        SeriesBuffer {
            window_s: window_s.clamp(1, LONGEST_WINDOW_S),
            points: Vec::new(),
            end_ms: None,
        }
    }

    pub fn window_s(&self) -> u64 {
        self.window_s
    }

    pub fn points(&self) -> &[SeriesPoint] {
        &self.points
    }

    /// A new window: the points go, and the panel loads the window's series.
    pub fn set_window(&mut self, window_s: u64) {
        *self = SeriesBuffer::new(window_s);
    }

    /// A `telemetry.series` answer replaces the points, but for live samples held already that
    /// are newer than its last point: the answer may land after them.
    pub fn load(&mut self, mut loaded: Vec<SeriesPoint>) {
        loaded.sort_by_key(|p| p.0);
        let last = loaded.last().map_or(i64::MIN, |p| p.0);
        let step = step_ms(self.window_s);
        let newer: Vec<SeriesPoint> = self.points.iter().copied().filter(|p| p.0 > last).collect();
        self.points = loaded;
        for (ts, min, avg, max) in newer {
            self.merge((ts, min, avg, max), step);
        }
        self.trim();
    }

    /// A live sample: merged into the newest point when it falls in the same resolution step,
    /// appended when newer, and dropped when older (events can overtake one another).
    pub fn push(&mut self, ts: i64, value: f64) {
        if !value.is_finite() {
            return;
        }
        self.merge((ts, value, value, value), step_ms(self.window_s));
        self.trim();
    }

    fn merge(&mut self, (ts, min, avg, max): SeriesPoint, step: i64) {
        match self.points.last_mut() {
            Some(last) if last.0.div_euclid(step) == ts.div_euclid(step) => {
                last.1 = last.1.min(min);
                last.2 = avg;
                last.3 = last.3.max(max);
            }
            Some(last) if ts < last.0 => {}
            _ => self.points.push((ts, min, avg, max)),
        }
    }

    /// The window's end: the time of the newest sample the panel has (of any metric). A later
    /// point of this series moves it on by itself; an earlier end changes nothing.
    pub fn set_end(&mut self, end_ms: i64) {
        self.end_ms = Some(self.end_ms.map_or(end_ms, |e| e.max(end_ms)));
        self.trim();
    }

    /// The newest point's time.
    pub fn newest_ms(&self) -> Option<i64> {
        self.points.last().map(|p| p.0)
    }

    fn end(&self) -> Option<i64> {
        match (self.newest_ms(), self.end_ms) {
            (Some(n), Some(e)) => Some(n.max(e)),
            (n, e) => n.or(e),
        }
    }

    /// The series has stopped: its newest point is more than two steps before the window's end.
    pub fn is_stale(&self) -> bool {
        match (self.newest_ms(), self.end()) {
            (Some(newest), Some(end)) => end - newest > 2 * step_ms(self.window_s),
            _ => false,
        }
    }

    fn trim(&mut self) {
        let Some(end) = self.end() else {
            return;
        };
        let start = end - self.window_ms();
        let keep_from = self.points.partition_point(|p| p.0 < start);
        self.points.drain(..keep_from);
    }

    fn window_ms(&self) -> i64 {
        i64::try_from(self.window_s).map_or(i64::MAX / 1000, |s| s * 1000)
    }

    /// The newest point's average: the latest value.
    pub fn latest(&self) -> Option<f64> {
        self.points.last().map(|p| p.2)
    }

    /// The greatest max in the window (0 for none): a chart's scale.
    pub fn peak(&self) -> f64 {
        self.points.iter().map(|p| p.3).fold(0.0, f64::max)
    }

    /// What a plot `width_px` pixels wide draws: at most `width_px` points `(x in pixels from
    /// the window's start, min, max)` in all, the window's end at `width_px`, as lines broken
    /// wherever two neighbours are further apart than two steps and two pixels (no data was
    /// stored between them: the core was stopped, the PC slept, a source failed).
    pub fn view(&self, width_px: usize) -> Vec<Vec<(f64, f64, f64)>> {
        let Some(end) = self.end() else {
            return Vec::new();
        };
        if width_px == 0 {
            return Vec::new();
        }
        let window_ms = self.window_ms();
        let window = window_ms as f64;
        let start = end - window_ms;
        let pixel_ms = window_ms / width_px as i64;
        let gap = (2 * step_ms(self.window_s)).max(2 * pixel_ms);
        let mut lines: Vec<Vec<(f64, f64, f64)>> = Vec::new();
        let mut before: Option<i64> = None;
        for (ts, min, max) in downsample(&self.points, width_px) {
            let point = ((ts - start) as f64 * width_px as f64 / window, min, max);
            match (before, lines.last_mut()) {
                (Some(b), Some(line)) if ts - b <= gap => line.push(point),
                _ => lines.push(vec![point]),
            }
            before = Some(ts);
        }
        lines
    }
}

/// The points of a `telemetry.series` metric, `[[ts, min, average, max], ...]` with RFC 3339
/// UTC times; a point that doesn't read is skipped.
pub fn parse_points(points: &serde_json::Value) -> Vec<SeriesPoint> {
    let Some(rows) = points.as_array() else {
        return Vec::new();
    };
    rows.iter()
        .filter_map(|row| {
            let row = row.as_array()?;
            let ts = parse_utc_ms(row.first()?.as_str()?)?;
            let n = |i: usize| row.get(i).and_then(serde_json::Value::as_f64);
            Some((ts, n(1)?, n(2)?, n(3)?))
        })
        .collect()
}

/// Unix ms of an RFC 3339 UTC time `YYYY-MM-DDTHH:MM:SS[.fraction]Z`, the telemetry tools'
/// form.
pub fn parse_utc_ms(text: &str) -> Option<i64> {
    let b = text.as_bytes();
    if b.len() < 20
        || b[4] != b'-'
        || b[7] != b'-'
        || b[10] != b'T'
        || b[13] != b':'
        || b[16] != b':'
        || b[b.len() - 1] != b'Z'
    {
        return None;
    }
    let num = |from: usize, to: usize| -> Option<i64> {
        let s = text.get(from..to)?;
        if s.bytes().all(|c| c.is_ascii_digit()) {
            s.parse().ok()
        } else {
            None
        }
    };
    let (y, mo, d) = (num(0, 4)?, num(5, 7)?, num(8, 10)?);
    let (h, mi, s) = (num(11, 13)?, num(14, 16)?, num(17, 19)?);
    if !(1..=12).contains(&mo) || !(1..=31).contains(&d) || h > 23 || mi > 59 || s > 60 {
        return None;
    }
    let millis = match text.get(19..text.len() - 1)? {
        "" => 0,
        frac => {
            let digits = frac.strip_prefix('.')?;
            if digits.is_empty() || !digits.bytes().all(|c| c.is_ascii_digit()) {
                return None;
            }
            let three: String = digits.chars().chain("000".chars()).take(3).collect();
            three.parse().ok()?
        }
    };
    let days = days_from_civil(y, mo as u32, d as u32);
    Some(((days * 24 + h) * 60 + mi) * 60_000 + s * 1000 + millis)
}

/// Days since 1970-01-01 of a proleptic Gregorian date (Hinnant's `days_from_civil`).
fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = i64::from((m + 9) % 12);
    let doy = (153 * mp + 2) / 5 + i64::from(d) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// `(year, month)` of a day count since 1970-01-01 (Hinnant's `civil_from_days`).
fn year_month_of(days: i64) -> (i64, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = yoe + era * 400;
    (if m <= 2 { y + 1 } else { y }, m)
}

/// The energy table's periods (APP-131): `("session", the session's start, now)`, then the
/// local day, month and year that hold `now`, each from its local midnight (in UTC ms) to now.
/// `offset_at(utc_ms)` is local time minus UTC in seconds at that instant, daylight saving time
/// included; a midnight takes the offset that held at it, not now's, so a day or month that
/// began before a daylight-saving change began an hour apart. A session start after now is now.
pub fn energy_periods(
    now_utc_ms: i64,
    offset_at: &dyn Fn(i64) -> i64,
    session_start_utc_ms: i64,
) -> [(&'static str, i64, i64); 4] {
    let off = offset_at(now_utc_ms);
    let local_day = (now_utc_ms.div_euclid(1000) + off).div_euclid(DAY_S);
    let (y, m) = year_month_of(local_day);
    let midnight = |day: i64| {
        let local_s = day * DAY_S;
        let at_midnight = offset_at((local_s - off) * 1000);
        ((local_s - at_midnight) * 1000).min(now_utc_ms)
    };
    [
        ("session", session_start_utc_ms.min(now_utc_ms), now_utc_ms),
        ("day", midnight(local_day), now_utc_ms),
        ("month", midnight(days_from_civil(y, m, 1)), now_utc_ms),
        ("year", midnight(days_from_civil(y, 1, 1)), now_utc_ms),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Unix ms of a UTC date and time.
    fn utc(y: i64, mo: u32, d: u32, h: i64, mi: i64) -> i64 {
        // Days from 1970-01-01 (Hinnant's days_from_civil).
        let y = if mo <= 2 { y - 1 } else { y };
        let era = y.div_euclid(400);
        let yoe = y - era * 400;
        let mp = i64::from((mo + 9) % 12);
        let doy = (153 * mp + 2) / 5 + i64::from(d) - 1;
        let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
        let days = era * 146_097 + doe - 719_468;
        ((days * 24 + h) * 60 + mi) * 60_000
    }

    mod resolution {
        use super::super::*;

        #[test] // spec: APP-129
        fn fifteen_minutes_use_1s_and_24_hours_use_1m() {
            assert_eq!(resolution_for(15 * 60), "1s");
            assert_eq!(resolution_for(3600), "1s");
            assert_eq!(resolution_for(3601), "1m");
            assert_eq!(resolution_for(6 * 3600), "1m");
            assert_eq!(resolution_for(24 * 3600), "1m");
        }
    }

    #[test] // spec: APP-129
    fn downsampling_keeps_min_and_max_per_pixel() {
        // An hour of 1-second points, flat at 50 but for a spike to 97 and a dip to 3.
        let points: Vec<SeriesPoint> = (0..3600)
            .map(|i| {
                let ts = 1_000_000 + i * 1000;
                match i {
                    1234 => (ts, 50.0, 70.0, 97.0),
                    2500 => (ts, 3.0, 20.0, 50.0),
                    _ => (ts, 49.0, 50.0, 51.0),
                }
            })
            .collect();
        let out = downsample(&points, 300);
        assert!(!out.is_empty() && out.len() <= 300, "{} points", out.len());
        // Each output point is a pixel: 12 seconds here, in time order, none dropped.
        assert_eq!(out.len(), 300);
        assert!(out.windows(2).all(|w| w[0].0 < w[1].0));
        assert_eq!(out[0].0, 1_000_000);
        let max = out.iter().map(|p| p.2).fold(f64::MIN, f64::max);
        let min = out.iter().map(|p| p.1).fold(f64::MAX, f64::min);
        assert_eq!(max, 97.0, "the spike's max is kept");
        assert_eq!(min, 3.0, "the dip's min is kept");
        // The spike's pixel holds it; its neighbours stay at the flat line.
        let spike = out.iter().position(|p| p.2 == 97.0).unwrap();
        assert_eq!(spike, 1234 / 12);
        assert_eq!(out[spike - 1].2, 51.0);
        assert_eq!(out[spike + 1].2, 51.0);
        assert_eq!(out[spike].1, 49.0, "a pixel's min is its points' least min");

        // Fewer points than pixels: every one, as it is.
        let few = &points[..10];
        let out = downsample(few, 300);
        assert_eq!(out.len(), 10);
        assert_eq!(out[3], (few[3].0, few[3].1, few[3].3));
        assert!(downsample(&points, 0).is_empty());
        assert!(downsample(&[], 300).is_empty());
    }

    #[test] // spec: APP-129
    fn the_buffer_merges_live_samples_into_its_step_and_keeps_its_window() {
        // 15 minutes at 1 s: an answer, then live samples.
        let mut b = SeriesBuffer::new(LIVE_WINDOW_S);
        let t = utc(2026, 10, 28, 12, 0);
        b.load((0..900).map(|i| (t + i * 1000, 1.0, 1.0, 1.0)).collect());
        assert_eq!(b.points().len(), 900);
        b.push(t + 900_000, 5.0);
        b.push(t + 900_400, 7.0); // the same second: merged
        b.push(t + 899_000, 9.0); // older than the newest: dropped
        assert_eq!(
            b.points().len(),
            901,
            "the first point is exactly 15 minutes old"
        );
        assert_eq!(*b.points().last().unwrap(), (t + 900_000, 5.0, 7.0, 7.0));
        assert_eq!(b.latest(), Some(7.0));
        assert_eq!(b.peak(), 7.0);
        b.push(t + 901_000, 2.0);
        assert_eq!(b.points()[0].0, t + 1000, "older than the window goes");

        // The view: at most a point per pixel, the newest pixel at the right edge.
        let v = b.view(300).concat();
        assert!(v.len() <= 300);
        let right = v.last().unwrap().0;
        assert!(right > 299.0 && right <= 300.0, "{right}");
        assert!(v[0].0 >= 0.0);
        assert_eq!(
            b.view(300).concat().iter().map(|p| p.2).fold(0.0, f64::max),
            7.0
        );

        // 24 hours at 1 m: live seconds merge into their minute.
        b.set_window(LONGEST_WINDOW_S);
        assert!(b.points().is_empty());
        let m = utc(2026, 10, 28, 12, 0);
        b.load(vec![(m - 60_000, 10.0, 20.0, 30.0), (m, 40.0, 45.0, 50.0)]);
        b.push(m + 1000, 60.0);
        b.push(m + 59_000, 35.0);
        assert_eq!(b.points().len(), 2);
        assert_eq!(b.points()[1], (m, 35.0, 35.0, 60.0));
        b.push(m + 60_000, 1.0);
        assert_eq!(b.points().len(), 3);

        // An answer that lands after live samples keeps the newer ones.
        let mut b = SeriesBuffer::new(LIVE_WINDOW_S);
        b.push(t + 10_000, 4.0);
        b.load(vec![(t + 8000, 1.0, 1.0, 1.0), (t + 9000, 2.0, 2.0, 2.0)]);
        assert_eq!(
            b.points(),
            [
                (t + 8000, 1.0, 1.0, 1.0),
                (t + 9000, 2.0, 2.0, 2.0),
                (t + 10_000, 4.0, 4.0, 4.0)
            ]
        );
        b.push(t + 11_000, f64::NAN);
        assert_eq!(
            b.points().len(),
            3,
            "a value that isn't a number is skipped"
        );
    }

    #[test] // spec: APP-129
    fn a_gap_breaks_the_line_and_a_stopped_series_shows_it() {
        let t = utc(2026, 10, 28, 12, 0);
        let p = |ts: i64, v: f64| (ts, v, v, v);
        // 3 minutes, a 10-minute hole (the core was stopped), then 2 minutes.
        let mut b = SeriesBuffer::new(LIVE_WINDOW_S);
        let mut points: Vec<SeriesPoint> = (0..180).map(|i| p(t + i * 1000, 1.0)).collect();
        points.extend((780..900).map(|i| p(t + i * 1000, 2.0)));
        b.load(points);
        let segments = b.view(300);
        assert_eq!(segments.len(), 2, "the hole breaks the line");
        assert!(segments[0].last().unwrap().0 < segments[1][0].0);
        assert!(segments.iter().map(Vec::len).sum::<usize>() <= 300);
        assert!(!b.is_stale());

        // A sample every second is one line; so is 24 hours of minutes, a pixel being 6 minutes.
        let mut c = SeriesBuffer::new(LIVE_WINDOW_S);
        c.load((0..900).map(|i| p(t + i * 1000, 1.0)).collect());
        assert_eq!(c.view(300).len(), 1);
        let mut d = SeriesBuffer::new(LONGEST_WINDOW_S);
        d.load((0..1440).map(|i| p(t + i * 60_000, 1.0)).collect());
        assert_eq!(d.view(224).len(), 1);

        // A series that stopped while the others went on: the end moves with them, so its right
        // edge is empty and it says it is stale.
        let last = t + 899_000;
        b.set_end(last + 60_000);
        assert!(b.is_stale());
        assert_eq!(b.newest_ms(), Some(last));
        let right = b.view(300).last().unwrap().last().unwrap().0;
        assert!(right < 300.0 - 15.0, "a minute of 15 is 20 px: {right}");
        // The window follows the end: 13 minutes on, the first stretch has gone.
        b.set_end(last + 13 * 60_000);
        assert_eq!(b.view(300).len(), 1);
        // Data again at the end: current.
        b.push(last + 13 * 60_000, 3.0);
        assert!(!b.is_stale());
        assert_eq!(b.view(300).len(), 2, "the stop is a hole now");
        // An end before the newest point changes nothing.
        b.set_end(t);
        assert!(!b.is_stale());
    }

    #[test] // spec: APP-129
    fn series_points_read_from_the_tools_answer() {
        let t = utc(2026, 10, 18, 12, 0);
        assert_eq!(parse_utc_ms("2026-10-18T12:00:00Z"), Some(t));
        assert_eq!(parse_utc_ms("2026-10-18T12:00:00.123Z"), Some(t + 123));
        assert_eq!(parse_utc_ms("2026-10-18T12:00:00.5Z"), Some(t + 500));
        assert_eq!(parse_utc_ms("2026-10-18T12:00:01.999999Z"), Some(t + 1999));
        for bad in [
            "",
            "2026-10-18 12:00:00Z",
            "2026-10-18T12:00:00",
            "2026-10-18T12:00:00+02:00",
            "2026-13-18T12:00:00Z",
            "2026-10-18T12:00:00.Z",
            "2026-1x-18T12:00:00Z",
        ] {
            assert_eq!(parse_utc_ms(bad), None, "{bad}");
        }
        let points = serde_json::json!([
            ["2026-10-18T12:00:00.000Z", 1, 2.5, 4],
            ["not a time", 1, 2, 3],
            ["2026-10-18T12:00:01.000Z", 5, 5, 5],
            ["2026-10-18T12:00:02.000Z", null, 1, 1]
        ]);
        assert_eq!(
            parse_points(&points),
            [(t, 1.0, 2.5, 4.0), (t + 1000, 5.0, 5.0, 5.0)]
        );
        assert!(parse_points(&serde_json::json!({})).is_empty());
    }

    #[test] // spec: APP-131
    fn energy_periods_follow_the_local_calendar() {
        // Central European time: +1 h, +2 h in summer (2026-03-29 01:00 UTC to 2026-10-25
        // 01:00 UTC).
        let cet = |ms: i64| -> i64 {
            if (utc(2026, 3, 29, 1, 0)..utc(2026, 10, 25, 1, 0)).contains(&ms) {
                7200
            } else {
                3600
            }
        };
        let session = utc(2026, 3, 29, 8, 0);

        // The day daylight saving time starts: midnight was still +1 h.
        let now = utc(2026, 3, 29, 12, 0);
        assert_eq!(
            energy_periods(now, &cet, session),
            [
                ("session", session, now),
                ("day", utc(2026, 3, 28, 23, 0), now),
                ("month", utc(2026, 2, 28, 23, 0), now),
                ("year", utc(2025, 12, 31, 23, 0), now),
            ]
        );

        // The day it ends: midnight was +2 h, and so was the 1st of the month.
        let now = utc(2026, 10, 25, 12, 0);
        let p = energy_periods(now, &cet, session);
        assert_eq!(p[1], ("day", utc(2026, 10, 24, 22, 0), now));
        assert_eq!(p[2], ("month", utc(2026, 9, 30, 22, 0), now));
        assert_eq!(p[3], ("year", utc(2025, 12, 31, 23, 0), now));

        // Just after local midnight, the local day is already the new one; just before, not.
        let now = utc(2026, 1, 14, 23, 30); // 00:30 on the 15th, local
        assert_eq!(
            energy_periods(now, &cet, session)[1].1,
            utc(2026, 1, 14, 23, 0)
        );
        let now = utc(2026, 1, 14, 22, 30); // 23:30 on the 14th, local
        assert_eq!(
            energy_periods(now, &cet, session)[1].1,
            utc(2026, 1, 13, 23, 0)
        );

        // West of UTC (-5 h): 22:00 on New Year's Eve, local, is still last year.
        let est = |_: i64| -> i64 { -5 * 3600 };
        let now = utc(2026, 1, 1, 3, 0);
        let p = energy_periods(now, &est, utc(2025, 12, 31, 20, 0));
        assert_eq!(p[0], ("session", utc(2025, 12, 31, 20, 0), now));
        assert_eq!(p[1], ("day", utc(2025, 12, 31, 5, 0), now));
        assert_eq!(p[2], ("month", utc(2025, 12, 1, 5, 0), now));
        assert_eq!(p[3], ("year", utc(2025, 1, 1, 5, 0), now));

        // UTC, and a session that would start after now starts now.
        let zero = |_: i64| -> i64 { 0 };
        let now = utc(2026, 7, 4, 10, 15);
        let p = energy_periods(now, &zero, now + 5000);
        assert_eq!(p[0], ("session", now, now));
        assert_eq!(p[1].1, utc(2026, 7, 4, 0, 0));
        assert_eq!(p[2].1, utc(2026, 7, 1, 0, 0));
        assert_eq!(p[3].1, utc(2026, 1, 1, 0, 0));
    }
}
