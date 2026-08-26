use crate::types::{BackoffConfig, PollPhase};

/// Explicit settle/backoff state machine.
///
/// - working:  no suspected transition, cruise at the long interval.
/// - settling: output just changed (or keeps changing); poll fast until it
///             stops moving for `settle_window_ms`.
/// - settled:  output stopped moving; one confirmation tick, then back to
///             `working` if the pause was a normal mid-task lull.
pub struct SettleMachine {
    backoff: BackoffConfig,
    settle_window_ms: u64,
    phase: PollPhase,
    ms_since_last_change: u64,
    interval_ms: u64,
}

impl SettleMachine {
    pub fn new(backoff: BackoffConfig, settle_window_ms: u64) -> Self {
        let phase = PollPhase::Working;
        let interval_ms = interval_for(&backoff, phase);
        Self {
            backoff,
            settle_window_ms,
            phase,
            ms_since_last_change: 0,
            interval_ms,
        }
    }

    pub fn phase(&self) -> PollPhase {
        self.phase
    }

    pub fn interval_ms(&self) -> u64 {
        self.interval_ms
    }

    pub fn ms_since_last_change(&self) -> u64 {
        self.ms_since_last_change
    }

    /// Advance by one poll tick. `changed` reports whether the captured delta
    /// differed from the previous capture.
    pub fn on_poll(&mut self, changed: bool) {
        if changed {
            self.phase = PollPhase::Settling;
            self.ms_since_last_change = 0;
        } else {
            match self.phase {
                PollPhase::Working => {}
                PollPhase::Settling => {
                    self.ms_since_last_change += self.interval_ms;
                    if self.ms_since_last_change >= self.settle_window_ms {
                        self.phase = PollPhase::Settled;
                    }
                }
                PollPhase::Settled => {
                    self.phase = PollPhase::Working;
                    self.ms_since_last_change = 0;
                }
            }
        }
        self.interval_ms = interval_for(&self.backoff, self.phase);
    }
}

fn interval_for(backoff: &BackoffConfig, phase: PollPhase) -> u64 {
    match phase {
        PollPhase::Working => backoff.working_ms,
        PollPhase::Settling => backoff.settling_ms,
        PollPhase::Settled => backoff.settled_ms,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BACKOFF: BackoffConfig = BackoffConfig {
        working_ms: 12000,
        settling_ms: 2000,
        settled_ms: 3000,
    };
    const SETTLE_WINDOW_MS: u64 = 4000;

    fn machine() -> SettleMachine {
        SettleMachine::new(BACKOFF, SETTLE_WINDOW_MS)
    }

    #[test]
    fn starts_in_the_working_phase_polling_at_the_long_interval() {
        let m = machine();
        assert_eq!(m.phase(), PollPhase::Working);
        assert_eq!(m.interval_ms(), 12000);
    }

    #[test]
    fn stays_in_working_long_interval_when_nothing_changes() {
        let mut m = machine();
        m.on_poll(false);
        assert_eq!(m.phase(), PollPhase::Working);
        assert_eq!(m.interval_ms(), 12000);
    }

    #[test]
    fn drops_to_the_short_settling_interval_the_instant_output_changes() {
        let mut m = machine();
        m.on_poll(true);
        assert_eq!(m.phase(), PollPhase::Settling);
        assert_eq!(m.interval_ms(), 2000);
    }

    #[test]
    fn keeps_resetting_the_settle_timer_on_every_further_change() {
        let mut m = machine();
        m.on_poll(true);
        m.on_poll(true);
        assert_eq!(m.phase(), PollPhase::Settling);
        assert_eq!(m.ms_since_last_change(), 0);
    }

    #[test]
    fn only_promotes_settling_to_settled_after_settle_window_of_no_change() {
        let mut m = machine();
        m.on_poll(true); // settling, elapsed 0, next interval 2000
        m.on_poll(false); // elapsed 2000 < 4000
        assert_eq!(m.phase(), PollPhase::Settling);
        m.on_poll(false); // elapsed 4000 >= settle window
        assert_eq!(m.phase(), PollPhase::Settled);
        assert_eq!(m.interval_ms(), 3000);
    }

    #[test]
    fn never_uses_the_settled_interval_while_still_actively_settling() {
        let mut m = machine();
        m.on_poll(true);
        m.on_poll(false);
        assert_eq!(m.interval_ms(), 2000);
    }

    #[test]
    fn returns_to_the_long_working_interval_after_one_settled_tick() {
        let mut m = machine();
        m.on_poll(true);
        m.on_poll(false);
        m.on_poll(false);
        assert_eq!(m.phase(), PollPhase::Settled);
        m.on_poll(false);
        assert_eq!(m.phase(), PollPhase::Working);
        assert_eq!(m.interval_ms(), 12000);
    }

    #[test]
    fn a_change_during_settled_immediately_drops_back_to_settling_not_working() {
        let mut m = machine();
        m.on_poll(true);
        m.on_poll(false);
        m.on_poll(false); // settled
        m.on_poll(true);
        assert_eq!(m.phase(), PollPhase::Settling);
    }
}
