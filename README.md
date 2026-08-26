# tmux-watch (Rust) — deterministic watch/decide split for any tmux-driven coding agent

This is the Rust implementation of tmux-watch — a straight behavioral port of
the original TypeScript prototype (`scripts/tmux-watch/`). Same tests-as-spec
approach, ported 1:1 module-for-module; if you're digging through the TS
project's history for context on a decision, the rationale below still
applies unchanged.

**Problem:** an LLM orchestrator (Hermes, or your own agent loop) watching a
coding agent's tmux session — Claude Code, Codex, OpenCode, Grok, Kimi,
whatever — typically calls the LLM on **every** polling tick, even while the
agent is still visibly mid-task with nothing to decide. That's redundant LLM
calls plus re-sending accumulated context every round.

**Fix:** a zero-LLM watch loop that only wakes the orchestrator up on a real
decision point (`waiting_input` / `done` / `error`), with a bounded JSON
event instead of the full transcript. Agent-agnostic — the classifier reads
from a config file, not source, so any tmux-driven CLI can be supported by
adding a pattern preset (see `config/presets/`).

## Install

```
curl -fsSL https://raw.githubusercontent.com/kennedy-f/claude-tmux-watch/main/install.sh | bash -s -- \
  --home ~/.hermes --profile default --agent claude-code
```

Drop `--home`/`--profile` to just install the CLI standalone (any
orchestrator, not just Hermes) — see `install.sh` for all flags. This
originated in, and is documented against, a Hermes ↔ Claude Code
integration, but nothing in the watch loop itself is Hermes- or
Claude-Code-specific.

This package splits the orchestration loop into two independent
responsibilities:

- **Watch loop** (`src/watch_loop.rs`) — zero LLM calls. Pure deterministic
  code: capture → content-hash diff → regex classify the new delta only →
  settle/backoff state machine. Runs forever, cheap.
- **Decide loop** — not a separate process; it's just "whatever calls
  `tmux-watch watch --once` and reads the one JSON line it prints." The LLM
  (Hermes) is only invoked when that call returns an `event` — i.e. on
  `waiting_input`, `done`, `error`, or a forced `safety_timeout` check-in.
  Never on `working`.

## Architecture

```
tmux pane
   │  tmux capture-pane -p -J  (no -e: no ANSI, or anchored regexes break)
   ▼
text_diff.rs       content-hash + longest-suffix/prefix overlap diff
   │  (never a saved line offset — tmux history-limit shifts everything
   │   once the scrollback ceiling is hit)
   ▼
classifier.rs       regex classify the NEW DELTA ONLY, priority:
   │                error > waiting_input > done > working
   ▼
state_machine.rs     settle/backoff phases: working(12s) → settling(2s)
   │                 → settled(3s) → back to working, OR emit event
   ▼
watch_loop.rs         orchestrates the above + safety timeout + circuit
   │                 breaker + raw log persistence (log_store.rs, rotated)
   ▼
DecisionEvent        { session, state, delta, summary, fullLogPath, ... }
   │                 summary built by summarizer.rs + rolling_context.rs —
   │                 deterministic regex extraction (PR #s, file paths,
   │                 commands), NEVER an LLM call
   ▼
Hermes/LLM reasons about ONE event, decides what to do next
```

## Why every threshold is config, not a constant

`config/default.config.json` and `config/patterns.default.json` are the
shipped defaults. Every profile (Hermes calls them "profiles" — `default`,
`prod`, etc., each a fully isolated `HERMES_HOME`) can override any of
it without touching the shared files:

```
<profile-home>/tmux-watch.config.json     (deep-merged over config/default.config.json)
<profile-home>/tmux-watch.patterns.json   (deep-merged over config/patterns.default.json)
```

`<profile-home>` is `~/.hermes` for `default`, `~/.hermes/profiles/<name>`
for any named profile.

## Regex pattern corrections already applied

The first pattern draft had three bugs, all fixed here — see
`config/patterns.default.json` and `src/classifier.rs`:

1. **Missing `m` flag.** `compile_patterns()` always forces the multiline
   flag onto every regex, regardless of what's in the config file. Without
   it, `^` only anchors at index 0 of the whole delta string, not the start
   of each line — most patterns silently never matched a multi-line delta.
2. **"Jump to bottom" removed from done-patterns.** That text means the
   terminal view is scrolled, not that a task finished.
