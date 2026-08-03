# Thinking-effort control

Status: implemented and physically verified.

## Question

Can one Herdr plugin action target the focused existing TUI and raise or lower
thinking effort without typing into the frontmost macOS application?

## Design

Herdr supplies `focused_pane_id` and `focused_pane_agent` in
`HERDR_PLUGIN_CONTEXT_JSON`. The action freezes those values, builds one
agent-specific plan, and sends every operation to that exact pane through
`HERDR_BIN_PATH`.

| Agent | Mechanism | Readback |
|---|---|---|
| Codex | `chat.increase_reasoning_effort` / `chat.decrease_reasoning_effort` through user-configured key bindings | Visible Codex status |
| Claude | Open the native `/effort` picker, move left/right, accept | Visible picker/status |
| Pi | Extension-owned `Ctrl+Shift+Left/Right` shortcuts using `getThinkingLevel()` / `setThinkingLevel()` | Extension notification |

## Boundary

- Codex shortcuts are intentionally user-configurable in `effort.json`.
- Claude's picker saves the choice as its default for new sessions.
- Invoke Claude effort changes from an empty prompt; its public interface has no
  direct relative-effort action, so the integration types `/effort`.
- The `setup-pi-effort` action installs the Pi extension in
  `~/.pi/agent/extensions/herdr-micro-effort.ts`; existing sessions need
  `/reload`.
- The changed effort affects later provider calls, not a request already sent.

The [research record](research/README.md) contains the dated physical test
results and tested agent versions. Rust tests cover the planner and installer.

Install the bundled JavaScript extension for a manual test; Pi loads it
directly:

```sh
bin/herdr-micro setup-pi-effort
```

## Sources

- [Claude Code model configuration](https://code.claude.com/docs/en/model-config)
- [Claude Code keyboard shortcuts](https://code.claude.com/docs/en/keybindings)
- Installed Pi extension API:
  `@earendil-works/pi-coding-agent/docs/extensions.md`
