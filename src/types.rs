use serde::{Deserialize, Serialize};

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

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DecisionReason {
    StateTransition,
    SafetyTimeout,
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
