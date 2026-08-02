# Codex Micro bridge

## Current architecture

`herdr-micro` is one detached Rust daemon and the sole owner of the Codex
Micro vendor HID interface. A Unix socket in `HERDR_PLUGIN_STATE_DIR` provides
the single-instance lock plus status and stop control; `micro.log` is stored
beside it.

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
| Unrelated application | Preserve the last applicable layer; dispatch nothing unsafe |

Layer 2 must retain `KV_OAI_AG00` through `KV_OAI_AG05` and the OAI encoder
actions created by `micro-setup`.

## Compatibility

| Component | Current boundary |
|---|---|
| Platform | macOS only |
| Herdr | 0.7.5 or newer |
| Ghostty | 1.3 or newer; Automation permission required |
| Build toolchain | Rust 1.71 or newer |
| Codex Micro firmware 0.4.1 | USB and BLE physically verified |
| Codex Micro firmware 0.6.1 | USB and BLE physically verified; a fresh BLE host pairing may be required after upgrade |
| Codex CLI 0.145.0 | Effort raise/lower physically verified |
| Claude Code 2.1.220 | Effort raise/lower physically verified |
| Pi 0.82.1 | Effort raise/lower physically verified with the bundled extension |

Firmware 0.6.1 can leave an existing macOS BLE pairing with stale GATT
metadata. Pair an unused BLE host slot; if none is free, forget and re-pair
the affected `Codex Micro #N`. USB remains the recovery transport.

## Ownership and safety

- Do not run Work Louder Input or another third-party HID writer beside the
  bridge. The daemon yields when Input is running and while Codex is frontmost.
- The daemon never writes firmware or keymaps. Only the explicit `micro-setup`
  action changes the keymap; it requires a blank Layer 2, creates a backup, and
  verifies the full read-back.
- Controls target the captured Herdr session and pane. `scroll` additionally
  rechecks the focused Ghostty UUID before posting wheel events.
- `scroll` uses targeted CoreGraphics mouse/wheel events, restores the cursor,
  and requires Accessibility permission. Other controls do not synthesize
  global keyboard events.
- A selected-session failure does not fall back to another session. Controlled
  shutdown and 60 seconds without any Herdr session blank the LEDs.

## Current limitations

- The OAI HID protocol and firmware actions are proprietary and unsupported;
  Work Louder publishes firmware binaries, not a third-party SDK or protocol
  contract. Firmware or host-app changes may break the bridge.
- Only the six Agent keys are independently addressable on the tested Codex
  Micro. The seven lower keys and perimeter lighting are aggregate zones.
- Aggregate-zone synchronization flags exist in the protocol but have not been
  physically verified.
- True held macOS keys, voice control, eight-way joystick sectors, and analog
  pointer mode are not implemented.
- Bluetooth standby requires a physical key, dial, or joystick action to wake
  the device; the daemon reconnects and repaints after wake.
- Claude effort changes require an empty prompt. Existing Pi sessions need
  `/reload` after installing the extension. Effort changes affect later model
  requests, not a request already in flight.

The compact [research record](research/README.md) preserves the physical facts,
version boundaries, caveat, and source links behind these claims.
