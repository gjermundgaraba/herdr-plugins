# Codex Micro RGB device protocol evidence

Research date: 2026-07-25  
Scope: per-agent RGB/status control, device discovery, transport, RPC payloads, layer gating, and multi-client safety.

## Bottom line

The Codex Micro has a real host-to-device RGB/status protocol. The installed Codex desktop app uses JSON messages over a vendor HID interface to set six per-agent lights plus the keys and ambient lighting zones.

However:

- Work Louder has not published a Codex Micro SDK, protocol specification, package, or third-party license.
- The installed device library is private, `UNLICENSED`, and marked proprietary/confidential.
- macOS opens the HID interface non-exclusively, but the observed protocol has no lease, owner identity, generation number, or cross-process write arbitration.
- Work Louder explicitly documents communication interference, and Input 0.17.2 needed a fix for interference with Codex.

**Recommendation:** do not run a separate RGB writer alongside Codex or Input. A clean-room community implementation now exists: `house-of-herdr` drives the six LEDs as the sole owner of reserved Layer 1. Use it only as a pinned, isolated experiment. For a supported version—or live Herdr RGB specifically on Layer 2—use ordinary Layer 2 controls while requesting Work Louder SDK access and ownership rules. See [source/community research](./source-and-community-workarounds.md).

## Evidence labels

- **Official fact** — documented by Work Louder or OpenAI.
- **Artifact fact** — reproduced from the installed Codex/Input application artifacts.
- **Inference** — conclusion from the observed behavior; not a vendor guarantee.
- **Unknown** — not established by public documentation or safe local inspection.

## 1. Public support status

### Official facts

