#!/usr/bin/env bash
#
# install-profile.sh — wires the tmux-watch watch/decide split into a Hermes
# profile (existing or brand new), on this machine or a fresh one.
#
# Usage:
#   ./install-profile.sh <profile-name> [--session-prefix <prefix>] [--clone-from <profile>]
#
# Examples:
#   ./install-profile.sh default
#   ./install-profile.sh prod --session-prefix prod-
#   ./install-profile.sh newprofile --session-prefix newprofile- --clone-from prod
#
# What it does (idempotent — safe to re-run):
#   1. Builds tmux-watch (cargo build --release) if target/release/tmux-watch is missing.
#   2. Ensures the profile's HERMES_HOME exists (does NOT create the Hermes
#      profile itself — run `hermes profile create <name>` first for a new one).
#   3. Drops a per-profile config override + patterns override next to the
#      profile's config.yaml, seeded from the shared defaults (or cloned from
#      another profile's tuned overrides with --clone-from).
#   4. Creates the profile's logs/ dir (raw tmux logs + changelog live there).
#   5. Prints the exact command to start watching a session under this profile.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
PROFILE_NAME="${1:-}"
SESSION_PREFIX=""
CLONE_FROM=""

if [ -z "$PROFILE_NAME" ]; then
  echo "Usage: $0 <profile-name> [--session-prefix <prefix>] [--clone-from <profile>]" >&2
  exit 2
fi
shift || true

while [ $# -gt 0 ]; do
  case "$1" in
    --session-prefix) SESSION_PREFIX="$2"; shift 2 ;;
    --clone-from) CLONE_FROM="$2"; shift 2 ;;
    *) echo "Unknown flag: $1" >&2; exit 2 ;;
  esac
done

SESSION_PREFIX="${SESSION_PREFIX:-${PROFILE_NAME}-}"
HERMES_HOME="${HERMES_HOME:-$HOME/.hermes}"

if [ "$PROFILE_NAME" = "default" ]; then
  PROFILE_HOME="$HERMES_HOME"
else
  PROFILE_HOME="$HERMES_HOME/profiles/$PROFILE_NAME"
fi

if [ ! -d "$PROFILE_HOME" ]; then
  echo "Profile home not found: $PROFILE_HOME" >&2
  echo "Create the Hermes profile first: hermes profile create $PROFILE_NAME" >&2
  exit 1
fi

echo "==> Profile home: $PROFILE_HOME"

# 1. Build if needed
if [ ! -f "$SCRIPT_DIR/target/release/tmux-watch" ]; then
  echo "==> Building tmux-watch (first run)..."
  (cd "$SCRIPT_DIR" && cargo build --release)
fi

# 2. Config override
CONFIG_OVERRIDE="$PROFILE_HOME/tmux-watch.config.json"
PATTERNS_OVERRIDE="$PROFILE_HOME/tmux-watch.patterns.json"

if [ -n "$CLONE_FROM" ]; then
  SRC_HOME="$HERMES_HOME"
  [ "$CLONE_FROM" != "default" ] && SRC_HOME="$HERMES_HOME/profiles/$CLONE_FROM"
  echo "==> Cloning tuned overrides from profile '$CLONE_FROM'"
  [ -f "$SRC_HOME/tmux-watch.config.json" ] && cp "$SRC_HOME/tmux-watch.config.json" "$CONFIG_OVERRIDE"
  [ -f "$SRC_HOME/tmux-watch.patterns.json" ] && cp "$SRC_HOME/tmux-watch.patterns.json" "$PATTERNS_OVERRIDE"
fi

if [ ! -f "$CONFIG_OVERRIDE" ]; then
  echo "==> Seeding $CONFIG_OVERRIDE (empty override — inherits shared defaults; edit to tune thresholds for this profile/environment)"
  echo "{}" > "$CONFIG_OVERRIDE"
fi

if [ ! -f "$PATTERNS_OVERRIDE" ]; then
  echo "==> Seeding $PATTERNS_OVERRIDE (empty override — inherits shared default patterns; add profile-specific regexes here, never edit config/patterns.default.json in place per profile)"
  echo "{}" > "$PATTERNS_OVERRIDE"
fi

# 3. Session registry entry (used by autodetect/list-sessions to respect profile isolation)
# Rust port has no Node dependency — use python3 (near-universal) instead of
# the original `node -e` snippet. Non-fatal if neither is available: the
# registry is a convenience for autodetect, not required for `watch`/`list-sessions
# --prefixes` to work when the prefix is passed explicitly.
REGISTRY="$HERMES_HOME/profiles.registry.json"
if command -v python3 >/dev/null 2>&1; then
  python3 - "$REGISTRY" "$PROFILE_NAME" "$SESSION_PREFIX" <<'PYEOF'
import json, sys, os

path, profile, prefix = sys.argv[1], sys.argv[2], sys.argv[3]
reg = {}
if os.path.exists(path):
    with open(path) as f:
        reg = json.load(f)
entry = reg.get(profile, {"sessionPrefixes": []})
prefixes = entry.get("sessionPrefixes", [])
if prefix not in prefixes:
    prefixes.append(prefix)
entry["sessionPrefixes"] = prefixes
reg[profile] = entry
with open(path, "w") as f:
    json.dump(reg, f, indent=2)
    f.write("\n")
PYEOF
  echo "==> Registered session prefix '$SESSION_PREFIX' for profile '$PROFILE_NAME' in $REGISTRY"
else
  echo "==> WARNING: python3 not found — skipping profiles.registry.json update." >&2
  echo "    Pass --prefixes '$SESSION_PREFIX' explicitly to 'tmux-watch list-sessions' instead of relying on autodetect." >&2
fi

# 4. Logs dir
mkdir -p "$PROFILE_HOME/logs"

cat <<EOF

==> Done. This profile now uses the watch/decide split instead of naive per-tick LLM polling.

Start a tmux session for this profile using the registered prefix, e.g.:
  tmux new-session -d -s ${SESSION_PREFIX}mytask

Then watch it (blocks until a real decision point, prints one JSON line, exits with --once):
  cd $SCRIPT_DIR && target/release/tmux-watch watch --session ${SESSION_PREFIX}mytask --profile $PROFILE_NAME --once

If the session runs something other than Claude Code, add --agent <name>
(codex|opencode|grok|kimi) to layer that agent's pattern preset from
config/presets/ on top of the shared default — see config/presets/README.md
before trusting an unvalidated preset in production.

Wire this into the Hermes skill's orchestration loop instead of a bare capture-pane poll:
  each decide-loop iteration = one blocking 'tmux-watch watch --once' call, THEN (and only
  then) the LLM reasons about the returned JSON event. Never call the LLM on 'working'.

To validate before switching a profile to production, run in parallel first:
  target/release/tmux-watch watch --session ${SESSION_PREFIX}mytask --profile $PROFILE_NAME --dry-run
  (events land in $PROFILE_HOME/logs/dry-run-events.jsonl instead of driving anything)

To tune thresholds/patterns for this profile only, edit:
  $CONFIG_OVERRIDE
  $PATTERNS_OVERRIDE
(These are deep-merged on top of config/default.config.json and config/patterns.default.json —
never edit the shared defaults for a single profile's quirks.)
EOF
