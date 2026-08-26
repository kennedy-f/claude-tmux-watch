# Rust port contract — fixed public API, do not change signatures

`src/types.rs` and `Cargo.toml` are already written (lead) and frozen. Every
module below must expose EXACTLY this public API so the three worker file
sets compile together without coordination. If a signature here is wrong
for a behavior the TS test requires, ADD a private helper — do not change
the public signature without messaging team-lead first.

Source of truth for behavior: the TS files at
`/Users/kennedyferreiradasilvaribeiro/.hermes/hermes-agent/scripts/tmux-watch/src/*.ts`
and their tests at `.../scripts/tmux-watch/test/*.test.ts`. Read both before
writing each module — the tests pin exact edge cases (timing math, overlap
diff algorithm, rotation byte math, etc). Translate test names 1:1 into
`#[test]` functions in a `#[cfg(test)] mod tests` block at the bottom of
each file — this is TDD, write/port the test before/alongside the impl.

All JSON config field names are camelCase on disk (see `types.rs` serde
rename_all attributes) — do not rename JSON keys.

---

## WORKER A: `src/classifier.rs`, `src/text_diff.rs`, `src/state_machine.rs`

```rust
// classifier.rs
use crate::types::{ClassificationResult, PaneState, PatternConfig};
use regex::Regex;

pub struct CompiledPatterns {
    pub error: Vec<Regex>,
    pub waiting_input: Vec<Regex>,
    pub done: Vec<Regex>,
    pub working: Vec<Regex>,
}

/// Always compiles with multiline mode forced — prefix every pattern source
/// with "(?m)" regardless of what's in the config (mirrors TS forcing the
/// "m" flag). Priority on classify: error > waiting_input > done > working.
pub fn compile_patterns(patterns: &PatternConfig) -> CompiledPatterns { .. }

pub fn classify(delta: &str, compiled: &CompiledPatterns) -> ClassificationResult { .. }
```

```rust
// text_diff.rs
use crate::types::DeltaResult;

pub fn hash_content(content: &str) -> String { .. } // sha256 hex

/// Longest-suffix-of-prev == prefix-of-next overlap diff, line-based, never
/// offset-based (tmux history-limit eviction shifts everything).
pub fn diff_lines(prev_content: &str, next_content: &str) -> DeltaResult { .. }
```

```rust
// state_machine.rs
use crate::types::{BackoffConfig, PollPhase};

pub struct SettleMachine {
    // private fields
}

impl SettleMachine {
    pub fn new(backoff: BackoffConfig, settle_window_ms: u64) -> Self { .. }
    pub fn phase(&self) -> PollPhase { .. }
    pub fn interval_ms(&self) -> u64 { .. }
    pub fn ms_since_last_change(&self) -> u64 { .. }
    pub fn on_poll(&mut self, changed: bool) { .. }
}
```

Behavior spec: `test/classifier.test.ts`, `test/textDiff.test.ts`,
`test/stateMachine.test.ts` — port every test case as a `#[test]`.

---

## WORKER B: `src/summarizer.rs`, `src/rolling_context.rs`, `src/log_store.rs`, `src/circuit_breaker.rs`, `src/session_discovery.rs`, `src/changelog.rs`

```rust
// summarizer.rs
use crate::types::PaneState;

pub fn extract_pr_numbers(text: &str) -> Vec<String> { .. } // unique, first-seen order, from "#123"
pub fn extract_file_paths(text: &str) -> Vec<String> { .. } // unique, extensions: ts,tsx,js,jsx,py,md,json,yaml,yml
pub fn extract_commands(text: &str) -> Vec<String> { .. } // lines starting with "❯ " or "$ ", trimmed, the rest of the line
pub fn summarize(text: &str, state: PaneState) -> String { .. } // deterministic, no LLM — see TS docstring for exact PT-BR phrasing per state
```

