#[derive(Debug, Clone)]
pub struct MeasurementInput {
    pub task_duration_ms: u64,
    pub old_poll_interval_ms: u64,
    pub new_decision_event_count: u32,
    pub old_avg_payload_bytes: u64,
    pub new_avg_payload_bytes: u64,
}

#[derive(Debug, Clone)]
pub struct MeasurementResult {
    pub old_call_count: u64,
    pub new_call_count: u32,
    pub call_reduction_rate: f64,
    pub payload_reduction_rate: f64,
    pub meets_fifty_percent_goal: bool,
}

/// Acceptance-criteria math: for a given task, how many LLM calls did the old
/// fixed-interval polling loop make vs. the new decide loop (one per real
/// DecisionEvent), and how much smaller is the average payload per call.
pub fn estimate_reduction(input: &MeasurementInput) -> MeasurementResult {
    let old_call_count = if input.old_poll_interval_ms == 0 {
        0
    } else {
        // Mirrors JS Math.round (half away from zero on non-negative values).
        (input.task_duration_ms as f64 / input.old_poll_interval_ms as f64).round() as u64
    };
    let new_call_count = input.new_decision_event_count;

    let call_reduction_rate = if old_call_count == 0 {
        0.0
    } else {
        1.0 - f64::from(new_call_count) / old_call_count as f64
    };
    let payload_reduction_rate = if input.old_avg_payload_bytes == 0 {
        0.0
    } else {
        1.0 - input.new_avg_payload_bytes as f64 / input.old_avg_payload_bytes as f64
    };

    MeasurementResult {
        old_call_count,
        new_call_count,
        call_reduction_rate,
        payload_reduction_rate,
        meets_fifty_percent_goal: call_reduction_rate >= 0.5 && payload_reduction_rate >= 0.5,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn computes_old_call_count_from_duration_over_poll_interval() {
        let r = estimate_reduction(&MeasurementInput {
            task_duration_ms: 300_000,
            old_poll_interval_ms: 3000,
            new_decision_event_count: 4,
            old_avg_payload_bytes: 42_000,
            new_avg_payload_bytes: 900,
        });
        assert_eq!(r.old_call_count, 100);
        assert_eq!(r.new_call_count, 4);
        assert!((r.call_reduction_rate - 0.96).abs() < 0.005);
    }

    #[test]
    fn computes_payload_reduction_alongside_call_reduction() {
        let r = estimate_reduction(&MeasurementInput {
            task_duration_ms: 60_000,
            old_poll_interval_ms: 3000,
            new_decision_event_count: 2,
            old_avg_payload_bytes: 10_000,
            new_avg_payload_bytes: 500,
        });
        assert!((r.payload_reduction_rate - 0.95).abs() < 0.005);
        assert!(r.meets_fifty_percent_goal);
    }

    #[test]
    fn flags_when_fifty_percent_goal_not_met() {
        let r = estimate_reduction(&MeasurementInput {
            task_duration_ms: 10_000,
            old_poll_interval_ms: 3000,
            new_decision_event_count: 3,
            old_avg_payload_bytes: 1000,
            new_avg_payload_bytes: 900,
        });
        assert!(!r.meets_fifty_percent_goal);
    }
}
