# Codex Micro bridge

Status: Rust implementation complete. Its connected-device canary mapped
`default` and `werk`, discovered 23 agents, and selected Layer 2. Earlier
Node/Swift builds were physically verified over USB and Bluetooth LE; the Rust
canary does not establish that transport matrix.

## Scope

The bridge is the sole owner of the Codex Micro vendor HID interface over USB
or Bluetooth LE while
Work Louder Input and the Codex/ChatGPT host are closed. It:

- discovers running default and named Herdr sessions once per second;
- paints six sticky Agent slots from only the last foreground Herdr session;
- maps each session to a stable Ghostty terminal UUID in memory;
- selects Layer 1 for Codex and Layer 2 for a mapped Ghostty terminal while
  preserving the previous selection in unrelated applications;
- focuses the pane assigned to `AG00` through `AG05`;
- maps action buttons, dial turns and press, and joystick directions through
  one agent-aware control configuration;
- blanks and releases the device on shutdown or when a known official owner
  starts.

`controls.json` in `HERDR_PLUGIN_CONFIG_DIR` maps physical buttons 1 through 7,
dial clockwise/counterclockwise/press, and four joystick directions to
validated action objects. Every binding can use a direct action, `null`, or a
`byAgent` map selected from the currently focused Herdr agent. The config is
reloaded once per bridge poll.

Buttons and dial press additionally support `tap`, `doubleTap`, `hold`, and
`release` gesture bindings. Hold and double-tap timing is configurable; direct
bindings retain their immediate press behavior.

Layer 2 must retain `KV_OAI_AG00` through `KV_OAI_AG05` plus
`KV_OAI_ENC_CW` and `KV_OAI_ENC_CC`. The same bridge can be tested on the
already-proven OAI-enabled Layer 3.

## Process model

Herdr's startup hook runs `bin/herdr-micro start`. It launches one detached
`bin/herdr-micro daemon`; a Unix socket in `HERDR_PLUGIN_STATE_DIR` is both its
single-instance lock and its status/stop control. Logs live beside that socket
as `micro.log`. The start command exits after ensuring the daemon exists because
Herdr startup hooks are one-shot initialization, not process supervision;
disabling or unlinking the plugin does not stop the daemon, so use the explicit
stop action.

The daemon discovers running names through `herdr session list --json`. For
each session it briefly sets a unique terminal title, reads the matching stable
terminal UUID from Ghostty's AppleScript API, restores the original title, and
retains the mapping only in memory. Every Herdr CLI call selects the captured
name with `HERDR_SESSION` after clearing inherited pane, workspace, and plugin
invocation context. Controls and delayed gestures capture that name before
entering the single action queue.

The Rust daemon uses direct `objc2` IOKit HID bindings. It matches the Micro by
VID/PID, prefers USB when both transports are present, and selects framing from
IOKit's `Transport` property. USB sends the 63-byte payload after the Report
ID; BLE sends the full 64-byte Report-ID-prefixed payload. A successful
`device.status` round trip is required before the daemon reports connected.

The daemon and manifest actions share Rust effort planning. It resolves the
currently focused Herdr agent immediately before each configured control action,
so it never depends on the startup action's stale context.

The joystick arrives as `v.oai.rad` with normalized angle and distance. The
default config engages beyond `0.75`, releases below `0.3`, and fires again
when the stick crosses into another quadrant. Both thresholds and all four
direction actions are configurable. By default, up/down scroll the focused
pane by 50% of its visible rows and left/right focus the adjacent pane.
Scroll is a no-op unless the currently focused Ghostty terminal UUID maps to
the selected Herdr session. Other actions continue targeting the sticky
selected session.

`lighting.json` controls the per-state color, brightness, effect, and speed.
The focused Agent key is raised to `focusedBrightness`. Optional aggregate
`ambient` and `keys` zones follow the highest-priority slotted status through
`v.oai.rgbcfg`; the six per-agent keys are then applied through
`v.oai.thstatus`.

The Rust binary uses AppKit `NSWorkspace.frontmostApplication` for
Codex/Ghostty detection and CoreGraphics for optional scrolling.
Ghostty 1.3 or newer exposes window, tab, and terminal UUIDs through its native
AppleScript API; Automation permission is required. The optional `scroll`
action posts a temporary mouse move plus wheel events at the focused pane, then
restores the original cursor; macOS requires post-event Accessibility
permission. Regrant Accessibility after upgrading if macOS treats the new
binary as a new trusted executable.

