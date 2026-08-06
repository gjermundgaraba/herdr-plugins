# Codex Micro bridge

## Current architecture

`herdr-micro` has two deliberately unequal processes. A launchd-activated,
root-owned helper contains only the Codex Micro USB transport and holds one
exclusive device lease. The detached user daemon owns Herdr routing,
configuration, Ghostty inspection, gestures, and macOS event output. They use a
versioned local socket whose peers are checked by effective UID. The helper
accepts only the six device methods the bridge and guarded setup require.

A separate Unix socket in `HERDR_PLUGIN_STATE_DIR` provides user-daemon status
and stop control; `micro.log` is stored beside it. The helper restores the
normal macOS HID driver when its authenticated client disconnects and exits
after five idle seconds. launchd starts it again on the next connection.

```text
Codex Micro over USB
  → root helper: capture USB device and detach the system HID driver
  → authenticated local event/request stream
  → unprivileged herdr-micro daemon
  → six sticky Herdr agent slots and configured controls
  → focused Ghostty terminal UUID
  → matching default or named Herdr session
  → exact pane/agent action
```

The daemon discovers Herdr sessions once per second. It briefly gives each
session a unique terminal title, reads Ghostty's stable terminal UUID through
its AppleScript API, restores the title, and keeps the mapping only in memory.
Every Herdr CLI call then uses the selected session's `HERDR_SESSION` value.
Agents and actions are never mixed across sessions.

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
required for six-way status lighting. Each bound action switch uses a fixed
internal code from F21–F24/EXECUTE/SELECT/STOP (HID usages macOS maps to no
virtual keycode, so an uncaptured Micro cannot type anything); unbound switches
are disabled. The double-width action key spans two switches, so one half is
unbound by default. `micro-setup` applies and verifies the managed keymap.
Agent presses focus their slot directly through Herdr; action switches
dispatch bindings internally.

## Compatibility

| Component | Current boundary |
|---|---|
| Platform | macOS only |
| Herdr | 0.7.5 or newer |
| Ghostty | 1.3 or newer; Automation permission required |
| Build toolchain | Rust 1.85 or newer |
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
- Controls target the captured Herdr session and pane. `scroll` additionally
  rechecks the focused Ghostty UUID before posting wheel events. `key`
  bindings are the deliberate exception: they tap system-wide from any
  frontmost application while the bridge owns the device.
- CoreGraphics output requires Accessibility permission and is used only for
  `scroll`, which posts targeted wheel events and restores the cursor, and for
  explicitly configured `key` bindings, which tap their configured keycode.
  No other keyboard events are synthesized; all switch HID codes remain
  internal to the bridge.
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

The [research record](research/README.md) preserves tested versions, results,
hardware evidence, caveats, and source links.
[Future investigations](future-investigations.md) tracks deferred hardware and
protocol work.

## Lifecycle

Herdr v1 startup hooks are not supervised services and have no teardown hook.
launchd supervises only the on-demand USB helper; the startup hook starts the
user daemon. Stop the bridge before updating. Ordinary plugin updates do not
require reinstalling the privileged helper; reinstall it only when its explicit
build version changes. Uninstall the helper before removing the plugin.
