# Input.app artifact evidence

Inspected 2026-07-25, read-only. I did not launch Input, send HID traffic, change its caches, or touch keyboard state.

Unless a statement is explicitly marked as an inference, it is directly supported by the installed artifact, its signed updater copy, the local device cache, or Input's own log.

## Answer

Input 0.17.2 does not contain the Codex per-agent status transport or expose a third-party API. It **does**, however, provide the configuration seam used by the working Layer 2 bypass: its importer and serializer preserve copied `KV_OAI_*` keycodes and allow the target layer's generic `lights` block to be omitted. Firmware then recognizes that layer as OAI-enabled and exposes its six individual Agent LEDs.

The most important operational finding is that Input should be quit while a Herdr/Codex Micro bridge owns the vendor HID interface. Input opens the interface non-exclusively, but local logs show it receiving another client's replies and intermittently failing its own writes.

## Artifact identity and source quality

- Installed bundle: `/Applications/input.app`
- Bundle/version: `it.focusense.input-app`, `0.17.2`
- Signing identity: `Developer ID Application: Focusense Srl (86245L52HA)`
- Packaged code: `/Applications/input.app/Contents/Resources/app.asar`
- ASAR SHA-256: `5ffc6ed367e8b823e516e3011e22fee256b820df6092b9c2f070f8ec4acf37cf`
- The ASAR is byte-identical to the one in Input's locally cached updater ZIP at `~/Library/Caches/input-updater/pending/input-0.17.2-arm64-mac.zip`. The app extracted from that ZIP passes strict, deep signature verification.

The ASAR contains:

- `dist-electron/main/index.js`: Input's Electron main process.
- `dist-electron/preload/preload.mjs`: the renderer's private IPC surface.
- `dist/assets/device_config_data-DNi9jlMz.js`: the configurator, device defaults, and feature gates.
- `node_modules/@worklouder/wl-device-kit` 0.1.23.

The `wl-device-kit` JavaScript source map contains complete `sourcesContent` for the original TypeScript, including device discovery, HID framing, JSON-RPC, and release handling. Those sources carry Work Louder proprietary/confidential headers, and the package declares `UNLICENSED`. They are strong primary evidence but are not a redistributable SDK.

The installed bundle currently fails deep signature verification because `Resources/scripts/window-info-retriever.scpt` differs byte-for-byte from the signed updater copy. Both files decompile to identical AppleScript source; the installed file has a later modification time. This is consistent with compiled-script metadata being rewritten, but that explanation is an inference. The application JavaScript/ASAR itself exactly matches the signed updater artifact.

## Device and layer behavior

The device registry in `wl-device-kit` uses USB vendor ID `0x303A` and vendor HID usage page `0xFF00`.

| Device | Product IDs | Input 0.17.2 behavior |
| --- | --- | --- |
| Codex Micro | `0x8360` | Special OAI Layer 1, locked in Input; up to five additional ordinary layers |
| Creator Micro 2 | `0x8297`, `0x8298` | Ordinary blank default layer; no OAI keycodes or Layer 1 lock |

The Codex Micro default Layer 1 is hard-coded as:

- Agent Keys: `KV_OAI_AG00` through `KV_OAI_AG05`
- Action Keys: `KV_OAI_ACT06` through `KV_OAI_ACT12`
- Dial: `KV_OAI_ENC_CC`, `KV_OAI_ENC_CW`, `KV_OAI_ENC_CLK`
- Joystick: vendor mode

Input enforces the reservation in several independent UI paths:

- It overlays Layer 1 with “To edit this layer please use Codex Micro app.”
- It disables key selection, renaming, lighting, deletion, duplication, reordering, and moving another layer into position 1.
- It permits a maximum of six layers.
- When creating a Codex profile, it deliberately does not attach an ordinary lighting configuration to the first layer.

The user's cached device configuration independently confirms the firmware-derived state:

