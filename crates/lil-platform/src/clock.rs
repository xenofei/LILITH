//! Time behind a trait so tests never sleep (QA-031). `lil-testkit` supplies `FakeClock`.

use std::time::{Duration, Instant, SystemTime};

/// The two clocks the core reads. Monotonic time is for durations and deadlines (event
/// history, timeouts); UTC time is for stamps people read.
pub trait Clock: Send + Sync + std::fmt::Debug {
    /// Time since an arbitrary fixed origin. Never decreases.
    fn monotonic(&self) -> Duration;
    /// The wall clock.
    fn utc_now(&self) -> SystemTime;
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
