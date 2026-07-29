# Codex Micro layer hardware test log

Test dates: **2026-07-25–27**

## Current state

- Connection: USB and BLE both physically verified
- Device: Work Louder Codex Micro, VID `0x303A`, PID `0x8360`
- Firmware: `v0.4.1`
- Input: fully quit after configuration/read-back
- Active hardware layer: selected by the user
- Layer 2: present on-device with the OAI layout cloned from Layer 1
- Layer 3: present on-device with the OAI layout cloned from Layer 1
- Volatile lighting: controlled by the running `herdr-micro` bridge

No firmware was flashed. Layer 1 was not modified.

## Backups

Initial two-layer state:

```text
backups/20260725-210849-CEST/input_storage.json
backups/20260725-210849-CEST/device-keymap-33632.json
```

Synchronized three-layer state immediately before the successful clone:

```text
backups/20260725-210849-CEST/pre-clone-synced-input_storage.json
backups/20260725-210849-CEST/pre-clone-synced-device-keymap-33632.json
```

All four backup copies were byte-compared with their source immediately after creation.

## Configuration result

Input created a disposable blank Layer 3 and acknowledged the initial three-layer device write. After Input's local device cache and hardware read-back agreed on that baseline, [`clone-oai-layout.mjs`](./clone-oai-layout.mjs) copied exactly:

```text
Layer 1.layout → Layer 3.layout
```

Every Layer 3 field outside `layout` was preserved. Input then acknowledged two full device writes while its temporary sync name was restored to `Layer`.

The final hardware read-back completed through `fs.readbin` at `2026-07-25 21:24:18 CEST`. Both authoritative local representations now pass:

```text
Layer 1.layout == Layer 3.layout
Layer 3 OAI keycodes == 16
Layer 3 name == "Layer"
Layer 3 lights block preserved == true
```

Layer 3 therefore contains:

```text
KV_OAI_AG00 ... KV_OAI_AG05
KV_OAI_ACT06 ... KV_OAI_ACT12
KV_OAI_ENC_CC / KV_OAI_ENC_CW / KV_OAI_ENC_CLK
joystick type VENDOR
```

## Vendor-HID health check

With Input quit, the clean-room FreeMicro device layer opened the keyboard over USB and completed a `device.status` round trip:

```text
version=v0.4.1
profile_index=0
layer_index=1
battery=95
is_charging=true
```

This proves the test process can read and write the vendor channel. Live switching later established that `layer_index` is one-based on this firmware: `1` is Layer 1 and `3` is Layer 3.

The bridge then preloaded this moderate-brightness, solid-color slot model:

```text
AG00 red
AG01 green
AG02 blue
AG03 yellow
AG04 magenta
AG05 cyan
```

The write occurred with `layer_index: 1`. After switching to the OAI-enabled Layer 3, the device reported `layer_index: 3` and displayed the stored pattern. The user physically confirmed that the six Agent keys showed the six distinct colors simultaneously.

This proves on this exact Codex Micro that:

- an OAI-enabled layer is not restricted to Layer 1;
- `KV_OAI_AG00`–`AG05` retain six independent RGB addresses on Layer 3; and
- `v.oai.thstatus` can preload the slot model before switching layers.

## Vendor-key event proof

With Layer 3 active and Input and Codex quit, a sole-owner listener captured all six physical Agent keys:

```text
AG00 act=1 → AG00 act=0
AG01 act=1 → AG01 act=0
AG02 act=1 → AG02 act=0
AG03 act=1 → AG03 act=0
AG04 act=1 → AG04 act=0
AG05 act=1 → AG05 act=0
```

Each key therefore retains both its individual lighting address and its distinct `v.oai.hid` press/release event on Layer 3.

## Live Herdr proof