3. **No persistent-chrome patterns in `working`.** A status-bar segment
   that renders every single frame (e.g. a `profile:` label) would always
   match and permanently pin classification to `working`, starving
   `done`/`waiting_input` of any chance to win. The `#[cfg(test)]` block at
   the bottom of `src/classifier.rs` has a regression guard for this.

If you're adding a new pattern (see "Self-improvement" below), keep both
invariants: no bare `^`/`$` assumptions that only work single-line, and
no persistent-UI-chrome text.

## Multi-agent presets

`config/patterns.default.json` is validated against real Claude Code TUI
output. Other agents get their own layer instead of forking the default:

```
target/release/tmux-watch watch --session mytask --profile default --agent codex
```

Layer order (last one wins on a given key): shared default →
`config/presets/<agent>.patterns.json` → the profile's own
`tmux-watch.patterns.json` override. See `config/presets/README.md` —
most presets ship as empty placeholders and are **unvalidated** until
someone runs them against real captures of that agent's TUI. Don't trust
an unvalidated preset in production; use `--dry-run` first.

## Settle/backoff, explicit in code

The intervals are a state machine (`src/state_machine.rs`), not prose:

| phase      | meaning                                    | poll interval (default) |
|------------|---------------------------------------------|--------------------------|
| `working`  | no suspected transition, cruising           | `backoff.workingMs` (12s) |
| `settling` | output just changed, confirming it's real   | `backoff.settlingMs` (2s) |
| `settled`  | output stopped moving for `settleWindowMs`  | `backoff.settledMs` (3s), then classify |

A transition is only ever acted on once `settled` is reached — i.e. after
the output stops changing for `settleWindowMs` (default 4s). Landing in
`settled` with a delta that turns out to classify as plain `working` (a
mid-task lull, not a real transition) does **not** emit an event; the next
quiet tick returns straight to the long `working` interval.

## Rolling context, not full transcript resend

`src/rolling_context.rs` accumulates deltas and compacts them **every N
interactions** (`rollingContextEveryN`, default 5) by re-running the same
deterministic extraction over the whole window — never an LLM call. If you
ever swap this for an LLM-based summarizer, that's a deliberate, separate
cost and must be measured apart from the rest of this reduction (see
below) — don't let it hide inside the "50%+ reduction" number.

The raw, full pane output is always persisted separately
(`<profile-home>/logs/tmux-<session>.log`, rotated — see
`src/log_store.rs` / `logRotation` config) — the LLM only ever sees the
bounded `DecisionEvent`, not the raw log.

## Dry-run validation before switching a profile to production

```
target/release/tmux-watch watch --session <name> --profile <profile> --dry-run
```

Events land in `<profile-home>/logs/dry-run-events.jsonl` instead of being
printed to stdout for a decide loop to consume. Run this in parallel to
whatever loop is currently in production. Feed `{predicted, actual}` pairs
(actual = what the old LLM-driven loop decided for the same moment) into
`evaluate_dry_run()` (`src/dry_run.rs`). It graduates once **either**:

- a streak of `minConsecutiveCorrect` (default 20) consecutive correct
  classifications is observed, **or**
- the overall `agreementRate` across the whole sample clears
  `minAgreementRate` (default 0.95).

Only after that should the profile's orchestration actually consume the
non-dry-run event stream.

## Circuit breaker (fallback safety)

If the watch loop crashes, whatever wired it in should fall back to the old
per-tick polling loop — `src/circuit_breaker.rs` tracks crash timestamps in
a sliding window and trips into a fixed degraded state after
`circuitBreaker.maxCrashes` crashes within `circuitBreaker.windowMs`
(default: 3 crashes / 10 min), sending a Telegram notification
(`hermes send --to telegram`) and requiring an explicit reset — it will
never bounce between the two loops forever.

## Session autodetect respects profile isolation

`tmux-watch list-sessions --profile <name> --prefixes <p1,p2>` only ever
returns sessions whose name starts with one of that profile's configured
prefixes (`src/session_discovery.rs`). An empty allowlist returns nothing —
never "everything." Prefixes are tracked per profile in
`~/.hermes/profiles.registry.json`, written by `profiles/install-profile.sh`.
This is what keeps a personal profile from ever auto-attaching to a work
session and vice versa.

## Safety timeout

If `safetyTimeoutMs` (default 25 min) passes with no real state transition
at all (not even entering `settling`), the watch loop forces a
`safety_timeout` DecisionEvent so the decide loop always gets a periodic
check-in even in a fully ambiguous state — it never waits forever.

## Self-improvement protocol (read this before hand-editing patterns in prod)

