use crate::classifier::{classify, CompiledPatterns};
use crate::log_store::append_with_rotation;
use crate::rolling_context::RollingContextAccumulator;
use crate::state_machine::SettleMachine;
use crate::text_diff::diff_lines;
use crate::types::{DecisionEvent, DecisionReason, PaneState, PollPhase, WatchDecideConfig};
use std::io;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

pub struct WatchLoopDeps<F: FnMut() -> io::Result<String>> {
    pub session: String,
    pub capture_fn: F,
    pub patterns: CompiledPatterns,
    pub config: WatchDecideConfig,
    /// Full path to the raw persistence log for this session. Optional for pure unit testing.
    pub log_path: Option<PathBuf>,
}

pub struct WatchStepResult {
    pub interval_ms: u64,
    pub event: Option<DecisionEvent>,
}

/// Watch loop: zero LLM calls. Captures the pane, diffs by content (never by
/// line offset — tmux's history-limit ceiling shifts everything once hit),
/// classifies only the new delta, and runs it through the settle/backoff
/// state machine. A DecisionEvent is only produced on a confirmed real
/// transition (waiting_input/done/error) or a safety-timeout check-in.
pub struct WatchLoop<F: FnMut() -> io::Result<String>> {
    session: String,
    capture_fn: F,
    patterns: CompiledPatterns,
    config: WatchDecideConfig,
    log_path: Option<PathBuf>,
    full_log_path: String,
    machine: SettleMachine,
    rolling_context: RollingContextAccumulator,
    prev_content: Option<String>,
    pending_delta_lines: Vec<String>,
    ms_since_last_event: u64,
    last_interval_ms: u64,
}

impl<F: FnMut() -> io::Result<String>> WatchLoop<F> {
    pub fn new(deps: WatchLoopDeps<F>) -> Self {
        let machine = SettleMachine::new(deps.config.backoff, deps.config.settle_window_ms);
        let rolling_context = RollingContextAccumulator::new(deps.config.rolling_context_every_n);
        let full_log_path = match &deps.log_path {
            Some(p) => p.display().to_string(),
            None => format!("~/.hermes/logs/tmux-{}.log", deps.session),
        };
        Self {
            session: deps.session,
            capture_fn: deps.capture_fn,
            patterns: deps.patterns,
            config: deps.config,
            log_path: deps.log_path,
            full_log_path,
            machine,
            rolling_context,
            prev_content: None,
            pending_delta_lines: Vec::new(),
            ms_since_last_event: 0,
            last_interval_ms: 0,
        }
    }

    fn persist_raw(&self, lines: &[String]) {
        let Some(path) = &self.log_path else { return };
        if lines.is_empty() {
            return;
        }
        let _ = append_with_rotation(
            path,
            &(lines.join("\n") + "\n"),
            &self.config.log_rotation,
        );
    }

