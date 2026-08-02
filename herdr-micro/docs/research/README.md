# Codex Micro evidence record

This is the durable evidence behind `herdr-micro`. It intentionally omits the
superseded option surveys, experiment plans, and implementation diary.

## Physically verified facts

- The Codex Micro vendor HID interface is `0x303A:0x8360`, usage page
  `0xFF00`, Report ID 6. Direct macOS IOKit transport completed
  `device.status`, received Agent-key/encoder/joystick events, and controlled
  lighting over USB and Bluetooth LE.
- A Layer 2 containing `KV_OAI_AG00` through `KV_OAI_AG05` can display six
  independent Herdr agent lights and emit the corresponding vendor events.
  The device's original Layer 1 remains available for the Codex desktop app.
- Six Agent LEDs are independently controllable. On the tested Codex Micro,
  thread IDs 6–12 did not light the seven lower keys; lower-key backlight and
  perimeter lighting are aggregate zones.
- Agent-key focus, configured prompts/actions, dial effort changes for Codex,
  Claude Code, and Pi, joystick scrolling, automatic Layer 1/2 selection, and
  default/named Herdr session routing were exercised on the physical device.
- USB/BLE reconnect and BLE standby recovery repaint the current Herdr state.
  The device enters battery standby after about 15 minutes and requires a
  physical input to wake.

## Tested version boundaries

| Component | Physically tested result |
|---|---|
| Codex Micro firmware 0.4.1 | USB and BLE vendor channel, controls, routing, and RGB passed |
| Codex Micro firmware 0.6.1 | USB passed; BLE passed after pairing a fresh host slot |
| Work Louder Input 0.17.2 | OAI-enabled Layer 2 clone and read-back passed |
| Work Louder Input 0.18.0 | Firmware 0.6.1 update and retained keymap passed |
| Herdr 0.7.5 | Agent targeting and effort actions passed |
| Codex CLI 0.145.0 | `high → xhigh → high` passed |
| Claude Code 2.1.220 | `xhigh → high → xhigh` passed |
| Pi 0.82.1 | `medium → high → medium` passed with the bundled extension |

After the 0.6.1 firmware update, an existing BLE host pairing retained stale
GATT metadata and rejected output reports. Pairing an unused BLE slot restored
the full vendor channel. If every slot is occupied, forget and re-pair the
affected `Codex Micro #N`; that same-slot recovery follows Work Louder's
documented pairing flow but was not separately retested. USB is the safe
recovery path.

These are tested boundaries, not compatibility guarantees for later firmware,
apps, operating systems, or other Work Louder models.

## Unsupported protocol caveat

The OAI actions, JSON-RPC methods, and transport framing are proprietary and
have no published third-party SDK, protocol specification, support contract,
or device-writer coordination API. Do not copy or redistribute bundled vendor
packages. Run exactly one vendor-HID writer. A firmware, Input, Codex, or macOS
change can break this integration without notice.

## Primary sources

- [Herdr plugin guide](https://herdr.dev/docs/plugins/)
- [Herdr CLI reference](https://herdr.dev/docs/cli-reference/)
- [Herdr agents and states](https://herdr.dev/docs/agents/)
- [Work Louder Codex Micro product page](https://worklouder.cc/codex-micro)
- [Work Louder Codex Micro setup and BLE pairing](https://worklouder.cc/openai-micro-setup)
- [Work Louder firmware 0.6.1 release](https://github.com/worklouder/cm-v2-fw-releases/releases/tag/v0.6.1)
- [OpenAI × Work Louder product page](https://openai.com/supply/co-lab/work-louder/)
- [FreeMicro transport implementation and hardware record](https://github.com/eliBenven/freemicro/tree/1e78198c1b4bfe43b7e4aee3246c73314b9bcc0f)
- [house-of-herdr behavioral reference](https://github.com/alasano/house-of-herdr/tree/7d8eadaed41a1bb4456565d6bcba8cdb7380b77e/packages/codex-micro)
