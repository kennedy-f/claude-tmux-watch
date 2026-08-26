#[derive(Debug, Clone)]
pub struct ClassificationPair {
    pub predicted: String,
    pub actual: String,
}

#[derive(Debug, Clone)]
pub struct DryRunCriteria {
    pub min_consecutive_correct: u32,
    pub min_agreement_rate: f64,
}

#[derive(Debug, Clone)]
pub struct DryRunResult {
    pub agreement_rate: f64,
    pub consecutive_correct: u32,
    pub ready_to_graduate: bool,
}

/// Exit criterion for running the watch loop in parallel to the existing
/// LLM-driven loop before it replaces it in production: graduate once EITHER
/// a long enough streak of consecutive correct classifications is observed,
/// OR the overall agreement rate across the whole dry-run sample clears the
/// threshold.
pub fn evaluate_dry_run(pairs: &[ClassificationPair], criteria: &DryRunCriteria) -> DryRunResult {
    if pairs.is_empty() {
        return DryRunResult {
            agreement_rate: 0.0,
            consecutive_correct: 0,
            ready_to_graduate: false,
        };
    }

    let mut correct: u32 = 0;
    let mut consecutive_correct: u32 = 0;
    for pair in pairs {
        if pair.predicted == pair.actual {
            correct += 1;
            consecutive_correct += 1;
        } else {
            consecutive_correct = 0;
        }
    }

    let agreement_rate = f64::from(correct) / pairs.len() as f64;
    let ready_to_graduate = consecutive_correct >= criteria.min_consecutive_correct
        || agreement_rate >= criteria.min_agreement_rate;

    DryRunResult {
        agreement_rate,
        consecutive_correct,
        ready_to_graduate,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pair(predicted: &str, actual: &str) -> ClassificationPair {
        ClassificationPair {
            predicted: predicted.to_string(),
            actual: actual.to_string(),
        }
    }

    fn criteria() -> DryRunCriteria {
        DryRunCriteria {
            min_consecutive_correct: 20,
            min_agreement_rate: 0.95,
        }
    }

    #[test]
    fn not_ready_while_below_both_thresholds() {
        let pairs = vec![pair("working", "working"), pair("done", "waiting_input")];
        assert!(!evaluate_dry_run(&pairs, &criteria()).ready_to_graduate);
    }

    #[test]
    fn graduates_on_consecutive_correct_streak() {
        let pairs: Vec<_> = (0..20).map(|_| pair("done", "done")).collect();
        let result = evaluate_dry_run(&pairs, &criteria());
        assert!(result.ready_to_graduate);
        assert_eq!(result.consecutive_correct, 20);
    }

    #[test]
    fn graduates_on_agreement_rate_without_perfect_streak() {
        let mut pairs: Vec<_> = (0..95).map(|_| pair("done", "done")).collect();
        pairs.extend((0..4).map(|_| pair("done", "error")));
        pairs.push(pair("done", "done"));
        let result = evaluate_dry_run(&pairs, &criteria());
        assert!((result.agreement_rate - 0.96).abs() < 0.005);
        assert!(result.ready_to_graduate);
    }

    #[test]
    fn mismatch_resets_streak_to_zero() {
        let pairs = vec![
            pair("done", "done"),
            pair("done", "done"),
            pair("done", "error"),
        ];
        assert_eq!(evaluate_dry_run(&pairs, &criteria()).consecutive_correct, 0);
    }
}
