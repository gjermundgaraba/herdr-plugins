# Codex Micro bridge

## Current architecture

`herdr-micro` has two deliberately unequal processes. A launchd-activated,
root-owned helper contains only the Codex Micro USB transport and holds one
exclusive device lease. The detached user daemon owns Herdr routing,
configuration, Ghostty inspection, gestures, and macOS event output. They use a
versioned local socket whose peers are checked by effective UID. The helper
accepts only the six device methods the bridge and guarded setup require.

A Unix socket at `HERDR_PLUGIN_STATE_DIR/run/micro.sock` provides user-daemon
status and stop control; bounded logs live under `logs/`. The helper restores the
normal macOS HID driver when its authenticated client disconnects, then exits.
launchd starts a fresh helper for the next authenticated device lease. A native
open, teardown, or final restoration that exceeds the single hard deadline
terminates the disposable helper so a stuck IOKit client cannot poison a later
lease.

```text
Codex Micro over USB
  → root helper: capture USB device and detach the system HID driver
  → authenticated local event/request stream
  → unprivileged herdr-micro daemon
  → six sticky Herdr agent slots and configured controls
  → focused Ghostty terminal UUID
  → matching default or named Herdr session
  → exact pane/agent built-in or script action
```

The daemon uses Herdr's CLI only to discover running session names and their
socket paths at startup or when discovery becomes stale. It polls the selected
session's stable `session.snapshot` API four times per second; changed agent
state drives routing and lighting without additional discovery subprocesses.
Built-in actions are direct socket requests after a fresh snapshot confirms
the captured agent identity. Configured script actions run synchronously from
the plugin root after the same validation. They receive `HERDR_SOCKET_PATH`,
`HERDR_SESSION`, and `HERDR_PANE_ID`, and inherit `HERDR_BIN_PATH`. Child output
is bounded; failures report stderr and their binding. A process group that
exceeds the five-second deadline is terminated and reaped.

The single action worker runs each accepted script once in FIFO order. Its
bounded queue accepts 16 pending actions and logs inputs rejected while full.

Ghostty inspection uses a cached native ScriptingBridge client from Rust. The
focused terminal UUID is queried while Ghostty is active and immediately before
session-targeted actions. The daemon refreshes the full mapping when the focused
terminal changes and every five seconds while Ghostty remains active. It briefly
gives each Herdr session a unique title to associate it with a Ghostty UUID,
restores the title, and keeps the mapping only in memory.

Routing supports one active Ghostty attachment per Herdr session. If another
Ghostty attachment to the same session becomes foreground, routing pauses while
the session is remapped; simultaneous attachment mappings are not represented.

The helper uses the Micro's JSON-RPC HID reports over its raw USB interface and
requires a successful `device.status` round trip before reporting a
connection. Bluetooth is intentionally unsupported because it cannot provide
the exclusive ownership that prevents duplicate ChatGPT input.

Layer routing is fixed:

| Frontmost context | Result |
|---|---|
| Codex desktop app | Yield device ownership; select Layer 1 |
| Ghostty terminal mapped to a running Herdr session | Own device; select Layer 2 and that session |
| Unrelated application | Preserve the last applicable layer; dispatch nothing |

Layer 2 retains `KV_OAI_AG00` through `KV_OAI_AG05`; those private codes are
required for six-way status lighting. Bound action switches use the Micro's
native `KV_OAI_ACT06` through `KV_OAI_ACT12` events; the privileged helper's
exclusive USB capture prevents ChatGPT from receiving them. Only configured
macOS key bindings are synthesized back into the system. Unbound switches are
disabled. The double-width action key spans two switches, so one half is
unbound by default. `micro-setup` applies and verifies the managed keymap.
Agent presses focus their slot directly through Herdr; action switches dispatch
bindings internally.

## Compatibility