- `~/Library/Application Support/input/devices/33632/keymap.json`
- `~/Library/Application Support/input/input_storage.json`
- Device PID `33632` is `0x8360`.
- The current cached profile has only the reserved OAI Layer 1. Input's historical log also records a configuration containing an ordinary Layer 2 with whole-zone lighting.
- A live status recorded in `~/Library/Logs/input/main.log` reported firmware `v0.4.1`, profile index `0`, and layer index `1` (zero-based Layer 2).

Creator Micro 2 is materially different inside Input: its default profile is one blank ordinary layer, and the Codex-only lock conditions are not applied. Input does recognize CM2 lighting, app-aware layers, presets, dial, and radial joystick support. It contains no CM2 Agent Mode configuration. **Inference:** the advertised CM2 Agent Mode must be supplied by some other app/firmware path; Input 0.17.2 is not that provider.

## RGB: what Input can and cannot do

Input's general device API exposes `lights.preview`. Its own TypeScript documentation says the preview is immediate and non-persistent. The payload contains only:

```text
backlight: effect, brightness, speed, magic, color
underglow: effect, brightness, speed, magic, color
```

Supported effects are `off`, `solid`, `snake`, `rainbow`, `breath`, and `gradient`.

Input enables that whole-zone editor on Codex Layers 2–6 and on Creator Micro 2. It explicitly hides it on Codex Layer 1. **Integration inference, not hardware-tested in this inspection:** an external bridge could use the same preview call as an aggregate Layer 2 signal—for example, make the whole backlight breathe amber when any Herdr agent is blocked. It cannot display six independent agent states.

Exact string inspection across Input's main process, configurator, and `wl-device-kit` found:

- `v.oai.thstatus`: absent
- `v.oai.rgbcfg`: absent
- `v.oai.hid`: absent
- `v.oai.rad`: absent
- `@worklouder/device-kit-oai`: absent

The OAI keycode names occur only in the Codex default keymap. Input knows how to preserve the reserved layer, but it does not implement its live agent-status transport. That transport lives in Codex.app and the clean-room community bridges previously documented.

Input's layer-import code checks the target device type and keyboard language but does not reject arbitrary keycode strings. Its rehydrator stores the raw `layout`, and the device serializer preserves `KV_OAI_*` while omitting an absent `lights` property. Subsequent physical-device tests confirmed that copying Layer 1's OAI layout through this seam enables the six live Agent LEDs on Layer 2. Input is the configuration carrier; the firmware and external status publisher provide the behavior.

## App-aware Layer 2

Both Codex Micro and Creator Micro 2 have Input's `focusedApp` feature enabled.

While either is connected, Input:

1. Polls the frontmost macOS application every second.
2. Sends `host.focused_app` only when the app identity changes.
3. Lets each layer reference a linked application.
4. Allows firmware to select that layer from the reported foreground app.

This is usable: link Layer 2 to Herdr if Herdr has its own macOS bundle, or to the terminal application that contains Herdr.

The limitation is significant. Input's AppleScript retrieves application name, bundle ID, path, and focused-window title, but the main process retains only application name and bundle ID. It discards path and window title before calling `host.focused_app`. Consequently it cannot distinguish Herdr, Codex CLI, Claude Code, and Pi when they all run inside the same Terminal, Ghostty, iTerm, or similar bundle.

## Coexistence and HID ownership

The general device kit:

- scans for devices every second;
- automatically connects to every recognized device;
- opens HID with `nonExclusive: true` on macOS;
- uses 64-byte reports with report ID `0x06`, debug channel `1`, RPC channel `2`, and up to 61 payload bytes;
- serializes only its own outgoing queue, with no cross-process lock or ownership protocol.

There is no awareness of Codex.app, ChatGPT, Herdr, or another bridge, and no handoff mechanism.

The user's Input log supplies direct coexistence evidence. At `main.log` lines 2334–2336, Input records replies for `v.oai.rgbcfg`, `v.oai.thstatus`, and `device.status` with “No resolver found,” meaning those requests originated from another HID client but their replies were also delivered to Input. Six seconds later, Input's own `host.focused_app` write fails with:

```text
IOHIDDeviceSetReport failed: (0xE00002E2) ... not permitted
```

