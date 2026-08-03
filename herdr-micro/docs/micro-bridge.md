# Codex Micro bridge

## Current architecture

`herdr-micro` is one detached Rust daemon and the sole owner of the Codex
Micro vendor HID interface. The workspace's `codex-micro` library contains
only HID transport, device framing, raw events, and keymap I/O; Herdr routing
and behavior stay in the `herdr-micro` crate and link into the same process.
There is no second daemon or IPC boundary. A Unix socket in
`HERDR_PLUGIN_STATE_DIR` provides status and stop control; `micro.log` is
stored beside it, and a process lock protects hardware ownership.

```text
Codex Micro over USB or BLE
  → direct macOS IOKit HID transport
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

USB and BLE use the same JSON-RPC payloads with transport-specific HID report
framing. The daemon prefers USB when both transports are present and requires
a successful `device.status` round trip before reporting a connection.

Layer routing is fixed:

| Frontmost context | Result |
|---|---|
| Codex desktop app | Yield device ownership; select Layer 1 |
| Ghostty terminal mapped to a running Herdr session | Own device; select Layer 2 and that session |
| Unrelated application | Preserve the last applicable layer; dispatch nothing |

Layer 2 retains `KV_OAI_AG00` through `KV_OAI_AG05` and the OAI encoder actions.
Logical action buttons may instead use unique `F13` through `F24` codes selected
by `controls.json`; `micro-setup` applies and verifies that managed keymap.

## Compatibility

| Component | Current boundary |
|---|---|
| Platform | macOS only |
| Herdr | 0.7.5 or newer |
| Ghostty | 1.3 or newer; Automation permission required |
| Build toolchain | Rust 1.71 or newer |
| Codex Micro firmware 0.4.1 | USB and BLE physically verified |
| Codex Micro firmware 0.6.1 | USB and BLE physically verified; a fresh BLE host pairing may be required after upgrade |
| Effort control | Codex CLI, Claude Code, and Pi with the bundled extension |

Firmware 0.6.1 can leave an existing macOS BLE pairing with stale GATT
metadata. Pair an unused BLE host slot; if none is free, forget and re-pair
the affected `Codex Micro #N`. USB remains the recovery transport.

## Ownership and safety

- Direct HID access requires macOS Input Monitoring permission. Do not run Work
  Louder Input or another Input Monitoring/HID client beside the bridge. The
  daemon yields when Input is running and while Codex is frontmost.
- The daemon never writes firmware or keymaps. Only the explicit `micro-setup`
  action changes the keymap; it requires a blank or previously managed Layer 2,
  creates a backup, and verifies the full read-back.
- Controls target the captured Herdr session and pane. `scroll` additionally
  rechecks the focused Ghostty UUID before posting wheel events.
- `scroll` uses targeted CoreGraphics mouse/wheel events, restores the cursor,
  and requires Accessibility permission. Configured F13–F24 controls are
  ordinary system-wide keys emitted by the hardware and must otherwise be
  unbound; other controls do not synthesize global keyboard events.
- A selected-session failure does not fall back to another session. Controlled
  shutdown and 60 seconds without any Herdr session blank the LEDs.

## Current limitations

- The OAI HID protocol and firmware actions are proprietary and unsupported;
  Work Louder publishes firmware binaries, not a third-party SDK or protocol
  contract. Firmware or host-app changes may break the bridge.
- Only the six Agent keys are independently addressable on the tested Codex
  Micro. The stock double-width lower key actuates ACT10 and ACT11 as one
  logical action key; perimeter lighting is an aggregate zone.
- Aggregate-zone synchronization flags exist in the protocol but have not been
  physically verified.
- True held macOS keys, voice control, eight-way joystick sectors, and analog
  pointer mode are not implemented.
- Bluetooth standby requires a physical key, dial, or joystick action to wake
  the device; the daemon reconnects and repaints after wake.
- Claude effort changes require an empty prompt. Existing Pi sessions need
  `/reload` after installing the extension. Effort changes affect later model
  requests, not a request already in flight.

The [research record](research/README.md) preserves tested versions, results,
hardware evidence, caveats, and source links.

## Lifecycle

Herdr v1 startup hooks are not supervised services and have no teardown hook.
Run `micro-stop` before disabling, uninstalling, unlinking, or updating the
plugin. `micro-start` replaces a daemon from a different plugin version.
