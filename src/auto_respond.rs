use crate::changelog::{append_changelog_entry, notify_via_hermes_send, ChangelogEntry};
use crate::types::AutoRespondConfig;
use regex::Regex;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

// ── public re-exports of types defined in types.rs ──────────────────────────
pub use crate::types::{AutoRespondLimits, AutoRespondNotify, AutoRespondRule, RuleRisk};

// ── rate-limit / cooldown state ──────────────────────────────────────────────

/// Mutable runtime state for a single compiled rule.
struct RuleState {
    /// Millisecond timestamps of every successful auto-response in the last hour.
    responses_this_hour: Vec<u128>,
    /// Set to true when the per-rule-per-hour limit was hit for this session.
    suspended: bool,
}

impl RuleState {
    fn new() -> Self {
        Self {
            responses_this_hour: Vec::new(),
            suspended: false,
        }
    }

    /// Returns the number of successful auto-responses in the last 3600 s.
    fn count_in_hour(&mut self, now_ms: u128) -> usize {
        let cutoff = now_ms.saturating_sub(3_600_000);
        self.responses_this_hour.retain(|&t| t >= cutoff);
        self.responses_this_hour.len()
    }

    fn record(&mut self, now_ms: u128) {
        self.responses_this_hour.push(now_ms);
    }
}

// ── compiled rule ─────────────────────────────────────────────────────────────

pub struct CompiledRule {
    pub rule: AutoRespondRule,
    match_re: Regex,
    context_allow_res: Vec<Regex>,
    state: RuleState,
}

fn compile_multiline(source: &str) -> Result<Regex, regex::Error> {
    Regex::new(&format!("(?m){source}"))
}

impl CompiledRule {
    pub fn try_compile(rule: AutoRespondRule) -> Result<Self, String> {
        let match_re = compile_multiline(&rule.match_pattern)
            .map_err(|e| format!("rule {:?} match regex invalid: {e}", rule.id))?;
        let mut context_allow_res = Vec::new();
        for pat in &rule.requires_context_allow {
            let re = compile_multiline(pat).map_err(|e| {
                format!("rule {:?} requiresContextAllow regex invalid: {e}", rule.id)
            })?;
            context_allow_res.push(re);
        }
        Ok(Self {
            rule,
            match_re,
            context_allow_res,
            state: RuleState::new(),
        })
    }

    /// True if this rule (when enabled) matches the given delta text.
    pub fn matches_delta(&self, delta: &str) -> bool {
        self.match_re.is_match(delta)
    }

    /// True if all `requiresContextAllow` patterns match the rolling summary.
    /// Returns `true` vacuously when the list is empty (so `safe` rules work
    /// correctly; `confirm` rules should always have at least one entry).
    pub fn context_allows(&self, summary: &str) -> bool {
        self.context_allow_res.is_empty()
            || self.context_allow_res.iter().any(|re| re.is_match(summary))
    }
}

// ── chrome regression guard ──────────────────────────────────────────────────

/// Returns an error string if `rule.match` would match known persistent
/// status-bar chrome lines. Mirrors the classifier's regression-guard test.
pub fn validate_rule_not_chrome(rule: &AutoRespondRule) -> Result<(), String> {
    let chrome_samples = [
        "[hermes] profile: prod | branch: main | 12:04",
        "session: my-session | 2026-08-27",
        "tmux-watch | working | profile: default",
    ];
    let re = compile_multiline(&rule.match_pattern)
        .map_err(|e| format!("rule {:?}: invalid regex: {e}", rule.id))?;
    for sample in &chrome_samples {
        if re.is_match(sample) {
            return Err(format!(
                "rule {:?} match pattern {:?} matches persistent status-bar chrome {:?}; \
this would cause spurious auto-responses — rejecting the rule",
                rule.id, rule.match_pattern, sample
            ));
        }
    }
    Ok(())
}

// ── top-level handler ─────────────────────────────────────────────────────────

pub struct AutoResponder {
    config: AutoRespondConfig,
    rules: Vec<CompiledRule>,
    /// Total successful auto-responses this process lifetime (session-scoped).
    session_total: usize,
    /// Millisecond timestamp of the last successful auto-response (for cooldown).
    last_response_ms: Option<u128>,
}

