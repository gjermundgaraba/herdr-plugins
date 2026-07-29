# Codex Micro wireless bridge evidence

Research date: 2026-07-26  
Updated: 2026-07-27  
Scope: Bluetooth LE versus a possible 2.4 GHz receiver, vendor HID/RGB parity,
and the current `herdr-micro` transport assumptions.

## Conclusion

**The Codex Micro hardware and vendor protocol work over Bluetooth LE. The
Herdr Micro bridge now uses direct IOKit, detects USB versus BLE, and is
physically verified on both transports.**

There is no first-party evidence of a 2.4 GHz dongle mode. Work Louder
documents only Bluetooth and USB-C, describes three **BLE** host channels plus
wired mode, and lists only a USB-C cable in the box. This establishes that no
dongle is documented or supplied; it does not prove the radio silicon could
never support another mode.

## Evidence

### 1. The documented wireless connection is BLE, not a dongle

**Evidence level: high — first-party product and setup documentation.**

- Work Louder specifies `Bluetooth / USB-C` and lists the included cable, with
  no receiver: [Codex Micro specifications](https://worklouder.cc/codex-micro).
- Work Louder calls the wireless mode `BLE`, provides channels 1–3, and says
  the fourth selection is wired. A cable inserted while BLE is selected only
  charges the device; it does not switch transport:
  [Codex Micro setup](https://worklouder.cc/openai-micro-setup).
- OpenAI likewise lists Bluetooth and USB-C and no receiver:
  [OpenAI × Work Louder](https://openai.com/supply/co-lab/work-louder/).

### 2. The complete vendor channel works over BLE

**Evidence level: high for one shipping unit — independent MIT implementation,
hardware-verified on macOS with firmware v0.4.1.**

FreeMicro verified the exact `0x303A:0x8360`, usage-page `0xFF00`, Report-ID 6
channel with the cable unplugged. Key, encoder, and joystick notifications
arrived over BLE, while `v.oai.rgbcfg` and `v.oai.thstatus` visibly drove the
lighting. See the
[protocol transport table and wireless test](https://github.com/eliBenven/freemicro/blob/1e78198c1b4bfe43b7e4aee3246c73314b9bcc0f/docs/PROTOCOL.md#L7-L35)
and the
[capability record](https://github.com/eliBenven/freemicro/blob/1e78198c1b4bfe43b7e4aee3246c73314b9bcc0f/hardware/capabilities.json#L166-L180).

BLE uses the same JSON-RPC methods but different direct-IOKit write framing:

| Transport | Output buffer | Length | Report ID argument |
|---|---|---:|---:|
| USB | `[0x02][len][JSON…]` | 63 | 6 |
| Bluetooth LE | `[0x06][0x02][len][JSON…]` | 64 | 6 |

A malformed BLE write can return success and still be discarded. FreeMicro
therefore validates the link with a `device.status` request/reply rather than a
write return code. Its
[transport implementation](https://github.com/eliBenven/freemicro/blob/1e78198c1b4bfe43b7e4aee3246c73314b9bcc0f/src/freemicro/device/codex_micro.py#L39-L110)
selects framing from IOKit's `Transport` property, and its
[discovery code](https://github.com/eliBenven/freemicro/blob/1e78198c1b4bfe43b7e4aee3246c73314b9bcc0f/src/freemicro/device/codex_micro.py#L563-L610)
matches VID/PID, prefers USB when both paths exist, and handles the different
BLE product string and collection count.

### 3. Original bridge limitation

**Historical evidence:** the first `herdr-micro` bridge used `node-hid`. It
could open the BLE device, but `device.status` timed out because the direct
IOKit BLE framing differs from USB. The bridge now uses direct IOKit,
validates the connection with `device.status`, and is physically verified on
both transports.

## Physical test result

Tested on 2026-07-26 with the cable unplugged:

- macOS enumerated `Codex Micro #1` over Bluetooth LE, including usage page
  `0xFF00`;
- the current `node-hid` bridge opened a path but `device.status` timed out,
  including after trying the BLE-prefixed write;
- direct IOKit returned firmware `v0.4.1`, battery `100`, and
  `layer_index: 2`; and
- direct IOKit set all six Agent keys red independently of USB. The user
  confirmed they stayed red beyond the 30-second hold test.

The failure boundary was therefore the macOS `node-hid` path, not BLE,
firmware, or RGB. The required fix was to replace the HID transport with the
proven direct-IOKit path while keeping the existing Herdr state, slot, layer,
and effort logic.

That replacement is now implemented. The bridge uses one direct-IOKit helper,
keeps the existing Herdr logic, detects the IOKit `Transport` property, and
requires a `device.status` reply before reporting `connected`. The user
physically confirmed RGB, Agent-key focus, effort control, sticky automatic
layers, BLE sleep/wake recovery, and BLE-to-USB reconnection. USB was then
reverified for RGB, Agent-key focus, and effort control.

## Battery standby

Investigated on 2026-07-27 after normal BLE use repeatedly left the bridge
reporting the device `absent`.

- The bridge log contains a clean 14 minute 59 second connected interval before
  IOKit reported `Codex Micro disconnected`. Longer intervals included physical
  activity. This matches Work Louder's intentional 15-minute battery standby.
- Work Louder's firmware `v0.6.0-rc.7` release notes specify that lights dim
  after 5 minutes and standby begins after 15 minutes on battery; pressing or
  rotating a control wakes the device.
- Firmware inspection found internal `keep_awake`, `set_idle_time`,
  `set_stand_by_time`, and `set_min_power_state` functions, but none is
  registered in the vendor JSON-RPC method table.
- The timeout setters use internal NVS keys with defaults 5 and 15. They are not
  files reachable through the public `fs.*` methods, and Input exposes no
  standby setting.
- A host cannot wake the pad after standby because the firmware removes the BLE
  device from macOS. The bridge already reconnects and repaints after a physical
  key or dial wake.

There is no proven supported cable-free way to disable standby. The safe
always-on choices are wired mode or BLE mode with USB power left connected.
Do not probe undocumented power methods, edit board metadata, or flash modified
firmware merely to change this timeout. A periodic `device.status` request is
not a justified workaround: it is unproven to reset the firmware's physical
activity timer, and the normal Codex host already polls status once per minute.

Primary source:
[Work Louder firmware v0.6.0-rc.7](https://github.com/worklouder/cm-v2-fw-releases/releases/tag/v0.6.0-rc.7).
