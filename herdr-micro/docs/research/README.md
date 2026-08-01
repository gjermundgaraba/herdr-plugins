# Codex Micro layer 2 for Herdr

Research snapshot: **2026-07-25**; lighting capability audit updated
**2026-08-01**.

> **Current implementation (2026-07-27):** [herdr-micro](../..) is physically
> verified on Layer 2 over USB and on firmware `v0.4.1` and `v0.6.1` over BLE.
> Existing BLE pairings can retain stale GATT handles after the `v0.6.1`
> update; pairing a fresh host slot restores the channel. See the
> [wireless evidence](./wireless-bridge-evidence.md). It provides per-agent
> RGB, Agent-key focus, Codex/Claude/Pi effort
> control, sticky app-driven layer switching, and reconnect/sleep recovery.
> The dated documents in this folder preserve the research that led there.

The fresh [lighting capability audit](./lighting-capability-audit.md) records
every known lighting surface, current application and firmware artifacts, and
a physical negative test proving that Codex Micro firmware `v0.4.1` does not
individually address the seven lower Command keys.

## Recommendation

Use the implemented Layer 2 path:

```text
Codex Micro layer 2 containing KV_OAI_AG00…AG05
  → six firmware-mapped Agent LEDs and v.oai.hid key events
  → herdr-micro as the sole vendor-HID owner
  → direct IOKit over USB or BLE
  → Herdr RGB/focus plus Codex CLI, Claude Code, and Pi controls
```

**The Layer 2 RGB restriction has been bypassed on stock firmware.** The gate is not the layer number: firmware scans the active layer for private OAI keycodes. A physical-device test copied Layer 1's `layout` into editable Layer 2 and retained all six live Agent LEDs on Input `0.17.2` / firmware `v0.4.1`. An independent implementation confirmed that `KV_OAI_AG00` through `KV_OAI_AG05` provide the per-slot lighting and vendor events. See [Layer 2 individual-RGB bypass](./layer2-individual-rgb-bypass-research.md).

