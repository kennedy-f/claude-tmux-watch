# Agent presets

Each `<agent>.patterns.json` here is deep-merged **on top of**
`config/patterns.default.json`, then the profile's own
`tmux-watch.patterns.json` override (if any) is merged on top of that.
Load one with `--agent <name>`:

```
node dist/cli.js watch --session mytask --profile default --agent codex
```

## Validation status

| Preset | Status |
|---|---|
| `claude-code.patterns.json` | **Validated** — grounded in real Claude Code TUI indicators (`❯`, `●`, `⏵⏵ bypass permissions on`, `esc to interrupt`, etc. — see the `claude-code` Hermes skill). This is also what ships as the shared default. |
| `codex.patterns.json` | **Unvalidated placeholder.** Empty (`{}` — inherits the Claude Code default as a starting point). Codex's TUI has different prompts/spinner glyphs; do not trust this in production until it's been checked against real `tmux capture-pane -p -J` output from a Codex session. |
| `opencode.patterns.json` | **Unvalidated placeholder.** Same caveat as above. |
| `grok.patterns.json` | **Unvalidated placeholder.** Same caveat as above. |
| `kimi.patterns.json` | **Unvalidated placeholder.** Same caveat as above. |

## Adding/fixing a preset

Don't guess-and-ship a regex for a tool you haven't seen real output from —
that's exactly the mistake this project's own classifier patterns almost
shipped with (see the main README's "Regex pattern corrections" section).
Instead:

1. Capture 3-4 real sessions covering all four states
   (`working`/`waiting_input`/`done`/`error`) with
   `tmux capture-pane -p -J -t <session>`.
2. Write the regressions in `test/classifier.test.ts`-style tests first
   (TDD) against those captures.
3. Fill in the preset file, always with the `m` flag assumption (the
   loader forces it, but write patterns as if it were literal) and never a
   pattern that matches persistent TUI chrome (status bars, permanent
   banners) — see the main README for why.
4. Log the change via `tmux-watch changelog add` so it's traceable and
   other agents' presets can pick up the same fix if applicable.
