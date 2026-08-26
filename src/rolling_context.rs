use crate::summarizer::summarize;
use crate::types::PaneState;

pub struct RollingContextResult {
    pub summary: String,
    pub compacted: bool,
}

/// Compacts the rolling context every N decide-loop interactions instead of
/// resending the whole accumulated transcript every time. Compaction is
/// deterministic — it re-runs the same regex/heuristic extraction over the
/// concatenated window, never an LLM call.
pub struct RollingContextAccumulator {
    every_n: u32,
    window_texts: Vec<String>,
    count: u32,
}

impl RollingContextAccumulator {
    pub fn new(every_n: u32) -> Self {
        Self {
            every_n,
            window_texts: Vec::new(),
            count: 0,
        }
    }

    pub fn record(&mut self, delta_text: &str, state: PaneState) -> RollingContextResult {
        self.window_texts.push(delta_text.to_string());
        self.count += 1;
        if self.count >= self.every_n {
            let compacted_summary = summarize(&self.window_texts.join("\n"), state);
            self.window_texts.clear();
            self.count = 0;
            return RollingContextResult {
                summary: compacted_summary,
                compacted: true,
            };
        }
        RollingContextResult {
            summary: summarize(delta_text, state),
            compacted: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn returns_plain_per_delta_summary_for_interactions_before_n() {
        let mut acc = RollingContextAccumulator::new(5);
        let r1 = acc.record("Reviewed #10", PaneState::WaitingInput);
        assert!(!r1.compacted);
        assert!(r1.summary.contains("#10"));
    }

    #[test]
    fn compacts_on_nth_interaction_merging_facts_across_window() {
        let mut acc = RollingContextAccumulator::new(3);
        acc.record("Reviewed #1237", PaneState::Working);
        acc.record("Reviewed #1239", PaneState::Working);
        let r3 = acc.record("Reviewed #1295, waiting now", PaneState::WaitingInput);
        assert!(r3.compacted);
        assert!(r3.summary.contains("#1237"));
        assert!(r3.summary.contains("#1239"));
        assert!(r3.summary.contains("#1295"));
    }

    #[test]
    fn resets_window_after_compaction_so_next_n_start_fresh() {
        let mut acc = RollingContextAccumulator::new(2);
        acc.record("Reviewed #1", PaneState::Working);
        acc.record("Reviewed #2", PaneState::Working);
        let r3 = acc.record("Reviewed #3", PaneState::Working);
        assert!(!r3.compacted);
        assert!(!r3.summary.contains("#1"));
        assert!(r3.summary.contains("#3"));
    }
}
