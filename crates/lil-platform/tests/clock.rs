//! The local time offset (CORE-012: the cap period is the local calendar month). Checked
//! against the time zone's raw fields from `GetTimeZoneInformation`, not its conversion routine.

use std::time::{Duration, UNIX_EPOCH};

use lil_platform::{Clock, SystemClock};
use windows::Win32::System::Time::{GetTimeZoneInformation, TIME_ZONE_INFORMATION};

/// 2026-01-15T12:00:00Z and 2026-07-15T12:00:00Z.
const JANUARY: u64 = 1_768_478_400;
const JULY: u64 = 1_784_116_800;

#[test] // spec: CORE-012
fn local_offset_follows_dst() {
    let clock = SystemClock::new();
    let at = |secs: u64| UNIX_EPOCH + Duration::from_secs(secs);
    let january = clock.local_offset_secs(at(JANUARY));
    let july = clock.local_offset_secs(at(JULY));
    for offset in [january, july] {
        assert_eq!(offset % 900, 0, "{offset} is a multiple of 15 minutes");
        assert!(offset.abs() <= 50_400, "{offset} is within 14 hours");
    }

    let mut tz = TIME_ZONE_INFORMATION::default();
    // SAFETY: `tz` is a valid out-pointer for the call.
    let rc = unsafe { GetTimeZoneInformation(&mut tz) };
    assert_ne!(rc, u32::MAX, "GetTimeZoneInformation failed");
    let standard = -(tz.Bias + tz.StandardBias) * 60;
    if tz.DaylightDate.wMonth != 0 {
        assert_eq!(
            (january - july).abs(),
            ((tz.StandardBias - tz.DaylightBias) * 60).abs(),
            "january {january}, july {july}"
        );
    } else {
        assert_eq!(january, july);
    }
    assert!(
        january == standard || july == standard,
        "one of {january} and {july} is the standard offset {standard}"
    );
}
