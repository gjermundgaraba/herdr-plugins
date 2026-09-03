# Codex Micro evidence record

This is the durable evidence behind `herdr-micro`. It intentionally omits the
superseded option surveys, experiment plans, and implementation diary.

## Current cutover baseline (2026-08-26)

- The connected Codex Micro reported stock firmware `0.6.2` from a successful
  `device.status` round trip over USB.
- The clean-break implementation uses shared, unprivileged IOHIDManager access
  with USB preferred and Bluetooth Low Energy supported. At this checkpoint,
  the complete new service path has not yet been physically exercised end to
  end, and BLE has not yet been reverified on firmware 0.6.2. Do not interpret
  the older traces below as that verification.
- The service alone enforces the official-writer gate: it closes the physical
  device while Work Louder Input is running or ChatGPT/Codex desktop is
  frontmost, then reopens and replays state after the gate clears. Herdr may
  read the same detector only to suppress routing. Physical verification of
  this gate is included in the pending end-to-end check above.
- The new per-user service consumes Button 5 (`ACT10`) and launches Handy with
  `--toggle-transcription` directly. Physical end-to-end verification of this
  new no-CGEvent path is pending at this checkpoint.

## Historical physical evidence

The following experiments established the hardware and protocol behavior used
by the redesign. Exclusive-helper and synthetic-key traces are preserved as
evidence, but their ownership architecture and F19 action path are superseded.

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
  Claude Code, and Pi, automatic Layer 1/2 selection, and default/named Herdr
  session routing were exercised on the physical device.
- USB/BLE reconnect and BLE standby recovery repaint the current Herdr state.
  The device enters battery standby after about 15 minutes and requires a
  physical input to wake.

### Wide-key trace (2026-08-03)

A passive Bluetooth LE capture requested five presses of the left switch,
five of the right switch, then five of both. It received five `ACT10` and
seven `ACT11` press/release pairs with no disconnect. The first two complete
pairs were `ACT11` alone; later reports contained both codes, including one
`ACT10` press 832 ms before the corresponding `ACT11` press. This proves that
both physical switches are independently usable.

A slower capture of three installed-cap presses at each of its left, center,
and right positions received exactly nine `ACT10` press/release pairs and
eleven `ACT11` pairs. BLE buffered some edges, but there was no disconnect.
The stock double-width keycap can actuate either or both switches depending on
press position. Dual events are therefore expected mechanical behavior, not
evidence that `ACT11` is unusable; bind the two switches independently only
when that is the intended interaction.

### Historical native Layer 2 action trace (2026-08-08)

After programming Layer 2 with native OAI action codes under exclusive USB
capture, physical `ACT12` submitted to the focused Codex pane. Physical
`ACT10` produced the then-configured synthetic F19 tap; Handy received it and
completed record, transcription, and paste. Both inputs arrived through the
old helper's exclusive vendor-event path; no host keyboard report was involved.
This proves the switch and vendor event, not the current service action. The
current design consumes `ACT10` and invokes Handy directly without F19 or a
CGEvent.

### Historical exclusive-helper recovery and focus handoff (2026-08-04)

These results apply only to the deleted USB-seize/root-helper architecture.
They are retained as failure and latency evidence, not as current behavior.

- With the user daemon frozen, `SIGKILL` left the Micro captured and absent
  from `hidutil` after five seconds. A fresh helper recaptured it, and a
  controlled close restored both native HID services in 100 ms without an
  unplug. A second forced helper death recovered in 1.09 s with helper build 2
  while ChatGPT was frontmost; status cleared and both HID services returned.
- With one-shot helper build 13 on 2026-08-09, a clean daemon stop removed both
  processes and the next start received a fresh launchd helper PID. Killing the
  captured helper with `SIGKILL` produced a disconnect at `11:28:21.019`, a
  fresh device connection at `11:28:21.117`, and Layer 2 at `11:28:21.168`
  without a replug. F19, Submit, an Agent key, one-notch effort changes, and
  Diff all remained prompt after recovery.
- With helper build 14, killing captured helper PID 39224 produced a disconnect
  at `16:32:48.577`, a fresh helper and device connection 94 ms later, and
  Layer 2 after 145 ms without a replug. F19, Submit, Agent focus and lighting,
  effort, and Diff all passed afterward.

## Tested version boundaries

| Component | Physically tested result |
|---|---|
| Codex Micro firmware 0.4.1 | USB and BLE vendor channel, controls, routing, and RGB passed |
| Codex Micro firmware 0.6.1 | USB passed; BLE passed after pairing a fresh host slot |
| Codex Micro firmware 0.6.2 | USB `device.status` passed; full new service and BLE checks pending |
| Work Louder Input 0.17.2 | OAI-enabled Layer 2 clone and read-back passed |
| Work Louder Input 0.18.0 | Firmware 0.6.1 update and retained keymap passed |
| Herdr 0.8.0 | Direct snapshots, targeting, and effort actions passed |
| Codex CLI 0.145.0 | `high → xhigh → high` passed |
| Claude Code 2.1.220 | `xhigh → high → xhigh` passed |
| Pi 0.82.1 | `medium → high → medium` passed with the bundled extension |

The effort-control rows were exercised through Herdr on 2026-07-26. The
action runner targeted pane IDs directly rather than the frontmost macOS
window; the final Layer 2 test confirmed the physical dial integration. Those
results predate the configurable script adapter.

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
- [Codex CLI 0.145.0 TUI keymap source](https://github.com/openai/codex/blob/rust-v0.145.0/codex-rs/tui/src/keymap.rs)
- [Work Louder Codex Micro product page](https://worklouder.cc/codex-micro)
- [Work Louder Codex Micro setup and BLE pairing](https://worklouder.cc/openai-micro-setup)
- [Work Louder firmware 0.6.1 release](https://github.com/worklouder/cm-v2-fw-releases/releases/tag/v0.6.1)
- [Work Louder firmware 0.6.2 release](https://github.com/worklouder/cm-v2-fw-releases/releases/tag/v0.6.2)
- [OpenAI × Work Louder product page](https://openai.com/supply/co-lab/work-louder/)
- [FreeMicro transport implementation and hardware record](https://github.com/eliBenven/freemicro/tree/1e78198c1b4bfe43b7e4aee3246c73314b9bcc0f)
- [house-of-herdr behavioral reference](https://github.com/alasano/house-of-herdr/tree/7d8eadaed41a1bb4456565d6bcba8cdb7380b77e/packages/codex-micro)
