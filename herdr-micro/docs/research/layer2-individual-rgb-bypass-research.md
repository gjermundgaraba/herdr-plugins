# Codex Micro: individual Agent RGB on Layer 2+

**Research date:** 2026-07-25
**Scope:** Codex Micro firmware `v0.4.1`, Work Louder Input, the Codex desktop device package, Creator Micro 2 firmware, Herdr bridges, and public community implementations.

## Answer

**Yes. This has been cracked on real Codex Micro hardware.**

The six live Agent LEDs are not intrinsically tied to physical Layer 1. They are tied to the six physical positions being assigned the firmware's private Agent keycodes:

```text
KV_OAI_AG00 ... KV_OAI_AG05
```

Copying Layer 1's `layout` into Layer 2 makes Layer 2 emit the private Agent events and lets `v.oai.thstatus` address the six LEDs individually. Pejman Pour-Moezzi reports this exact Layer 1 → Layer 2 procedure working on Codex Micro firmware `v0.4.1`, including live Agent lights, then customizes the remaining keys in Input. His post includes a hardware video and a reproducible guide. [Hardware demonstration](https://x.com/pejmanjohn/status/2080289901795525008), [procedure and verified versions](https://gist.github.com/pejmanjohn/d8f1fb99698c1599a533e65514e24469)

This means the best Herdr design is:

1. Keep `KV_OAI_AG00`–`AG05` on Layer 2's six translucent Agent-key positions.
2. Let one Herdr bridge own the vendor HID channel, translate Herdr's six agent states into `v.oai.thstatus`, and handle the resulting `v.oai.hid` Agent-key presses.
3. Customize the other seven keys, encoder, and joystick for Herdr without replacing the six `AG` keycodes.
4. Quit Codex/ChatGPT while using this mode, because its host integration also reacts to those same Agent events.

The exact combination **Herdr + this Mac + cloned Layer 2** was subsequently
physically verified with the local `herdr-micro` plugin over USB and BLE. It
drives six independent statuses, focuses the matching pane, changes effort
for Codex/Claude/Pi, and recovers after reconnect and BLE sleep.

## Why ordinary Layer 2 failed