Hermes (or you) can tune `config/patterns.default.json`,
`config/default.config.json`, or a profile's overrides while the loop is in
use. Every such change must be logged:

```
target/release/tmux-watch changelog add \
  --profile prod \
  --what "Added pattern for Codex-style '? for suggestions' waiting_input prompt" \
  --why "3 false 'working' classifications in prod session prod-deploy, 2026-08-26 — loop never fired a decision event for ~40min" \
  --how "Add the same regex under waiting_input in every other agent using this watch/decide pattern (Codex, OpenCode, ...)" \
  --notify
```

This appends a structured entry to
`<profile-home>/logs/watch-decide-changelog.md` (what / why / how to
replicate elsewhere) and, with `--notify`, sends a Telegram message via
`hermes send --to telegram` — no LLM call, reuses the platform credentials
the profile already has configured. `telegramNotifyOnAutoImprove` in config
controls whether this should happen automatically when Hermes makes the
change itself (Hermes should always pass `--notify` when it does).

## Measuring the reduction (acceptance criterion)

`src/measurement.rs` turns a captured before/after run into the numbers the
acceptance criterion asks for:

```rust
use tmux_watch::measurement::estimate_reduction;

estimate_reduction(EstimateReductionInput {
    task_duration_ms,          // wall-clock length of the test task
    old_poll_interval_ms,      // the old loop's fixed LLM-call interval
    new_decision_event_count,  // count of `event` lines the new loop actually printed
    old_avg_payload_bytes,     // avg size of what got sent to the LLM under the old loop
    new_avg_payload_bytes,     // avg size of a DecisionEvent JSON line
});
// => { old_call_count, new_call_count, call_reduction_rate, payload_reduction_rate, meets_fifty_percent_goal }
```

If a rolling summary is ever generated by an LLM instead of
`summarizer.rs`, log its cost as a separate line item — don't fold it into
`new_avg_payload_bytes` or the reduction number stops being honest.

## Adding a new profile or a brand new environment (VPS, container, ...)

```
./profiles/install-profile.sh <profile-name> [--session-prefix <prefix>] [--clone-from <profile>]
```

This is idempotent and does:
1. Builds `target/release/tmux-watch` if missing (`cargo build --release`).
2. Requires the Hermes profile to already exist (`hermes profile create
   <name>` first, for a new one) — this script only wires tmux-watch into
   an existing `HERMES_HOME`, it doesn't create Hermes profiles.
3. Seeds `tmux-watch.config.json` / `tmux-watch.patterns.json` overrides in
   the profile home (empty = inherit shared defaults; `--clone-from`
   copies another profile's tuned overrides as a starting point instead).
4. Registers the profile's session prefix in
   `~/.hermes/profiles.registry.json` for isolated autodetect.
5. Creates `<profile-home>/logs/`.
6. Prints the exact commands to start watching and to dry-run validate.

Already installed on this machine for `default` and `prod` — see
`~/.hermes/tmux-watch.config.json`, `~/.hermes/profiles/prod/tmux-watch.config.json`,
and `~/.hermes/profiles.registry.json`.

For a brand-new environment (fresh VPS, container, CI box): clone this repo
(or just copy `scripts/tmux-watch-rs/`), install a Rust toolchain (`cargo`),
run `cargo build --release` once, then run `install-profile.sh` per
profile exactly as above — everything else (patterns, thresholds, log
paths) is self-contained under that profile's `HERMES_HOME`.

## Wiring it into a Hermes/Claude-Code orchestration skill

Copy-pasteable version of this for a skill/prompt file:
`integrations/claude-code-skill-snippet.md`.

Instead of:
```
loop:
  capture-pane
  call LLM with full context   # <- redundant most iterations
  sleep 3s
```

do:
```
loop:
  event = run("tmux-watch watch --session <name> --profile <profile> --once")
  # blocks (deterministically, no LLM) until a real decision point
  call LLM with just `event` (bounded JSON, not full transcript)
```

`Hermes continua atuando só como transporte` — the actual reasoning and
tool execution stays exactly where it already was (Hermes/Claude Code);
only the *timing* of when to bother the LLM at all has changed.

## Development

```
cargo build
cargo test          # TDD — write the test before the implementation
cargo build --release
```

There's no separate `test/` tree — `#[cfg(test)] mod tests` blocks live at
the bottom of each `src/*.rs` file, next to the code they cover. Every
module in this package was written test-first per this project's TDD
requirement.