- OpenAI describes each Agent Key as showing live RGB status from Codex. [OpenAI × Work Louder product page](https://openai.com/supply/co-lab/work-louder/)
- Work Louder documents white for idle, blue for thinking, green for complete, amber for input required, red for error, and off for no assigned agent. [Codex Micro setup](https://worklouder.cc/openai-micro-setup)
- Work Louder's public repositories contain release artifacts, but no public Codex Micro SDK or firmware source. [Work Louder repositories](https://github.com/orgs/worklouder/repositories)
- The only public Work Louder SDK alpha located is explicitly restricted to Nomad v1. [SDK alpha 0.1](https://github.com/worklouder/input-releases-internal/releases/tag/sdk-alpha-0.1)
- `input-linux` is hosted by the Work Louder organization but describes itself as unofficial, community-developed, and unsupported. It does not provide a Codex status API. [Input Linux](https://github.com/worklouder/input-linux)

### Artifact facts

Codex desktop 26.721.41059 bundles:

| Package | Version | Status |
|---|---:|---|
| `@worklouder/device-kit-oai` | 0.1.11 | Private OpenAI-specific device layer |
| `@worklouder/wl-device-kit` | 0.1.23 | Nested transport/device layer |

The bundled package metadata says `license: "UNLICENSED"`. Its source headers mark it proprietary and confidential. Its README describes a private GitHub Packages dependency requiring authenticated package access. The public [package URL](https://github.com/worklouder/device-kit-oai/packages) is not accessible.

This establishes that an integration surface exists. It does **not** establish permission to copy, redistribute, import, or depend on that private package.

## 2. Device discovery

### Runtime HID device

| Property | Observed value |
|---|---|
| Vendor ID | `0x303A` / 12346 |
| Codex Micro product ID | `0x8360` / 33632 |
| Required HID usage page | `0xFF00` / 65280 |
| Manufacturer strings used during discovery | `Work Louder`, `Work_Louder` |
| Host transports | USB HID and Bluetooth HID |

The device registry also recognizes Creator Micro 2 product IDs 33431 and 33432. Codex discovery prefers USB over Bluetooth and the Codex Micro PID over the Creator Micro 2 PIDs.

The app considers a device USB-connected when the transport reports `usb`; if the transport is unknown, it uses `release % 4 == 0` as a fallback heuristic.

### Serial is not the normal RGB path

The underlying device kit contains a 115200-baud serial transport, but the Codex Micro runtime service discovers and controls the normal device through HID. Serial discovery is used for an Espressif bootloader/DFU state.

**Conclusion:** an RGB bridge should not assume that plugging in USB exposes a serial status API. It should identify the vendor HID interface by VID, PID, and usage page.

Work Louder also notes that plugging in USB while a Bluetooth channel is active only charges the keyboard; it remains on Bluetooth until wired mode is selected. [Codex Micro setup](https://worklouder.cc/openai-micro-setup)

## 3. HID transport and message framing

### HID report format

Each report is 64 bytes:

| Byte(s) | Meaning |
|---|---|
| 0 | Report ID `0x06` |
| 1 | Channel: `1` debug, `2` RPC |
| 2 | UTF-8 payload length in this report |
| 3–63 | Up to 61 payload bytes |

Messages longer than 61 bytes are split across reports. Incoming payload is accumulated until a newline, then parsed as JSON.

The parser accepts both long and compact response/notification field names:

| Long | Compact |
|---|---|
| `method` | `m` |
| `params` | `p` |
| `id` | `i` |

### Request envelope

Observed requests use:

```json
{
  "method": "v.oai.thstatus",
  "params": [],
  "id": 123
}
```

The request does not include `"jsonrpc": "2.0"`, although the library describes the transport as JSON-RPC. Request IDs are independently generated random integers from 0 through 998.

Within one library instance:

- only one request is in flight;
- requests use a FIFO queue;
- there is a 50 ms cooldown between RPC tasks;
- a request times out after 10 seconds.

Those controls are process-local. They do not coordinate two independent programs.

### macOS handle mode

On macOS the base library opens the HID path with `{ nonExclusive: true }`. Its bundled README says Input Monitoring permission is required for communication.

**Inference:** non-exclusive mode allows more than one operating-system handle. It does not guarantee that multiple writers can safely interleave fragmented JSON messages or share response IDs.

## 4. Exact RGB RPCs

### `v.oai.thstatus` — six per-agent slots

The high-level method accepts an array of thread/agent status objects. Codex sends slot IDs 0–5.

High-level fields:

| Field | Type | Meaning |
|---|---|---|
| `id` | number | Agent slot; Codex uses 0–5 |
| `color` | number | Packed 24-bit `0xRRGGBB` |
| `brightness` | number | Normalized 0–1 |
| `effect` | number | Effect enum |
| `speed` | number | Normalized 0–1 |
| `syncKeysLighting` | boolean | Synchronize keys lighting |
| `syncAmbientLighting` | boolean | Synchronize ambient lighting |

Wire-field compaction:

| High-level field | Wire field |
|---|---|
| `id` | `id` |
| `color` | `c` |
| `brightness` | `b` |
| `effect` | `e` |
| `speed` | `s` |
| `syncKeysLighting` | `sk`, encoded 1/0 |
| `syncAmbientLighting` | `sa`, encoded 1/0 |

Undefined optional values are omitted when serialized.

Example payload before the transport adds the random request ID:

```json
{
  "method": "v.oai.thstatus",
  "params": [
    {"id": 0, "c": 3166206, "b": 1, "e": 4, "s": 0.4, "sk": 0, "sa": 0},
    {"id": 1, "b": 0.5}
  ]
}
```

### `v.oai.rgbcfg` — ambient and keys zones

Wire shape:

```json
{
  "method": "v.oai.rgbcfg",
  "params": {
    "ambient": {"e": 2, "b": 0.8, "s": 0.4, "m": 0, "c": 65356},
    "keys": {"e": 0, "b": 0, "s": 0, "m": 0, "c": 0}
  }
}
```

Zone fields:

| Field | Meaning |
|---|---|
| `e` | Effect |
| `b` | Brightness, normalized 0–1 |
| `s` | Speed, normalized 0–1 |
| `m` | Effect-specific “magic” value, normalized 0–1 |
| `c` | Packed 24-bit `0xRRGGBB` |

Both high-level lighting methods catch transport errors and return `true` or `false` instead of propagating an exception.

### Related device notifications

These are device-to-host notifications, not lighting writes:

| Method | Wire params | Decoded meaning |
|---|---|---|
| `v.oai.hid` | `{k, act?, ag?}` | Key, optional action, optional agent |
| `v.oai.rad` | `{a, d}` | Joystick angle and distance |

## 5. Effects and colors

### Effect enum

| Code | Effect |
|---:|---|
| 0 | off |
| 1 | solid |
| 2 | snake |
| 3 | rainbow |
| 4 | breath |
| 5 | gradient |
| 6 | shallow breath, approximately 0.5–1 brightness |

Codes 0–5 also exist in the base Work Louder device kit. Code 6 is added by the OpenAI-specific layer.

### Codex status palette

| Codex state | Decimal | Hex |
|---|---:|---|
| Working / thinking | 3166206 | `#304FFE` |
| Unread / complete | 65356 | `#00FF4C` |
| Idle | 16777215 | `#FFFFFF` |
| Awaiting approval | 16739584 | `#FF6D00` |
| Awaiting response | 16739584 | `#FF6D00` |
| Error | 16711731 | `#FF0033` |
| Off / unassigned | 0 | `#000000` |

Voice-state colors:

| Voice state | Effect/color |
|---|---|
| Recording | snake at `#2E8B57`, speed 0.4 |
| Processing | snake at `#FFFFFF`, speed 0.4 |
| Completed | solid `#FFFFFF` |

These exact application colors are close to, but more specific than, the public white/blue/green/amber/red legend.

## 6. How Codex composes lighting

### Per-agent slots

For each of six slots:

- off uses color 0, brightness 0, effect off, and speed 0;
- another state uses its state color and the global brightness;
- a selected or pulsing slot uses breath at speed 0.4;
- otherwise it uses solid;
- Codex currently sends both synchronization flags as false.

### Ambient and keys zones

Normally the keys zone is off.

For the selected thread:

- working uses ambient snake at the status color and speed 0.4;
- non-working uses ambient solid at the status color;
- a selection highlight remains visible for 4 seconds;
- during that 4-second highlight, the keys zone is solid in the same color.

Voice activity overrides the selected-thread ambient behavior. A separate internal “snaking ambient status” override can also force snake at a status color.

Other runtime behavior:

- duplicate JSON payloads are not resent;
- lighting writes are serialized through a promise chain;
- writes wait until 100 ms after keyboard/joystick input becomes quiet;
- inactivity auto-off and service shutdown turn off both zones and all six slots;
- a lighting RPC failure invalidates the connection and starts reconnect backoff at 1, 2, 5, then 10 seconds.

## 7. Layer gating

### Official facts

- Work Louder's demo states that Layer 1 is reserved for Codex and Input unlocks five additional layers. [Official demo at 2:19](https://youtu.be/3-2OH6ReiPM?t=139)
- Input exposes ordinary HID, macro, radial-menu, and lighting configuration on editable Layers 2–6.
- Creator Micro v2 firmware 0.6.0-rc.6 says Codex lighting is shown only while a Codex-enabled layer is active, switching layers restores normal lighting, and the release fixes Codex lighting on unrelated layers. [Firmware 0.6.0-rc.6](https://github.com/worklouder/cm-v2-fw-releases/releases/tag/v0.6.0-rc.6)

The firmware release is named for Creator Micro v2, not Codex Micro. It is evidence of the shared implementation direction, but not permission to flash that firmware onto a Codex Micro.

### Artifact fact and inference

The Codex host service does not check the active keyboard layer before sending status updates. Therefore the observed layer suppression is firmware-side.

**Inference:** the keyboard can keep receiving Codex status while Layer 2 is active, but the firmware should display Layer 2's normal lighting rather than Codex status. A third-party status stream would need the same firmware-recognized Codex-enabled-layer behavior; the public Input UI does not expose a way to mark Layer 2 as a Herdr status layer.

## 8. Ownership and contention

### Ownership inside Codex

Codex creates one device service per application process. Within that process:

- only the primary window may submit UI-originated lighting and agent-thread-key updates;
- non-primary windows receive `false` from those update calls;
- key and joystick events are delivered to the primary window;
- primary-window changes generate an ownership-change event.

This solves ownership among Codex windows. It is not a machine-wide device lock and does not coordinate Input or a third-party process.

### Official contention evidence

- Work Louder warns that Karabiner and Logitech Options+ with Input Monitoring can interfere with Codex–Micro communication; a fix is described as in progress. [Codex Micro setup](https://worklouder.cc/openai-micro-setup)
- Input 0.17.2 says it fixed device communication interfering with Codex. [Input 0.17.2](https://github.com/worklouder/input-releases/releases/tag/v0.17.2)
- Input 0.18.0-rc.5 repeats that fix. [Input 0.18.0-rc.5](https://github.com/worklouder/input-releases/releases/tag/v0.18.0-rc.5)

This is evidence that Work Louder intends Input and Codex to coexist after targeted fixes. It is not a general third-party coexistence guarantee.

### Why an independent RGB client is unsafe

The following are strong inferences from the observed protocol:

1. **Last writer wins.** Lighting is immediate mutable device state with no owner or generation token.
2. **Packet streams can interleave.** A status request exceeds one 61-byte packet. The 50 ms queue serializes only one process's writes.
3. **IDs can collide.** Each process chooses from only 999 request IDs without coordination.
4. **Responses are not client-addressed.** Multiple open handles may receive the same replies and notifications; a colliding ID could resolve the wrong request.
5. **An external mutex is incomplete.** A custom bridge can coordinate its own processes, but cannot force the closed Codex/Input apps to join that mutex.

Therefore, **non-exclusive HID is necessary for the vendor's own coexistence strategy on macOS, but insufficient to make arbitrary concurrent writers safe.**

## 9. Integration options, ranked

### 1. Supported now: Layer 2 controls, no custom device writer

Use Input to emit uncommon HID chords from Layer 2. A host adapter maps those chords to:

- Herdr navigation and agent targeting;
- Codex, Claude Code, and Pi effort/model operations;
- approve, reject, interrupt, prompt, and voice actions where each agent supports them.

Use Herdr's agent-state stream for host-side status and display it in Herdr or another desktop surface. Codex retains native Layer 1 RGB.

This gets most controls with the least device risk, but not Herdr per-agent RGB on Layer 2.

### 2. Preferred full integration: vendor-supported single-owner API

Ask Work Louder for:

- supported Codex Micro SDK/API access and redistribution terms;
- per-Agent-Key, keys-zone, and ambient-zone RGB methods;
- exact USB/BLE discovery identifiers and feature parity;
- a daemon, broker, lease, or ownership contract for Codex/Input/third-party coexistence;
- layer-status APIs or a supported way to mark a custom status-enabled layer;
- exact supported firmware and recovery image.

One broker should own the device and accept status updates from Codex, Herdr, and agent adapters.

### 3. Existing community prototype: custom single owner

`house-of-herdr` and `microd` already implement this architecture. If evaluating either before vendor support:

1. Fully quit Codex and Input.
2. Select wired mode explicitly for initial testing.
3. Let exactly one long-lived bridge own discovery, request IDs, framing, and writes.
4. Subscribe to Herdr events and map six selected agents to slots 0–5.
5. Restore all lights to off or the previous safe state on shutdown.

Test Bluetooth separately. Treat protocol/package changes after any Codex, Input, or firmware update as breaking until revalidated.

This is demonstrated by community authors but remains unsupported. Use their clean-room source; do not copy or redistribute the private bundled Work Louder library.

### 4. Not recommended: concurrent direct HID client

Opening a second non-exclusive handle while Codex/Input remains active may appear to work in a light test. It cannot be considered safe from the available evidence because fragmentation, response routing, IDs, and write ownership are uncoordinated.

## 10. Reproducibility record

Local artifacts inspected:

| Artifact | Version / digest |
|---|---|
| `/Applications/Codex.app` | 26.721.41059 |
| Codex `app.asar` SHA-256 | `da39a51b06fb4c728d418b8f0f05fc8fd8c6b1f74c4fb4d47c20c7914a798f45` |
| `@worklouder/device-kit-oai` | 0.1.11 |
| `@worklouder/wl-device-kit` | 0.1.23 |
| Input stable release inspected | 0.17.2 |
| Input 0.17.2 Apple Silicon ZIP SHA-256 | `72bb2ccd2f0de0b21a61cd4006367a64e628010c3e7303e94f219cff5ad45d35` |
| Input 0.18.0-rc.5 Apple Silicon ZIP SHA-256 | `c23139e06f703b7545fa6723a01d57db4c06dec0c6d1ad21c3d733158d060039` |

Relevant installed Codex bundle paths:

- `node_modules/@worklouder/device-kit-oai/dist/rpc_api_oai/rpc_api_oai.js`
- `node_modules/@worklouder/device-kit-oai/dist/rpc_api_oai/rpc_api_oai.d.ts`
- `node_modules/@worklouder/device-kit-oai/dist/rpc_api_oai/types/`
- nested `node_modules/@worklouder/wl-device-kit/dist/index.js`
- Vite chunks containing the Codex Micro host service and state/color mapping

No live HID writes were made during this research.

## 11. Unknowns that require vendor confirmation or hardware testing

- Whether device firmware atomically reassembles packets per HID handle or merges all writers into one byte stream.
- Whether macOS distributes or duplicates input reports across non-exclusive handles.
- Whether Windows HID opening is exclusive in practice.
- Whether USB and Bluetooth have identical RPC behavior and maximum throughput.
- The firmware's guaranteed range and persistence rules for thread slot IDs.
- Exact semantics of `syncKeysLighting`, `syncAmbientLighting`, and the `m` field for every effect.
- Whether a supported local Codex IPC exists for submitting third-party agent status without opening HID.
- Whether Codex Micro has a vendor-supported recovery image distinct from Creator Micro v2.

## Sources

Primary public sources, accessed 2026-07-25:

- [OpenAI × Work Louder](https://openai.com/supply/co-lab/work-louder/)
- [Codex Micro product page](https://worklouder.cc/codex-micro)
- [Codex Micro setup and troubleshooting](https://worklouder.cc/openai-micro-setup)
- [Official Codex Micro demo](https://www.youtube.com/watch?v=3-2OH6ReiPM)
- [Work Louder Input](https://worklouder.cc/input)
- [Input 0.17.2 release](https://github.com/worklouder/input-releases/releases/tag/v0.17.2)
- [Input 0.18.0-rc.5 release](https://github.com/worklouder/input-releases/releases/tag/v0.18.0-rc.5)
- [Creator Micro v2 firmware 0.6.0-rc.6](https://github.com/worklouder/cm-v2-fw-releases/releases/tag/v0.6.0-rc.6)
- [Work Louder public repositories](https://github.com/orgs/worklouder/repositories)
- [Nomad v1-only SDK alpha](https://github.com/worklouder/input-releases-internal/releases/tag/sdk-alpha-0.1)

OpenAI GitHub reports [#33409](https://github.com/openai/codex/issues/33409) and [#33381](https://github.com/openai/codex/issues/33381) were reviewed as user reports only; neither is treated as an official API contract.