fn now_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
}

pub struct AutoRespondOutcome {
    /// Which rule fired (id).
    pub rule_id: String,
    /// The keys that were sent.
    pub keys: Vec<String>,
}

/// Decision returned by `try_auto_respond` before any side effects are applied.
pub enum AutoRespondDecision {
    /// All auto-respond is disabled or there are no rules.
    Disabled,
    /// A rule matched and should fire. Caller must call send_keys, then commit().
    ShouldFire {
        rule_index: usize,
        keys: Vec<String>,
    },
    /// Nothing matched or was eligible — fall through to the LLM.
    NoMatch,
}

impl AutoResponder {
    pub fn new(config: AutoRespondConfig) -> Result<Self, String> {
        let mut rules = Vec::new();
        for rule in &config.rules {
            if !rule.enabled {
                continue;
            }
            validate_rule_not_chrome(rule)?;
            rules.push(CompiledRule::try_compile(rule.clone())?);
        }
        Ok(Self {
            config,
            rules,
            session_total: 0,
            last_response_ms: None,
        })
    }

    /// Returns `Disabled` immediately when `config.enabled` is false (byte-identical
    /// to today's behavior).
    pub fn decide(
        &mut self,
        delta: &str,
        rolling_summary: &str,
        now_ms_override: Option<u128>,
    ) -> AutoRespondDecision {
        if !self.config.enabled {
            return AutoRespondDecision::Disabled;
        }

        let now = now_ms_override.unwrap_or_else(now_ms);

        // Session-wide total limit.
        if self.session_total >= self.config.limits.max_auto_responses_per_session {
            return AutoRespondDecision::NoMatch;
        }

        // Cooldown after last response.
        if let Some(last) = self.last_response_ms {
            if now.saturating_sub(last) < self.config.limits.cooldown_ms_after_response {
                return AutoRespondDecision::NoMatch;
            }
        }

        for (idx, rule) in self.rules.iter_mut().enumerate() {
            if rule.state.suspended {
                continue;
            }
            if !rule.matches_delta(delta) {
                continue;
            }
            // confirm rules require context allowlist.
            if matches!(rule.rule.risk, RuleRisk::Confirm) && !rule.context_allows(rolling_summary)
            {
                continue;
            }
            // Per-rule-per-hour rate limit.
            if rule.state.count_in_hour(now)
                >= self.config.limits.max_auto_responses_per_rule_per_hour
            {
                rule.state.suspended = true;
                return AutoRespondDecision::NoMatch;
            }
            return AutoRespondDecision::ShouldFire {
                rule_index: idx,
                keys: rule.rule.keys.clone(),
            };
        }

        AutoRespondDecision::NoMatch
    }

    /// Called after a successful send_keys to record the auto-response.
    pub fn commit(
        &mut self,
        rule_index: usize,
        keys: &[String],
        changelog_path: Option<&Path>,
        notify_telegram: bool,
        now_ms_override: Option<u128>,
    ) -> AutoRespondOutcome {
        let now = now_ms_override.unwrap_or_else(now_ms);
        let rule = &mut self.rules[rule_index];
        rule.state.record(now);
        self.session_total += 1;
        self.last_response_ms = Some(now);
        let rule_id = rule.rule.id.clone();

        if let Some(path) = changelog_path {
            let entry = ChangelogEntry {
                what: format!(
                    "Auto-responded to waiting_input prompt via rule {:?} (keys: {})",
                    rule_id,
                    keys.join(", ")
                ),
                why: "Auto-respond rule matched the settled delta; zero LLM tokens consumed."
                    .to_string(),
                how_to_replicate:
                    "Configure the same rule in tmux-watch.auto-respond.json for other profiles."
                        .to_string(),
            };
            // Best effort — never crash the watch loop.
            let ts = {
                let ms = now;
                let secs = (ms / 1000) as i64;
                let millis = (ms % 1000) as u32;
                format!("{}-auto-respond-{}ms.{}", secs, millis, rule_id)
            };
            let _ = append_changelog_entry(path, &entry, &ts);
            if notify_telegram {
                notify_via_hermes_send(&entry, "telegram");
            }
        }

        AutoRespondOutcome {
            rule_id,
            keys: keys.to_vec(),
        }
    }

