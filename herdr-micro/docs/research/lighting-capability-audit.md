# Codex Micro lighting capability audit

**Research date:** 2026-08-01

**Device tested:** Codex Micro `0x303A:0x8360`, firmware `v0.4.1`, Layer 2

## Result

The Codex Micro exposes **eight independently controllable logical lighting
outputs** through the known host protocol:

| Surface | Host control on this device |
|---|---|
| Six upper Agent-key LEDs | Six independent outputs, thread IDs `0`–`5` |
| Seven lower Command-key LEDs | One collective `keys` / `backlight` output |
| Frame/perimeter lighting | One collective `ambient` / `underglow` output |
| Three layer indicators | Firmware-owned; no host lighting RPC found |

The dial, joystick, touch sensor itself, and rear button have no additional
addressable lighting surface in the installed applications, current firmware
method table, official product specification, or surveyed community
implementations. The three lights beside the touch control are the layer and
communication indicators described below.

`keys` and `backlight` are two API names for the same key-lighting surface.
Likewise, `ambient` and `underglow` name the same frame/perimeter surface. They
do not describe four separate zones.

## Exact-device test: lower keys are not individually addressable

The daemon was stopped and a sole test client completed a `device.status`
round trip before writing lighting. With firmware `v0.4.1` and Layer 2 active,
the test:

1. turned off both aggregate zones and cleared thread IDs `0`–`19`;
2. set IDs `0`–`5` to green;
3. assigned seven distinct colors to IDs `6`–`12`; and
4. left the pattern visible for physical inspection.

The six upper keys lit green. Every lower key remained dark, including one
inspected with its keycap removed. Therefore `v.oai.thstatus` IDs `6`–`12` do
not address the lower switches on this Codex Micro firmware. This closes the
previously open question about the usable thread-ID range for this exact
device; it is not a vendor guarantee for other models or firmware revisions.

The test then cleared IDs `0`–`19` and both zones. The normal `herdr-micro`
bridge was restarted and returned to `connected` / `routing: ready` on Layer 2.
No keymap or persistent device file was changed.

## Available color and effect controls

Per-Agent entries use `v.oai.thstatus` with compact fields:

```text
{ id, c, b, e, s, sk, sa }
```

The two aggregate zones use `v.oai.rgbcfg`:

```text
{ ambient: { e, b, s, m, c }, keys: { e, b, s, m, c } }
```

`c` is packed 24-bit RGB. Brightness `b` and speed `s` are normalized from
`0` through `1`. `sk` and `sa` request synchronization of one Agent entry onto
the keys or ambient zone. The aggregate-only `m` field is effect-specific, but
its exact behavior remains uncharacterized.

| Code | Effect |
|---:|---|
| `0` | off |
| `1` | solid |
| `2` | snake |
| `3` | rainbow |
| `4` | breath |
| `5` | gradient |
| `6` | shallow breath, approximately half-to-full brightness |

Input's generic layer editor exposes codes `0`–`5`. The OpenAI-specific device
layer adds code `6`. The protocol permits each of the six Agent entries to use
its own color, brightness, effect, and speed simultaneously.

## Layer indicator LEDs

Work Louder documents three LEDs beside the touch control as the active-layer
indicator for up to six layers. Communication mode also uses device lighting
to show USB/Bluetooth selection and connection state. [Codex Micro setup](https://worklouder.cc/openai-micro-setup)

The official Creator Micro 2 firmware `v0.6.1` binary contains an internal
three-argument layer-indicator brightness function. Its complete visible RPC
method table contains `lights.preview`, `v.oai.rgbcfg`, and
`v.oai.thstatus`, but no method for setting those three indicators directly.
The defensible boundary is therefore:

- firmware can drive the indicators independently by brightness;
- applications can influence them indirectly by changing layer or connection
  mode; and
- no direct host API for arbitrary indicator color, brightness, or animation
  was found.

The internal function is evidence from Creator Micro 2 firmware, not a public
Codex Micro API contract.

## Fresh artifact and source audit

| Artifact | Fresh result |
|---|---|
| Installed Input `0.17.3` | Only generic `backlight` and `underglow`; no `v.oai.*` implementation |
| Installed ChatGPT/Codex `26.727.51351` | Bundles `@worklouder/device-kit-oai` `0.1.11` |
| OAI device package | Six thread slots plus `keys` and `ambient`; no third lighting zone |
| Creator Micro 2 firmware `v0.6.1` | Same visible lighting RPC names; no layer-indicator RPC |
| Current community implementations | No additional Codex Micro lighting surface or lower-key addressing method found |

The installed Input ASAR inspected for this pass has SHA-256
`9741d3ede5651b82db8a56453271b80bfa707b934f1f6ae490481780a7232681`.
The official Creator Micro 2 `v0.6.1` firmware image inspected has SHA-256
`c0d288d5e709cbd7c3f5e4e11e57e26dd1e07e6d83c513e84a9f19d08039794b`.

## Creator Micro 2 is a different result

A current community hardware test reports that Creator Micro 2 firmware
`v0.6.0-rc.10` can walk one lit thread through IDs `0`–`12`, lighting all 13
keys individually. That result is credible for that model and firmware, but it
does not override the negative physical test on this Codex Micro `v0.4.1`.
[Creator Micro 2 hardware results](https://github.com/schacon/micro-manager)

Work Louder released Creator Micro 2 firmware `v0.6.1` on 2026-08-01 with
stable Codex integration and lighting improvements. It is a Creator Micro 2
image and must not be treated as a Codex Micro recovery or upgrade image.
[Creator Micro 2 firmware v0.6.1](https://github.com/worklouder/cm-v2-fw-releases/releases/tag/v0.6.1)

## Practical boundary for `herdr-micro`

The plugin can safely model:

- six independent Agent-status lights;
- one optional aggregate Command-key/backlight status; and
- one optional aggregate ambient/perimeter status.

Individual lower-key RGB and direct layer-indicator control should remain
documented as unavailable on the tested Codex Micro rather than exposed as
speculative configuration.
