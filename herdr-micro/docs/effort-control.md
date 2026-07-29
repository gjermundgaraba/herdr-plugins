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

## Live result

Tested through Herdr 0.7.5 on 2026-07-26:

| Agent | Version | Proven change |
|---|---:|---|
| Codex | 0.145.0 | `high` → `xhigh` → `high` |
| Claude Code | 2.1.220 | `xhigh` → `high` → `xhigh` |
| Pi | 0.82.1 | `medium` → `high` → `medium` |

The action runner targeted disposable pane IDs directly; it did not rely on the
frontmost macOS window. The Node tests cover operation selection, ordered
delivery, Pi's level boundaries, and extension installation.

The final Codex Micro Layer 2 test physically confirmed the dial integration
for Codex, Claude Code, and Pi.

## Boundary

- Codex shortcuts are intentionally user-configurable in `effort.json`.
- Claude's picker saves the choice as its default for new sessions.
- Invoke Claude effort changes from an empty prompt; its public interface has no
  direct relative-effort action, so the integration types `/effort`.
- The `setup-pi-effort` action installs the Pi extension in
  `~/.pi/agent/extensions/herdr-micro-effort.ts`; existing sessions need
  `/reload`.
- The changed effort affects later provider calls, not a request already sent.

Start Pi with the integration for a manual test:

```sh
node src/setup-pi-effort.mjs
```

## Sources

- [Claude Code model configuration](https://code.claude.com/docs/en/model-config)
- [Claude Code keyboard shortcuts](https://code.claude.com/docs/en/keybindings)
- Installed Pi extension API:
  `@earendil-works/pi-coding-agent/docs/extensions.md`