Successful Input requests and these failures alternate elsewhere in the log. This does not prove every failure was caused by contention, but it proves non-exclusive opening is not reliable ownership isolation and that Input observes other clients' protocol traffic.

**Practical rule:** configure and persist Layer 2 in Input, then quit Input before starting `house-of-herdr`, `microd`, Codex.app, or another live RGB bridge. Do not design around simultaneous writers.

## Integration surface and disabled capabilities

Input has no plugin loader, local HTTP/WebSocket server, socket protocol, or documented external API. Its preload script exposes fixed Electron IPC methods only to Input's renderer. These include device status, file operations, configuration writes, and generic lighting preview; they do not include arbitrary JSON-RPC or OAI status calls.

`WLRPCApi.getRpcClient()` can issue raw experimental calls from code already using the proprietary package. This is not a usable public integration surface because the package is bundled, private, and `UNLICENSED`. The clean-room protocol implementations remain the safer boundary.

Input also contains a mostly complete “Smart Actions” pipeline:

- firmware notifications for inserting text, executing a shell command, opening an application, and opening a URL;
- storage and import/export handling;
- a separate consent flag before commands execute.

However, `isSmartActionEnabled()` is hard-coded to `false` for every device in this build. The normal UI hides Smart Actions and the command-consent setting; the local command setting is currently `false`. Treat this as dormant code, not a supported way to dispatch Codex/Claude/Pi effort changes.

## Firmware and updates

- Input updates itself from `worklouder/input-releases`.
- Creator Micro 2 firmware maps to `worklouder/cm-v2-fw-releases`.
- Codex Micro has no firmware repository mapping in `WLRelease`; it falls through to `unknown`.
- Stable Input builds only offer firmware updates after confirming that Input itself is current.
- Creator Micro 2 firmware `0.2.1-dirty` is specially treated as a stable build.
- The advanced UI can manually select and flash a `.bin`, but this is not evidence that arbitrary CM2 or Codex firmware is interchangeable.

Therefore Input can update Creator Micro 2 firmware, but Input 0.17.2 does not automatically discover or download Codex Micro firmware.

## Privacy-related artifact note

Input's normal analytics service checks `analyticsConsented`; the local setting is `false`.

Separately, the device diagnostic notification `diag.report` posts the device-provided category and payload, a persistent locally generated user ID, and device model to a Google Forms endpoint. No analytics-consent check is visible in that diagnostic handler. This is a code-path observation, not evidence that the keyboard emitted a diagnostic report.

## Reproduction

The inspection can be repeated without launching Input:

```sh
/usr/libexec/PlistBuddy -c 'Print :CFBundleShortVersionString' \
  /Applications/input.app/Contents/Info.plist

shasum -a 256 /Applications/input.app/Contents/Resources/app.asar

node /path/to/@electron/asar/bin/asar.mjs extract \
  /Applications/input.app/Contents/Resources/app.asar /tmp/input-asar

rg -n 'KV_OAI|CodexMicro|CreatorMicroV2|sendLightingPreview|deviceHasAppAware' \
  /tmp/input-asar/dist/assets/device_config_data-*.js

rg -n 'focus_app_service|IOHIDDeviceSetReport|v\\.oai|layer_index' \
  "$HOME/Library/Logs/input/main.log"
```

For the original general device-kit TypeScript, inspect the `sources` and `sourcesContent` arrays in:

```text
/tmp/input-asar/node_modules/@worklouder/wl-device-kit/dist/index.js.map
```

## Bottom line for the proposed setup

- Use Input to sync an OAI-enabled Layer 2 containing `KV_OAI_AG00` through `KV_OAI_AG05`.
- Initially omit generic Layer 2 lighting because it may paint over Agent status colors.
- Quit Input and Codex while the Herdr bridge owns the device.
- Use a clean-room Herdr bridge to publish the six states and consume the vendor events.
- Do not expect Creator Micro 2 Agent Mode to come from Input 0.17.2.
- Do not build against Input's bundled proprietary package; reuse a clean-room bridge.
