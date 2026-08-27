use crate::auto_respond::{AutoRespondDecision, AutoResponder};
use crate::classifier::{classify, CompiledPatterns};
use crate::log_store::append_with_rotation;
use crate::rolling_context::RollingContextAccumulator;
use crate::state_machine::SettleMachine;
use crate::text_diff::diff_lines;
use crate::types::{
    AutoRespondConfig, DecisionEvent, DecisionReason, PaneState, PollPhase, WatchDecideConfig,
};
use std::io;
use std::path::PathBuf;
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub struct WatchLoopDeps<F, S>
where
    F: FnMut() -> io::Result<String>,
    S: FnMut(&[String]) -> io::Result<()>,
{
    pub session: String,
    pub capture_fn: F,
    pub send_keys_fn: S,
    pub patterns: CompiledPatterns,
    pub config: WatchDecideConfig,
    pub auto_respond_config: AutoRespondConfig,
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
pub struct WatchLoop<F, S>
where
    F: FnMut() -> io::Result<String>,
    S: FnMut(&[String]) -> io::Result<()>,
{
    session: String,
    capture_fn: F,
    send_keys_fn: S,
    patterns: CompiledPatterns,
    config: WatchDecideConfig,
    log_path: Option<PathBuf>,
    full_log_path: String,
    machine: SettleMachine,
    rolling_context: RollingContextAccumulator,
    prev_content: Option<String>,
    pending_delta_lines: Vec<String>,
    consecutive_capture_failures: u64,
    ms_since_last_event: u64,
    last_interval_ms: u64,
    auto_responder: AutoResponder,
}

impl<F, S> WatchLoop<F, S>
where
    F: FnMut() -> io::Result<String>,
    S: FnMut(&[String]) -> io::Result<()>,
{
    pub fn new(deps: WatchLoopDeps<F, S>) -> Self {
        let machine = SettleMachine::new(deps.config.backoff, deps.config.settle_window_ms);
        let rolling_context = RollingContextAccumulator::new(deps.config.rolling_context_every_n);
        let full_log_path = match &deps.log_path {
            Some(p) => p.display().to_string(),
            None => format!("~/.hermes/logs/tmux-{}.log", deps.session),
        };
        // Compilation errors are logged to stderr but never crash the watch loop;
        // the auto-responder falls back to always returning NoMatch when construction
        // fails (the ok_or fallback below creates a no-op responder via disabled config).
        let auto_responder =
            AutoResponder::new(deps.auto_respond_config.clone()).unwrap_or_else(|e| {
                eprintln!("[tmux-watch] auto-respond config error (feature disabled): {e}");
                AutoResponder::new(crate::types::AutoRespondConfig {
                    enabled: false,
                    rules: vec![],
                    limits: deps.auto_respond_config.limits.clone(),
                    notify: deps.auto_respond_config.notify.clone(),
                })
                .expect("fallback disabled config always valid")
            });
        Self {
            session: deps.session,
            capture_fn: deps.capture_fn,
            send_keys_fn: deps.send_keys_fn,
            patterns: deps.patterns,
            config: deps.config,
            log_path: deps.log_path,
            full_log_path,
            machine,
            rolling_context,
            prev_content: None,
            pending_delta_lines: Vec::new(),
            consecutive_capture_failures: 0,
            ms_since_last_event: 0,
            last_interval_ms: 0,
            auto_responder,
        }
    }

    fn persist_raw(&self, lines: &[String]) {
        let Some(path) = &self.log_path else { return };
        if lines.is_empty() {
            return;
        }
        let _ = append_with_rotation(path, &(lines.join("\n") + "\n"), &self.config.log_rotation);
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

    fn build_capture_failure_event(&mut self, err: &io::Error) -> DecisionEvent {
        self.pending_delta_lines.clear();
        let message = format!("tmux capture failed: {err}");
        DecisionEvent {
            session: self.session.clone(),
            state: PaneState::Error,
            delta: message.clone(),
            summary: message,
            full_log_path: self.full_log_path.clone(),
            timestamp_ms: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_millis())
                .unwrap_or(0),
            reason: DecisionReason::CaptureFailure,
        }
    }

    pub fn step(&mut self) -> WatchStepResult {
        self.ms_since_last_event += self.last_interval_ms;

        let content = match (self.capture_fn)() {
            Ok(content) => {
                self.consecutive_capture_failures = 0;
                content
            }
            Err(err) => {
                let threshold = u64::from(self.config.max_capture_failures.max(1));
                self.consecutive_capture_failures += 1;
                self.last_interval_ms = self.config.backoff.settling_ms;
                if self.consecutive_capture_failures == threshold {
                    let event = Some(self.build_capture_failure_event(&err));
                    self.consecutive_capture_failures = 0;
                    self.ms_since_last_event = 0;
                    return WatchStepResult {
                        interval_ms: self.last_interval_ms,
                        event,
                    };
                }
                return WatchStepResult {
                    interval_ms: self.last_interval_ms,
                    event: None,
                };
            }
        };
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
            let delta_text = self.pending_delta_lines.join("\n");
            let classification = classify(&delta_text, &self.patterns);
            if classification.state == PaneState::WaitingInput {
                // ── auto-respond path ────────────────────────────────────────
                let current_summary = self.rolling_context.peek_summary().unwrap_or_default();
                match self
                    .auto_responder
                    .decide(&delta_text, &current_summary, None)
                {
                    AutoRespondDecision::ShouldFire { rule_index, keys } => {
                        // TOCTOU double-check: sleep `requireStableIdleMs`, re-capture,
                        // verify the match still holds before sending.
                        let stable_ms = self.auto_responder.require_stable_idle_ms();
                        thread::sleep(Duration::from_millis(stable_ms));
                        let fresh = (self.capture_fn)().ok();
                        let still_matches = fresh
                            .as_deref()
                            .map(|c| self.auto_responder.rule_still_matches(rule_index, c))
                            .unwrap_or(false);
                        if still_matches {
                            match (self.send_keys_fn)(&keys) {
                                Ok(()) => {
                                    let changelog_path = self.log_path.as_ref().map(|p| {
                                        p.parent().unwrap_or(p).join("watch-decide-changelog.md")
                                    });
                                    let notify_telegram =
                                        self.auto_responder.telegram_on_every_auto_response();
                                    let outcome = self.auto_responder.commit(
                                        rule_index,
                                        &keys,
                                        changelog_path.as_deref(),
                                        notify_telegram,
                                        None,
                                    );
                                    // Raw session log entry.
                                    self.persist_raw(&[format!(
                                        "[auto-respond] rule={} keys={}",
                                        outcome.rule_id,
                                        keys.join(" ")
                                    )]);
                                    // Emit event to decide loop if configured.
                                    if self.auto_responder.emit_event_to_decide_loop() {
                                        let summary = format!(
                                            "auto_responded: rule={} keys={}",
                                            outcome.rule_id,
                                            keys.join(" ")
                                        );
                                        let ev = DecisionEvent {
                                            session: self.session.clone(),
                                            state: PaneState::WaitingInput,
                                            delta: format!(
                                                "+{} lines since last capture",
                                                self.pending_delta_lines.len()
                                            ),
                                            summary,
                                            full_log_path: self.full_log_path.clone(),
                                            timestamp_ms: SystemTime::now()
                                                .duration_since(UNIX_EPOCH)
                                                .map(|d| d.as_millis())
                                                .unwrap_or(0),
                                            reason: DecisionReason::AutoResponded,
                                        };
                                        self.pending_delta_lines.clear();
                                        event = Some(ev);
                                        self.ms_since_last_event = 0;
                                    } else {
                                        self.pending_delta_lines.clear();
                                    }
                                }
                                Err(err) => {
                                    // send-keys failure → fall through to the LLM, never panic.
                                    eprintln!(
                                        "[tmux-watch] send-keys failed (routing to LLM): {err}"
                                    );
                                    event = Some(self.build_event(
                                        classification.state,
                                        DecisionReason::StateTransition,
                                    ));
                                    self.ms_since_last_event = 0;
                                    self.pending_delta_lines.clear();
                                }
                            }
                        } else {
                            // TOCTOU check failed: content changed — abort silently and resume.
                            self.pending_delta_lines.clear();
                        }
                    }
                    AutoRespondDecision::Disabled | AutoRespondDecision::NoMatch => {
                        // Normal path: emit event to the decide loop.
                        event = Some(
                            self.build_event(classification.state, DecisionReason::StateTransition),
                        );
                        self.ms_since_last_event = 0;
                        self.pending_delta_lines.clear();
                    }
                }
            } else if classification.state != PaneState::Working {
                event =
                    Some(self.build_event(classification.state, DecisionReason::StateTransition));
                self.ms_since_last_event = 0;
                self.pending_delta_lines.clear();
            } else {
                self.pending_delta_lines.clear();
            }
        }

        if event.is_none() && self.ms_since_last_event >= self.config.safety_timeout_ms {
            let classification = classify(&self.pending_delta_lines.join("\n"), &self.patterns);
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
        AutoRespondConfig, AutoRespondLimits, AutoRespondNotify, AutoRespondRule, BackoffConfig,
        CircuitBreakerConfig, LogRotationConfig, PatternConfig, RuleRisk,
    };
    use std::sync::{Arc, Mutex};

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
            max_capture_failures: 3,
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

    fn disabled_auto_respond() -> AutoRespondConfig {
        AutoRespondConfig {
            enabled: false,
            rules: vec![],
            limits: AutoRespondLimits {
                max_auto_responses_per_session: 20,
                max_auto_responses_per_rule_per_hour: 10,
                cooldown_ms_after_response: 5000,
                require_stable_idle_ms: 0, // 0 for tests to avoid sleeping
            },
            notify: AutoRespondNotify {
                telegram_on_every_auto_response: false,
                emit_event_to_decide_loop: true,
            },
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

    fn make_erroring_capture(results: Vec<io::Result<&str>>) -> impl FnMut() -> io::Result<String> {
        assert!(
            !results.is_empty(),
            "test capture sequence must not be empty"
        );
        let results: Vec<io::Result<String>> = results
            .into_iter()
            .map(|result| result.map(String::from))
            .collect();
        let mut i = 0usize;
        move || {
            let idx = i.min(results.len() - 1);
            i += 1;
            match &results[idx] {
                Ok(frame) => Ok(frame.clone()),
                Err(err) => Err(io::Error::new(err.kind(), err.to_string())),
            }
        }
    }

    fn make_loop(
        frames: Vec<&str>,
    ) -> WatchLoop<impl FnMut() -> io::Result<String>, impl FnMut(&[String]) -> io::Result<()>>
    {
        WatchLoop::new(WatchLoopDeps {
            session: "s1".to_string(),
            capture_fn: make_capture_queue(frames),
            send_keys_fn: |_: &[String]| Ok(()),
            patterns: compile_patterns(&patterns()),
            config: config(),
            auto_respond_config: disabled_auto_respond(),
            log_path: None,
        })
    }

    #[test]
    fn capture_failure_emits_event_after_exact_threshold_without_panicking() {
        let mut cfg = config();
        cfg.max_capture_failures = 3;
        let mut l = WatchLoop::new(WatchLoopDeps {
            session: "s1".to_string(),
            capture_fn: make_erroring_capture(vec![
                Err(io::Error::new(io::ErrorKind::NotFound, "pane missing")),
                Err(io::Error::new(io::ErrorKind::NotFound, "pane missing")),
                Err(io::Error::new(io::ErrorKind::NotFound, "pane missing")),
            ]),
            send_keys_fn: |_: &[String]| Ok(()),
            patterns: compile_patterns(&patterns()),
            config: cfg,
            auto_respond_config: disabled_auto_respond(),
            log_path: None,
        });

        let r1 = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| l.step()));
        let r2 = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| l.step()));
        let r3 = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| l.step()));

        assert!(r1.is_ok());
        assert!(r2.is_ok());
        let event = r3.unwrap().event.expect("threshold should emit event");
        assert_eq!(event.state, PaneState::Error);
        assert_eq!(event.reason, DecisionReason::CaptureFailure);
        assert!(event.delta.contains("pane missing"));
        assert!(event.summary.contains("pane missing"));
    }

    #[test]
    fn capture_failure_below_threshold_emits_nothing_and_uses_short_interval() {
        let mut cfg = config();
        cfg.max_capture_failures = 3;
        let mut l = WatchLoop::new(WatchLoopDeps {
            session: "s1".to_string(),
            capture_fn: make_erroring_capture(vec![
                Err(io::Error::new(io::ErrorKind::NotFound, "pane missing")),
                Err(io::Error::new(io::ErrorKind::NotFound, "pane missing")),
            ]),
            send_keys_fn: |_: &[String]| Ok(()),
            patterns: compile_patterns(&patterns()),
            config: cfg,
            auto_respond_config: disabled_auto_respond(),
            log_path: None,
        });

        let r1 = l.step();
        let r2 = l.step();

        assert!(r1.event.is_none());
        assert!(r2.event.is_none());
        assert_eq!(r1.interval_ms, 2000);
        assert_eq!(r2.interval_ms, 2000);
    }

    #[test]
    fn successful_capture_resets_consecutive_failure_counter() {
        let mut cfg = config();
        cfg.max_capture_failures = 3;
        let mut l = WatchLoop::new(WatchLoopDeps {
            session: "s1".to_string(),
            capture_fn: make_erroring_capture(vec![
                Err(io::Error::new(io::ErrorKind::NotFound, "pane missing")),
                Err(io::Error::new(io::ErrorKind::NotFound, "pane missing")),
                Ok("● working again"),
                Err(io::Error::new(io::ErrorKind::NotFound, "pane missing")),
                Err(io::Error::new(io::ErrorKind::NotFound, "pane missing")),
                Err(io::Error::new(io::ErrorKind::NotFound, "pane missing")),
            ]),
            send_keys_fn: |_: &[String]| Ok(()),
            patterns: compile_patterns(&patterns()),
            config: cfg,
            auto_respond_config: disabled_auto_respond(),
            log_path: None,
        });

        assert!(l.step().event.is_none());
        assert!(l.step().event.is_none());
        assert!(l.step().event.is_none());
        assert!(l.step().event.is_none());
        let r5 = l.step();
        assert!(r5.event.is_none());
        assert_eq!(r5.interval_ms, 2000);
        let r6 = l.step();
        assert!(matches!(
            r6.event.as_ref().map(|event| event.reason),
            Some(DecisionReason::CaptureFailure)
        ));
    }

    #[test]
    fn repeated_capture_failure_episodes_emit_again_after_reset() {
        let mut cfg = config();
        cfg.max_capture_failures = 2;
        let mut l = WatchLoop::new(WatchLoopDeps {
            session: "s1".to_string(),
            capture_fn: make_erroring_capture(vec![
                Err(io::Error::new(io::ErrorKind::NotFound, "pane missing")),
                Err(io::Error::new(io::ErrorKind::NotFound, "pane missing")),
                Err(io::Error::new(io::ErrorKind::NotFound, "pane missing")),
                Err(io::Error::new(io::ErrorKind::NotFound, "pane missing")),
            ]),
            send_keys_fn: |_: &[String]| Ok(()),
            patterns: compile_patterns(&patterns()),
            config: cfg,
            auto_respond_config: disabled_auto_respond(),
            log_path: None,
        });

        assert!(l.step().event.is_none());
        assert!(matches!(
            l.step().event.as_ref().map(|event| event.reason),
            Some(DecisionReason::CaptureFailure)
        ));
        assert!(l.step().event.is_none());
        assert!(matches!(
            l.step().event.as_ref().map(|event| event.reason),
            Some(DecisionReason::CaptureFailure)
        ));
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

    // ── auto-respond integration tests ────────────────────────────────────────

    fn auto_respond_config_with_safe_rule(match_pattern: &str) -> AutoRespondConfig {
        AutoRespondConfig {
            enabled: true,
            rules: vec![AutoRespondRule {
                id: "test-safe".to_string(),
                match_pattern: match_pattern.to_string(),
                keys: vec!["1".to_string(), "Enter".to_string()],
                risk: RuleRisk::Safe,
                requires_context_allow: vec![],
                enabled: true,
            }],
            limits: AutoRespondLimits {
                max_auto_responses_per_session: 20,
                max_auto_responses_per_rule_per_hour: 10,
                cooldown_ms_after_response: 5000,
                require_stable_idle_ms: 0,
            },
            notify: AutoRespondNotify {
                telegram_on_every_auto_response: false,
                emit_event_to_decide_loop: true,
            },
        }
    }

    #[test]
    fn disabled_auto_respond_config_is_identical_to_today() {
        // With disabled config, a waiting_input event is emitted normally.
        let frames = vec![
            "● working on it",
            "● working on it\n❯ 1. Proceed?",
            "● working on it\n❯ 1. Proceed?",
            "● working on it\n❯ 1. Proceed?",
        ];
        let mut l = make_loop(frames);
        l.step();
        l.step();
        l.step();
        let r = l.step();
        let event = r.event.expect("event emitted");
        assert_eq!(event.state, PaneState::WaitingInput);
        assert!(matches!(event.reason, DecisionReason::StateTransition));
    }

    #[test]
    fn safe_rule_match_sends_keys_and_emits_auto_responded_event() {
        let sent_keys: Arc<Mutex<Vec<Vec<String>>>> = Arc::new(Mutex::new(vec![]));
        let sent_clone = Arc::clone(&sent_keys);

        let frames: Vec<String> = vec![
            "● working on it".to_string(),
            "● working on it\n❯ 1. Proceed?".to_string(),
            "● working on it\n❯ 1. Proceed?".to_string(),
            // Extra frame returned by TOCTOU re-capture (same content = still matches).
            "● working on it\n❯ 1. Proceed?".to_string(),
        ];
        let mut fi = 0usize;
        let frames_cap = frames.clone();
        let capture_fn = move || {
            let idx = fi.min(frames_cap.len() - 1);
            fi += 1;
            Ok(frames_cap[idx].clone())
        };

        let mut l = WatchLoop::new(WatchLoopDeps {
            session: "s1".to_string(),
            capture_fn,
            send_keys_fn: move |keys: &[String]| {
                sent_clone.lock().unwrap().push(keys.to_vec());
                Ok(())
            },
            patterns: compile_patterns(&patterns()),
            config: config(),
            auto_respond_config: auto_respond_config_with_safe_rule(r"❯\s*\d+\."),
            log_path: None,
        });

        l.step(); // init
        l.step(); // change detected
        l.step(); // settling

        let r = l.step(); // settled
                          // Should have emitted an AutoResponded event (emit_event_to_decide_loop: true).
        let event = r.event.expect("event emitted");
        assert_eq!(event.state, PaneState::WaitingInput);
        assert!(matches!(event.reason, DecisionReason::AutoResponded));
        assert!(event.summary.contains("test-safe"));

        // Keys were sent via the injected mock.
        let keys_sent = sent_keys.lock().unwrap();
        assert_eq!(keys_sent.len(), 1);
        assert_eq!(keys_sent[0], vec!["1", "Enter"]);
    }

    #[test]
    fn toctou_abort_when_content_changes_between_classify_and_recheck() {
        // The TOCTOU re-capture returns different content that no longer matches.
        let frames: Vec<String> = vec![
            "● working on it".to_string(),
            "● working on it\n❯ 1. Proceed?".to_string(),
            "● working on it\n❯ 1. Proceed?".to_string(),
            // TOCTOU re-capture frame: prompt is gone.
            "● working on something else entirely".to_string(),
        ];
        let sent_keys: Arc<Mutex<Vec<Vec<String>>>> = Arc::new(Mutex::new(vec![]));
        let sent_clone = Arc::clone(&sent_keys);
        let mut fi = 0usize;
        let frames_cap = frames.clone();
        let capture_fn = move || {
            let idx = fi.min(frames_cap.len() - 1);
            fi += 1;
            Ok(frames_cap[idx].clone())
        };

        let mut l = WatchLoop::new(WatchLoopDeps {
            session: "s1".to_string(),
            capture_fn,
            send_keys_fn: move |keys: &[String]| {
                sent_clone.lock().unwrap().push(keys.to_vec());
                Ok(())
            },
            patterns: compile_patterns(&patterns()),
            config: config(),
            auto_respond_config: auto_respond_config_with_safe_rule(r"❯\s*\d+\."),
            log_path: None,
        });

        l.step();
        l.step();
        l.step();
        let r = l.step();
        // TOCTOU check failed → no event, no keys sent.
        assert!(r.event.is_none(), "no event when TOCTOU fails");
        assert!(
            sent_keys.lock().unwrap().is_empty(),
            "no keys sent when TOCTOU fails"
        );
    }

    #[test]
    fn send_keys_failure_routes_event_to_llm() {
        let frames: Vec<String> = vec![
            "● working on it".to_string(),
            "● working on it\n❯ 1. Proceed?".to_string(),
            "● working on it\n❯ 1. Proceed?".to_string(),
            "● working on it\n❯ 1. Proceed?".to_string(),
        ];
        let mut fi = 0usize;
        let frames_cap = frames.clone();
        let capture_fn = move || {
            let idx = fi.min(frames_cap.len() - 1);
            fi += 1;
            Ok(frames_cap[idx].clone())
        };

        let mut l = WatchLoop::new(WatchLoopDeps {
            session: "s1".to_string(),
            capture_fn,
            send_keys_fn: |_: &[String]| Err(io::Error::other("simulated send-keys failure")),
            patterns: compile_patterns(&patterns()),
            config: config(),
            auto_respond_config: auto_respond_config_with_safe_rule(r"❯\s*\d+\."),
            log_path: None,
        });

        l.step();
        l.step();
        l.step();
        let r = l.step();
        // Failure → event routed to LLM as a StateTransition, never panics.
        let event = r.event.expect("event still emitted on send-keys failure");
        assert_eq!(event.state, PaneState::WaitingInput);
        assert!(matches!(event.reason, DecisionReason::StateTransition));
    }
}
