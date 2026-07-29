# Cross-agent controls: Codex CLI, Claude Code, and Pi

Evidence snapshot: **2026-07-25**. Sources are first-party documentation, first-party source code, and the locally installed CLIs' own help/version output.

## Bottom line

All three agents can be driven from a programmable keyboard, but they expose different levels of control:

1. **Codex CLI** has the strongest native TUI keymap for the requested controls and a comprehensive app-server protocol.
2. **Claude Code** has excellent remappable TUI controls and true mid-session `/model` and `/effort` commands. Its TypeScript Agent SDK can also apply an effort setting to a running SDK-owned session.
3. **Pi** is the easiest to extend and has the cleanest documented JSONL RPC protocol. Its core deliberately has no sandbox, approval ladder, or built-in plan mode.

Reasoning effort is changeable without restarting in all three:

- **Codex:** direct lower/raise shortcuts or `/model`; app-server settings update for the next turn.
- **Claude Code:** `/effort <level>` takes effect immediately as session state, even while a response is running.
- **Pi:** thinking-cycle shortcut, RPC, SDK, or extension API.

No agent can change the reasoning parameter of an upstream model request that has already been sent. A runtime change affects a later model call: normally the next turn, or a later call in the same multi-call agent loop. To guarantee the whole task uses the new level, interrupt, change effort, and resubmit.

## Versions and local overrides

Commands observed locally:

```text
codex --version   → codex-cli 0.145.0
claude --version  → 2.1.220 (Claude Code)
pi --version      → 0.82.0
```

The user's current keymaps already differ from upstream defaults:

| Agent | Action | Upstream default | Current local binding |
|---|---|---|---|
| Codex | Raise reasoning | `Alt+.` or `Shift+Up` | `Ctrl+Shift+T` |
| Codex | Lower reasoning | `Alt+,` or `Shift+Down` | `Ctrl+T` |
| Pi | Cycle thinking | `Shift+Tab` | `Ctrl+Shift+T` |
| Pi | Model selector | `Ctrl+L` | `Ctrl+Shift+L` |
| Pi | Queue follow-up | `Alt+Enter` | `Shift+Tab` |
| Claude Code | Custom keymap | none found | defaults apply |

This is important for Layer 2: `Ctrl+Shift+T` means **raise one step** in Codex but **cycle to the next level** in Pi. A semantic dispatcher should send agent-specific actions instead of assuming a single chord has identical behavior everywhere.

