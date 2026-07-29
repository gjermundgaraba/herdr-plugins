# Codex Micro layer 2: macOS integration options for Herdr and terminal agents

Research date: **2026-07-25**. Sources are first-party documentation or first-party source repositories unless a limitation is explicitly identified as an inference.

## Recommendation

Use **Work Louder Input → reserved layer-2 chords → Herdr keybindings and Herdr shell actions → a small agent-aware dispatcher**.

This keeps the normal path inside Herdr: no GUI automation, no guessing which terminal pane is active, and no system-wide keyboard interceptor. Herdr already knows the focused pane, the agent occupying it, its lifecycle state, and how to send input to that exact PTY. Its custom shell bindings receive `HERDR_ACTIVE_PANE_ID` and related context. That makes Herdr the right router even though each agent still owns its model, effort, and permission semantics.

```text
Codex Micro layer 2
  │ reserved ctrl+alt chords configured in Work Louder Input
  ▼
Herdr keybinding
  ├─ built-in Herdr action (focus, split, tab, zoom)
  └─ type="shell" action
       │ exact active pane via HERDR_ACTIVE_PANE_ID
       ▼
    agentctl dispatcher
       ├─ Codex adapter ───── native keymap, or app-server JSON-RPC
       ├─ Claude adapter ──── /effort, /model, /plan, keybinding
       └─ Pi adapter ──────── extension command, shortcut, or RPC
```