[`house-of-herdr`](https://github.com/alasano/house-of-herdr/tree/7d8eadaed41a1bb4456565d6bcba8cdb7380b77e/packages/codex-micro) supplied the useful behavioral reference. The local `herdr-micro` plugin now owns the complete device and Herdr integration.

The disposable Layer 3 experiment has now succeeded end to end on this keyboard. On stock firmware `v0.4.1`, the cloned OAI layout:

- rendered six independent `v.oai.thstatus` colors;
- emitted clean `AG00`–`AG05` press/release events;
- displayed live statuses for a mixed set of Codex, Claude, and Pi panes; and
- focused the correct Herdr pane when its Agent key was pressed.

The exact **Herdr + OAI-enabled Layer 2** combination is now physically verified. The remaining seven non-Agent keys can be customized without replacing `AG00`–`AG05`.

Do not flash firmware. Do not run Input, Codex, and the Herdr bridge as competing writers. OAI key presses can also focus/open Codex while it is running, so the reliable experimental profile gives the Herdr bridge sole ownership. Native Codex Layer 1 remains stored and can be used after handing the device back.

The local bridge always yields to Input, yields to Codex only while its window is frontmost, and reclaims the device afterward. Its dial changes effort in the expected clockwise direction for Codex CLI, Claude Code, and Pi.

### Creator Micro 2 Agent Mode

Creator Micro 2 **does** provide the same six-key live RGB experience in its new Agent Mode, but the published implementation is still specifically a **Codex** integration. Firmware `v0.6.1` scopes Codex commands and live Codex lighting to a Codex-enabled layer; Work Louder does not document a third-party status-provider API for Herdr, Claude Code, or Pi.

As of 2026-08-01, Creator Micro 2 firmware `v0.6.1` is stable. A community
hardware test on `v0.6.0-rc.10` also found that its thread IDs `0`–`12` can
light all 13 keys individually. The exact Codex Micro `v0.4.1` test produced a
different result: only IDs `0`–`5` lit. Treat this as a model/firmware
difference, not a portable protocol guarantee. See the
[fresh lighting audit](./lighting-capability-audit.md).

This proves the Creator Micro 2 firmware can isolate dynamic agent lighting to
a designated layer and may expose broader per-key lighting. It still does
**not** provide a supported third-party status-provider API. See
[Creator Micro 2 Agent Mode evidence](./creator-micro-2-agent-mode-evidence.md).

## Supported-input fallback

1. Configure layer 2 in Work Louder Input to emit reserved `ctrl+alt` chords.
2. Bind those chords directly in Herdr for focus, navigation, split, zoom, and agent selection.
3. Add one Herdr shell action for `effort up` and one for `effort down`.
4. Have the shell action inspect the focused agent and dispatch the correct Codex, Claude, or Pi control.

Use this if the OAI-enabled Layer 2 experiment fails or if unsupported vendor-HID access is unacceptable. It needs no Hammerspoon, Karabiner-Elements, AppleScript, or custom firmware, but its lighting is static.

## Codex feature parity

| Codex layer-1 behavior | Herdr layer-2 equivalent | Coverage |
|---|---|---|
| Six agent selectors | Herdr indexed agent focus | Full |
| Agent state feedback | Herdr sidebar, sounds, notifications | Full on screen |
| Per-agent RGB state | OAI-enabled layer + sole-owner `herdr-micro` | Full on Layer 2 for live Codex/Claude/Pi |
| Accept/reject controls | Guarded confirm, back, and interrupt | Partial by design |
| New chat | New pane/tab and agent start | Full |
| Dial changes thinking effort | Agent-aware effort adapter | Full for next model call |
| Joystick runs skills | Pane navigation or four Herdr commands | Full |
| Push-to-talk | Claude voice; Dictation fallback; Hammerspoon for hold/release | Partial to full |
| Custom actions | Herdr shell, popup, pane, and plugin actions | Full |

## What the hardware and Codex integration provide

The Codex Micro has:

| Input/output | Quantity | Useful Herdr role |
|---|---:|---|
| Mechanical keys | 13 | Six agent selectors plus seven commands |
| Touch sensor | 1 | Layer switching |
| Rotary encoder | 1 | Effort down/up; press to open/confirm |
| Planar joystick | 1 | Pane navigation or four custom actions |
| RGB lighting | Per-key plus ambient | Six live slots work on a Layer 2 containing `KV_OAI_AG00`–`AG05` |
| Connection | USB-C and Bluetooth | Both physically verified |

The physical key matrix is `2 + 4 + 4 + 3`. The locked Codex layer assigns the first six keys to agent slots `AG00` through `AG05`, the remaining seven to actions `ACT06` through `ACT12`, the three encoder directions to vendor actions, and the joystick to a vendor radial control. This is visible in the current Work Louder Input app and in the firmware artifacts.

The turnkey layer-1 integration targets the Codex desktop app, not Codex CLI. Layer 2 needs the Herdr/agent adapter described here even when the active terminal agent is Codex CLI.

Work Louder advertises these Codex behaviors:

- six Agent Keys whose colors reflect `idle`, `thinking`, `complete`, `needs input`, and `error`;
- command keys for accepting, rejecting, push-to-talk, new chats, and custom actions;
- a dial that controls how deeply the agent thinks;
- four joystick directions for preset or custom skills;
- six programmable layers through Work Louder Input.

For user layers, Work Louder Input supports ordinary shortcuts, macros/multi-actions, encoder mappings, joystick actions, per-layer lighting, and app-linked layer switching. Export/import support exists, but the reserved Codex layer and vendor actions are not an open protocol.

Sources: [Codex Micro product page](https://worklouder.cc/codex-micro), [Codex Micro setup](https://worklouder.cc/openai-micro-setup), [Work Louder Input](https://worklouder.cc/input).

### Important firmware boundary

Codex layer 1 does **not** emit ordinary shortcuts. It uses proprietary `KV_OAI_*` firmware actions and is locked in Work Louder Input with the instruction to edit it in Codex.

Layer 2 should therefore use ordinary HID key chords. Do not flash QMK/VIA firmware:

- current firmware is an ESP32-S3/ESP-IDF stack with Work Louder JSON keymaps;
- no official QMK, VIA, or Vial source/definition exists for Codex Micro;
- generic firmware could remove the vendor actions and the layer-1 lighting/control path.

The public firmware release that adds Codex support describes assignable Codex commands and live lighting, but it publishes binaries rather than source or a public protocol. See [Work Louder firmware v0.6.1](https://github.com/worklouder/cm-v2-fw-releases/releases/tag/v0.6.1).

## What Herdr provides

Herdr is a terminal-native agent multiplexer with this model:

```text
session → workspace → tab → pane → recognized agent
```

Each pane is a real PTY. Agents continue running after the client detaches, Herdr can restore native agent sessions after a server restart, and its sidebar rolls agent state up through panes, tabs, and workspaces. Its recognized states are `blocked`, `working`, `done`, `idle`, and `unknown`.

Herdr already has the control surfaces needed for the keyboard:

| Surface | What it adds |
|---|---|
| Direct keybindings | Prefix-free focus, navigation, split, zoom, tabs, workspaces, agent selection |
| Custom commands | Background shell, popup, temporary pane, or plugin action |
| CLI | Agent/pane list, get, focus, prompt, send-keys, read, wait, start, split, resize, and move |
| Socket API | Session snapshots, the same commands, event subscriptions, and state waits |
| Plugins | Reusable actions, event hooks, panes, and link handlers |
| Metadata | Sidebar tokens such as model, effort, or a short task summary |

Custom commands receive the exact active context:

```text
HERDR_ACTIVE_WORKSPACE_ID
HERDR_ACTIVE_TAB_ID
HERDR_ACTIVE_PANE_ID
HERDR_ACTIVE_PANE_CWD
HERDR_SOCKET_PATH
HERDR_BIN_PATH
```

This is the core integration point. A hardware chord can trigger a Herdr command that freezes the active pane ID, identifies the agent in it, and sends the appropriate semantic action to that exact PTY.

Sources: [Herdr keyboard guide](https://herdr.dev/docs/keyboard/), [configuration](https://herdr.dev/docs/configuration/), [agent automation](https://herdr.dev/docs/agent-automation/), [CLI reference](https://herdr.dev/docs/cli-reference/), [socket API](https://herdr.dev/docs/socket-api/).

### Agent detection and restore

Install the three official integrations:

```sh
herdr integration install pi
herdr integration install claude
herdr integration install codex
herdr integration status
```

Their depth differs:

| Agent | State authority | Integration contribution |
|---|---|---|
| Pi | Lifecycle extension when installed | State and native session |
| Claude Code | Herdr screen manifest | Native session identity |
| Codex | Herdr screen manifest | Native session identity |

Claude and Codex hooks intentionally do not author lifecycle state because their hook streams miss some permission results, interrupts, and transitions. Herdr uses their visible bottom-buffer UI instead. Sources: [Herdr agents](https://herdr.dev/docs/agents/), [integrations](https://herdr.dev/docs/integrations/).

### The key limitation

Herdr normalizes **identity, lifecycle state, targeting, persistence, and terminal transport**. It does not normalize model names, reasoning levels, permission modes, or voice. Those semantics still belong to each agent.

## Cross-agent controls

Local versions inspected:

```text
Codex CLI   0.145.0
Claude Code 2.1.220
Pi          0.82.0
```

### Capability summary

| Control | Codex CLI | Claude Code | Pi |
|---|---|---|---|
| Model in live TUI | `/model`; model picker | `/model`; `Option+P` | `/model`; model picker/cycle |
| Effort in live TUI | Direct effort up/down or `/model` | `/effort <level>` | Thinking-cycle action |
| Exact structured control | App-server `thread/settings/update` | Agent SDK plus `/effort` fallback | RPC or extension API |
| Plan/permission | `/plan`, `/permissions` | `/plan`, mode cycle, SDK permission setter | Extension/tool preset; no core mode |
| Interrupt | `Esc` or app-server | `Esc`/`Ctrl+C` or SDK | `Esc` or RPC abort |
| Voice | No ordinary CLI voice | First-party `/voice` | External dictation/extension |
| Status feedback | TUI/status line/app-server events | Status line/hooks/stream events | RPC state/extension events |

### Reasoning-effort behavior

Codex:

- `chat.increase_reasoning_effort` and `chat.decrease_reasoning_effort` are remappable TUI actions.
- `/model` changes the model and reasoning effort.
- App-server `thread/settings/update` changes next-turn settings deterministically.
- The ordinary CLI has no dedicated `--reasoning-effort` flag; use `-c 'model_reasoning_effort="high"'` at launch.

Claude Code:

- `/effort low|medium|high|xhigh|max|auto` changes the live session immediately, even while a response is running.
- `CLAUDE_CODE_EFFORT_LEVEL` overrides `/effort`; do not set it in a launcher if the dial must work.
- Exact effort can also be changed in a TypeScript Agent SDK session with live flag settings.
- `/model` and `Option+P` handle model selection.

Pi:

- The thinking-cycle action changes thinking mid-session.
- RPC and the extension API can set an exact thinking level.
- A new value affects the next provider request, not one already streaming.
- Pi has no built-in approval ladder or sandbox; tool restrictions and plan/read-only modes need an extension or launch preset.

No agent can rewrite a reasoning parameter already sent to a model provider. To guarantee an entire task restarts at the new effort: interrupt, change effort, then resubmit.

Sources: Codex [keymap source for 0.145.0](https://github.com/openai/codex/blob/rust-v0.145.0/codex-rs/tui/src/keymap.rs) and [app-server protocol](https://github.com/openai/codex/blob/rust-v0.145.0/codex-rs/app-server/README.md); Claude Code [commands](https://code.claude.com/docs/en/commands), [model configuration](https://code.claude.com/docs/en/model-config), and [Agent SDK](https://code.claude.com/docs/en/agent-sdk/typescript); Pi [keybindings](https://github.com/earendil-works/pi/blob/b711e26616a741a110f092b9dd761e1a7eb30939/packages/coding-agent/docs/keybindings.md), [RPC](https://github.com/earendil-works/pi/blob/b711e26616a741a110f092b9dd761e1a7eb30939/packages/coding-agent/docs/rpc.md), and [extensions](https://github.com/earendil-works/pi/blob/b711e26616a741a110f092b9dd761e1a7eb30939/packages/coding-agent/docs/extensions.md).

### Local keymap differences

The current machine already overrides important defaults:

| Agent action | Upstream default | Current local binding |
|---|---|---|
| Codex effort up | `Alt+.` or `Shift+Up` | `Ctrl+Shift+T` |
| Codex effort down | `Alt+,` or `Shift+Down` | `Ctrl+T` |
| Pi thinking cycle | `Shift+Tab` | `Ctrl+Shift+T` |
| Pi model selector | `Ctrl+L` | `Ctrl+Shift+L` |
| Pi follow-up queue | `Alt+Enter` | `Shift+Tab` |

This proves why the keyboard should emit **semantic hardware actions** into a dispatcher instead of assuming one terminal chord means the same thing in every agent.

## Proposed layer-2 layout

The goal is to preserve the physical meaning of Codex layer 1 while making it Herdr-centric.

```text
             [ dial: effort − / + ; press: model/confirm ]      [ joystick ]

                       [ Agent 1 ] [ Agent 2 ]
             [ Agent 3 ] [ Agent 4 ] [ Agent 5 ] [ Agent 6 ]
             [ Confirm ] [ Back    ] [ Interrupt ] [ Voice    ]
     [ touch/layer ]      [ New     ] [ Controls  ] [ Attention]
```

### Six Agent Keys

Map them to direct indexed agent focus:

```toml
[keys]
focus_agent = ["prefix+alt+1..9", "ctrl+alt+1..9"]
```

Work Louder layer 2 emits `ctrl+alt+1` through `ctrl+alt+6`. Herdr then focuses the corresponding visible agent. If fixed numbers prove too positional, switch the first two keys to `previous_agent`/`next_agent` and keep four indexed choices.

### Seven command keys

| Physical role | Semantic action | Guard |
|---|---|---|
| Confirm | Send `enter` to the exact active agent | Only if a recognized agent owns the pane |
| Back | Send `esc` | Always safe; report target |
| Interrupt | Agent-specific interrupt | Require a recognized active agent |
| Voice | Claude native voice; macOS Dictation elsewhere | Show recording state |
| New | New tab/shell, then start selected/default agent | Do not replace a busy pane |
| Controls | Open model/effort control or show status | Non-destructive |
| Attention | Focus next `blocked`, then `done`, agent | Never auto-approve |

“Confirm” and “Back” are deliberately weaker labels than “Accept” and “Reject.” Approval dialogs differ across agents, and a universal blind “yes” would be unsafe.

### Joystick

Use it for pane focus:

```toml
[keys]
focus_pane_left  = ["prefix+h", "ctrl+alt+h"]
focus_pane_down  = ["prefix+j", "ctrl+alt+j"]
focus_pane_up    = ["prefix+k", "ctrl+alt+k"]
focus_pane_right = ["prefix+l", "ctrl+alt+l"]
```

Do not use `ctrl+alt+arrow`: Herdr documents conflicts with desktop and terminal shortcuts. The letter chords are its recommended portable family.

### Dial

Recommended behavior:

- counter-clockwise: `effort previous`;
- clockwise: `effort next`;
- press: open model/effort controls or confirm the visible picker.

The dispatcher should use:

| Agent | Down/up implementation |
|---|---|
| Codex | Native decrease/increase reasoning action; app-server for exact value |
| Claude | Track known level, then submit exact `/effort <level>` |
| Pi | Native thinking-cycle for one direction; exact extension/RPC setter for both directions |

Encoder ticks can arrive quickly. Coalesce repeated ticks for 50–100 ms so the Herdr shell binding does not start a process per detent during a fast spin.

## Minimal Herdr configuration shape

Use Herdr’s recommended direct-chord family, then call one absolute-path dispatcher:

```toml
[keys]
focus_agent      = ["prefix+alt+1..9", "ctrl+alt+1..9"]
focus_pane_left  = ["prefix+h", "ctrl+alt+h"]
focus_pane_down  = ["prefix+j", "ctrl+alt+j"]
focus_pane_up    = ["prefix+k", "ctrl+alt+k"]
focus_pane_right = ["prefix+l", "ctrl+alt+l"]

[[keys.command]]
key = "ctrl+alt+comma"
type = "shell"
command = "/absolute/path/agentctl effort previous"
description = "decrease active agent effort"

[[keys.command]]
key = "ctrl+alt+period"
type = "shell"
command = "/absolute/path/agentctl effort next"
description = "increase active agent effort"

[[keys.command]]
key = "ctrl+alt+m"
type = "shell"
command = "/absolute/path/agentctl controls"
description = "open active agent controls"
```

`agentctl` should capture `HERDR_ACTIVE_PANE_ID` before doing any asynchronous work:

```text
hardware action
  → frozen pane ID
  → `herdr agent get <pane>`
  → identify codex / claude / pi
  → structured API if available
  → guarded `herdr agent send-keys` or `agent prompt` fallback
  → report result through a Herdr notification or metadata token
```

## Voice options

### Recommended first version

Use Claude Code’s first-party voice in Claude panes and macOS Dictation elsewhere.

- Claude supports `/voice hold`, `/voice tap`, `/voice off`, and a remappable `voice:pushToTalk` action.
- macOS Dictation can start and stop from a customized keyboard shortcut anywhere text input is active.
- Pi has no built-in voice feature.
- The ordinary Codex CLI has no documented voice command.

Sources: [Claude voice dictation](https://code.claude.com/docs/en/voice-dictation), [Apple Dictation](https://support.apple.com/guide/mac-help/use-dictation-mh40584/26/mac/26).

### True push-to-talk across all agents

Work Louder Input can emit a key chord, but a terminal normally cannot act on key release. A small Hammerspoon hotkey can receive press and release callbacks:

- press: start recording;
- release: transcribe;
- freeze the Herdr pane ID from press time;
- submit the transcript with `herdr agent prompt`.

Add this only after ordinary controls work. Hammerspoon requires Accessibility permission and Secure Input can block event interception.

## Status and lighting

### Critical constraint

An **ordinary** Layer 2 cannot render the six-slot live status model. A Layer 2 containing `KV_OAI_AG00` through `KV_OAI_AG05` can: the firmware gate follows active-layer keycodes, not the numeric layer index.

`house-of-herdr` is the smallest existing Herdr publisher: it displays six Herdr agents and focuses them from the matching vendor keys. [`microd`](https://github.com/PlaneshiftDev/microd) independently implements the same direct-HID/Herdr architecture. Neither currently automates the Layer 2 layout conversion, and simultaneous official Codex plus Herdr writers remain unsafe.

Input 0.17.2's UI hides OAI actions outside the locked layer, but its importer and device serializer preserve copied `KV_OAI_*` strings. `v.oai.thstatus` is accepted while Layer 2 is active, and public hardware tests show the six LEDs rendering after the OAI layout is copied there. Initially omit generic Layer 2 lighting because one independent test found it could paint over status colors.

The exact gate, source validation, safe experiment, and limitations are in [Layer 2 individual-RGB bypass](./layer2-individual-rgb-bypass-research.md), [Critical per-agent RGB](./rgb-status-research.md), [Input.app artifact evidence](./input-app-artifact-evidence.md), and [source/community research](./source-and-community-workarounds.md).

### Supported feedback now

Use Herdr’s native sidebar, notifications, sounds, and metadata.

A dispatcher can report:

```sh
herdr pane report-metadata "$pane_id" \
  --source codex-micro-agentctl \
  --token model="$model" \
  --token effort="$effort" \
  --token summary="$summary"
```

Those values are display-only; Herdr’s own lifecycle authority remains responsible for `working`, `blocked`, `done`, and `idle`.

### Private RGB capability: research evidence only

Local inspection found:

| Artifact | Observed version/details |
|---|---|
| Codex app | `26.721.41059` |
| Work Louder Input | `0.17.2` |
| `@worklouder/device-kit-oai` | `0.1.11`, bundled in Codex, `UNLICENSED` |
| `@worklouder/wl-device-kit` | Bundled in Codex and Input, proprietary JSON-RPC transport |

The bundled type definitions expose:

```text
onHidReceived()
onJoystickMove()
getDeviceStatus()
sendThreadsLighting()
sendLightingConfig()
```

The Codex service:

- discovers Codex Micro on vendor HID usage page `0xFF00`;
- maintains six lighting slots;
- maps agent states to solid, breathing, or snake effects;
- receives `AG00` through `AG05` and joystick angle/distance;
- writes per-slot and ambient lighting.

This is no longer hypothetical. Community projects reimplemented the protocol cleanly and built this bridge:

```text
Herdr session snapshot + event subscription
  → choose six visible agents
  → map Herdr state to six lighting slots
  → write Work Louder thread/ambient lighting
  → route hardware vendor events back to Herdr focus/actions
```

Do not copy or ship the private package, and do not run a community bridge as a concurrent writer:

- the package is unlicensed and has no public support contract;
- Input and Codex have had device-communication interference;
- two processes may contend for the same device connection;
- package paths, methods, firmware, and protocol may change without notice.

For this personal setup, use the OAI-enabled Layer 2 with `herdr-micro` as the sole device owner. For a supported product, Work Louder still needs to publish an SDK/status-provider extension and ownership rules.

## Integration options ranked

| Rank | Option | Decision |
|---:|---|---|
| 1 | OAI-enabled Layer 2 + `herdr-micro` | Implemented and physically verified over USB and BLE |
| 2 | Ordinary Layer 2 chords → Herdr dispatcher | Supported-input fallback; static lighting |
| 3 | Codex app-server, Claude commands/SDK, Pi extension/RPC | Cross-agent effort/model controls |
| 4 | Hammerspoon for true PTT | Add only when needed |
| 5 | Vendor status-provider API | Required for a supported RGB integration |

Avoid AppleScript/UI scripting for terminal controls. Avoid Karabiner-Elements initially: it adds Input Monitoring/virtual-HID complexity, and Work Louder currently warns about communication interference on the [Codex Micro setup page](https://worklouder.cc/openai-micro-setup).

## Remote Herdr caveat

Standard `herdr --remote` can use local ordinary keybindings, but local custom command bindings are not sent to the remote host. For the dispatcher:

1. Install `agentctl` and its Herdr custom-command config on the remote host.
2. Use `--remote-keybindings server`.
3. Keep the Work Louder keyboard connected to the local client.

This preserves exact remote pane targeting without trying to run the agent-specific CLI locally.

## Security rules

1. Freeze the pane ID before acting; focus may change during asynchronous work.
2. Reject actions when no supported agent owns the pane.
3. Never create a global blind approval key.
4. Keep Herdr and agent-control sockets local to the user.
5. Show action, target, requested value, and failure reason after every semantic control.

## Historical implementation phases

These were the pre-implementation phases. Their RGB, focus, cross-agent
effort, layer, USB, and BLE milestones are now complete; voice and a
vendor-supported product path remain optional.

### Phase 0 — choose the RGB trade-off

1. Keep the completed backup and successful disposable Layer 3 end-to-end proof.
2. Choose the final Agent/dial/command mappings and pin the bridge.
3. Move the proven layout to Layer 2; use ordinary static lighting only as fallback.
4. Never run Codex, Input, and a community RGB bridge as competing writers.

### Phase 1 — supported controls baseline

Estimated engineering time: **30–90 minutes**.

1. Install Herdr and the Pi/Claude/Codex integrations.
2. Create layer-2 direct chords in Work Louder Input.
3. Add Herdr focus, pane navigation, split, zoom, and interrupt bindings.
4. Use a static layer-2 color.

### Phase 2 — cross-agent control

Estimated engineering time: **2–5 hours**.

1. Add `agentctl` with Codex, Claude, and Pi adapters.
2. Implement effort down/up, model controls, interrupt, and status.
3. Add a tiny Pi extension for exact thinking/tool presets.
4. Add Herdr metadata and notifications.

### Phase 3 — voice and polished feedback

Estimated engineering time: **2–6 hours**.

1. Wire Claude native voice.
2. Add macOS Dictation fallback.
3. Add Hammerspoon only if true press/release PTT or global routing is required.
4. Test USB, Bluetooth, Secure Input, focus changes, and remote Herdr.

### Phase 4 — supported Layer 2 RGB

Estimated prototype time: **1–3 days**, plus ongoing maintenance.

1. Implement the published/licensed status-provider interface.
2. Use snapshot-authoritative Herdr collection and stable terminal-based slots.
3. Make the supported broker or host application the sole device writer.
4. Pass crash TTL, layer isolation, reconnect, transport, and upgrade gates.

## Evidence notes

- [Codex Micro and Work Louder evidence](./codex-micro-evidence.md)
- [Herdr evidence](./herdr-evidence.md)
- [Cross-agent controls](./cross-agent-controls-evidence.md)
- [Integration options](./integration-options-evidence.md)
- [Critical per-agent RGB decision](./rgb-status-research.md)
- [Creator Micro 2 Agent Mode](./creator-micro-2-agent-mode-evidence.md)
- [RGB device protocol evidence](./rgb-device-protocol-evidence.md)
- [Herdr RGB state-pipeline evidence](./rgb-herdr-state-evidence.md)
- [RGB deployment architecture evidence](./rgb-integration-architecture-evidence.md)
- [Source code and community workarounds](./source-and-community-workarounds.md)
- [Input.app artifact evidence](./input-app-artifact-evidence.md)
- [Layer 2 individual-RGB bypass](./layer2-individual-rgb-bypass-research.md)
- [Live Layer 3 hardware test log](./hardware-test-log.md)

The evidence notes contain exact versions, source links, limitations, and unresolved questions. This README is the recommended implementation decision.