The pinned [`house-of-herdr` Codex Micro plugin](https://github.com/alasano/house-of-herdr/tree/7d8eadaed41a1bb4456565d6bcba8cdb7380b77e/packages/codex-micro) was built in `/tmp` and run directly in the foreground with temporary plugin configuration/state. It was not installed.

Runtime:

```text
Herdr 0.7.5
house-of-herdr 7d8eadaed41a1bb4456565d6bcba8cdb7380b77e
device connected
subscribed to 10 agent panes
policy sticky
```

The six slots included live Codex, Claude, and Pi panes in `done`, `working`, and `idle` states. The user physically confirmed:

- all six Layer 3 keys repainted to their live Herdr status colors; and
- pressing Agent Key 1 focused its assigned Pi pane, after which the user changed focus back to the current Codex pane.

The daemon was stopped with `SIGINT`. Its shutdown path blanked the six keys/ring, cleared its Herdr key metadata, removed the temporary control socket, and released HID. A fresh `device.status` round trip then returned firmware `v0.4.1` and `layer_index: 3`.

Validation caveats:

- the production TypeScript build passed and `npm audit` found no vulnerabilities;
- the pinned package's lockfile omits its declared `vitest` dependency, so the upstream test suite was not runnable with `npm ci`; and
- the plugin auto-yields only to the literal `ChatGPT.app` path. It does not detect `/Applications/Codex.app/Contents/MacOS/ChatGPT` or Input, so those apps must be kept quit while the bridge owns the device.

## Layer 2 sync

On 2026-07-26, Layer 2 was diagnosed as having zero OAI assignments while
Layers 1 and 3 each had 16. Before the repair, the complete Input database and
the original Layer 2 were copied to:

```text
backups/20260726-layer2-oai/input_storage.json
backups/20260726-layer2-oai/layer2-layout.json
```

The complete backup SHA-256 is:

```text
6ea3a4d8da05f5c7801a837bf314a00965d1acce60f8459d33b242f8f7858abd
```

[`clone-oai-layout.mjs`](./clone-oai-layout.mjs) copied only
`Layer 1.layout` to `Layer 2.layout`; every Layer 2 field outside `layout`
was preserved. Input 0.17.2 then synchronized the edit over USB after a
temporary layer-name change. A clean quit/reopen forced a hardware read-back,
which passed:

```text
OAI keycodes by layer == 16 / 16 / 16
Layer 1.layout == Layer 2.layout
Layer 2 name == "Layer"
Layer 2 lights block preserved == true
```

Input was fully quit again. The uncommitted `herdr-micro` bridge reopened the
device, reported `connected`, discovered 10 agents, and filled all six slots.
The user then switched to Layer 2 and physically confirmed that the six Agent
keys displayed their individual live Herdr status colors. This proves that the
Layer 2 OAI addresses and volatile per-key RGB work together on-device.
The user pressed multiple Agent keys and confirmed that each moved focus to its
assigned Herdr pane, then returned to the current pane. Dial direction remains
pending.

The first dial test exposed two faults:

- the firmware's `ENC_CW`/`ENC_CC` events were opposite the physical effort
  direction, so the bridge mapping was reversed; and
- the tested Pi pane had not loaded `herdr-effort.js`.

The bridge now maps `ENC_CW` to lower and `ENC_CC` to raise, with a regression
test. Pi's explicit extension configuration now includes:

[`integrations/pi/herdr-effort.js`](../../integrations/pi/herdr-effort.js)

The previous Pi settings were backed up at
`~/.config/herdr/plugins/config/gjermundgaraba.herdr-micro/archive/20260726-pi-effort/settings.json`.
The tested Pi pane passed a direct
`medium → high → medium` check. Existing Pi processes that predate the settings
change require a restart; they were not interrupted.

The user physically confirmed the corrected direction in Pi. The bridge logged
21 successful Pi effort changes during the test with no command errors. Some
small turns required additional movement before the firmware emitted a
detent/event; this is a hardware/firmware sensitivity caveat, not a failed
agent command.

The user also physically confirmed Layer 2 dial effort control in Claude Code.
Together with the Codex and Pi checks, all three supported agent integrations
are proven on the hardware.

## Automatic Layer 2 selection

On 2026-07-26, `herdr-micro` added a permission-free native macOS foreground
probe. It reports the frontmost bundle ID and the top layer-zero window title.
The bridge evaluates `claims.json` once per second; the last matching rule
wins. It sends synthetic `host.focused_app` identities so firmware AppSense,
not simulated key presses, performs the layer change.

The device now contains these explicit bindings:

```text
Layer 1 linkedAppId=1 → gjermundgaraba.herdr-micro.layer-1
Layer 2 linkedAppId=0 → gjermundgaraba.herdr-micro.layer-2
```

The first clean-room `fs.writebin` attempt revealed that the firmware requires
both `offset` and `append: true`. Without `append`, it committed only the final
chunk. The complete 1,811-byte pre-write file at:

```text
~/.local/state/herdr/plugins/gjermundgaraba.herdr-micro/
  keymap-before-appsense-1785054237569.json
```

was immediately restored with the corrected parameters and parsed successfully.
The final setup created a second pre-write backup,
`keymap-before-appsense-1785054323291.json`, wrote the two bindings, and passed
an exact 1,898-byte device read-back.

Two `device.status` round trips then proved both transitions:

```text
foreground claim absent  → layer_index=1
foreground claim matched → layer_index=2
```

The temporary Chrome test claim was removed. The final latch rules are:

```text
com.mitchellh.ghostty → Layer 2
com.openai.codex       → Layer 1
unclaimed app          → no command; preserve the last applicable layer
```

This corrects the initial no-match fallback, which explicitly selected Layer 1
instead of preserving the last claim. Work Louder Input is quit and the bridge
is running. The exact Ghostty → Chrome → Codex → Chrome command sequence passes
its regression test as Layer 2 → no command → Layer 1 → no command. The
keyboard was USB-absent at the final daemon reload, so repeating that physical
four-step sequence remains pending reconnect.

After USB reconnect, the user confirmed Ghostty → Chrome preserved Layer 2 and
Codex selected Layer 1, but returning to Ghostty did not restore Layer 2. Live
status isolated the cause:

```text
foreground: Ghostty
device owner: /Applications/Codex.app/Contents/MacOS/ChatGPT
Codex process: still running in the background
```

The bridge's owner policy was process-lifetime based. A clean-room shared-HID
check opened the device while background Codex remained running, sent the
Layer 2 AppSense identity, and read back `layer_index: 2`. The bridge now:

- refreshes the foreground window even while yielded;
- always yields to a running Input process;
- yields to Codex/ChatGPT only while its bundle is frontmost; and
- rechecks ownership once per second.

The exact background-Codex regression test passes. With Codex still running
and Ghostty frontmost, the reloaded bridge reclaimed the device, reported
`connected`, and logged `layer 2 claimed by herdr`. All nine tests pass.

## Runtime proof

- USB/BLE reconnect and BLE sleep/wake recovery were physically verified on
  2026-07-26–27; see [wireless bridge evidence](./wireless-bridge-evidence.md).
- Window-title restrictions remain optional if Ghostty later hosts non-Herdr
  work that should not claim Layer 2.

## Rollback

To roll back Layer 2, use its 2026-07-26 backup as source data and restore only
that layer through Input. The simplest Layer 3 rollback is to delete disposable
Layer 3 in Input after allowing it to read the current hardware state.

Do not blindly overwrite the current Input database with the old two-layer backup after later changes. Use the backups as evidence/source data, or restore only the intended layer through Input so its hardware checksum remains current.