Codex defaults and action names are verified against the exact [`rust-v0.145.0` keymap source](https://github.com/openai/codex/blob/rust-v0.145.0/codex-rs/tui/src/keymap.rs). Pi's installed package is `@earendil-works/pi-coding-agent@0.82.0`, matching official source commit [`b711e266…`](https://github.com/earendil-works/pi/tree/b711e26616a741a110f092b9dd761e1a7eb30939/packages/coding-agent).

## Control timing matrix

These labels separate two questions that are easy to conflate:

- **Live session**: the setting can be changed without restarting the agent.
- **Next model call**: the new value is read when the agent next sends a provider request; it cannot rewrite a request already sent.
- **Launch/default**: the surface sets initial state for a new process or session.

| Agent and control surface | Timing | Persistence | In-flight request |
|---|---|---|---|
| Codex TUI reasoning shortcuts | Live session; used by a subsequent model call | Session-only in normal chat | Not changed |
| Codex `/model` picker | Live session; used by a subsequent model call | Selected model/effort can be saved | Not changed |
| Codex app-server `thread/settings/update` | Explicitly **next turn** | Loaded thread settings | Not changed |
| Codex config, profile, or `-c model_reasoning_effort=…` | Launch/default | Config persists; `-c` is one invocation | Not applicable until a request starts |
| Claude Code `/effort` | Live session; command is accepted immediately, including while Claude is responding | `low`–`xhigh` can persist; `max`/`ultracode` are session-only | Already-generated or already-requested work is not rewritten; later calls use the new value |
| Claude TypeScript SDK `applyFlagSettings({ effortLevel })` | Live SDK-owned streaming session | Runtime flag layer for that session | Already-sent provider request is not rewritten |
| Claude settings, `--effort`, or `CLAUDE_CODE_EFFORT_LEVEL` | Launch/default | Settings persist; flag/env apply to that invocation | The env value overrides `/effort`, so do not set it for a session that needs live effort buttons |
| Pi TUI shortcut, `/settings`, RPC, SDK, or extension API | Live session; **next provider call** | Current session; settings surfaces can also update defaults | Not changed |
| Pi settings or `--thinking` | Launch/default | Settings persist; flag applies to that invocation | Not applicable until a request starts |

Practical keyboard rule: if the agent is already working and the effort must apply to the entire task, send **interrupt → set exact effort → resubmit**. If changing only subsequent reasoning is acceptable, set effort live and let the current agent loop continue.

## Comparison matrix

Legend: **P** = persistent config, **F** = startup flag, **S** = interactive slash command, **K** = TUI key binding, **API** = structured programmatic control.

| Capability | Codex CLI 0.145.0 | Claude Code 2.1.220 | Pi 0.82.0 |
|---|---|---|---|
| Model | **P/F/S/K/API**: config, `-m`, `/model`, model picker, app-server | **P/F/S/K/API**: settings, `--model`, `/model`, `Alt+P`, SDK `set_model` | **P/F/S/K/API**: settings, `--model`, `/model`, picker/cycle, RPC/SDK |
| Reasoning effort | **P/F/S/K/API**: `model_reasoning_effort`, `-c`, `/model`, direct raise/lower, app-server | **P/F/S/K/API**: `effortLevel`, `--effort`, `/effort`, model-picker slider, TypeScript SDK live flags | **P/F/K/API**: default thinking, `--thinking`, cycle key, RPC/SDK/extension |
| Change effort mid-session | **Yes**; TUI shortcut is session-only; app-server update applies to next-turn settings | **Yes**; `/effort` explicitly works while a response is running | **Yes**; affects the next provider call, not one already streaming |
| Permissions/autonomy | **P/F/S/API**: approval + sandbox policies, `/permissions`, per-thread/turn app-server settings | **P/F/K/API**: permission rules/mode, `Shift+Tab`, SDK `set_permission_mode` | No core approval ladder or sandbox; startup tool allow/deny, runtime extension tool set |
| Plan/mode | `/plan`, `Shift+Tab`, app-server collaboration mode | `/plan`, `Shift+Tab` cycles modes | No core plan mode; install/write an extension |
| Interrupt | `Esc`; app-server `turn/interrupt` | `Esc` or `Ctrl+C`; Agent SDK `interrupt()` | `Esc`; RPC `abort`; SDK `abort()` |
| Submit/newline | `Enter`; newline `Ctrl+J`, `Shift+Enter`, or `Alt+Enter`; remappable | `Enter`; newline `Ctrl+J`/`Shift+Enter`; remappable | `Enter`; newline `Ctrl+J`/`Shift+Enter`; remappable |
| Steer/queue | `Enter` injects into current turn; `Tab` queues next turn | Typed commands queue while responding; status/tasks/usage can run immediately | `Enter` queues steering; follow-up is separately bindable; RPC has `steer`/`follow_up` |
| New/resume/fork | `/new`, `/resume`, `/fork`; CLI subcommands; app-server | `/clear` (`/new` alias), `/resume`, `/branch`, `/fork`; flags | `/new`, `/resume`, `/tree`, `/fork`, `/clone`; flags and RPC |
| Compact/context | `/compact`; `/status`; app-server compact | `/compact`; `/context`; rewind can summarize | `/compact`; auto-compaction settings; RPC `compact` |
| Voice | No ordinary CLI voice command; experimental app-server realtime audio | Built-in `/voice`, push-to-talk/tap, remappable | No built-in voice; extension or external dictation |
| Status | `/status`, `/usage`, status line; app-server events/read APIs | `/status`, `/context`, `/usage`, status line, stream JSON | footer, `/session`; RPC `get_state` and `get_session_stats` |
| Hooks/extensions | Configured lifecycle hooks, MCP, plugins; app-server events | Extensive hooks, skills, plugins, MCP, channels | First-class TypeScript extensions and lifecycle events |
| External control of existing TUI | Experimental remote app-server mode; otherwise targeted keystrokes | Remote Control is a Claude.ai/app surface, not a generic local socket; otherwise targeted keystrokes | No attach socket for an existing TUI; use an extension-owned IPC bridge |
| Best structured integration | Codex app-server | TypeScript Agent SDK; Python SDK uses slash-command fallback for live effort | Pi RPC or embedded SDK |

Primary references: Codex [developer commands](https://learn.chatgpt.com/docs/developer-commands?surface=cli), [app-server protocol](https://github.com/openai/codex/blob/rust-v0.145.0/codex-rs/app-server/README.md); Claude Code [commands](https://code.claude.com/docs/en/commands), [interactive mode](https://code.claude.com/docs/en/interactive-mode), [model configuration](https://code.claude.com/docs/en/model-config); Pi [usage](https://github.com/earendil-works/pi/blob/b711e26616a741a110f092b9dd761e1a7eb30939/packages/coding-agent/docs/usage.md), [RPC](https://github.com/earendil-works/pi/blob/b711e26616a741a110f092b9dd761e1a7eb30939/packages/coding-agent/docs/rpc.md), and [keybindings](https://github.com/earendil-works/pi/blob/b711e26616a741a110f092b9dd761e1a7eb30939/packages/coding-agent/docs/keybindings.md).

## Codex CLI

### Persistent and startup controls

Codex loads personal defaults from `~/.codex/config.toml` and trusted project defaults from `.codex/config.toml`. Relevant keys include `model`, `model_reasoning_effort`, `approval_policy`, `sandbox_mode`, profiles, TUI keymaps, MCP servers, hooks, and status-line fields. CLI `-c key=value` overrides config for one invocation; dedicated flags include `--model`, `--sandbox`, and `--ask-for-approval`. See the [configuration reference](https://learn.chatgpt.com/docs/config-file/config-reference) and [global CLI flags](https://learn.chatgpt.com/docs/developer-commands?surface=cli#global-flags).

Observed `codex --help` also exposes:

```text
-m, --model
-s, --sandbox read-only|workspace-write|danger-full-access
-a, --ask-for-approval untrusted|on-request|never
-c, --config key=value
--remote ws://…|wss://…|unix://…
```

There is no dedicated top-level `--reasoning-effort` flag in 0.145.0. Use `-c 'model_reasoning_effort="high"'` or a profile.

### Interactive controls

The exact 0.145.0 built-in slash-command enum describes `/model` as choosing “what model and reasoning effort to use.” It also includes `/permissions`, `/keymap`, `/plan`, `/new`, `/resume`, `/fork`, `/compact`, `/status`, `/usage`, `/statusline`, `/ps`, and `/stop`. [Exact version source](https://github.com/openai/codex/blob/rust-v0.145.0/codex-rs/tui/src/slash_command.rs).

High-value TUI actions are remappable through `/keymap` and `[tui.keymap]`:

| Function | 0.145.0 default/action |
|---|---|
| Interrupt turn | `Esc` / `chat.interrupt_turn` |
| Raise reasoning | `Alt+.` or `Shift+Up` / `chat.increase_reasoning_effort` |
| Lower reasoning | `Alt+,` or `Shift+Down` / `chat.decrease_reasoning_effort` |
| Submit | `Enter` / `composer.submit` |
| Queue next-turn input | `Tab` / `composer.queue` |
| Newline | `Ctrl+J`, `Shift+Enter`, `Alt+Enter` / `editor.insert_newline` |
| Prompt history search | `Ctrl+R` / `composer.history_search_previous` |
| External editor | `Ctrl+G` / `global.open_external_editor` |
| Status | `/status` |
| Compact | `/compact` |

The reasoning shortcuts step only through efforts advertised by the active model. Raising does not cross into advanced Max/Ultra levels; those require the `/model` advanced-reasoning picker. The change is deliberately **not persisted** in normal chat mode. [Reasoning-shortcut implementation](https://github.com/openai/codex/blob/rust-v0.145.0/codex-rs/tui/src/chatwidget/reasoning_shortcuts.rs).

While Codex works, `Enter` injects instructions into the active turn and `Tab` queues input for the next turn. These are distinct semantics and deserve separate keyboard buttons. [Interactive shortcut reference](https://learn.chatgpt.com/docs/developer-commands?surface=cli#interactive-shortcuts).

### Programmatic interfaces

`codex app-server` is the strongest interface for a Herdr bridge. It is JSON-RPC over stdio by default and also supports websocket listeners. Its v2 protocol includes:

- `thread/start`, `thread/resume`, `thread/fork`, `thread/read`, and `thread/list`
- `turn/start`, `turn/steer`, and `turn/interrupt`
- experimental `thread/settings/update` for the loaded thread's **next-turn** model, reasoning, collaboration, service-tier, approval, and sandbox settings
- `thread/compact/start`
- thread status, token-usage, settings, item, and turn notifications
- experimental realtime text/audio input and output

See the [app-server protocol for 0.145.0](https://github.com/openai/codex/blob/rust-v0.145.0/codex-rs/app-server/README.md). The same lifecycle is also exposed by the Codex SDKs.

Important limitation: `turn/interrupt` stops the active model/agent turn but does not terminate background terminals; the protocol provides separate background-terminal clean/terminate methods.

Codex can also run as an MCP server, but its own interface documentation says new integrations should use the v2 thread/turn APIs, including `turn/interrupt`. [Codex MCP interface](https://github.com/openai/codex/blob/rust-v0.145.0/codex-rs/docs/codex_mcp_interface.md).

### Voice

The ordinary Codex CLI command set has no documented voice/dictation command. The app-server protocol does expose experimental thread-scoped realtime sessions with text or audio output plus audio/text append calls. That is suitable for a purpose-built client, not a simple keystroke macro. [App-server realtime methods](https://github.com/openai/codex/blob/rust-v0.145.0/codex-rs/app-server/README.md).

## Claude Code

### Persistent and startup controls

Claude Code uses scoped settings:

- user: `~/.claude/settings.json`
- project: `.claude/settings.json`
- local project override: `.claude/settings.local.json`
- managed policy above those scopes

The documented precedence is managed → CLI flags → local → project → user. Most settings reload in a running session; `model` is a startup exception, so `/model` is the documented mid-session path. [Settings scopes and reload behavior](https://code.claude.com/docs/en/settings#configuration-scopes).

Relevant startup flags observed in 2.1.220 and documented in the [CLI reference](https://code.claude.com/docs/en/cli-reference):

```text
--model <model>
--effort low|medium|high|xhigh|max
--permission-mode acceptEdits|auto|bypassPermissions|manual|dontAsk|plan
--allowedTools / --disallowedTools / --tools
--continue / --resume / --fork-session / --session-id
--input-format stream-json / --output-format stream-json
--remote-control
```

`effortLevel` persists `low`, `medium`, `high`, or `xhigh`; `max` and `ultracode` are session-only. `CLAUDE_CODE_EFFORT_LEVEL` is startup-read and overrides `/effort`, so a keyboard command cannot override that environment variable in the running session. [Effort configuration](https://code.claude.com/docs/en/model-config#set-the-effort-level), [environment-variable precedence](https://code.claude.com/docs/en/env-vars).

### Interactive controls

Claude's keybindings are customizable in `~/.claude/keybindings.json`; changes apply without restarting. Action IDs allow a keyboard layer to bind semantic operations instead of typing text. [Keybinding reference](https://code.claude.com/docs/en/keybindings).

| Function | Default/action |
|---|---|
| Interrupt | `Esc` or `Ctrl+C` / `app:interrupt` |
| Submit | `Enter` / `chat:submit` |
| Newline | `Ctrl+J` or `Shift+Enter` / `chat:newline` |
| Model picker | `Option+P` / `chat:modelPicker` |
| Toggle thinking | `Option+T` / `chat:thinkingToggle` |
| Cycle permission modes | `Shift+Tab` / `chat:cycleMode` |
| Stop background subagents | `Ctrl+X Ctrl+K` / `chat:killAgents` |
| Toggle transcript | `Ctrl+O` / `app:toggleTranscript` |
| Voice push-to-talk/tap | `Space` / `voice:pushToTalk` |

Claude does not document a global keybinding action for “effort up/down.” The model picker has `modelPicker:decreaseEffort` and `modelPicker:increaseEffort` on Left/Right, but only while the picker is open. Dedicated effort buttons should therefore invoke `/effort low`, `/effort medium`, etc., or open `/model` and navigate the picker.

The key slash commands are:

- `/model [model]`: changes the model and can save it as the default; `s` makes the picker choice session-only
- `/effort [low|medium|high|xhigh|max|ultracode|auto]`
- `/permissions` and `/plan`
- `/clear` (aliases `/reset`, `/new`), `/resume`, `/branch`, and `/fork`
- `/compact [instructions]` and `/context [all]`
- `/status`, `/usage`, `/tasks`, and `/statusline`
- `/voice [hold|tap|off]`

The `/effort` docs explicitly say the change “takes effect immediately without waiting for the current response to finish.” `/model` has the same out-of-band property after any required confirmation. [Commands reference](https://code.claude.com/docs/en/commands).

### Permissions and autonomy

`Shift+Tab` cycles Manual/default → acceptEdits → plan, plus enabled optional modes such as bypass or auto. `dontAsk` is launch-only rather than part of the cycle. Defaults live under `permissions.defaultMode`; startup uses `--permission-mode`. [Permission modes](https://code.claude.com/docs/en/permission-modes).

This is more than an “approval on/off” switch:

- `acceptEdits` auto-approves file edits and common in-workspace filesystem commands.
- `plan` researches and proposes without editing.
- `auto` uses a classifier for actions.
- `dontAsk` denies unapproved actions instead of prompting.
- `bypassPermissions` skips ordinary prompts but does not override every managed or hook restriction.

### Programmatic interfaces

`claude -p` supports JSON and streaming JSON, session IDs, resume/continue, model, effort, and permission flags. From v2.1.205, argument forms of `/model`, `/effort`, `/fast`, `/color`, `/rename`, and `/config key=value` also work in non-interactive mode. [Programmatic usage](https://code.claude.com/docs/en/headless).

The Agent SDK's interactive client exposes:

- `interrupt()`
- `set_model()`
- `set_permission_mode()`
- multi-turn query streaming and tool-permission callbacks

The TypeScript SDK additionally exposes `applyFlagSettings()` on a running streaming session. It accepts an `effortLevel` update, making exact live effort a native structured operation for a TypeScript Herdr adapter. [TypeScript `applyFlagSettings`](https://code.claude.com/docs/en/agent-sdk/typescript#applyflagsettings), [streaming input mode](https://code.claude.com/docs/en/agent-sdk/streaming-vs-single-mode#streaming-input-mode-recommended).

The current official Python SDK does **not** expose the equivalent general `applyFlagSettings()` or a dedicated runtime `set_effort()` method; effort is a startup option. A long-lived Python client can still submit the supported `/effort <level>` command, while retaining native `interrupt()`, `set_model()`, and `set_permission_mode()` controls. [Python client controls](https://code.claude.com/docs/en/agent-sdk/python#claudesdkclient), [SDK option types](https://github.com/anthropics/claude-agent-sdk-python/blob/main/src/claude_agent_sdk/types.py).

Remote Control makes a local session available through claude.ai and the Claude app. It is useful for another human-facing surface, but it is not documented as a generic local IPC socket for a keyboard daemon. [Remote Control](https://code.claude.com/docs/en/remote-control).

For process signals, documented headless `SIGTERM` behavior is a graceful abort: terminate the running command tree, run `SessionEnd` hooks, and exit 143. It ends the process, so the SDK `interrupt()` or TUI `Esc` is preferable for a reusable session. [Programmatic usage](https://code.claude.com/docs/en/headless#background-tasks-at-exit).

### Hooks and status

Claude has the broadest documented hook system of the three. Hooks receive structured JSON containing the session ID, working directory, event, tool, and active effort where applicable. `PreToolUse` can allow, ask, deny, or defer; `Stop` can continue work; `ConfigChange`, `SessionStart`, `SessionEnd`, and other lifecycle events are available. [Hooks guide](https://code.claude.com/docs/en/hooks-guide), [hooks reference](https://code.claude.com/docs/en/hooks).

Status is available interactively through `/status`, `/context`, `/usage`, `/tasks`, and a customizable status line. Stream-JSON `system/init` reports model, tools, MCP servers, plugins, and protocol capabilities.

### Voice

Voice dictation is built in for Claude.ai accounts: `/voice hold`, `/voice tap`, and `/voice off`; the `voice:pushToTalk` action is remappable. This is the only one of the three terminal agents with a documented first-class TUI voice feature. [Voice command](https://code.claude.com/docs/en/commands), [voice keybinding](https://code.claude.com/docs/en/keybindings#voice-actions).

## Pi

### Persistent and startup controls

Pi uses:

- `~/.pi/agent/settings.json` for user defaults
- `.pi/settings.json` for project overrides
- `~/.pi/agent/keybindings.json` for keyboard mappings

Relevant persistent settings include default provider/model/thinking, thinking budgets, enabled model cycle, compaction, message-delivery modes, session directory, extensions, and packages. `/reload` applies keybinding/resource changes without restarting. [Settings reference](https://github.com/earendil-works/pi/blob/b711e26616a741a110f092b9dd761e1a7eb30939/packages/coding-agent/docs/settings.md).

Startup controls include `--provider`, `--model provider/id[:thinking]`, `--thinking`, `--models`, `--continue`, `--resume`, `--session`, `--fork`, `--no-session`, `--name`, and tool allow/deny flags. [CLI/usage reference](https://github.com/earendil-works/pi/blob/b711e26616a741a110f092b9dd761e1a7eb30939/packages/coding-agent/docs/usage.md).

### Interactive controls

Every action below is remappable:

| Function | Default/action ID |
|---|---|
| Submit | `Enter` / `tui.input.submit` |
| Newline | `Shift+Enter` or `Ctrl+J` / `tui.input.newLine` |
| Abort | `Esc` / `app.interrupt` |
| Thinking cycle | `Shift+Tab` / `app.thinking.cycle` |
| Model picker | `Ctrl+L` / `app.model.select` |
| Model next/previous | `Ctrl+P` / `Shift+Ctrl+P` |
| Steering queue | `Enter` while working |
| Follow-up queue | `Alt+Enter` / `app.message.followUp` |
| Retrieve queued message | `Alt+Up` / `app.message.dequeue` |
| New/resume/fork/tree | bindable action IDs; unbound by default |

See the [complete official action table](https://github.com/earendil-works/pi/blob/b711e26616a741a110f092b9dd761e1a7eb30939/packages/coding-agent/docs/keybindings.md).

Core slash commands include `/model`, `/scoped-models`, `/settings`, `/resume`, `/new`, `/session`, `/tree`, `/fork`, `/clone`, `/compact`, and `/reload`. There is no built-in `/thinking <level>` command or directly bindable “set exact thinking level” action. Exact buttons require RPC, the SDK, or a small extension calling `pi.setThinkingLevel(level)`.

### Permissions and modes

Pi intentionally has **no built-in sandbox, permission popups, approval ladder, plan mode, subagents, or background Bash**. It runs with the launching user's OS permissions. Project trust only controls whether project-local settings and extensions load; it is not a runtime sandbox. [Security model](https://github.com/earendil-works/pi/blob/b711e26616a741a110f092b9dd761e1a7eb30939/packages/coding-agent/docs/security.md), [design principles](https://github.com/earendil-works/pi/blob/b711e26616a741a110f092b9dd761e1a7eb30939/packages/coding-agent/docs/usage.md#design-principles).

Available building blocks are:

- startup read-only tools: `--tools read,grep,find,ls`
- startup `--tools`, `--exclude-tools`, `--no-tools`, and `--no-builtin-tools`
- runtime extension `pi.setActiveTools()`
- extension `tool_call` hooks that can block and optionally show a custom confirmation UI
- official example extensions for a permission gate and read-only plan mode

Do not label `/trust`, `--approve`, or `--no-approve` as an autonomy control. They decide whether Pi loads project-owned code/configuration; they do not constrain what active tools can do.

### RPC, SDK, and extensions

`pi --mode rpc` provides strict JSONL commands on stdin with events on stdout. It supports:

- `prompt`, `steer`, `follow_up`, and `abort`
- `new_session`, `switch_session`, `fork`, and `clone`
- `set_model`, `cycle_model`, and model discovery
- `set_thinking_level`, `cycle_thinking_level`, and supported-level discovery
- `compact` and auto-compaction control
- `get_state`, `get_messages`, and `get_session_stats`
- streamed turn, message, tool, model, thinking, and session events

[RPC protocol](https://github.com/earendil-works/pi/blob/b711e26616a741a110f092b9dd761e1a7eb30939/packages/coding-agent/docs/rpc.md).

The RPC `get_state` result includes model, thinking level, streaming/compacting state, delivery modes, session ID/file/name, and pending-message count. This is enough for illuminated key state or a Herdr status display.

RPC is process-owned stdin/stdout; it does not attach to an already-running TUI. For a visible TUI plus structured keyboard commands, build a small Pi extension that:

1. registers shortcuts for exact effort/model/tool presets;
2. calls `pi.setThinkingLevel`, `pi.setModel`, `pi.setActiveTools`, and `ctx.compact`;
3. exposes a carefully permissioned local socket or another explicit IPC channel.

Pi's extension API emits `model_select` and `thinking_level_select` events, and can add commands, flags, shortcuts, tools, UI status, widgets, and tool-call policy. [Extension API](https://github.com/earendil-works/pi/blob/b711e26616a741a110f092b9dd761e1a7eb30939/packages/coding-agent/docs/extensions.md).

`SIGTERM` and `SIGHUP` are graceful shutdown paths, not semantic turn controls. Use TUI `Esc` or RPC `abort`.

### Voice

No built-in voice command, shortcut, setting, RPC method, or microphone API exists in current official Pi docs/source. Voice must come from macOS/Herdr dictation, an external speech-to-text command, or a Pi extension.

## Recommended semantic action contract

The keyboard layer should emit semantic actions and let an agent adapter translate them:

```text
agent.interrupt
input.submit
input.newline
input.steer
input.follow_up
effort.set(low|medium|high|xhigh|max)
effort.raise
effort.lower
model.select
permissions.read_only
permissions.auto
mode.plan
session.new
session.resume
context.compact
status.show
voice.toggle
```

Support levels should be explicit:

| Adapter path | Exact effort preset | Read status back | Control an existing TUI | Risk |
|---|---:|---:|---:|---|
| Focused-terminal keystrokes | Claude: yes via `/effort`; Codex/Pi: relative unless using a picker | No | Yes | Draft/modal/focus sensitive |
| Codex app-server | Yes | Yes | Only sessions hosted/connected through it | Experimental API surface |
| Claude TypeScript Agent SDK | Yes, via `applyFlagSettings` | Stream/events | SDK-owned session | No attach API for an arbitrary existing TUI |
| Claude Python Agent SDK | Slash-command fallback | Stream/events | SDK-owned session | No general live flag-settings method |
| Pi RPC | Yes | Yes | No; owns the process | Headless unless a custom UI is built |
| Pi extension IPC | Yes | Yes, if implemented | Yes | Custom code runs with user permissions |

The safest implementation rule is: **capture the exact target session/pane first, then dispatch, then show the confirmed resulting value**. Avoid blindly typing a slash command when the prompt already contains a draft, a permission dialog is open, or another terminal pane may receive focus.

## Source index

All accessed 2026-07-25.

### OpenAI

- [Codex developer commands and interactive shortcuts](https://learn.chatgpt.com/docs/developer-commands?surface=cli)
- [Codex configuration reference](https://learn.chatgpt.com/docs/config-file/config-reference)
- [Codex CLI 0.145.0 slash-command source](https://github.com/openai/codex/blob/rust-v0.145.0/codex-rs/tui/src/slash_command.rs)
- [Codex CLI 0.145.0 keymap source](https://github.com/openai/codex/blob/rust-v0.145.0/codex-rs/tui/src/keymap.rs)
- [Codex CLI 0.145.0 reasoning-shortcut source](https://github.com/openai/codex/blob/rust-v0.145.0/codex-rs/tui/src/chatwidget/reasoning_shortcuts.rs)
- [Codex CLI 0.145.0 app-server protocol](https://github.com/openai/codex/blob/rust-v0.145.0/codex-rs/app-server/README.md)

### Anthropic

- [Claude Code commands](https://code.claude.com/docs/en/commands)
- [Interactive mode](https://code.claude.com/docs/en/interactive-mode)
- [Keybindings](https://code.claude.com/docs/en/keybindings)
- [Model and effort configuration](https://code.claude.com/docs/en/model-config)
- [Permission modes](https://code.claude.com/docs/en/permission-modes)
- [Settings](https://code.claude.com/docs/en/settings)
- [Programmatic usage](https://code.claude.com/docs/en/headless)
- [Hooks guide](https://code.claude.com/docs/en/hooks-guide)
- [Agent SDK TypeScript `applyFlagSettings`](https://code.claude.com/docs/en/agent-sdk/typescript#applyflagsettings)
- [Agent SDK streaming input mode](https://code.claude.com/docs/en/agent-sdk/streaming-vs-single-mode#streaming-input-mode-recommended)
- [Agent SDK Python client](https://code.claude.com/docs/en/agent-sdk/python#claudesdkclient)
- [Agent SDK Python client source](https://github.com/anthropics/claude-agent-sdk-python/blob/main/src/claude_agent_sdk/client.py)

### Pi

- [Pi usage and CLI reference](https://github.com/earendil-works/pi/blob/b711e26616a741a110f092b9dd761e1a7eb30939/packages/coding-agent/docs/usage.md)
- [Keybindings](https://github.com/earendil-works/pi/blob/b711e26616a741a110f092b9dd761e1a7eb30939/packages/coding-agent/docs/keybindings.md)
- [Settings](https://github.com/earendil-works/pi/blob/b711e26616a741a110f092b9dd761e1a7eb30939/packages/coding-agent/docs/settings.md)
- [RPC protocol](https://github.com/earendil-works/pi/blob/b711e26616a741a110f092b9dd761e1a7eb30939/packages/coding-agent/docs/rpc.md)
- [Extensions](https://github.com/earendil-works/pi/blob/b711e26616a741a110f092b9dd761e1a7eb30939/packages/coding-agent/docs/extensions.md)
- [Security model](https://github.com/earendil-works/pi/blob/b711e26616a741a110f092b9dd761e1a7eb30939/packages/coding-agent/docs/security.md)