## Software result

On 2026-07-26 the bridge:

- opened the connected USB Codex Micro vendor interface;
- opened the paired Codex Micro over Bluetooth LE with the cable unplugged;
- discovered 18 Herdr agents and populated all six sticky slots;
- survived a controlled stop/start while blanking and repainting the keys; and
- passed the Node tests plus Swift compilation, syntax, and whitespace checks.

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

The user physically confirmed agent-specific prompts, the optional Hunk popup,
`/fast`, `/copy`, submit, and an ordinary F19 mapping.

The user physically confirmed configurable tap, double-tap, and hold actions
on button 4, after which its original direct `/copy` binding was restored.

The user physically confirmed 50% joystick scrolling in both directions
without the terminal-controller resize snap-back. In a two-pane split, the
synthetic mouse move correctly routed scrolling to whichever pane was focused.

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

That BLE verification used firmware `v0.4.1`. A 2026-08-01 retest after Input
0.18.0 installed official firmware `v0.6.1` found that USB still works, while
the new firmware rejects macOS BLE HID output reports. See the
[wireless compatibility evidence](./research/wireless-bridge-evidence.md).

On 2026-07-31 active-session routing was added for default and named Herdr
sessions. A later live probe established that Ghostty's AppleScript terminal
name exposes Herdr's temporary title even when a custom visible tab title
hides it. The daemon now uses that reversible title handshake to map stable
terminal UUIDs to sessions without configuration. The user physically
confirmed `default → werk → default` UUID focus detection.

## Safety boundary

- The daemon performs no firmware or keymap writes.
- The explicit one-time setup action backs up `keymap.json`, clones the Layer
  1 OAI controls into blank Layer 2, adds two AppSense bindings, then
  reads the full file back.
- No global synthetic keyboard events. The optional scroll action posts
  targeted mouse-move and wheel events, then restores the cursor.
- Gesture actions are semantic plugin actions, not held macOS key events, and
  require no additional Accessibility permission.
- A running Input process always makes the bridge yield.
- Codex/ChatGPT makes the bridge yield only while its bundle is frontmost;
  the tested non-exclusive HID handle can safely reclaim the device when
  Ghostty becomes frontmost while Codex remains running.
- Sixty seconds with no running Herdr session blanks the LEDs and stops the
  bridge. A selected-session failure does not silently fall back to another.
- Unknown third-party HID writers cannot participate in that ownership check;
  do not run one beside this bridge.
- Claude effort control requires an empty prompt.
- Pi auto-discovers the installed extension; existing sessions need `/reload`.

## Documented, not implemented

- True held macOS keys. They require synthetic input and Accessibility
  permission; the implemented gestures dispatch plugin actions instead.
- Eight-way joystick sectors, radial UI, and analog pointer mode.
- Per-action-key RGB. A 2026-08-01 hardware test on this exact Codex Micro
  `v0.4.1` set IDs `0`–`5` green and IDs `6`–`12` to seven distinct colors;
  only the six Agent keys lit. General key backlight and underglow remain
  aggregate zones. See the
  [lighting capability audit](./research/lighting-capability-audit.md).
- Agent-key synchronization flags for the aggregate key and ambient zones.
  They exist in the protocol but remain hardware-unverified here.

## Commands

```sh
cargo build --release --locked
mkdir -p bin
install -m 750 target/release/herdr-micro bin/.herdr-micro.new
mv -f bin/.herdr-micro.new bin/herdr-micro
bin/herdr-micro setup
bin/herdr-micro setup-pi-effort
bin/herdr-micro doctor
bin/herdr-micro start
bin/herdr-micro status
bin/herdr-micro stop
```

## Source basis

The framing, VID/PID, OAI methods, event names, and single-owner constraint were
confirmed in the archived [physical-device research](research/). The
direct-IOKit access and transport-dependent framing were independently
verified against the
MIT-licensed
[`eliBenven/freemicro`](https://github.com/eliBenven/freemicro/tree/1e78198c1b4bfe43b7e4aee3246c73314b9bcc0f).
The MIT-licensed
[`alasano/house-of-herdr`](https://github.com/alasano/house-of-herdr) Codex
Micro package at `7d8eadaed41a1bb4456565d6bcba8cdb7380b77e` was used as a
behavioral reference; this integration keeps only RGB, Agent focus, and effort-dial
behavior.