A useful negative hardware test used an ordinary Layer 2 whose six top positions were standard keys. On firmware `v0.4.1`, `v.oai.thstatus` worked on Layer 1 but was ignored on that Layer 2; `v.oai.rgbcfg` could only paint the entire key area as one zone. [Measured Layer 1/Layer 2 matrix](https://github.com/JeongJaeSoon/paneglow/blob/8f3a76a4f0a489699e06fcadfa0d1d0e52a09c0a/docs/hardware-notes.md#2-레이어별-동작--가장-중요)

That result did **not** test an OAI-keycode clone. The distinction is the keymap, not the layer number:

| Active layer contents | Six `thstatus` LEDs |
|---|---:|
| Ordinary `KC_*` / macro keycodes | No |
| Six positions mapped to `KV_OAI_AG00`–`AG05` | Yes |

At the time of the initial read-only inspection, the Input cache matched that
distinction:

- Layer 1 contains 16 private `KV_OAI_*` assignments: six Agent keys, seven actions, and three encoder events.
- Layer 2 contains zero `KV_OAI_*` assignments.

This was read without mutation from:

```text
~/Library/Application Support/input/input_storage.json
```

## Evidence chain

### 1. A real Codex Micro Layer 2 already works

Pejman's guide says to deep-copy only:

```text
Layer 1.layout → Layer 2.layout
```

Layer 2 retains its own ID, name, color, and layer-level lighting. Input then performs the normal hardware synchronization. His verified setup was:

- Codex desktop `26.715.72359`, build `5718`
- Input `0.17.2`
- Codex Micro firmware `v0.4.1`
- `@worklouder/device-kit-oai` `0.1.10`

The guide's validation includes all six Agent keys, individual white/blue status changes, command keys, dial, push-to-talk, and joystick. It also says Layer 2 can subsequently replace individual positions with ordinary Input actions. [Full guide](https://gist.github.com/pejmanjohn/d8f1fb99698c1599a533e65514e24469)

**Evidence level:** public, reproducible, author-reported hardware test with video.

### 2. A Claude implementation found the same RGB requirement

The `duolahypercho/claude-micro-layer` project independently changed its Layer 2 top positions to `KV_OAI_AG00`–`AG05` and set `lights` to `null`. The commit explains that macro keycodes gave firmware no per-thread target, while a layer backlight could paint over thread colors. Its host helper sends six independent entries through `v.oai.thstatus`. [Keymap at the tested commit](https://github.com/duolahypercho/claude-micro-layer/blob/3597273f98f5b458f9ab70d4a5ab2b7bcc212d10/layers/claude-starter.json#L15-L52), [lighting sender](https://github.com/duolahypercho/claude-micro-layer/blob/3597273f98f5b458f9ab70d4a5ab2b7bcc212d10/macos/ClaudeMicroLights.swift#L103-L136), [full commit](https://github.com/duolahypercho/claude-micro-layer/commit/3597273f98f5b458f9ab70d4a5ab2b7bcc212d10)

The project later reverted those six positions to macros for one specific reason: pressing an OpenAI Agent key also opened/focused Codex. Its revert explicitly documents the trade-off that macros stop the Codex focus side effect but lose individual status lighting. It did **not** revert because Layer 2 RGB failed. [Focus-side-effect revert](https://github.com/duolahypercho/claude-micro-layer/commit/bd49990282a3f0aa0f103aa9ab83915b9e8a7ff8)

**Implication for Herdr:** preserve `AG00`–`AG05`, consume their vendor events in the Herdr bridge, and keep Codex closed. Replacing those positions with Herdr keyboard macros sacrifices the feature we need.

### 3. Input 0.18 identifies a Codex layer from keycodes

The compiled configuration code in official Input `0.18.0-rc.5` contains a predicate that scans the base keys, encoder, and joystick and returns true when **any keycode starts with `KV_OAI_`**. That predicate:

- locks the layer's visual editor;
- prevents ordinary duplication of that layer;
- detects whether the profile already contains a Codex layer; and
- controls the new “Add a new Codex layer” option.

The same bundle creates the Codex preset from a private default layout. It does not define “Codex layer” as `layer index === 0`.

The inspected official artifact was:

```text
Input 0.18.0-rc.5
dist/assets/device_config_data-CBfDtIAJ.js
SHA-256 64aba7163df6f4e119b12bc54d7e3b041a6ce24817db8f9500dadc8b6a788c1a
```

Relevant locations in the extracted bundle:

```text
13917-13932    KV_OAI_* layer predicate
90853-90856    private Codex preset creation
91929-92006    layer import and insertion
92048-92075    import validates device type, but does not reject KV_OAI_*
92172-92186    “Add a new Codex layer”
92267-92296    Codex Micro Layer 1 movement protection / duplication guard
100792-100795  official create option: Creator Micro 2, firmware >= 0.6.0
```

Official release: [Input v0.18.0-rc.5](https://github.com/worklouder/input-releases/releases/tag/v0.18.0-rc.5)

Two version details matter:

- On **Codex Micro**, Input still protects physical Layer 1 and does not expose the official “add Codex layer” button.
- On **Creator Micro 2 firmware 0.6+**, Input exposes that button, but only when no Codex layer is already present.

Input 0.18's import path nevertheless accepts a same-device layer containing the private keycodes. Once imported, the predicate locks that layer's editor. Therefore Input `0.17.2` is the known editable path from Pejman's test; on `0.18`, prepare the mixed Herdr layout before import or use the narrowly scoped database-copy workflow.

### 4. Firmware checks the active keymap, not the layer index

Static analysis of two official Creator Micro 2 firmware releases independently corroborates the behavior:

| Firmware | OAI predicate | Active-layer 13-key scanner | Wrapper |
|---|---:|---:|---:|
| `v0.4.0` | `0x420098dc` | `0x4200990c` | `0x4200a998` |
| `v0.6.0-rc.6` | `0x4200eb1c` | `0x4200eb4c` | `0x4200f108` |

The scanner walks the active layer's 13 physical key positions. It recognizes vendor keycodes with high byte `0x06` and maps:

- low values `0x1001`–`0x1014` to Agent slots `AG00`–`AG19`;
- `0x1101`–`0x1115` to Action events; and
- `0x1201`–`0x1203` to encoder events.

The relevant position mapping is at `0x4200a9b8`–`0x4200aa6e` in `v0.4.0` and `0x4200f1bc`–`0x4200f272` in `v0.6.0-rc.6`. The wrappers resolve the current layer and call the scanner; no layer-index comparison is present in that path.

These addresses are local reverse-engineering findings, not published source-level documentation. The official binaries and hashes are:

- [`v0.4.0`](https://github.com/worklouder/cm-v2-fw-releases/releases/tag/v0.4.0): SHA-256 `93596dec08f36a74e6860cd47ff7951e11ca3c8e4f3a2870665d1f95f768dee8`
- [`v0.6.0-rc.6`](https://github.com/worklouder/cm-v2-fw-releases/releases/tag/v0.6.0-rc.6): SHA-256 `05d8d8ad6ecfb34fa20a62f6b1822e3d9b8b50a569f78af56b5200703c4066dc`

Read-only extracted artifacts and full linear disassemblies are at:

```text
/tmp/cm2-fw-archaeology/app-0.4.bin
/tmp/cm2-fw-archaeology/app-0.6.bin
/tmp/cm2-fw-archaeology/irom-0.4.dis
/tmp/cm2-fw-archaeology/irom-0.6.dis
```

The app-image SHA-256 values are `1b099d9dff4363566ecfd2b207a6e541ef236394fc6ff9205765e7b91d65380d` (`v0.4.0`) and `050a0ac29dd6224db139d5e4b0933e3f35b9e10ecde1026a9bff50f9b04d729c` (`v0.6.0-rc.6`).

The `v0.6.0-rc.6` binary also contains:

```text
v.oai.thstatus
OAI BRIDGE: init, v.oai.thstatus registered on all variants
src/oai/wl_oai_bridge.cpp
```

Its release notes say Codex commands are assignable to a **keymap layer**, lighting appears only while a **Codex-enabled layer** is active, and the release fixes Codex lighting on unrelated layers. This is consistent with the active-keymap scanner. Do not flash Creator Micro 2 firmware onto Codex Micro; the binary inspection is corroborating evidence, not a flashing recommendation.

### 5. The host has no hidden “target layer” lighting field

The installed Codex desktop app contains Work Louder's proprietary `@worklouder/device-kit-oai` `0.1.11`. Its `sendThreadsLighting` method sends only:

```text
v.oai.thstatus
[{ id, c, b, e, s, sk, sa }, ...]
```

There is no layer index or layer token. `v.oai.rgbcfg` similarly carries only the `ambient` and `keys` zones.

Inspected local artifact:

```text
/Applications/Codex.app/Contents/Resources/app.asar
→ node_modules/@worklouder/device-kit-oai/dist/rpc_api_oai/rpc_api_oai.js
```

**Conclusion:** this is not bypassed by discovering a secret layer argument. The firmware decides where the six entries render from the active layer's `AG` assignments.

### 6. Work Louder is productizing the same any-layer concept

On 2026-07-24 Work Louder said that its August 1 Creator Micro 2 release would let users map the Codex layer to **any layer**, instead of limiting it to Layer 1. [Work Louder statement](https://x.com/work_louder/status/2080742820126532046)

This is first-party confirmation of the model, but it is specifically an upcoming **Creator Micro 2** Input integration. It is not a promise that the Codex Micro UI will gain the same button, nor is it a provider-neutral Herdr API.

## Community implementations worth using

### Pejman procedure: strongest Codex Micro Layer 2 proof

Use the guide as the test protocol, but always copy the current machine's live Layer 1 rather than installing someone else's complete database. It includes backups, narrow JSON validation, Input synchronization, read-back verification, and rollback. [Guide](https://gist.github.com/pejmanjohn/d8f1fb99698c1599a533e65514e24469)

### MegaMicro: source implementation of the safe clone

MegaMicro implements the same operation in Swift:

- find exactly one Codex device and one source layer containing `KV_OAI_AG00`;
- copy only the source layer's `layout` into the selected target;
- preserve the target's ID, name, color, and lights;
- make full and target-layout backups;
- atomically replace Input's database; and
- require Input to perform the hardware sync and a later read-back.

[Layer manager source](https://github.com/jessewaites/MegaMicro/blob/e7c13d43e0f6647e26dbb5c02e20e2dbdbae4263/MegaMicro/Services/WorkLouderLayerManager.swift#L83-L152), [documented workflow](https://github.com/jessewaites/MegaMicro/blob/e7c13d43e0f6647e26dbb5c02e20e2dbdbae4263/README.md#hardware-layers)

MegaMicro currently has no open-source license, so treat it as a procedure/reference, not copy-paste source.

### Herdr bridges: reuse the state and focus side

`house-of-herdr` and the `microd` Herdr integration already map Herdr lifecycle state to the six `thstatus` slots and map physical Agent events back to Herdr panes. They do not need a new RGB protocol for Layer 2; they need the cloned `AG00`–`AG05` keymap and single-writer ownership. [house-of-herdr](https://github.com/alasano/house-of-herdr), [microd Herdr PR](https://github.com/spencerbull/microd/pull/1)

## Ranked experiment matrix

No HID writes, app launches, firmware flashes, or device-state changes were performed during this research.

### Preparation and clone

| Rank | Test | Risk | Pass condition | Rollback |
|---:|---|---|---|---|
| 1 | Back up the full Input database; record Input, Codex, and firmware versions. | Read-only / negligible | The backup parses and the selected device/profile/layers are unambiguous. | None needed. |
| 2 | Create a disposable blank Layer 3 in Input and let it synchronize normally. Do not touch the intended Herdr Layer 2 yet. | Reversible layer addition / low | Layer 3 appears on-device and Layer 1/2 remain unchanged. | Delete Layer 3 in Input. |
| 3 | Copy only live Layer 1's `layout` into Layer 3, preserving every Layer 3 field outside `layout`; sync via Input and read back. | Reversible keymap write / moderate | Layer 3 read-back `layout` equals Layer 1; Layer 1 and Layer 2 are unchanged. | Restore only the saved Layer 3 `layout`, then sync/read back, or delete Layer 3. |

### Behavioral test and final Layer 2

| Rank | Test | Risk | Pass condition | Rollback |
|---:|---|---|---|---|
| 4 | With Layer 3 active, Input quit, and Codex as the sole writer, start a fresh Codex task. | Volatile / low | Six Agent keys select tasks and repaint independently as in Pejman's test. | Quit Codex; no persistent lighting state matters. |
| 5 | Quit Codex and Input. Run one Herdr bridge as the sole device writer and send six deliberately different states. | Volatile / low | All six Layer 3 LEDs simultaneously show their intended independent colors/effects; presses focus the matching Herdr panes without opening Codex. | Stop the bridge; restart normal app later. |
| 6 | Once Layer 3 proves the mechanism, apply the same layout to Layer 2 and customize only the seven non-Agent keys, encoder, and joystick. Keep `AG00`–`AG05`; if ordinary backlight overwrites them, use `lights: null` / disable that layer's backlight. | Reversible keymap write / moderate | Herdr controls work and all six status lights remain independent after reconnect and layer changes. | Restore the saved Layer 2 layout. |

### Exact recommended first trial

Prepare the disposable layer:

1. Add a disposable blank **Layer 3** in Input; wait for `layout updated`, then fully quit Input.
2. Back up the full database and Layer 3's original `layout`.
3. Use Pejman's narrow copy to put only live Layer 1's `layout` into Layer 3; do not start with a hand-built partial layer.
4. Reopen Input, force its normal full-layout sync using the temporary-name procedure, read back, and fully quit it.

Then test and promote it:

1. Confirm six-way RGB on Layer 3 with a fresh Codex status change, then quit Codex.
2. Start only the selected Herdr bridge and paint six visibly different test states.
3. Only after that passes, apply the known-good method to Layer 2 and customize its other seven keys.

This isolates failure cleanly:

- If the Codex test fails, the keymap did not synchronize or the local version differs.
- If Codex passes but Herdr fails, the issue is in the Herdr bridge, device ownership, or its status mapping—not the layer.
- If RGB disappears during later customization, an ordinary layer backlight is overwriting the thread LEDs or an Agent keycode was replaced.

## Constraints and likely pitfalls

1. **`AG` keys are both lighting addresses and semantic input events.** Replacing them with normal macros loses individual RGB. Keep them and route their vendor notifications in software.
2. **Codex can react to the same events.** The Claude experiment proved that an `AG` press can open/focus Codex. For Herdr mode, Codex should not be running as another event consumer.
3. **One lighting writer is safest.** Codex, Herdr bridge, Input previews, and other RGB tools can overwrite each other. Run Input only for configuration and read-back.
4. **Layer lighting may mask thread colors.** Pejman's full clone worked while preserving Layer 2 fields, but the independent Claude implementation needed `lights: null`. Start with the proven clone; disable the ordinary backlight only if it masks `thstatus`.
5. **Input behavior is version-sensitive.** `0.17.2` is the physically verified editable workflow. `0.18.0-rc.5` recognizes and locks any imported `KV_OAI_*` layer, so prepare custom keycodes before import or use the narrow database procedure.

## What is now proven

**Proven:**

- Codex Micro Layer 2 can retain six independent Agent status LEDs.
- The necessary assignments are `KV_OAI_AG00`–`AG05`.
- Normal macro keys cannot serve as six `thstatus` targets.
- Other keys on the same layer can be ordinary custom actions.
- Firmware recognition follows the active keymap rather than a hard-coded Layer 1 comparison.
- Creator Micro 2 is receiving a first-party any-layer Codex flow.

**Subsequently verified on this setup:**

- Herdr plus the cloned Layer 2 on this keyboard;
- preserved Layer 2 backlighting with six independent Agent LEDs;
- repaint after reconnect, sleep/wake, and layer changes; and
- background Codex coexistence through frontmost-window ownership.

## Result

The reversible Layer 3 experiment was moved to Layer 2 and physically
verified. No firmware change is needed.