Keep one macOS Shortcut as an optional global “focus Herdr” entry point. Add Hammerspoon only if controls must work while another app is frontmost. Do **not** make Karabiner-Elements part of the first version: Work Louder currently warns that apps such as Karabiner with Input Monitoring can interfere with Codex Micro communication ([Work Louder setup](https://worklouder.cc/openai-micro-setup)).

## Ranked decision table

Scores are relative: 5 is best, except setup burden where 5 means least work.

| Rank | Architecture | Reliability | Context awareness | Low setup burden | Security / least privilege | Active model / effort control | Decision |
|---:|---|---:|---:|---:|---:|---:|---|
| 1 | Work Louder chords → Herdr native bindings + shell dispatcher | 5 | 5 | 4 | 5 | 4 | Build this first |
| 2 | Rank 1 plus per-agent structured control: Codex app-server, Pi extension/RPC, Claude slash commands | 5 | 5 | 2 | 4 | 5 | Best eventual design |
| 3 | macOS Shortcut → dispatcher / focus Herdr | 4 | 2 | 5 | 4 | 3 | Good global launcher, weak router |
| 4 | Hammerspoon global hotkeys → Herdr CLI dispatcher | 4 | 4 | 3 | 2 | 4 | Add only for cross-app global control |
| 5 | Terminal-emulator API or tmux `send-keys` | 3 | 4 | 2 | 3 | 3 | Fallback if not using Herdr |
| 6 | Karabiner-Elements device filter → dispatcher | 4 | 3 | 2 | 2 | 4 | Avoid for now due device conflict warning |
| 7 | AppleScript / Accessibility UI scripting or literal keyboard macros | 2 | 1 | 3 | 2 | 2 | Last resort only |
| 8 | Reverse-engineered Work Louder RGB/device bridge | 1 | 5 | 1 | 1 | n/a | Experimental only; unsupported and license-sensitive |

## What Herdr already provides

Herdr is more than a terminal multiplexer in this design:

- It detects Codex, Claude Code, and Pi. Pi's installed integration is a lifecycle authority; Claude and Codex integrations provide native session identity while their visible state still comes from screen-manifest detection. Herdr explains why: their hooks do not cover every approval, interrupt, or transition ([agents](https://herdr.dev/docs/agents/), [integrations](https://herdr.dev/docs/integrations/)).
- Its CLI and local socket API can inspect and control workspaces, tabs, panes, and agents; submit an agent prompt atomically; send logical keys; read output; wait on status; and subscribe to events ([socket API](https://herdr.dev/docs/socket-api/), [CLI reference](https://herdr.dev/docs/cli-reference/)).
- `herdr agent prompt <target> <text>` submits bracketed text plus Enter atomically, including while an agent is working. `herdr agent send-keys` handles `enter`, `esc`, arrows, `ctrl+c`, and similar logical keys ([CLI reference](https://herdr.dev/docs/cli-reference/)).
- Custom keybindings support direct chords and `type = "shell"` background commands. Herdr supplies the command with the active workspace, tab, pane, working directory, socket path, and Herdr binary path ([configuration](https://herdr.dev/docs/configuration/)).
- Herdr also has indexed agent focus bindings for slots 1–9 and previous/next-agent navigation. The Agents view query controls the filtered/sorted order those indexed jumps use. That is a direct fit for the six physical Agent Keys without inventing a second registry ([configuration](https://herdr.dev/docs/configuration/), [socket API](https://herdr.dev/docs/socket-api/)).
- A raw client can call `session.snapshot`, then subscribe to pane and agent events. Metadata reports can add display-only model/effort tokens to sidebar rows without taking over Herdr's lifecycle authority ([socket API](https://herdr.dev/docs/socket-api/)).

The important boundary is that a Herdr integration supplies **state, target identity, restore, and transport**. It does not define what “high effort” means to Codex, Claude, or Pi. Product-specific adapters still need to perform that semantic action.

### Minimal Herdr-side shape

Use direct `ctrl+alt` chords because Herdr's own keyboard guide identifies that family as the most consistently available across supported terminals and macOS; plain Option chords are often consumed by macOS character composition ([Herdr keyboard guide](https://herdr.dev/docs/keyboard/)).

```toml
[keys]
focus_pane_left  = ["prefix+h", "ctrl+alt+h"]
focus_pane_down  = ["prefix+j", "ctrl+alt+j"]
focus_pane_up    = ["prefix+k", "ctrl+alt+k"]
focus_pane_right = ["prefix+l", "ctrl+alt+l"]
zoom             = ["prefix+z", "ctrl+alt+z"]

[[keys.command]]
key = "ctrl+alt+u"
type = "shell"
command = "/absolute/path/agentctl effort previous"
description = "decrease active agent effort"

[[keys.command]]
key = "ctrl+alt+i"
type = "shell"
command = "/absolute/path/agentctl effort next"
description = "increase active agent effort"

[[keys.command]]
key = "ctrl+alt+m"
type = "shell"
command = "/absolute/path/agentctl model picker"
description = "open active agent model picker"
```

The dispatcher should capture `HERDR_ACTIVE_PANE_ID` immediately, then query `herdr agent get "$HERDR_ACTIVE_PANE_ID"`. It must not re-resolve “currently focused” after asynchronous work begins, because the user may have changed panes.

## Agent-control capability matrix

| Capability | Codex CLI | Claude Code | Pi |
|---|---|---|---|
| Change model in active native TUI | `/model` picker | `/model [model]`; documented as immediate | `/model`, `Ctrl+L`, or scoped-model cycling |
| Change effort in active native TUI | `/model`, or bind `increase_reasoning_effort` / `decrease_reasoning_effort` directly in `tui.keymap.chat` | `/effort low\|medium\|high\|xhigh\|max\|auto`; immediate, including while responding | `Shift+Tab` cycles thinking; `/settings` also exposes it |
| Exact programmatic model/effort | Codex app-server `thread/settings/update` when the TUI is connected with `codex --remote` | Agent SDK when it launches/owns the process; no documented API attaches to an arbitrary stock TUI | RPC `set_model`, `set_thinking_level`, and `cycle_thinking_level`; extensions can call `setModel` and `setThinkingLevel` |
| Plan / permission mode | `/plan`, `/permissions` | `/plan`; `Shift+Tab` cycles modes; keybindings expose `chat:cycleMode` | Not built in by philosophy; implement as an extension/preset/tool set |
| Approve / decline visible request | Bind native `tui.keymap.approval` actions | Bind `confirm:yes`, `confirm:no`, and permission-dialog actions | No built-in permission popups |
| Interrupt | terminal `Ctrl+C`, app-server `turn/interrupt` | hard-coded `Ctrl+C` | `Esc`; RPC `abort` |
| Best Layer-2 adapter | direct Codex TUI keybindings; app-server for exact set-to-value control | `herdr agent prompt` with exact built-in slash command | small Pi extension for native TUI, or RPC if headless/rehosted |

### Codex

Codex's documented interactive control is `/model`, which changes model and reasoning effort; `/plan`, `/permissions`, `/fast`, `/status`, and other session commands are also available. While Codex is working, a slash command can be queued with `Tab` for the next turn ([Codex CLI slash commands](https://learn.chatgpt.com/docs/developer-commands.md?surface=cli), [Codex models](https://learn.chatgpt.com/docs/models.md)).

More importantly for the physical dial, the current first-party config schema exposes direct `tui.keymap.chat.increase_reasoning_effort` and `decrease_reasoning_effort` actions. Built-in defaults are `Alt+,` / `Shift+Down` to decrease and `Alt+.` / `Shift+Up` to increase. The approval context also exposes `approve`, `approve_for_prefix`, `approve_for_session`, `decline`, `deny`, and `cancel` ([OpenAI Codex keymap source](https://github.com/openai/codex/blob/main/codex-rs/tui/src/keymap.rs#L942-L952), [config schema](https://github.com/openai/codex/blob/main/codex-rs/core/config.schema.json)). Because macOS terminals may turn plain Option chords into composed characters, remap the actions to the same explicit modifier chords used by the Herdr dispatcher:

```toml
[tui.keymap.chat]
decrease_reasoning_effort = "ctrl+alt+u"
increase_reasoning_effort = "ctrl+alt+i"

[tui.keymap.approval]
approve = "ctrl+alt+y"
decline = "ctrl+alt+n"
```

The physical `ctrl+alt+u` is consumed by Herdr's shell binding; the dispatcher then calls `herdr agent send-keys <pane> ctrl+alt+u` directly on the Codex PTY, bypassing macOS Option composition. This is a native action, not picker navigation, and should be the first Codex adapter. It cycles through supported effort values; use app-server only when the hardware must set a named value such as exactly `high`.

For reliable exact set-to-value control, start a dedicated app-server and connect the TUI:

```sh
codex app-server --listen unix://
codex --remote unix://
```

The app-server uses JSON-RPC and exposes thread and turn control. The schema is generated by the installed Codex version, so an integration should generate it rather than hard-code an old copy:

```sh
codex app-server generate-json-schema --experimental --out ./schemas
```

In the locally inspected Codex CLI 0.145.0 schema on 2026-07-25, `thread/settings/update` accepts `model`, `effort`, `personality`, `approvalPolicy`, `permissions`, `sandboxPolicy`, and experimental collaboration mode for subsequent turns. `turn/start` accepts model and effort for the current and subsequent turns. This is the closest CLI equivalent to Codex Micro's native reasoning dial. The server can listen on stdio, local WebSocket, or a Unix socket; plain WebSocket should remain loopback-only and authenticated when exposed ([Codex app-server](https://learn.chatgpt.com/docs/app-server.md)).

Limitation: this requires launching the session through the app-server architecture. A normal standalone Codex TUI supports native increase/decrease bindings, but not a documented external “set effort to high” command. Do not assume a controller can attach to an arbitrary already-running stock TUI or that multiple controlling clients can safely coexist.

### Claude Code

Claude Code has unusually good built-in active-session commands:

- `/effort [level|auto]` accepts `low`, `medium`, `high`, `xhigh`, or `max` when supported. The change takes effect immediately without waiting for a current response to finish.
- `/model [model]` changes the active model and also applies without waiting, though changing model after prior output requires confirmation because history is reread without cached context.
- `/plan`, `/permissions`, `/status`, `/diff`, `/tasks`, and other controls cover much of the remaining Codex-like workflow ([commands](https://code.claude.com/docs/en/commands), [model configuration](https://code.claude.com/docs/en/model-config)).

Therefore an exact Herdr adapter can target the active Claude pane and submit `/effort high` or `/model opus`. This is materially safer than a global typing macro because Herdr resolves the exact agent PTY first.

Claude's `~/.claude/keybindings.json` is useful for direct actions: `chat:modelPicker`, `chat:cycleMode`, `chat:fastMode`, `chat:thinkingToggle`, interrupt, submit, and confirmation actions are customizable by context. It does not currently document direct `set effort high` actions; exact effort is better sent as `/effort high` ([Claude Code keybindings](https://code.claude.com/docs/en/keybindings)).

If a controller launches and owns Claude rather than attaching to the stock TUI, the TypeScript Agent SDK can change model, permission mode, and running-session flag settings including effort. This is a deeper integration with a custom client, not an API for an arbitrary existing CLI session ([Claude Agent SDK](https://code.claude.com/docs/en/agent-sdk/typescript)).

### Pi

Pi's native TUI exposes `/model`, `Ctrl+L` for its model selector, `Ctrl+P` / `Shift+Ctrl+P` for scoped-model cycling, and `Shift+Tab` for thinking-level cycling. Its hot-reloadable keymap exposes model selection/cycling and thinking-level cycling actions. It accepts launch-time `--thinking` values from `off` through `xhigh` ([Pi keybindings](https://pi.dev/docs/latest/keybindings), [Pi coding-agent README](https://github.com/earendil-works/pi/blob/main/packages/coding-agent/README.md)).

Pi is the easiest agent to integrate deeply:

- Extensions can register commands and shortcuts, call `setModel` / `setThinkingLevel`, and expose a small local Unix-socket bridge to control the existing TUI; the repository includes a preset example combining model, tools, and thinking.
- A tiny `codex-micro.ts` extension can register semantic commands such as `/micro-effort-high`, `/micro-effort-next`, and `/micro-model-next`. The Herdr adapter submits those commands to the exact Pi pane.
- For a fully structured controller, `pi --mode rpc` exposes JSONL commands including `get_state`, `set_model`, `cycle_model`, `set_thinking_level`, `cycle_thinking_level`, prompt, steering, follow-up, and abort ([Pi extensions](https://pi.dev/docs/latest/extensions), [Pi RPC](https://pi.dev/docs/latest/rpc)).

RPC is strongest but replaces the normal native TUI with a client-controlled process. Prefer an extension for the first working version.

## macOS integration choices

### Shortcuts: use for one global entry key

A macOS Shortcut can have a global keyboard shortcut and run from any app. The `shortcuts run "Name"` command also supports input and output, but Apple recommends headless workflows because a shortcut that asks for input pauses the command-line process ([run while working](https://support.apple.com/en-md/guide/shortcuts-mac/-apd163eb9f95/mac), [command line](https://support.apple.com/en-ca/guide/shortcuts-mac/apd455c82f02/mac)).

Good use: focus or launch the terminal containing Herdr, then let Herdr-local bindings take over. Poor use: model the whole agent router in a visual Shortcut with prompts and UI automation.

For push-to-talk, keep the native Codex control on layer 1. A cross-agent layer-2 PTT is a separate speech-input feature, not a Herdr agent control: either invoke macOS Dictation while the target composer is focused, or have a global helper record on key-down, stop on key-up, transcribe, then call `herdr agent prompt` on the pane captured at key-down. Work Louder Input documents tap, double-tap, and tap-and-hold actions, while Hammerspoon hotkeys expose separate press/release callbacks ([Work Louder Input](https://worklouder.cc/input), [Hammerspoon hotkeys](https://www.hammerspoon.org/docs/hs.hotkey.html)). The latter requires Microphone plus Accessibility permissions and should not be in the minimum version.

### Hammerspoon: optional global dispatcher

Hammerspoon can bind global hotkeys, identify the frontmost application, run an executable asynchronously with exact arguments, and display a small confirmation HUD ([hotkeys](https://www.hammerspoon.org/docs/hs.hotkey.html), [applications](https://www.hammerspoon.org/docs/hs.application.html), [tasks](https://www.hammerspoon.org/docs/hs.task.html)).

If global actions are required, its job should stay small: hotkey → capture frontmost app → invoke `agentctl` → show success or failure. Herdr, not Hammerspoon, should resolve the pane. Hammerspoon requires broad Accessibility permission, and Secure Input can prevent event interception; it adds privilege and a new always-running component ([Hammerspoon eventtap](https://www.hammerspoon.org/docs/hs.eventtap.html), [FAQ](https://www.hammerspoon.org/faq/)).

### Karabiner-Elements: capable, but currently a poor fit

Karabiner can match the originating hardware by vendor/product ID, filter by frontmost app, and execute shell commands. This is technically excellent for distinguishing the Codex Micro from a normal keyboard ([device conditions](https://karabiner-elements.pqrs.org/docs/json/complex-modifications-manipulator-definition/conditions/device/), [frontmost app conditions](https://karabiner-elements.pqrs.org/docs/json/complex-modifications-manipulator-definition/conditions/frontmost-application/), [shell command](https://karabiner-elements.pqrs.org/docs/json/complex-modifications-manipulator-definition/to/shell-command/)).

However:

- Work Louder explicitly reports Codex Micro communication interference from Karabiner and other Input Monitoring apps.
- Karabiner requires background services, Input Monitoring, Accessibility, and a virtual-HID extension.
- `shell_command` receives a deliberately limited environment unless configured.

The hardware already supplies independent programmable layers, so Karabiner's main advantage is not needed. Re-evaluate only after Work Louder marks the conflict fixed.

### AppleScript and Accessibility: last resort

Apple explains that GUI scripting simulates clicks and keystrokes because not every app exposes the needed scripting commands. It relies on Accessibility, which is disabled by default and granted per controlling app ([Apple UI scripting guide](https://developer.apple.com/library/archive/documentation/LanguagesUtilities/Conceptual/MacAutomationScriptingGuide/AutomatetheUserInterface.html)).

Terminal TUIs do not expose their internal model picker as ordinary macOS controls. AppleScript would mainly inject keys into a terminal window, adding app focus, timing, localization, and Secure Input failure modes. Herdr's PTY APIs are strictly better for this task.

## Terminal, tmux, and PTY options

Herdr already owns the pane and PTY, so a second targeting layer is usually redundant:

- tmux can address stable pane IDs, run `send-keys`, and capture pane text. It is a sensible fallback if the same controller must also support sessions outside Herdr ([tmux Advanced Use](https://github.com/tmux/tmux/wiki/Advanced-Use)).
- WezTerm can enumerate panes and send text to an exact pane; kitty's remote control can target by ID, PID, cwd, command line, environment, or focus state; iTerm2 has a structured Python API for windows, tabs, and sessions ([WezTerm `send-text`](https://wezterm.org/cli/cli/send-text.html), [kitty remote control](https://sw.kovidgoyal.net/kitty/remote-control/), [iTerm2 Python API](https://iterm2.com/python-api/)).
- Kitty remote control is off by default because it can read and control terminal windows. If used, prefer a local socket plus action-restricted authorization, not unrestricted `allow_remote_control yes`.

Do not stack tmux inside Herdr solely for keyboard integration. It creates two pane namespaces and two prefixes without improving the Herdr case.

## Hardware / firmware boundary

Work Louder's supported Codex Micro path is:

- ChatGPT/Codex owns layer 1 and the dynamic Agent Key lighting.
- Work Louder Input maps custom shortcuts to keys, dial movements, and joystick directions on as many as six layers ([Codex Micro product page](https://worklouder.cc/codex-micro), [Codex Micro setup](https://worklouder.cc/openai-micro-setup), [Codex Micro documentation](https://learn.chatgpt.com/docs/features/codex-micro.md)).
- On Codex Micro, the touch sensor cycles the six layers and three device LEDs indicate the selected layer. Work Louder's AppSense can associate a layer with the focused application. Because Herdr runs inside a terminal app, AppSense may see only Terminal/iTerm2/etc.; the first-party page does not claim process or terminal-title matching, so automatic Herdr-layer selection needs a hands-on test ([Codex Micro setup](https://worklouder.cc/openai-micro-setup)).

No first-party Codex Micro documentation found in this investigation exposes a public lighting/device SDK for Herdr. Therefore a supported layer-2 design can reproduce **actions**, but not the six dynamic per-agent RGB states that Codex provides on layer 1. Herdr's sidebar status, notifications, and metadata tokens are the supported feedback path. Treat direct HID/serial reverse engineering or private bundled device packages as unsupported.

An experimental RGB bridge is conceptually possible—subscribe to `pane.agent_status_changed` and translate Herdr `idle` / `working` / `done` / `blocked` to the Codex-like colors—but there are three blockers: Herdr has no exact semantic `error` state, Work Louder publishes no output protocol, and private bundled packages may have redistribution or use restrictions. Do not copy a private package out of Codex or ship a reverse-engineered bridge without first establishing its license, vendor permission, device recovery path, and coexistence with layer 1. Expect firmware/app updates to break it.

Do not assume Work Louder's older VIA/QMK setup pages apply to Codex Micro:

- Work Louder explicitly says the current CM-2 family uses Input and is not VIA/QMK compatible; applying that statement to Codex Micro is a strong hardware-family inference, not a direct Codex Micro compatibility statement ([current Framer CM-2](https://worklouder.cc/framer-creator-micro)).
- Upstream QMK's `work_louder/micro` is the older 16-key, dual-encoder ATmega32U4 device, while Codex Micro has 13 keys, one encoder, and a joystick ([QMK target](https://github.com/qmk/qmk_firmware/tree/master/keyboards/work_louder/micro), [Codex Micro specifications](https://worklouder.cc/codex-micro)).

No current Codex Micro target was found in the first-party QMK/VIA/Vial sources. Flashing the old target is a wrong-hardware operation and could remove the proprietary layer-1 communication and lighting path.

## Security and failure rules

1. **Target first, act second.** Resolve and freeze `HERDR_ACTIVE_PANE_ID`; reject the action if no supported agent occupies that pane.
2. **Prefer semantic actions and APIs.** Use Codex's native effort keymap, Claude's exact built-in command, and Pi extension/RPC before any picker-navigation macro. Use Codex app-server when an exact named value is required.
3. **Make dangerous controls visible.** Approval and denial should only act while Herdr reports `blocked`, and the user should be looking at the target pane. Never implement a global blind “yes.”
4. **Keep the socket local.** Herdr uses a local Unix socket under its config directory. The API can inject input and stop or restructure sessions, so any same-user process with socket access is effectively trusted. This is an inference from the documented control surface; the docs do not describe an additional per-request authorization layer.
5. **Return confirmation.** Report action, target agent/pane, requested value, resulting value when queryable, and failure reason. For the first version, Herdr notification output or a brief Hammerspoon HUD is sufficient.

No effort change can rewrite a provider request already in flight. “Immediate” means the command can be accepted while the agent is working and will affect its next model call or later turn. Normalize the shared hardware labels to **Low / Medium / High**; expose xhigh, max, ultra/ultracode, and thinking-on/off as explicitly agent-specific advanced actions because they are not semantically equivalent across products.

## Phased implementation

1. Configure Layer 2 in Work Louder Input with a reserved chord namespace and implement only Herdr navigation, zoom, split, interrupt, and “focus agent.”
2. Add a Herdr `type="shell"` dispatcher with Codex native effort keys, Claude `/effort`, and a small Pi extension; require an explicit active Herdr agent.
3. Add Codex app-server sessions only if exact named model/effort values are worth the extra architecture. Normal standalone Codex can use native increase/decrease bindings.
4. Add Herdr metadata tokens or a tiny HUD for current agent/model/effort. Add Hammerspoon only if keys must work outside a focused Herdr client.

The first useful milestone is **Layer 2 → direct Herdr bindings plus one agent-aware effort key**. That validates the keyboard chord, Herdr context, and agent adapter before adding the full surface.