```rust
// rolling_context.rs
use crate::types::PaneState;

pub struct RollingContextResult {
    pub summary: String,
    pub compacted: bool,
}

pub struct RollingContextAccumulator {
    // private
}

impl RollingContextAccumulator {
    pub fn new(every_n: u32) -> Self { .. }
    pub fn record(&mut self, delta_text: &str, state: PaneState) -> RollingContextResult { .. }
}
```

```rust
// log_store.rs
use crate::types::LogRotationConfig;
use std::path::Path;

/// Numbered rotation (logrotate-style): .1 is newest rotated, higher number
/// older; evicts the oldest (.max_files) when rotating past the cap.
pub fn append_with_rotation(log_path: &Path, content: &str, config: &LogRotationConfig) -> std::io::Result<()> { .. }
```

```rust
// circuit_breaker.rs
use crate::types::CircuitBreakerConfig;

pub struct CrashResult {
    pub tripped: bool,
    pub crashes_in_window: usize,
}

pub struct CircuitBreaker {
    // private
}

impl CircuitBreaker {
    pub fn new(config: CircuitBreakerConfig) -> Self { .. }
    pub fn record_crash(&mut self, now_ms: u128) -> CrashResult { .. }
    pub fn is_tripped(&self) -> bool { .. }
    pub fn reset(&mut self) { .. }
}
```

```rust
// session_discovery.rs
pub fn parse_tmux_list_sessions(raw: &str) -> Vec<String> { .. } // split lines, trim, drop blanks

/// Empty allowlist -> empty result (explicit opt-in only, never "everything").
pub fn filter_sessions_for_profile(session_names: &[String], allowed_prefixes: &[String]) -> Vec<String> { .. }
```

```rust
// changelog.rs
use std::path::Path;

pub struct ChangelogEntry {
    pub what: String,
    pub why: String,
    pub how_to_replicate: String,
}

pub fn format_changelog_entry(entry: &ChangelogEntry, timestamp_iso: &str) -> String { .. }
pub fn append_changelog_entry(path: &Path, entry: &ChangelogEntry, timestamp_iso: &str) -> std::io::Result<()> { .. }
pub fn build_notify_message(entry: &ChangelogEntry) -> String { .. }

/// Shells out to `hermes send --to <target> <message>` via std::process::Command
/// with an argv array (never a shell string). Logs to stderr on non-zero exit,
/// never panics/propagates — a missed notification must not crash the caller.
pub fn notify_via_hermes_send(entry: &ChangelogEntry, target: &str) { .. }
```

Behavior spec: `test/summarizer.test.ts`, `test/rollingContext.test.ts`,
`test/logStore.test.ts`, `test/circuitBreaker.test.ts`,
`test/sessionDiscovery.test.ts`, `test/changelog.test.ts`.

---

## WORKER C: `src/tmux.rs`, `src/watch_loop.rs`, `src/dry_run.rs`, `src/measurement.rs`, `src/config.rs`, `src/cli.rs`, `src/main.rs`

```rust
// tmux.rs
pub fn build_capture_args(session: &str) -> Vec<String> { .. } // ["capture-pane","-p","-J","-t",session] — never "-e"
pub fn build_list_sessions_args() -> Vec<String> { .. } // ["list-sessions","-F","#{session_name}"]

/// std::process::Command::new("tmux").args(...) — argv array, no shell.
pub fn capture_pane(session: &str) -> std::io::Result<String> { .. }
pub fn list_sessions() -> String { .. } // "" if tmux errors (no server/sessions)
```

```rust
// dry_run.rs
pub struct ClassificationPair { pub predicted: String, pub actual: String }
pub struct DryRunCriteria { pub min_consecutive_correct: u32, pub min_agreement_rate: f64 }
pub struct DryRunResult { pub agreement_rate: f64, pub consecutive_correct: u32, pub ready_to_graduate: bool }

pub fn evaluate_dry_run(pairs: &[ClassificationPair], criteria: &DryRunCriteria) -> DryRunResult { .. }
```

