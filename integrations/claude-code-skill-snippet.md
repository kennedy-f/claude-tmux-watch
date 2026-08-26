# Wiring tmux-watch into a Claude Code orchestration skill (e.g. Hermes)

If you have a skill/prompt that tells an orchestrator (Hermes, a custom
agent, etc.) how to monitor a Claude Code tmux session, replace any
"periodically `capture-pane` and reason about it" instructions with this:

---

### Watch/decide split (PREFERRED — do not poll-and-call-LLM per tick)

Naively capturing the pane and reasoning about it on every polling tick
wastes an LLM call and resends accumulated context even while Claude Code
is still visibly mid-task with nothing to decide. Use the `tmux-watch` CLI
instead — it is a zero-LLM, deterministic watch loop that only surfaces a
decision point (JSON, one line) on a real `waiting_input` / `done` /
`error` transition, or a safety-timeout check-in if nothing has happened
for too long:

```
# One blocking call per real decision point — reasons about ONE bounded
# JSON event, never the full transcript.
terminal(command="target/release/tmux-watch watch --session dev --profile <profile> --agent claude-code --once", workdir="<path-to-tmux-watch>")
```

The returned line looks like:
```json
{"session":"dev","state":"waiting_input","delta":"+2 lines desde última captura","summary":"Revisou PRs #1237, #1239. Aguardando confirmação.","fullLogPath":"/home/.../logs/tmux-dev.log","timestampMs":...,"reason":"state_transition"}
```

Only THEN reason about what to do — never call the LLM while the loop is
just blocking on `working`. See the project README for the full
architecture (settle/backoff state machine, config per profile, circuit
breaker fallback, dry-run validation, self-improvement changelog protocol).

If you notice a real session your patterns misclassify, don't hand-patch
around it in the moment — fix the pattern/threshold and log it:
```
target/release/tmux-watch changelog add --profile <profile> --what "..." --why "..." --how "..." --notify
```
This appends to `<profile-home>/logs/watch-decide-changelog.md` and pings
whatever notification channel is configured for that profile — no LLM
call. Never make this kind of tuning change silently.

Note: any status-bar chrome that renders on **every** frame regardless of
state (e.g. a persistent `⏵⏵ bypass permissions on` / `profile:` segment)
must never be added to the `working` patterns — it will starve
`done`/`waiting_input` of any chance to match.

---

This snippet was extracted from a larger internal skill doc — adapt the
`terminal(command=...)` call to whatever tool-invocation syntax your
orchestrator uses.
