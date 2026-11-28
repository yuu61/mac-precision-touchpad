//! Precision Touchpad Scan Time in 100 µs units.

use core::sync::atomic::{AtomicI64, Ordering};

const NO_ORIGIN: i64 = -1;
const UNITS_PER_SECOND: u128 = 10_000;

/// A per-device clock for the current input-reporting session.
///
/// The first report establishes the origin. All frames use the same origin until the driver
/// restarts reporting. The 16-bit value wraps every 6.5536 seconds.
///
/// [`Self::restart`] and [`Self::sample`] may run concurrently on different processors (a PnP or
/// timer callback restarts the session while a read completion samples it). The frequency is
/// published before the session reset: `restart` stores it and then resets the origin with
/// `Release`, and `sample` loads the origin with `Acquire` before it loads the frequency. A sample
/// that sees the reset of a session therefore also sees that session's frequency, so it never
/// converts a new session's ticks with the frequency of an earlier one (or with the initial 0).
#[derive(Debug)]
pub struct ScanClock {
    frequency: AtomicI64,
    origin: AtomicI64,
}

impl Default for ScanClock {
    fn default() -> Self {
        Self::new()
    }
}

impl ScanClock {
    /// Creates a clock with no reporting origin.
    #[must_use]
    pub const fn new() -> Self {
        Self::with_frequency(0)
    }

    /// Creates a clock with a known frequency and no reporting origin.
    #[must_use]
    pub const fn with_frequency(frequency: i64) -> Self {
        Self {
            frequency: AtomicI64::new(frequency),
            origin: AtomicI64::new(NO_ORIGIN),
        }
    }

    /// Starts a new reporting session with the given tick rate per second.
    pub fn restart(&self, frequency: i64) {
        // The frequency first, then the reset that publishes it (see the type docs).
        self.frequency.store(frequency, Ordering::Relaxed);
        self.origin.store(NO_ORIGIN, Ordering::Release);
    }

    /// Returns the cached tick rate per second.
    pub fn frequency(&self) -> i64 {
        self.frequency.load(Ordering::Relaxed)
    }

    /// Converts a performance-counter sample to the Scan Time field of a report.
    ///
    /// `counter` is the current QPC tick count.
    /// An unavailable or invalid sample yields zero without setting the origin.
    pub fn sample(&self, counter: i64) -> u16 {
        // The origin before the frequency: seeing a session's reset implies seeing its frequency.
        let origin = self.origin.load(Ordering::Acquire);
        let frequency = self.frequency.load(Ordering::Relaxed);
        if counter < 0 || frequency <= 0 {
            return 0;
        }
        let origin = if origin == NO_ORIGIN {
            match self.origin.compare_exchange(
                NO_ORIGIN,
                counter,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return 0,
                Err(origin) => origin,
            }
        } else {
            origin
        };
        // Overlapping callbacks can sample the counter before another establishes the origin.
        let elapsed = counter.saturating_sub(origin).max(0) as u128;
        let units = elapsed * UNITS_PER_SECOND / frequency as u128;
        // HID serializes the low 16 bits of the monotonically increasing relative time.
        units as u16
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cumulative_time_wraps_at_sixteen_bits() {
        let clock = ScanClock::new();
        for frequency in [10_000_000_i64, 24_000_000] {
            clock.restart(frequency);
            let start = 100 * frequency;
            let unit = frequency / 10_000;
            let wrap = i64::from(u16::MAX) + 1;
            for (ticks, expected) in [
                (0, 0),
                (unit - 1, 0),
                (unit, 1),
                (256 * unit, 256),
                ((wrap - 1) * unit, u16::MAX),
                (wrap * unit, 0),
                ((wrap + 1) * unit, 1),
            ] {
                assert_eq!(clock.sample(start + ticks), expected);
            }
        }
    }

    #[test]
    fn invalid_samples_do_not_anchor_the_clock() {
        let clock = ScanClock::new();
        assert_eq!(clock.sample(200), 0);
        clock.restart(3_579_545);
        assert_eq!(clock.sample(-1), 0);
        assert_eq!(clock.sample(100_000), 0);
        assert_eq!(clock.sample(100_357), 0);
        assert_eq!(clock.sample(100_358), 1);
        assert_eq!(clock.sample(90_000), 0);
        assert_eq!(clock.sample(100_716), 2);
        clock.restart(3_579_545);
        assert_eq!(clock.sample(120_000), 0);
    }
}
