# Codex Micro bridge

Status: implemented and physically verified over USB and Bluetooth LE.

## Scope

The bridge is the sole owner of the Codex Micro vendor HID interface over USB
or Bluetooth LE while
Work Louder Input and the Codex/ChatGPT host are closed. It:

- polls Herdr once per second and paints six sticky Agent slots;
- polls the native macOS frontmost app and window once per second;
- selects the layer claimed by the frontmost window and preserves the previous
  applicable layer when no claim matches;
- focuses the pane assigned to `AG00` through `AG05`;
- maps the observed `ENC_CW` event to effort lower and `ENC_CC` to effort
  raise; and
- maps `ACT06` to a focused-agent review prompt using the native Codex,
  Claude Code, or Pi skill syntax;
- maps `ACT07` to a temporary Hunk diff popup rooted at the focused agent's
  repository;
- maps `ACT08` to `/fast` for the focused Codex or Pi agent;
- maps `ACT12` to Enter on the focused agent; and
- blanks and releases the device on shutdown or when a known official owner
  starts.

Layer 2 must retain `KV_OAI_AG00` through `KV_OAI_AG05` plus
`KV_OAI_ENC_CW` and `KV_OAI_ENC_CC`. The same bridge can be tested on the
already-proven OAI-enabled Layer 3.

## Process model

Herdr's startup hook runs `src/micro-start.mjs`. It launches one detached
`src/micro-daemon.mjs`; a Unix socket in `HERDR_PLUGIN_STATE_DIR` is both its
single-instance lock and its status/stop control. Logs live beside that socket
as `micro.log`.

The daemon launches `bin/micro-hid`, a native direct-IOKit helper. It matches
the Micro by VID/PID, prefers USB when both transports are present, and selects
the required framing from IOKit's `Transport` property. USB sends the 63-byte
payload after the Report ID; BLE sends the full 64-byte Report-ID-prefixed
payload. A successful `device.status` round trip is required before the daemon
reports the device connected.

The daemon shares `src/effort.mjs` with the manifest actions. It resolves the
currently focused Herdr agent immediately before each dial change, so it never
depends on the startup action's stale context.

`bin/frontmost` is compiled from a small Swift source during the plugin build.
It combines `NSWorkspace.frontmostApplication` with the top layer-zero
CoreGraphics window for that process. This needs no Accessibility permission.

Claims are read from `HERDR_PLUGIN_CONFIG_DIR/claims.json`. Exact bundle ID and
optional case-insensitive title substring are supported; the last matching
entry wins. The included local configuration claims Ghostty for Layer 2
and Codex for Layer 1. Chrome and other unclaimed apps send no layer command.
Ghostty has no title restriction because agent-driven titles are not stable.

## Software result

On 2026-07-26 the bridge:

- opened the connected USB Codex Micro vendor interface;
- opened the paired Codex Micro over Bluetooth LE with the cable unplugged;
- discovered 18 Herdr agents and populated all six sticky slots;
- survived a controlled stop/start while blanking and repainting the keys; and
- passed all 10 Node tests plus Swift compilation, syntax, and whitespace
  checks.

Layer 2 was repaired from zero to 16 OAI assignments, synchronized through
Input, and verified by a clean hardware read-back. It exactly matches the
Layer 1 layout while retaining its own name and lighting block. The user then
physically confirmed all six individual live Herdr status colors on Layer 2.
The user also confirmed focus switching with multiple Layer 2 Agent keys. Dial
testing then found the device event labels opposite the desired physical
direction; the bridge now maps `ENC_CW` to lower and `ENC_CC` to raise. The user
physically confirmed the corrected direction in Pi. Small movements sometimes
need additional rotation before the firmware emits a detent/event.

The user also physically confirmed dial effort control in Codex and Claude
Code. Layer 2 RGB, Agent-key focus, and effort control for Codex, Pi, and
Claude Code are now all proven on the physical device.

The user physically confirmed that `ACT06` submits the agent-specific review
prompt, `ACT07` opens and closes the focused repository's Hunk popup, and
`ACT12` submits the focused agent's composer.

Automatic selection is also proven on the device. A no-match sample selected
firmware `layer_index: 1`; a matching foreground Chrome sample selected
`layer_index: 2`. The live configuration was then restored to Ghostty.

The user physically confirmed the complete BLE acceptance sequence: five
`done` keys painted green while one breathing `working` key painted blue,
Agent key 1 focused its assigned Herdr pane, both dial directions changed
Codex effort, Ghostty and Codex switched between Layers 2 and 1, and Chrome
preserved the last applicable layer.

After replacing `node-hid` with the direct-IOKit helper, the user switched the
same device from BLE to its white wired mode. The daemon detected the BLE
disconnect, reconnected over USB, passed `device.status`, repainted the six
live statuses, focused the assigned pane from Agent key 1, and changed Codex
effort in both directions. The single native transport is therefore physically
verified over both USB and BLE.

## Safety boundary

- The daemon performs no firmware or keymap writes.
- The explicit one-time AppSense setup action backs up `keymap.json`, changes
  only two linked-app records and two `linkedAppId` fields, then reads the full
  file back.
- No global synthetic keyboard events.
- A running Input process always makes the bridge yield.
- Codex/ChatGPT makes the bridge yield only while its bundle is frontmost;
  the tested non-exclusive HID handle can safely reclaim the device when
  Ghostty becomes frontmost while Codex remains running.
- Sixty seconds without Herdr blanks the LEDs and stops the bridge.
- Unknown third-party HID writers cannot participate in that ownership check;
  do not run one beside this bridge.
- Claude effort control requires an empty prompt.
- Pi loads `integrations/pi/herdr-effort.js` through its global
  `settings.json`; Pi processes already running when it is added require a
  restart.

## Commands

```sh
npm install
mkdir -p bin
/usr/bin/swiftc native/frontmost.swift -o bin/frontmost
/usr/bin/swiftc native/micro-hid.swift -o bin/micro-hid -framework IOKit
node src/micro-setup-appsense.mjs
node src/micro-start.mjs
node src/micro-action.mjs status
node src/micro-action.mjs stop
```

## Source basis

The framing, VID/PID, OAI methods, event names, and single-owner constraint were
confirmed in the physical-device research under
`/Users/gg/Documents/codex-micro`. The direct-IOKit access and
transport-dependent framing were independently verified against the
MIT-licensed
[`eliBenven/freemicro`](https://github.com/eliBenven/freemicro/tree/1e78198c1b4bfe43b7e4aee3246c73314b9bcc0f).
The MIT-licensed
[`alasano/house-of-herdr`](https://github.com/alasano/house-of-herdr) Codex
Micro package at `7d8eadaed41a1bb4456565d6bcba8cdb7380b77e` was used as a
behavioral reference; this integration keeps only RGB, Agent focus, and effort-dial
behavior.
