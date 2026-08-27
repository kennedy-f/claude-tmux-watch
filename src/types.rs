use serde::{Deserialize, Serialize};

fn default_max_capture_failures() -> u32 {
    3
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PaneState {
    Working,
    WaitingInput,
    Done,
    Error,
}

impl PaneState {
    pub fn as_str(&self) -> &'static str {
        match self {
            PaneState::Working => "working",
            PaneState::WaitingInput => "waiting_input",
            PaneState::Done => "done",
            PaneState::Error => "error",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PollPhase {
    Working,
    Settling,
    Settled,
}

#[derive(Debug, Clone, Deserialize, Serialize, Default, PartialEq)]
pub struct PatternConfig {
    #[serde(default)]
    pub error: Vec<String>,
    #[serde(default)]
    pub waiting_input: Vec<String>,
    #[serde(default)]
    pub done: Vec<String>,
    #[serde(default)]
    pub working: Vec<String>,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct BackoffConfig {
    pub working_ms: u64,
    pub settling_ms: u64,
    pub settled_ms: u64,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct LogRotationConfig {
    pub max_bytes: u64,
    pub max_files: u32,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct CircuitBreakerConfig {
    pub max_crashes: u32,
    pub window_ms: u64,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct WatchDecideConfig {
    pub settle_window_ms: u64,
    pub backoff: BackoffConfig,
    #[serde(default = "default_max_capture_failures")]
    pub max_capture_failures: u32,
    pub rolling_context_every_n: u32,
    pub log_rotation: LogRotationConfig,
    pub safety_timeout_ms: u64,
    pub circuit_breaker: CircuitBreakerConfig,
    pub telegram_notify_on_auto_improve: bool,
}

#[derive(Debug, Clone)]
pub struct ClassificationResult {
    pub state: PaneState,
    pub matched_pattern: Option<String>,
}

#[derive(Debug, Clone)]
pub struct DeltaResult {
    /// New lines added since the previous snapshot (never the full buffer).
    pub added_lines: Vec<String>,
    pub changed: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DecisionReason {
    StateTransition,
    SafetyTimeout,
    CaptureFailure,
    /// Emitted when the watch loop answered a `waiting_input` prompt itself via
    /// `tmux send-keys` without calling the LLM. The `summary` field of the
    /// accompanying `DecisionEvent` includes the rule id and the keys sent.
    AutoResponded,
}

// ── auto-respond config types ─────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum RuleRisk {
    /// Execute directly when `match` hits the delta.
    Safe,
    /// Execute only when `match` hits the delta AND a `requiresContextAllow`
    /// regex hits the rolling summary.
    Confirm,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AutoRespondRule {
    pub id: String,
    /// Regex applied to the settled delta (multiline forced, same as classifier).
    #[serde(rename = "match")]
    pub match_pattern: String,
    /// argv sequence passed to `tmux send-keys`.
    pub keys: Vec<String>,
    pub risk: RuleRisk,
    /// Required for `confirm` rules: at least one must match the rolling summary.
    #[serde(default)]
    pub requires_context_allow: Vec<String>,
    #[serde(default = "default_true")]
    pub enabled: bool,
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AutoRespondLimits {
    pub max_auto_responses_per_session: usize,
    pub max_auto_responses_per_rule_per_hour: usize,
    pub cooldown_ms_after_response: u128,
    pub require_stable_idle_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AutoRespondNotify {
    pub telegram_on_every_auto_response: bool,
    pub emit_event_to_decide_loop: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AutoRespondConfig {
    pub enabled: bool,
    #[serde(default)]
    pub rules: Vec<AutoRespondRule>,
    pub limits: AutoRespondLimits,
    pub notify: AutoRespondNotify,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DecisionEvent {
    pub session: String,
    pub state: PaneState,
    pub delta: String,
    pub summary: String,
    pub full_log_path: String,
    pub timestamp_ms: u128,
    pub reason: DecisionReason,
}