```rust
// measurement.rs
pub struct MeasurementInput {
    pub task_duration_ms: u64,
    pub old_poll_interval_ms: u64,
    pub new_decision_event_count: u32,
    pub old_avg_payload_bytes: u64,
    pub new_avg_payload_bytes: u64,
}
pub struct MeasurementResult {
    pub old_call_count: u64,
    pub new_call_count: u32,
    pub call_reduction_rate: f64,
    pub payload_reduction_rate: f64,
    pub meets_fifty_percent_goal: bool,
}
pub fn estimate_reduction(input: &MeasurementInput) -> MeasurementResult { .. }
```

```rust
// config.rs
use crate::types::{PatternConfig, WatchDecideConfig};
use std::path::Path;

pub fn load_config(default_path: &Path, override_path: Option<&Path>) -> anyhow::Result<WatchDecideConfig> { .. }
pub fn load_patterns(default_path: &Path, override_path: Option<&Path>) -> anyhow::Result<PatternConfig> { .. }

/// Layers: default -> each Some(path) in layer_paths in order, deep-merged
/// as serde_json::Value (missing files skipped silently), THEN deserialized
/// into PatternConfig at the end.
pub fn load_patterns_layered(default_path: &Path, layer_paths: &[Option<&Path>]) -> anyhow::Result<PatternConfig> { .. }
```
NOTE: add `anyhow = "1"` to Cargo.toml `[dependencies]` if you use it (or use `Result<_, Box<dyn std::error::Error>>` instead — either is fine, just be consistent and message team-lead which you picked since cli.rs's `main` needs to match).

```rust
// watch_loop.rs
use crate::classifier::CompiledPatterns;
use crate::types::{DecisionEvent, WatchDecideConfig};

pub struct WatchLoopDeps<F: FnMut() -> std::io::Result<String>> {
    pub session: String,
    pub capture_fn: F,
    pub patterns: CompiledPatterns,
    pub config: WatchDecideConfig,
    pub log_path: Option<std::path::PathBuf>,
}

pub struct WatchStepResult {
    pub interval_ms: u64,
    pub event: Option<DecisionEvent>,
}

pub struct WatchLoop<F: FnMut() -> std::io::Result<String>> {
    // private, holds WatchLoopDeps + internal state (prev_content, pending_delta, machine, rolling_context, ms_since_last_event, last_interval_ms)
}

impl<F: FnMut() -> std::io::Result<String>> WatchLoop<F> {
    pub fn new(deps: WatchLoopDeps<F>) -> Self { .. }
    pub fn step(&mut self) -> WatchStepResult { .. }
}
```
Use a generic `F: FnMut() -> io::Result<String>` for `capture_fn` so tests
can inject a fake queue (mirrors the TS `makeCaptureQueue` test helper) —
do not hardcode `tmux::capture_pane` inside `WatchLoop`; `cli.rs` wires the
real one in via a closure.

```rust
// cli.rs
pub fn run() -> i32 { .. } // parses argv (clap), dispatches subcommands, returns process exit code
```

Subcommands (mirror `src/cli.ts` exactly): `watch --session --profile --agent --dry-run --once`,
`list-sessions --profile --prefixes`, `changelog add --profile --what --why --how --notify`.
Same profile-home resolution logic (`HERMES_HOME` env, `default` = home itself,
else `home/profiles/<name>`), same config/pattern layering (default -> agent
preset from `config/presets/<agent>.patterns.json` -> profile override),
same circuit breaker + crash-notify-and-exit(1) behavior on trip, same
stdout/stderr split (event JSON to stdout in real mode / to a dry-run JSONL
file + stderr in `--dry-run` mode, everything else to stderr).

```rust
// main.rs
fn main() { std::process::exit(tmux_watch::cli::run()); }
```

Behavior spec: `test/tmux.test.ts`, `test/watchLoop.test.ts`,
`test/dryRun.test.ts`, `test/measurement.test.ts`, `test/config.test.ts`
(the last one via `loadPatternsLayered`), and `src/cli.ts` (no test file —
port the subcommand behavior directly, it's mostly argv plumbing).
