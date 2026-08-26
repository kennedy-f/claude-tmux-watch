use crate::types::CircuitBreakerConfig;

pub struct CrashResult {
    pub tripped: bool,
    pub crashes_in_window: usize,
}

/// If the watch loop crashes, callers fall back to the old (pre-split) polling
/// loop. Without a limit, a persistent bug could bounce the process between the
/// two modes forever. This trips into a degraded, fixed state after
/// `max_crashes` within a sliding `window_ms`, requiring an explicit reset
/// (operator action / deploy) to resume the watch loop.
pub struct CircuitBreaker {
    config: CircuitBreakerConfig,
    crash_timestamps: Vec<u128>,
    tripped: bool,
}

impl CircuitBreaker {
    pub fn new(config: CircuitBreakerConfig) -> Self {
        Self {
            config,
            crash_timestamps: Vec::new(),
            tripped: false,
        }
    }

    pub fn record_crash(&mut self, now_ms: u128) -> CrashResult {
        let window = self.config.window_ms as u128;
        self.crash_timestamps
            .retain(|t| now_ms.saturating_sub(*t) < window);
        self.crash_timestamps.push(now_ms);
        if self.crash_timestamps.len() >= self.config.max_crashes as usize {
            self.tripped = true;
        }
        CrashResult {
            tripped: self.tripped,
            crashes_in_window: self.crash_timestamps.len(),
        }
    }

    pub fn is_tripped(&self) -> bool {
        self.tripped
    }

    pub fn reset(&mut self) {
        self.tripped = false;
        self.crash_timestamps.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(max_crashes: u32, window_ms: u64) -> CircuitBreakerConfig {
        CircuitBreakerConfig {
            max_crashes,
            window_ms,
        }
    }

    #[test]
    fn allows_a_restart_while_under_the_crash_threshold_within_the_window() {
        let mut cb = CircuitBreaker::new(config(3, 600_000));
        let mut now: u128 = 0;
        assert!(!cb.record_crash(now).tripped);
        now += 1000;
        assert!(!cb.record_crash(now).tripped);
    }

    #[test]
    fn trips_after_max_crashes_within_window_and_reports_degraded_mode() {
        let mut cb = CircuitBreaker::new(config(3, 600_000));
        let mut now: u128 = 0;
        cb.record_crash(now);
        now += 1000;
        cb.record_crash(now);
        now += 1000;
        assert!(cb.record_crash(now).tripped);
    }

    #[test]
    fn does_not_count_crashes_outside_the_sliding_window() {
        let mut cb = CircuitBreaker::new(config(3, 600_000));
        cb.record_crash(0);
        cb.record_crash(1000);
        let result = cb.record_crash(700_000);
        assert!(!result.tripped);
        assert_eq!(result.crashes_in_window, 1);
    }

    #[test]
    fn stays_tripped_once_tripped_requiring_an_explicit_reset() {
        let mut cb = CircuitBreaker::new(config(2, 600_000));
        cb.record_crash(0);
        assert!(cb.record_crash(100).tripped);
        assert!(cb.is_tripped());
        // Still tripped even for a crash whose window no longer holds the others.
        assert!(cb.record_crash(10_000_000).tripped);
        assert!(cb.is_tripped());
        cb.reset();
        assert!(!cb.is_tripped());
    }
}