| Component | Current boundary |
|---|---|
| Platform | macOS only |
| Herdr | 0.8.0 or newer; experimental Kitty graphics enabled for scroll metrics |
| Ghostty | 1.3 or newer; Automation permission required |
| Build toolchain | Rust 1.89 or newer |
| Codex Micro firmware 0.4.1 | USB physically verified |
| Codex Micro firmware 0.6.1 | USB physically verified |
| Effort control | Codex CLI, Claude Code, and Pi with the bundled extension |

## Ownership and safety

- Capturing the keyboard-class USB device requires root. The explicit installer
  copies a dedicated helper and launchd plist to root-owned system paths. While
  the user bridge owns Layer 2, the helper detaches the normal macOS HID driver
  and claims the Micro's USB interface; it restores the driver before yielding
  to ChatGPT/Codex.
- The helper socket is owner-only and mutually authenticates the configured
  user and root helper. Exact protocol and helper build versions must match; an
  upgrade never falls back to direct or shared HID access.
- The daemon never writes firmware or keymaps. Only the explicit `micro-setup`
  action changes the keymap; it requires a blank or previously managed Layer 2,
  creates a backup, and verifies the full read-back.
- Controls target the captured Herdr session and pane. `scroll` gets Herdr's
  live host-cell size, rechecks the focused Ghostty UUID, then sends native
  mouse-position and scroll commands to that exact Ghostty terminal. `key`
  bindings are the deliberate exception: they tap system-wide from any
  frontmost application while the bridge owns the device. Scripts recheck the
  frozen session, terminal, pane, agent, and routing generation before running.
- CoreGraphics output requires Accessibility permission only for explicitly
  configured `key` bindings, which tap their configured keycode. Scrolling does
  not move the system cursor. No other keyboard events are synthesized; all
  switch HID codes remain internal to the bridge.
- A selected-session failure does not fall back to another session. Controlled
  shutdown and 60 seconds without any Herdr session blank the LEDs.

## Current limitations

- The OAI HID protocol and firmware actions are proprietary and unsupported;
  Work Louder publishes firmware binaries, not a third-party SDK or protocol
  contract. Firmware or host-app changes may break the bridge.
- Only the six Agent keys are independently addressable for lighting on the
  tested Codex Micro. The stock double-width lower keycap spans action switches
  5 and 6. Perimeter lighting is an aggregate zone.
- Aggregate-zone synchronization flags exist in the protocol but have not been
  physically verified.
- Voice control, eight-way joystick sectors, and analog pointer mode are not
  implemented.
- The bridge requires a USB connection; Bluetooth would reintroduce shared HID
  delivery and duplicate ChatGPT input.
- Claude effort changes require an empty prompt. Existing Pi sessions need
  `/reload` after installing the extension. Effort changes affect later model
  requests, not a request already in flight.
## Thinking-effort control

The bridge freezes the focused agent and pane from the selected Herdr session,
then sends the agent-specific operation to that exact pane:

| Agent | Mechanism |
|---|---|
| Codex | Bundled adapter sends the CLI defaults `alt+.` and `alt+,` |
| Claude | The same adapter drives `/effort`, one step left or right |
| Pi | The same adapter sends an extension shortcut; the bundled extension uses `getThinkingLevel()` / `setThinkingLevel()` |

Install the Pi extension with `bin/herdr-micro setup-pi-effort`; existing
sessions need `/reload`. Claude persists `low` through `xhigh`, while `max` is
session-only. Codex CLI TUI bindings come from `~/.codex/config.toml`, not
Codex Desktop's `~/.codex/keybindings.json`; the bridge targets the CLI
defaults `alt+.` and `alt+,`. If those bindings are overridden, update the
script arguments in `config.json`. The changed effort applies to later
provider calls.

The [research record](research/README.md) preserves tested versions, results,
hardware evidence, caveats, and source links, including the upstream
[Claude model configuration](https://code.claude.com/docs/en/model-config).

## Lifecycle

Herdr v1 startup hooks are not supervised services and have no teardown hook.
launchd supervises only the on-demand USB helper; the startup hook starts the
user daemon. Stop the bridge before updating. Ordinary plugin updates do not
require reinstalling the privileged helper; reinstall it only when its explicit
build version changes. Uninstall the helper before removing the plugin.
