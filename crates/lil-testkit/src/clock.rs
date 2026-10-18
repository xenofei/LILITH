//! `FakeClock`: a [`Clock`] that moves only when a test says so (QA-031).

use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use lil_platform::Clock;

/// A clock that stands still until [`FakeClock::advance`] or [`FakeClock::shift_utc`] moves
/// it. Monotonic time starts at zero; UTC starts at a fixed instant (2027-01-15T08:00:00Z), so
/// runs are reproducible.
#[derive(Debug)]
pub struct FakeClock {
    state: Mutex<State>,
}

#[derive(Debug, Clone, Copy)]
struct State {
    monotonic: Duration,
    utc: SystemTime,
}

/// The fixed UTC start: 2027-01-15T08:00:00Z.
const UTC_START_SECS: u64 = 1_800_000_000;

impl FakeClock {
    pub fn new() -> Arc<FakeClock> {
        Arc::new(FakeClock {
            state: Mutex::new(State {
                monotonic: Duration::ZERO,
                utc: UNIX_EPOCH + Duration::from_secs(UTC_START_SECS),
            }),
        })
    }

    /// Moves both clocks forward by `d`.
    pub fn advance(&self, d: Duration) {
        let mut s = self.lock();
        s.monotonic += d;
        s.utc += d;
    }

    /// Moves only the wall clock, by `by` seconds (negative moves it back), as a user changing
    /// the system time would. Monotonic time is unaffected.
    pub fn shift_utc(&self, by: i64) {
        let mut s = self.lock();
        let d = Duration::from_secs(by.unsigned_abs());
        s.utc = if by >= 0 { s.utc + d } else { s.utc - d };
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        // A test that panicked while holding the lock leaves a usable value.
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }
}

impl Clock for FakeClock {
    fn monotonic(&self) -> Duration {
        self.lock().monotonic
    }

    fn utc_now(&self) -> SystemTime {
        self.lock().utc
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lil_platform::Clock;
    use std::time::Duration;

    #[test] // spec: QA-031
    fn fake_clock_advances_only_when_told() {
        let clock = FakeClock::new();
        let (m0, u0) = (clock.monotonic(), clock.utc_now());
        for _ in 0..1000 {
            assert_eq!(clock.monotonic(), m0);
            assert_eq!(clock.utc_now(), u0);
        }
        clock.advance(Duration::from_millis(1500));
        assert_eq!(clock.monotonic(), m0 + Duration::from_millis(1500));
        assert_eq!(clock.utc_now(), u0 + Duration::from_millis(1500));
        clock.shift_utc(-3600);
        assert_eq!(clock.monotonic(), m0 + Duration::from_millis(1500));
        assert_eq!(
            clock.utc_now(),
            u0 + Duration::from_millis(1500) - Duration::from_secs(3600)
        );
        clock.shift_utc(60);
        assert_eq!(clock.monotonic(), m0 + Duration::from_millis(1500));
        assert_eq!(
            clock.utc_now(),
            u0 + Duration::from_millis(1500) - Duration::from_secs(3540)
        );
    }
}
