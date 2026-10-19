//! Time behind a trait so tests never sleep (QA-031). `lil-testkit` supplies `FakeClock`.

use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use windows::Win32::Foundation::{FILETIME, SYSTEMTIME};
use windows::Win32::System::Time::{
    DYNAMIC_TIME_ZONE_INFORMATION, FileTimeToSystemTime, GetDynamicTimeZoneInformation,
    SystemTimeToFileTime, SystemTimeToTzSpecificLocalTimeEx, TIME_ZONE_ID_INVALID,
};

/// The two clocks the core reads. Monotonic time is for durations and deadlines (event
/// history, timeouts); UTC time is for stamps people read.
pub trait Clock: Send + Sync + std::fmt::Debug {
    /// Time since an arbitrary fixed origin. Never decreases.
    fn monotonic(&self) -> Duration;
    /// The wall clock.
    fn utc_now(&self) -> SystemTime;
    /// Local time minus UTC at the instant `utc`, in seconds, daylight saving time included
    /// (the local calendar month of CORE-012).
    fn local_offset_secs(&self, utc: SystemTime) -> i32;
}

/// The real clocks. `monotonic` is the time since construction.
#[derive(Debug, Clone, Copy)]
pub struct SystemClock {
    start: Instant,
}

impl SystemClock {
    pub fn new() -> SystemClock {
        SystemClock {
            start: Instant::now(),
        }
    }
}

impl Default for SystemClock {
    fn default() -> Self {
        SystemClock::new()
    }
}

impl Clock for SystemClock {
    fn monotonic(&self) -> Duration {
        self.start.elapsed()
    }

    fn utc_now(&self) -> SystemTime {
        SystemTime::now()
    }

    /// Through the Windows time-zone API with the current dynamic time zone, so each year's
    /// daylight rules apply. If the API fails (an instant outside the years `SYSTEMTIME` can
    /// hold, or no time zone), the offset is 0: UTC.
    fn local_offset_secs(&self, utc: SystemTime) -> i32 {
        system_local_offset(utc).unwrap_or(0)
    }
}

/// 100-ns intervals between 1601-01-01 (the `FILETIME` origin) and 1970-01-01.
const FILETIME_UNIX_EPOCH: i128 = 116_444_736_000_000_000;

fn to_filetime(t: SystemTime) -> Option<FILETIME> {
    let ticks = match t.duration_since(UNIX_EPOCH) {
        Ok(d) => FILETIME_UNIX_EPOCH + (d.as_nanos() / 100) as i128,
        Err(e) => FILETIME_UNIX_EPOCH - (e.duration().as_nanos() / 100) as i128,
    };
    let ticks = u64::try_from(ticks).ok()?;
    Some(FILETIME {
        dwLowDateTime: ticks as u32,
        dwHighDateTime: (ticks >> 32) as u32,
    })
}

fn ticks(ft: &FILETIME) -> i64 {
    ((u64::from(ft.dwHighDateTime) << 32) | u64::from(ft.dwLowDateTime)) as i64
}

fn system_local_offset(utc: SystemTime) -> Option<i32> {
    let ft = to_filetime(utc)?;
    let mut tz = DYNAMIC_TIME_ZONE_INFORMATION::default();
    // SAFETY: `tz` is a valid out-pointer for the call.
    if unsafe { GetDynamicTimeZoneInformation(&mut tz) } == TIME_ZONE_ID_INVALID {
        return None;
    }
    let mut utc_st = SYSTEMTIME::default();
    let mut local_st = SYSTEMTIME::default();
    let (mut utc_ft, mut local_ft) = (FILETIME::default(), FILETIME::default());
    // SAFETY: every pointer is to a live local of the type the call expects.
    unsafe {
        FileTimeToSystemTime(&ft, &mut utc_st).ok()?;
        SystemTimeToTzSpecificLocalTimeEx(Some(&tz), &utc_st, &mut local_st).ok()?;
        // Both sides go through SYSTEMTIME, which keeps only milliseconds, so the difference
        // is exact.
        SystemTimeToFileTime(&utc_st, &mut utc_ft).ok()?;
        SystemTimeToFileTime(&local_st, &mut local_ft).ok()?;
    }
    i32::try_from((ticks(&local_ft) - ticks(&utc_ft)) / 10_000_000).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::UNIX_EPOCH;

    #[test] // spec: QA-031
    fn system_clock_monotonic_never_decreases() {
        let clock = SystemClock::new();
        let mut last = clock.monotonic();
        for _ in 0..1000 {
            let now = clock.monotonic();
            assert!(now >= last, "{now:?} < {last:?}");
            last = now;
        }
        assert!(clock.utc_now() > UNIX_EPOCH);
    }
}
