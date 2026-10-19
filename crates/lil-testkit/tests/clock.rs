//! `FakeClock`'s scripted local offsets (CORE-012, QA-031).

use std::time::Duration;

use lil_platform::Clock;
use lil_testkit::clock::FakeClock;

#[test] // spec: CORE-012
fn fake_clock_offsets_follow_the_script() {
    let clock = FakeClock::new();
    let t0 = clock.utc_now();
    let t1 = t0 + Duration::from_secs(86_400);
    let second = Duration::from_secs(1);
    clock.set_offsets(&[(t0, 3600), (t1, 7200)]);

    assert_eq!(clock.local_offset_secs(t0), 3600);
    assert_eq!(clock.local_offset_secs(t1 - second), 3600);
    assert_eq!(clock.local_offset_secs(t1), 7200);
    assert_eq!(clock.local_offset_secs(t1 + 365 * 86_400 * second), 7200);
    // Before the first entry, the first entry's offset.
    assert_eq!(clock.local_offset_secs(t0 - 86_400 * second), 3600);

    // A new script replaces the old one, whatever order it is given in.
    clock.set_offsets(&[(t1, -18_000), (t0, 0)]);
    assert_eq!(clock.local_offset_secs(t0), 0);
    assert_eq!(clock.local_offset_secs(t1), -18_000);
}