    fn build_event(&mut self, state: PaneState, reason: DecisionReason) -> DecisionEvent {
        let delta_lines = std::mem::take(&mut self.pending_delta_lines);
        let n = delta_lines.len();
        let delta_text = delta_lines.join("\n");
        let summary = self.rolling_context.record(&delta_text, state).summary;
        DecisionEvent {
            session: self.session.clone(),
            state,
            delta: if n == 1 {
                "+1 line desde última captura".to_string()
            } else {
                format!("+{n} lines desde última captura")
            },
            summary,
            full_log_path: self.full_log_path.clone(),
            timestamp_ms: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_millis())
                .unwrap_or(0),
            reason,
        }
    }

    pub fn step(&mut self) -> WatchStepResult {
        self.ms_since_last_event += self.last_interval_ms;

        let content = (self.capture_fn)().expect("tmux capture failed");
        let mut changed = false;

        match &self.prev_content {
            None => self.prev_content = Some(content),
            Some(prev) => {
                let delta = diff_lines(prev, &content);
                changed = delta.changed;
                if changed {
                    self.pending_delta_lines
                        .extend(delta.added_lines.iter().cloned());
                    self.persist_raw(&delta.added_lines);
                }
                self.prev_content = Some(content);
            }
        }

        let was_settled = self.machine.phase() == PollPhase::Settled;
        self.machine.on_poll(changed);
        let just_settled = self.machine.phase() == PollPhase::Settled && !was_settled;

        let mut event: Option<DecisionEvent> = None;

        if just_settled {
            let classification =
                classify(&self.pending_delta_lines.join("\n"), &self.patterns);
            if classification.state != PaneState::Working {
                event = Some(self.build_event(classification.state, DecisionReason::StateTransition));
                self.ms_since_last_event = 0;
            }
            self.pending_delta_lines.clear();
        }

        if event.is_none() && self.ms_since_last_event >= self.config.safety_timeout_ms {
            let classification =
                classify(&self.pending_delta_lines.join("\n"), &self.patterns);
            event = Some(self.build_event(classification.state, DecisionReason::SafetyTimeout));
            self.pending_delta_lines.clear();
            self.ms_since_last_event = 0;
        }

        self.last_interval_ms = self.machine.interval_ms();
        WatchStepResult {
            interval_ms: self.machine.interval_ms(),
            event,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::classifier::compile_patterns;
    use crate::types::{
        BackoffConfig, CircuitBreakerConfig, LogRotationConfig, PatternConfig,
    };

    fn patterns() -> PatternConfig {
        PatternConfig {
            error: vec![r"^\s*Error:".to_string()],
            waiting_input: vec![r"^\s*❯\s*\d+\.".to_string()],
            done: vec![r"^\s*✓\s".to_string()],
            working: vec![r"^\s*●\s".to_string()],
        }
    }

    fn config() -> WatchDecideConfig {
        WatchDecideConfig {
            settle_window_ms: 4000,
            backoff: BackoffConfig {
                working_ms: 12000,
                settling_ms: 2000,
                settled_ms: 3000,
            },
            rolling_context_every_n: 5,
            log_rotation: LogRotationConfig {
                max_bytes: 10_000_000,
                max_files: 3,
            },
            safety_timeout_ms: 20000,
            circuit_breaker: CircuitBreakerConfig {
                max_crashes: 3,
                window_ms: 600_000,
            },
            telegram_notify_on_auto_improve: true,
        }
    }

    fn make_capture_queue(frames: Vec<&str>) -> impl FnMut() -> io::Result<String> {
        let frames: Vec<String> = frames.into_iter().map(String::from).collect();
        let mut i = 0usize;
        move || {
            let idx = i.min(frames.len() - 1);
            i += 1;
            Ok(frames[idx].clone())
        }
    }

    fn make_loop(frames: Vec<&str>) -> WatchLoop<impl FnMut() -> io::Result<String>> {
        WatchLoop::new(WatchLoopDeps {
            session: "s1".to_string(),
            capture_fn: make_capture_queue(frames),
            patterns: compile_patterns(&patterns()),
            config: config(),
            log_path: None,
        })
    }

    #[test]
    fn never_emits_while_pane_is_quietly_unchanged() {
        let mut l = make_loop(vec!["● working on it"]);
        let r1 = l.step();
        let r2 = l.step();
        assert!(r1.event.is_none());
        assert!(r2.event.is_none());
        assert_eq!(r1.interval_ms, 12000);
    }

    #[test]
    fn waits_for_settle_window_before_emitting_waiting_input_event() {
        let mut l = make_loop(vec![
            "● working on it",
            "● working on it\n❯ 1. Proceed?",
            "● working on it\n❯ 1. Proceed?",
            "● working on it\n❯ 1. Proceed?",
        ]);
        l.step();
        assert!(l.step().event.is_none());
        assert!(l.step().event.is_none());
        let r4 = l.step();
        let event = r4.event.expect("settled transition emits an event");
        assert_eq!(event.state, PaneState::WaitingInput);
        assert!(event.delta.contains("1 line"));
    }

    #[test]
    fn classifies_only_the_new_delta_not_the_full_scrollback() {
        let mut l = make_loop(vec![
            "✓ previous task done\n● now doing something new",
            "✓ previous task done\n● now doing something new\nmore working output",
            "✓ previous task done\n● now doing something new\nmore working output",
            "✓ previous task done\n● now doing something new\nmore working output",
        ]);
        l.step();
        l.step();
        l.step();
        assert!(l.step().event.is_none());
    }

    #[test]
    fn settled_mid_task_progress_emits_nothing_and_resumes_long_interval() {
        let mut l = make_loop(vec![
            "● step one",
            "● step one\n● step two",
            "● step one\n● step two",
            "● step one\n● step two",
            "● step one\n● step two",
        ]);
        l.step();
        l.step();
        l.step();
        assert!(l.step().event.is_none());
        let r5 = l.step();
        assert!(r5.event.is_none());
        assert_eq!(r5.interval_ms, 12000);
    }

    #[test]
    fn forces_safety_timeout_checkin_when_nothing_transitions() {
        let mut l = make_loop(vec!["● still working, unchanged forever"]);
        let mut saw_timeout_event = false;
        for _ in 0..5 {
            if let Some(event) = l.step().event {
                if matches!(event.reason, DecisionReason::SafetyTimeout) {
                    saw_timeout_event = true;
                    break;
                }
            }
        }
        assert!(saw_timeout_event);
    }
}
