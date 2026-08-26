#!/usr/bin/env bash
#
# tmux-watch installer — works for any coding agent driven through tmux
# (Claude Code, Codex, OpenCode, Grok, Kimi, ...), orchestrated by Hermes
# or anything else.
#
#   curl -fsSL https://raw.githubusercontent.com/kennedy-f/claude-tmux-watch/main/install.sh | bash -s -- \
#     --home ~/.hermes --profile default --agent claude-code
#
# Flags (all optional):
#   --repo <url>       Git URL to clone (default: this project's origin)
#   --dir <path>       Where to install (default: $HOME/.tmux-watch)
#   --home <path>      A Hermes-style HERMES_HOME to wire a profile into
#                       (equivalent to running profiles/install-profile.sh)
#   --profile <name>   Profile name under --home (default: default)
#   --session-prefix <prefix>  tmux session prefix for autodetect isolation
#   --agent <name>     Preset to use by default (claude-code|codex|opencode|grok|kimi)
#
# Without --home, this just clones + builds the tool and prints how to run
# it manually — no Hermes profile assumed. Any agent/orchestrator can shell
# out to `<dir>/target/release/tmux-watch watch --session <name> --agent <name> --once`.
set -euo pipefail

REPO_URL="${TMUX_WATCH_REPO:-https://github.com/kennedy-f/claude-tmux-watch.git}"
INSTALL_DIR="${TMUX_WATCH_HOME:-$HOME/.tmux-watch}"
HERMES_HOME_ARG=""
PROFILE="default"
SESSION_PREFIX=""
AGENT=""

while [ $# -gt 0 ]; do
  case "$1" in
    --repo) REPO_URL="$2"; shift 2 ;;
    --dir) INSTALL_DIR="$2"; shift 2 ;;
    --home) HERMES_HOME_ARG="$2"; shift 2 ;;
    --profile) PROFILE="$2"; shift 2 ;;
    --session-prefix) SESSION_PREFIX="$2"; shift 2 ;;
    --agent) AGENT="$2"; shift 2 ;;
    *) echo "Unknown flag: $1" >&2; exit 2 ;;
  esac
done

command -v cargo >/dev/null 2>&1 || { echo "cargo (Rust toolchain) is required" >&2; exit 1; }
command -v tmux >/dev/null 2>&1 || { echo "tmux is required" >&2; exit 1; }
command -v git >/dev/null 2>&1 || { echo "git is required" >&2; exit 1; }

if [ -d "$INSTALL_DIR/.git" ]; then
  echo "==> Updating existing install at $INSTALL_DIR"
  git -C "$INSTALL_DIR" pull --ff-only
else
  echo "==> Cloning $REPO_URL into $INSTALL_DIR"
  git clone --depth 1 "$REPO_URL" "$INSTALL_DIR"
fi

echo "==> Building"
(cd "$INSTALL_DIR" && cargo build --release)

if [ -n "$HERMES_HOME_ARG" ]; then
  echo "==> Wiring profile '$PROFILE' into HERMES_HOME=$HERMES_HOME_ARG"
  ARGS=("$PROFILE")
  [ -n "$SESSION_PREFIX" ] && ARGS+=(--session-prefix "$SESSION_PREFIX")
  HERMES_HOME="$HERMES_HOME_ARG" "$INSTALL_DIR/profiles/install-profile.sh" "${ARGS[@]}"
else
  cat <<EOF

==> Installed at $INSTALL_DIR (no Hermes profile wired — none requested via --home).

Run it directly against any tmux session:
  $INSTALL_DIR/target/release/tmux-watch watch --session <name>${AGENT:+ --agent $AGENT} --once

Or wire it into a Hermes profile later:
  $INSTALL_DIR/profiles/install-profile.sh <profile> --session-prefix <prefix>
EOF
fi
