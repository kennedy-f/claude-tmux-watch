use crate::types::{ClassificationResult, PaneState, PatternConfig};
use regex::Regex;

pub struct CompiledPatterns {
    pub error: Vec<Regex>,
    pub waiting_input: Vec<Regex>,
    pub done: Vec<Regex>,
    pub working: Vec<Regex>,
}

/// Multiline mode is forced on every pattern: without it "^" anchors only at
/// index 0 of the whole delta instead of the start of every line, so most TUI
/// patterns silently never match a multi-line delta.
fn compile_one(source: &str) -> Regex {
    Regex::new(&format!("(?m){source}"))
        .unwrap_or_else(|e| panic!("invalid pattern {source:?}: {e}"))
}

pub fn compile_patterns(patterns: &PatternConfig) -> CompiledPatterns {
    CompiledPatterns {
        error: patterns.error.iter().map(|p| compile_one(p)).collect(),
        waiting_input: patterns
            .waiting_input
            .iter()
            .map(|p| compile_one(p))
            .collect(),
        done: patterns.done.iter().map(|p| compile_one(p)).collect(),
        working: patterns.working.iter().map(|p| compile_one(p)).collect(),
    }
}

/// Priority is error > waiting_input > done > working so a real error is never
/// masked by leftover "done"/"working" chrome in the same delta.
pub fn classify(delta: &str, compiled: &CompiledPatterns) -> ClassificationResult {
    let priority = [
        (PaneState::Error, &compiled.error),
        (PaneState::WaitingInput, &compiled.waiting_input),
        (PaneState::Done, &compiled.done),
        (PaneState::Working, &compiled.working),
    ];
    for (state, list) in priority {
        for re in list {
            if re.is_match(delta) {
                return ClassificationResult {
                    state,
                    matched_pattern: Some(re.as_str().to_string()),
                };
            }
        }
    }
    ClassificationResult {
        state: PaneState::Working,
        matched_pattern: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn patterns() -> PatternConfig {
        PatternConfig {
            error: vec![r"^\s*Error:".into(), r"^\s*Traceback".into()],
            waiting_input: vec![r"^\s*❯\s*\d+\.".into(), r"\(y/n\)\s*$".into()],
            done: vec![r"^\s*✓\s".into(), r"\bTask complete\b".into()],
            working: vec![r"^\s*●\s".into(), "esc to interrupt".into()],
        }
    }

    #[test]
    fn always_compiles_with_the_multiline_flag_even_if_the_source_omits_it() {
        let compiled = compile_patterns(&patterns());
        assert!(compiled.error[0].as_str().starts_with("(?m)"));
        assert!(compiled.working[0].as_str().starts_with("(?m)"));
        // Behavioral proof: a "^"-anchored pattern matches a non-first line.
        assert!(compiled.error[0].is_match("noise\nError: boom"));
    }

    #[test]
    fn returns_working_for_an_active_tool_use_line() {
        let compiled = compile_patterns(&patterns());
        let delta = "● Running tests\nsome output\nesc to interrupt";
        assert_eq!(classify(delta, &compiled).state, PaneState::Working);
    }

    #[test]
    fn returns_waiting_input_for_a_numbered_prompt() {
        let compiled = compile_patterns(&patterns());
        let delta = "Pick an option:\n❯ 1. Yes, I trust this folder\n  2. No, exit";
        assert_eq!(classify(delta, &compiled).state, PaneState::WaitingInput);
    }

    #[test]
    fn returns_done_for_a_completion_marker() {
        let compiled = compile_patterns(&patterns());
        let delta = "✓ All good\nTask complete";
        assert_eq!(classify(delta, &compiled).state, PaneState::Done);
    }

    #[test]
    fn returns_error_even_when_other_states_also_match() {
        let compiled = compile_patterns(&patterns());
        let delta = "● doing a thing\nError: something broke\n✓ partial";
        assert_eq!(classify(delta, &compiled).state, PaneState::Error);
    }

    #[test]
    fn waiting_input_beats_done_when_both_match() {
        let compiled = compile_patterns(&patterns());
        let delta = "✓ step done\n❯ 1. Continue?";
        assert_eq!(classify(delta, &compiled).state, PaneState::WaitingInput);
    }

    #[test]
    fn done_beats_working_when_both_match() {
        let compiled = compile_patterns(&patterns());
        let delta = "● tool use line\nTask complete";
        assert_eq!(classify(delta, &compiled).state, PaneState::Done);
    }

    #[test]
    fn defaults_to_working_when_nothing_matches() {
        let compiled = compile_patterns(&patterns());
        let delta = "just some regular scrolling output\nnothing special here";
        let result = classify(delta, &compiled);
        assert_eq!(result.state, PaneState::Working);
        assert!(result.matched_pattern.is_none());
    }

    #[test]
    fn matches_multiline_anchors_against_every_line_not_just_buffer_start() {
        let compiled = compile_patterns(&patterns());
        let delta = "first unrelated line\nsecond line\n✓ Done deep in the buffer";
        assert_eq!(classify(delta, &compiled).state, PaneState::Done);
    }

    /// Regression guard: a status-bar segment that renders every frame (e.g.
    /// "profile: prod") would always match and permanently pin
    /// classification to 'working', starving done/waiting_input.
    #[test]
    fn a_status_bar_only_pattern_like_profile_must_not_be_present_in_working_patterns() {
        let profile_like = Regex::new("(?i)profile:").unwrap();
        assert!(!patterns().working.iter().any(|p| profile_like.is_match(p)));

        let default_working = default_working_patterns();
        assert!(!default_working.iter().any(|p| profile_like.is_match(p)));

        // And none of them may match a persistent status-bar chrome line.
        let chrome = "[hermes] profile: prod | branch: main | 12:04";
        let compiled = compile_patterns(&PatternConfig {
            working: default_working,
            ..Default::default()
        });
        for re in &compiled.working {
            assert!(
                !re.is_match(chrome),
                "working pattern {} matches persistent status-bar chrome",
                re.as_str()
            );
        }
    }

    /// Shared default config's `working` list. Read from the shipped config
    /// file when present so this stays honest as the config evolves.
    fn default_working_patterns() -> Vec<String> {
        let path =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("config/patterns.default.json");
        if let Ok(raw) = std::fs::read_to_string(&path) {
            let cfg: PatternConfig = serde_json::from_str(&raw).expect("default patterns parse");
            return cfg.working;
        }
        vec![
            r"^\s*●\s".into(),
            "esc to interrupt".into(),
            r"\bThinking…\b".into(),
            r"^\s*[⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏]\s".into(),
        ]
    }
}