    /// Called when rate-limit suspension is triggered to log it.
    pub fn log_suspension(rule_id: &str, changelog_path: Option<&Path>, limit: usize) {
        let entry = ChangelogEntry {
            what: format!(
                "Auto-respond rule {:?} suspended: per-rule-per-hour limit ({limit}) reached",
                rule_id
            ),
            why: "Rate limit exceeded to prevent runaway auto-responses.".to_string(),
            how_to_replicate:
                "Raise maxAutoResponsesPerRulePerHour in tmux-watch.auto-respond.json if this is expected."
                    .to_string(),
        };
        if let Some(path) = changelog_path {
            let _ = append_changelog_entry(path, &entry, "rate-limit-suspension");
        }
        eprintln!("[tmux-watch] {}", entry.what);
    }
    /// True if the rule at `rule_index` still matches the fresh pane content
    /// (used for the TOCTOU double-check).
    pub fn rule_still_matches(&self, rule_index: usize, fresh_content: &str) -> bool {
        self.rules
            .get(rule_index)
            .map(|r| r.matches_delta(fresh_content))
            .unwrap_or(false)
    }

    pub fn require_stable_idle_ms(&self) -> u64 {
        self.config.limits.require_stable_idle_ms
    }

    pub fn telegram_on_every_auto_response(&self) -> bool {
        self.config.notify.telegram_on_every_auto_response
    }

    pub fn emit_event_to_decide_loop(&self) -> bool {
        self.config.notify.emit_event_to_decide_loop
    }
}

pub fn load_auto_respond_config(
    default_path: &Path,
    override_path: Option<&Path>,
) -> anyhow::Result<AutoRespondConfig> {
    use crate::config::load_auto_respond_value;
    let value = load_auto_respond_value(default_path, override_path)?;
    Ok(serde_json::from_value(value)?)
}

// ── tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{AutoRespondLimits, AutoRespondNotify, AutoRespondRule, RuleRisk};
    use tempfile::TempDir;

    fn default_limits() -> AutoRespondLimits {
        AutoRespondLimits {
            max_auto_responses_per_session: 20,
            max_auto_responses_per_rule_per_hour: 10,
            cooldown_ms_after_response: 5000,
            require_stable_idle_ms: 2000,
        }
    }

    fn default_notify() -> AutoRespondNotify {
        AutoRespondNotify {
            telegram_on_every_auto_response: false,
            emit_event_to_decide_loop: true,
        }
    }

    fn safe_rule(id: &str, match_pattern: &str) -> AutoRespondRule {
        AutoRespondRule {
            id: id.to_string(),
            match_pattern: match_pattern.to_string(),
            keys: vec!["1".to_string(), "Enter".to_string()],
            risk: RuleRisk::Safe,
            requires_context_allow: vec![],
            enabled: true,
        }
    }

    fn confirm_rule(id: &str, match_pattern: &str, ctx: Vec<String>) -> AutoRespondRule {
        AutoRespondRule {
            id: id.to_string(),
            match_pattern: match_pattern.to_string(),
            keys: vec!["y".to_string(), "Enter".to_string()],
            risk: RuleRisk::Confirm,
            requires_context_allow: ctx,
            enabled: true,
        }
    }

    fn config_with_rules(rules: Vec<AutoRespondRule>) -> AutoRespondConfig {
        AutoRespondConfig {
            enabled: true,
            rules,
            limits: default_limits(),
            notify: default_notify(),
        }
    }

    fn disabled_config() -> AutoRespondConfig {
        AutoRespondConfig {
            enabled: false,
            rules: vec![safe_rule("any", r"❯\s*\d+\.")],
            limits: default_limits(),
            notify: default_notify(),
        }
    }

    // ── disabled config ───────────────────────────────────────────────────────

    #[test]
    fn disabled_config_returns_disabled_immediately() {
        let mut ar = AutoResponder::new(disabled_config()).unwrap();
        let delta = "❯ 1. Proceed?";
        let result = ar.decide(delta, "", None);
        assert!(matches!(result, AutoRespondDecision::Disabled));
    }

    // ── safe rule ─────────────────────────────────────────────────────────────

    #[test]
    fn safe_rule_match_returns_should_fire_with_correct_keys() {
        let cfg = config_with_rules(vec![safe_rule(
            "trust-folder",
            r"Do you trust the files in this folder\?",
        )]);
        let mut ar = AutoResponder::new(cfg).unwrap();
        let delta = "Do you trust the files in this folder?";
        let result = ar.decide(delta, "", None);
        match result {
            AutoRespondDecision::ShouldFire { keys, .. } => {
                assert_eq!(keys, vec!["1", "Enter"]);
            }
            _ => panic!("expected ShouldFire"),
        }
    }

    #[test]
    fn safe_rule_no_match_returns_no_match() {
        let cfg = config_with_rules(vec![safe_rule(
            "trust-folder",
            r"Do you trust the files in this folder\?",
        )]);
        let mut ar = AutoResponder::new(cfg).unwrap();
        let result = ar.decide("some other prompt", "", None);
        assert!(matches!(result, AutoRespondDecision::NoMatch));
    }

    // ── confirm rule ──────────────────────────────────────────────────────────

    #[test]
    fn confirm_rule_fires_when_context_matches() {
        let cfg = config_with_rules(vec![confirm_rule(
            "yn-build",
            r"\(y/n\)\s*$",
            vec!["cargo build".to_string()],
        )]);
        let mut ar = AutoResponder::new(cfg).unwrap();
        let delta = "Continue? (y/n)";
        let summary = "Running: cargo build --release";
        let result = ar.decide(delta, summary, None);
        assert!(matches!(result, AutoRespondDecision::ShouldFire { .. }));
    }

    #[test]
    fn confirm_rule_no_fire_when_context_missing() {
        let cfg = config_with_rules(vec![confirm_rule(
            "yn-build",
            r"\(y/n\)\s*$",
            vec!["cargo build".to_string()],
        )]);
        let mut ar = AutoResponder::new(cfg).unwrap();
        let delta = "Continue? (y/n)";
        let summary = "rm -rf /some/dangerous/path"; // not in allowlist
        let result = ar.decide(delta, summary, None);
        assert!(matches!(result, AutoRespondDecision::NoMatch));
    }

    // ── cooldown ──────────────────────────────────────────────────────────────

    #[test]
    fn cooldown_prevents_second_auto_response_within_window() {
        let cfg = config_with_rules(vec![safe_rule("r1", r"Proceed\?")]);
        let mut ar = AutoResponder::new(cfg).unwrap();
        let base_ms: u128 = 1_000_000;
        // First response.
        let r1 = ar.decide("Proceed?", "", Some(base_ms));
        assert!(matches!(r1, AutoRespondDecision::ShouldFire { .. }));
        if let AutoRespondDecision::ShouldFire { rule_index, keys } = r1 {
            ar.commit(rule_index, &keys, None, false, Some(base_ms));
        }
        // Second attempt within cooldown window (1 ms later).
        let r2 = ar.decide("Proceed?", "", Some(base_ms + 1));
        assert!(matches!(r2, AutoRespondDecision::NoMatch));
        // Third attempt after cooldown (5001 ms later).
        let r3 = ar.decide("Proceed?", "", Some(base_ms + 5001));
        assert!(matches!(r3, AutoRespondDecision::ShouldFire { .. }));
    }

    // ── rate limit ────────────────────────────────────────────────────────────

    #[test]
    fn rate_limit_per_rule_per_hour_suspends_rule() {
        let mut limits = default_limits();
        limits.max_auto_responses_per_rule_per_hour = 3;
        let cfg = AutoRespondConfig {
            enabled: true,
            rules: vec![safe_rule("r1", r"Proceed\?")],
            limits,
            notify: default_notify(),
        };
        let mut ar = AutoResponder::new(cfg).unwrap();

        let base_ms: u128 = 1_000_000;
        // Each response is spaced > cooldown (10 s apart) but within an hour.
        for i in 0..3usize {
            let t = base_ms + (i as u128) * 10_000;
            let r = ar.decide("Proceed?", "", Some(t));
            assert!(
                matches!(r, AutoRespondDecision::ShouldFire { .. }),
                "fire {i}"
            );
            if let AutoRespondDecision::ShouldFire { rule_index, keys } = r {
                ar.commit(rule_index, &keys, None, false, Some(t));
            }
        }
        // 4th attempt: limit reached → suspended.
        let r4 = ar.decide("Proceed?", "", Some(base_ms + 30_001));
        assert!(
            matches!(r4, AutoRespondDecision::NoMatch),
            "expected NoMatch after suspension"
        );
    }

    // ── session total cap ─────────────────────────────────────────────────────

    #[test]
    fn session_total_cap_prevents_further_responses() {
        let mut limits = default_limits();
        limits.max_auto_responses_per_session = 2;
        let cfg = AutoRespondConfig {
            enabled: true,
            rules: vec![safe_rule("r1", r"Proceed\?")],
            limits,
            notify: default_notify(),
        };
        let mut ar = AutoResponder::new(cfg).unwrap();

        let base_ms: u128 = 1_000_000;
        for i in 0..2usize {
            let t = base_ms + (i as u128) * 10_000;
            let r = ar.decide("Proceed?", "", Some(t));
            assert!(matches!(r, AutoRespondDecision::ShouldFire { .. }));
            if let AutoRespondDecision::ShouldFire { rule_index, keys } = r {
                ar.commit(rule_index, &keys, None, false, Some(t));
            }
        }
        let r3 = ar.decide("Proceed?", "", Some(base_ms + 30_000));
        assert!(matches!(r3, AutoRespondDecision::NoMatch));
    }

    // ── chrome regression guard ───────────────────────────────────────────────

    #[test]
    fn rule_matching_status_bar_chrome_is_rejected_at_compile_time() {
        // A naive "profile:" regex that would match persistent chrome must be rejected.
        let bad_rule = AutoRespondRule {
            id: "bad".to_string(),
            match_pattern: "profile:".to_string(),
            keys: vec!["Enter".to_string()],
            risk: RuleRisk::Safe,
            requires_context_allow: vec![],
            enabled: true,
        };
        let result = validate_rule_not_chrome(&bad_rule);
        assert!(result.is_err(), "expected chrome guard to reject pattern");
    }

    #[test]
    fn rule_not_matching_chrome_passes_validation() {
        let good_rule = safe_rule("trust", r"Do you trust the files in this folder\?");
        assert!(validate_rule_not_chrome(&good_rule).is_ok());
    }

    // ── multiline forced ──────────────────────────────────────────────────────

    #[test]
    fn compiled_rule_regex_always_has_multiline_flag() {
        let rule = safe_rule("r1", r"^\s*❯\s*\d+\.");
        let cr = CompiledRule::try_compile(rule).unwrap();
        assert!(cr.match_re.as_str().starts_with("(?m)"));
    }

    // ── disabled individual rule ──────────────────────────────────────────────

    #[test]
    fn disabled_rule_is_skipped() {
        let mut rule = safe_rule("r1", r"Proceed\?");
        rule.enabled = false;
        let cfg = config_with_rules(vec![rule]);
        let mut ar = AutoResponder::new(cfg).unwrap();
        // No compiled rules because the only rule is disabled.
        let r = ar.decide("Proceed?", "", None);
        assert!(matches!(r, AutoRespondDecision::NoMatch));
    }

    // ── changelog entry on commit ─────────────────────────────────────────────

    #[test]
    fn commit_writes_changelog_entry() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("changelog.md");
        let cfg = config_with_rules(vec![safe_rule("r1", r"Proceed\?")]);
        let mut ar = AutoResponder::new(cfg).unwrap();
        let t: u128 = 1_000_000_000;
        let r = ar.decide("Proceed?", "", Some(t));
        if let AutoRespondDecision::ShouldFire { rule_index, keys } = r {
            ar.commit(rule_index, &keys, Some(&path), false, Some(t));
        }
        assert!(path.exists());
        let content = std::fs::read_to_string(&path).unwrap();
        assert!(content.contains("r1"));
    }
}
