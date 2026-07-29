# Codex Micro: hardware, firmware, and shipped Codex behavior

Research date: **2026-07-25**  
Scope: the Work Louder/OpenAI Codex Micro itself, its shipped Codex-oriented layer, the Codex desktop integration, and Work Louder Input. This note intentionally does not design the Herdr layer.

## Bottom line

1. **The received device does not ship with a Layer 2 mapping.** Its cached factory `keymap.json` contains one profile and one layer, **Layer 1**, made entirely of Codex vendor keycodes. Layer 2 is a user-created Input layer.
2. **The native integration is Codex desktop integration, not Codex CLI integration.** The current Codex Electron app owns device discovery, vendor HID events, agent-slot lighting, and direct app commands. The public pages describe this as direct integration with “Codex” / “ChatGPT Codex,” while the installed implementation invokes desktop-app command IDs. ([Work Louder](https://worklouder.cc/codex-micro), [OpenAI](https://openai.com/supply/co-lab/work-louder/))
3. **The generic layer path is capable but not agent-aware.** Work Louder Input can create up to six layers and place ordinary HID keys, recorded macros/multi-actions, dial actions, and joystick actions on them. Smart Actions exist in the application/firmware model, but are feature-gated off for this device in the inspected stable Input 0.17.2 build; do not make a Layer 2 design depend on them without testing a newer release candidate. These generic controls do not expose Codex-style agent state or a normalized thinking-effort API. ([Input](https://worklouder.cc/input), [Creator Micro 2](https://worklouder.cc/creator-micro-2))
4. **Live Agent Key RGB is a host integration feature.** Firmware accepts vendor commands/events and only shows Codex activity lighting while a Codex-enabled layer is active; switching away restores ordinary lighting. ([firmware v0.6.0-rc.6](https://github.com/worklouder/cm-v2-fw-releases/releases/tag/v0.6.0-rc.6))
5. **The public development surface is incomplete.** Work Louder publishes Input installers and firmware binaries, not the Codex Micro firmware source or the Work Louder JavaScript device SDK source. A readable local JSON keymap is available, but there is no documented third-party Codex Micro RGB/control SDK for this model. ([Input releases](https://github.com/worklouder/input-releases), [Creator Micro 2 firmware releases](https://github.com/worklouder/cm-v2-fw-releases), [Work Louder repositories](https://github.com/orgs/worklouder/repositories))

## Evidence labels

- **Fact — public:** stated by Work Louder, OpenAI, or an official Work Louder release.
- **Fact — direct artifact:** observed in the installed applications or the received device's locally cached configuration on 2026-07-25.
- **Inference:** a conclusion from those facts, called out explicitly.

## Hardware inputs and outputs

| Surface | Verified behavior/capacity | Evidence |
|---|---|---|
| Mechanical keys | 13 mechanical switches in an exact **2 + 4 + 4 + 3** row layout. The top two rows are the six Agent Keys (`AG00`–`AG05`); the lower two rows are seven action switch positions (`ACT06`–`ACT12`). | **Fact — public:** [Work Louder specs](https://worklouder.cc/codex-micro), [OpenAI specs](https://openai.com/supply/co-lab/work-louder/). **Fact — direct artifact:** cached `keymap.json`. |
| Touch sensor | One touch sensor. A tap cycles through at most six layers. Three indicator LEDs show the active layer. A long hold enters connection-selection mode on the current Micro platform. | **Fact — public:** [Micro setup](https://worklouder.cc/micro-setup). |
| Rotary encoder | One rotary encoder: counterclockwise, clockwise, and press are separate logical inputs. Codex can use it for composer navigation, reasoning effort, or conversation scrolling. | **Fact — public:** [Work Louder](https://worklouder.cc/codex-micro). **Fact — direct artifact:** current Codex settings bundle and raw keymap. |
| Joystick | One planar joystick. Firmware reports continuous angle and distance; the Codex UI resolves four directions. Input can instead expose a radial menu. | **Fact — public:** [Work Louder](https://worklouder.cc/codex-micro), [Creator Micro 2](https://worklouder.cc/creator-micro-2). **Fact — direct artifact:** bundled `RPCApiOAI` and Codex joystick handler. |
| Key lighting | Six Agent Keys can receive per-thread status colors/effects. Codex can also control a general key-lighting zone. | **Fact — public:** [OpenAI](https://openai.com/supply/co-lab/work-louder/). **Fact — direct artifact:** vendor `v.oai.thstatus` and `v.oai.rgbcfg` RPC calls in the bundled SDK. |
| Ambient lighting | RGB underglow/ambient ring; Codex uses solid, breathing, and “snake” effects. Input can use underglow as an app/layer color cue. | **Fact — public:** [Work Louder](https://worklouder.cc/codex-micro), [Creator Micro 2](https://worklouder.cc/creator-micro-2). |
| Connectivity | Bluetooth and USB-C; the shared firmware platform stores three Bluetooth hosts. Marketed for Mac and Windows. | **Fact — public:** [Work Louder](https://worklouder.cc/codex-micro), [OpenAI](https://openai.com/supply/co-lab/work-louder/). Three hosts are **fact — direct firmware artifact**. |
| Physical keyset | Work Louder says “32 custom icons” and “11 solid color caps”; the included-cap line says `30×1U, 1×2U`. | **Fact — public, internally inconsistent count:** [Work Louder](https://worklouder.cc/codex-micro), [OpenAI](https://openai.com/supply/co-lab/work-louder/). |

The 2U command position is implemented specially. The raw device map contains both `ACT10` and `ACT11`; the Codex settings model merges them into one `ACT10_ACT11` double-width assignment, and the event handler treats `ACT10` as the actionable event while suppressing `ACT11`. **Fact — direct artifact.**

## Firmware and host stack

### 1. Device firmware

The Codex Micro is the Codex-specific device variant of the Creator Micro 2 firmware platform. The current public Codex-capable firmware release is the **prerelease** `v0.6.0-rc.6`, published 2026-07-23. It adds:

- Codex commands assignable to a keymap layer.
- Supported Codex actions on keys, encoder, and joystick.
- Live Codex activity/status lighting.
- Codex lighting only while a Codex-enabled layer is active; switching layers restores normal lighting.
- USB plus three Bluetooth host slots on the shared Micro platform.

**Fact — public:** [Creator Micro 2 firmware v0.6.0-rc.6](https://github.com/worklouder/cm-v2-fw-releases/releases/tag/v0.6.0-rc.6). The latest non-prerelease is `v0.4.0`, which predates the published Codex integration release. ([v0.4.0](https://github.com/worklouder/cm-v2-fw-releases/releases/tag/v0.4.0))

The firmware repository contains a README and downloadable merged `.bin` releases, but no firmware source or protocol documentation. **Fact — public:** [repository](https://github.com/worklouder/cm-v2-fw-releases).

Direct inspection of `firmware_v0.6.0-rc.6_merged.bin` identifies an ESP32-S3 / ESP-IDF 5.3.2 build, 16 MiB flash, a 2 MiB LittleFS partition, and the on-device keymap path `/fs/keymap.json`. Binary SHA-256: `05d8d8ad6ecfb34fa20a62f6b1822e3d9b8b50a569f78af56b5200703c4066dc`. **Fact — direct firmware artifact.** The binary contains both Creator Micro 2 and Codex Micro identifiers, but Work Louder does not publish a hardware-source statement equating them. Do not manually flash the Creator Micro 2 release onto a Codex Micro without first-party instructions.

### 2. Work Louder Input

Input is the separate Electron configurator. The current installed stable version is **0.17.2**; the matching release explicitly says it fixed device communication interfering with the Codex application. **Fact — public/direct artifact:** [Input v0.17.2](https://github.com/worklouder/input-releases/releases/tag/v0.17.2).

Input 0.17.2 treats layer index 0 as the reserved Codex layer: it cannot be edited, deleted, recolored, or assigned a different OS mode. The UI directs the user to the Codex Micro app instead. The official demonstration describes the same model as “Layer 1 is reserved for Codex,” with Input unlocking five additional layers. **Fact — direct artifact/public:** [official demonstration at 2:19](https://www.youtube.com/watch?v=3-2OH6ReiPM&t=139s).

As of the research date, **0.18.0-rc.5 is a prerelease, not the stable Input channel**. Its published changelog only records the same Codex-communication fix; the preceding 0.18 candidates mention a lighting-modal fix and the addition of another Work Louder device, not new Codex Micro Layer 2 features. Therefore this note does not promote any 0.18-only behavior to a supported capability without direct testing. **Fact — public:** [Input releases](https://github.com/worklouder/input-releases/releases).

Input reads and writes device files such as `keymap.json` and, when used, `smart_actions.json`. It also keeps a host-side LokiJS database at:

```text
~/Library/Application Support/input/input_storage.json
```

The received device's cached raw map is:

```text
~/Library/Application Support/input/devices/33632/keymap.json
```

**Fact — direct artifact.** This is useful for backup and inspection, but it is not a documented hand-editing API. Input also writes mappings to the device; editing the cache behind Input's back may be overwritten or fail validation.

### 3. Codex desktop

Installed Codex desktop version inspected: **26.721.41059**. Its Electron archive bundles:

- `@worklouder/device-kit-oai` `0.1.11`
- `@worklouder/wl-device-kit` `0.1.23`
- HID/serial device-discovery dependencies
- a dedicated `CodexMicroService`
- Codex Micro settings, command routing, lighting, joystick, and onboarding bundles

**Fact — direct artifact:** `/Applications/Codex.app/Contents/Resources/app.asar`, SHA-256 `da39a51b06fb4c728d418b8f0f05fc8fd8c6b1f74c4fb4d47c20c7914a798f45`.

The current host protocol uses a vendor-defined HID interface (usage page `0xFF00`) and JSON-RPC-style messages:

| Direction | Channel/method | Purpose |
|---|---|---|
| Device → host | `v.oai.hid` | Key/encoder event containing key, action, and optional agent. |
| Device → host | `v.oai.rad` | Joystick angle and distance. |
| Host → device | `v.oai.thstatus` | Six per-thread lighting states. |
| Host → device | `v.oai.rgbcfg` | General key and ambient lighting. |

**Fact — direct artifact.** The SDK source header labels it proprietary/confidential, the packages are not publicly listed in npm, and no corresponding source repository appears in Work Louder's public repositories. **Fact — direct artifact/public repository survey:** [Work Louder repositories](https://github.com/orgs/worklouder/repositories).

The observed USB identity is VID:PID `0x303A:0x8360`; Input names it `codex_micro`, separately from `creator_micro_v2`. **Fact — direct Input 0.18.0-rc.5 artifact.**

## What the received device actually ships with

### Raw factory map

The received device's cached `keymap.json` contains:

- profile `0`, named `Default`;
- exactly one layer, `id: 0`, named `Layer 1`;
- no macros, multi-actions, action groups, linked apps, or Smart Actions;
- vendor joystick mode with no ordinary radial sectors.

**Fact — direct artifact:** local file SHA-256 `c7930d448b78afb84e89f69a26f9a32c9ee088f3b6737b25e0ebfa2fc7648206`.

```json
{
  "version": 1,
  "activeProfileId": 0,
  "profiles": [{
    "name": "Default",
    "id": 0,
    "layers": [{
      "id": 0,
      "name": "Layer 1",
      "color": 16711680,
      "layout": {
        "encoders": [[
          "KV_OAI_ENC_CC",
          "KV_OAI_ENC_CW",
          "KV_OAI_ENC_CLK"
        ]],
        "keymap": [
          ["KV_OAI_AG00", "KV_OAI_AG01"],
          ["KV_OAI_AG02", "KV_OAI_AG03", "KV_OAI_AG04", "KV_OAI_AG05"],
          ["KV_OAI_ACT06", "KV_OAI_ACT07", "KV_OAI_ACT08", "KV_OAI_ACT09"],
          ["KV_OAI_ACT10", "KV_OAI_ACT11", "KV_OAI_ACT12"]
        ],
        "joystick": {"type": "VENDOR", "sectors": []}
      },
      "os": 0
    }]
  }]
}
```

The JSON above is normalized for readability; the values are unchanged.

### Layer 2 conclusion

**Fact — direct artifact:** there is no Layer 2 entry in the shipped/current cached map.

**Fact — public/direct artifact:** Layer 1 is reserved/read-only in Input. A newly created Layer 2 starts with all 13 keys, all three encoder events, and its radial-menu sectors unassigned.

**Inference:** “set up Layer 2” means adding a new ordinary Input layer beside the protected/useful Codex vendor Layer 1. It does not mean replacing a second factory preset. ([official demonstration at 2:19](https://www.youtube.com/watch?v=3-2OH6ReiPM&t=139s), [firmware release](https://github.com/worklouder/cm-v2-fw-releases/releases/tag/v0.6.0-rc.6))

Input 0.17.2 creates this logical Layer 2 template:

```json
{
  "encoders": [["KC_NONE", "KC_NONE", "KC_NONE"]],
  "keymap": [
    ["KC_NONE", "KC_NONE"],
    ["KC_NONE", "KC_NONE", "KC_NONE", "KC_NONE"],
    ["KC_NONE", "KC_NONE", "KC_NONE", "KC_NONE"],
    ["KC_NONE", "KC_NONE", "KC_NONE"]
  ],
  "joystick": {
    "type": "RADIAL",
    "sectors": [
      {"k": "KI_X", "a1": 0.1875, "a2": 0.3125},
      {"k": "KC_NONE", "a1": 0.3125, "a2": 0.1875}
    ]
  }
}
```

`KI_X` and the second sector are the empty radial-menu scaffold, not two useful default joystick assignments. **Fact — direct Input 0.17.2 artifact.**

## Current Codex desktop defaults

The raw firmware layer only emits vendor events. Their meaning is assigned by Codex desktop settings.

### Six Agent Keys

Default source: **Most recent chats** — the first six recently updated chats, pinned or unpinned.

Other selectable sources:

| Mode | Meaning |
|---|---|
| Pinned chats | First six chats in Pinned. |
| Priority chats | Waiting, unread, and active chats first. |
| Custom assignments | Each key can target a chat, Codex command, physical keycap action, or enabled skill. |

**Fact — direct artifact.** Pressing an assigned chat Agent Key opens/focuses that thread.

The current internal statuses and exact RGB colors are:

| Status | RGB | Behavior |
|---|---:|---|
| Working | `#304FFE` | Thread key can breathe; selected working thread can drive a snake ambient effect. |
| Unread | `#00FF4C` | Solid thread status. |
| Idle | `#FFFFFF` | Solid thread status. |
| Awaiting approval / response | `#FF6D00` | Solid attention status. |
| Error | `#FF0033` | Solid error status. |
| Off | `#000000` | Unassigned/off. |

**Fact — direct artifact.** Public marketing compresses these into idle, thinking/running, complete/done, needs input/waiting, and error. ([Work Louder](https://worklouder.cc/codex-micro), [OpenAI](https://openai.com/supply/co-lab/work-louder/))

### Seven action-switch positions

| Device event | Current default Codex assignment |
|---|---|
| `ACT06` | Toggle Fast mode (`composer.toggleFastMode`). |
| `ACT07` | Approve the current approval request (`approval.approve`). |
| `ACT08` | Decline the current approval request (`approval.decline`). |
| `ACT09` | Fork/split the current chat (`forkThread`). |
| `ACT10` + `ACT11` 2U position | Push to talk. Hold to dictate; double-tap to latch recording; press again to stop. |
| `ACT12` | Submit the composer (`composer.submit`). |

**Fact — direct artifact.** The public pages advertise accept, reject, push-to-talk, new chat, custom actions, and “more”; the exact current factory default is the table above, so **New chat is advertised and available but is not in the current default six command assignments**. ([Work Louder](https://worklouder.cc/codex-micro), [OpenAI](https://openai.com/supply/co-lab/work-louder/))

The microphone position can alternatively run Voice Chat: tap to start/toggle microphone; hold for 500 ms to end. **Fact — direct artifact.**

### Joystick

Current default:

| Direction | Default Codex action |
|---|---|
| Up | Toggle Plan mode. |
| Right | Navigate forward. |
| Down | Toggle sidebar. |
| Left | Navigate back. |

**Fact — direct artifact.**

Each direction can be reassigned inside Codex to an available desktop command or an enabled Codex skill. The marketing examples are PR review, debugging, and refactoring, but those examples are not the current default mapping. ([OpenAI](https://openai.com/supply/co-lab/work-louder/), [Work Louder](https://worklouder.cc/codex-micro))

### Encoder

The current Codex desktop default is **Composer navigation**, despite marketing foregrounding reasoning control:

| Mode | Clockwise | Counterclockwise | Press |
|---|---|---|---|
| Composer navigation (default) | Previous control/option | Next control/option | Open/select highlighted control |
| Reasoning only | Decrease reasoning effort | Increase reasoning effort | Open reasoning slider or advanced options |
| Conversation scrolling | Scroll down | Scroll up | Jump to latest message |

A 500 ms encoder hold opens Codex Micro settings. **Fact — direct artifact.** The reasoning mode itself is accurately advertised by both official product pages. ([Work Louder](https://worklouder.cc/codex-micro), [OpenAI](https://openai.com/supply/co-lab/work-louder/))

### Lighting settings and behavior

Codex settings expose:

- brightness from 0–100%, default 100%;
- auto-off: off, 30 seconds, 1, 3, 10, or 30 minutes, or 1 hour; default 3 minutes;
- selected-thread lighting and voice/dictation ambient effects;
- battery percentage/charging state when reported by firmware.

**Fact — direct artifact.**

## Full current Codex keycap/action catalog

The current Codex desktop bundle defines 38 logical keycap entries. This catalog is broader than the six default command assignments and is not a claim that all 38 physical caps are included.

| ID | Icon / label | Action |
|---|---|---|
| `FAST` | Lightning outline | Toggle Fast mode. |
| `APPR` | Check circle | Approve. |
| `REJ` | X circle | Decline. |
| `SPLIT` | Worktree | Fork/split chat. |
| `MIC` | Microphone, 2U | Push to talk / configured microphone mode. |
| `CODEX` | Codex | Submit composer. |
| `BUG` | Bug | Open feedback. |
| `OAI` | OpenAI | Open `https://developers.openai.com`. |
| `TERM` | Terminal | Toggle terminal. |
| `DWN` | Download | Copy conversation as Markdown. |
| `DEL` | Trash | Archive chat. |
| `NEW` | Compose | New task/chat. |
| `NAV` | Pointer outline | Open browser tab. |
| `MAGIC` | Star | Pin/unpin chat. |
| `DIFF` | Diff | Toggle review tab. |
| `PLAY` | Play outline | Run environment action 1. |
| `GIT` | Diff | Git commit. |
| `BRCH` | Draft PR | Create draft pull request. |
| `BRANCH` | Branch | Create branch. |
| `MRG` | Merged PR | Merge pull request. |
| `PR` | Pull request | Create pull request. |
| `PAINT` | Paint | Add photos. |
| `LAB` | Flask | Open settings. |
| `PARTY` | Confetti | Open side chat. |
| `TIME` | Clock | Manage tasks. |
| `MIND+` | Medium brain | Increase reasoning effort. |
| `MIND-` | Outline brain | Decrease reasoning effort. |
| `EMPT1`–`EMPT4` | Blank | Four user-assignable single-width shortcuts. |
| `SETUP` | Settings | Open settings. |
| `FOLD` | Folder plus | Open folder. |
| `UPL` | Cloud upload | Add files. |
| `APPS` | All products | Open Skills. |
| `YOLO` | Blank | Insert `:yolo:` into the composer. |
| `YEET` | Blank | Insert `:yeet:` into the composer. |
| `EMPT5` | Blank, 2U | User-assignable double-width shortcut. |

**Fact — direct artifact:** `codex-micro-layout-*.js` in Codex desktop 26.721.41059. Public support for remapping Codex commands is confirmed by [Work Louder](https://worklouder.cc/codex-micro).

## What Work Louder Input can put on Layer 2

| Capability | Verified scope | Source |
|---|---|---|
| Layers | Up to six layers, cycled by touch; software can auto-select a linked layer when an app has focus. | [Micro setup](https://worklouder.cc/micro-setup) |
| Keys and dial | Map ordinary shortcuts and custom actions to any key or dial. | [Input](https://worklouder.cc/input) |
| Joystick | Map joystick movement; Work Louder markets a seven-slot on-screen radial menu per layer. | [Codex Micro](https://worklouder.cc/codex-micro), [Creator Micro 2](https://worklouder.cc/creator-micro-2) |
| Multi-action | Different results for tap, hold, double-tap, and tap-hold. Work Louder's public example is tap=copy, double-tap=paste, hold=cut. | [Input](https://worklouder.cc/input); tap-hold is also **fact — direct artifact** in Input 0.17.2. |
| Smart Actions | The data model/UI bundle defines four types: type/paste text, run a shell command, open a website, and launch an app. **However, Input 0.17.2's Codex Micro feature gate returns false, so these are not a dependable current Layer 2 capability.** A firmware beta and Input release notes show work on Smart Actions, but not stable enablement for this device. | [firmware v0.5.0-rc.1](https://github.com/worklouder/cm-v2-fw-releases/releases/tag/v0.5.0-rc.1), [Input releases](https://github.com/worklouder/input-releases/releases); exact four types and the disabled gate are **fact — direct artifact** in Input 0.17.2. |
| Ordinary macros/actions | Create reusable Actions and group them; product copy calls these custom shortcuts/macros. | [Creator Micro 2](https://worklouder.cc/creator-micro-2) |
| AppSense | Link a software application to a layer; Input switches to it after that app receives focus. | [Micro setup](https://worklouder.cc/micro-setup) |
| App color cue | Underglow can change based on the app/layer in focus. | [Creator Micro 2](https://worklouder.cc/creator-micro-2) |
| Cheat Sheet | A key can open a host-side layout/shortcut reference. | [firmware v0.5.0-rc.1](https://github.com/worklouder/cm-v2-fw-releases/releases/tag/v0.5.0-rc.1) |

The action picker also exposes ordinary keyboard/modifier/navigation keys, F1–F24, numpad, media, brightness, sleep/power, layer selection, and profile selection. Recorded macros support press, release, press-and-release, and per-step delay. Input can import/export device-matched layer/profile JSON. **Fact — direct Input 0.17.2/0.18.0-rc.5 artifacts.**

### Platform support

- The Codex Micro product is officially specified for **Mac and Windows**. ([Work Louder](https://worklouder.cc/codex-micro))
- Official Input downloads are offered for Apple silicon Mac, Intel Mac, and Windows. ([Input](https://worklouder.cc/input))
- Work Louder links an **unofficial, community-developed Linux port** and explicitly says it is not officially supported or maintained. ([input-linux](https://github.com/worklouder/input-linux))
- A page for the same Creator Micro 2 platform states it is **not VIA/QMK compatible** and uses Input. This is strong platform-family evidence, but not an explicit Codex Micro FAQ entry. **Inference:** plan on Input/Work Louder firmware, not QMK/VIA. ([Framer Creator Micro](https://worklouder.cc/framer-creator-micro))

## Constraints that matter for a Herdr / multi-agent layer

1. **Keep Layer 1 intact.** It is the only shipped Codex vendor layer and the only layer whose controls and lighting are understood natively by Codex desktop. Layer 2 can carry generic Herdr/terminal controls without sacrificing Codex.
2. **Input mappings are output-only unless host software participates.** Standard HID shortcuts/macros can invoke Herdr or a host hotkey adapter. Smart Actions could provide direct command launching if a build enables them for Codex Micro, but stable 0.17.2 does not. None of these give Layer 2 the Codex Agent Key status feedback.
3. **The Codex reasoning knob is app-local.** Its current implementation invokes Codex desktop commands and manipulates Codex desktop UI state. It is not a generic HID “reasoning effort” standard and does not address Codex CLI, Claude Code, Pi, or arbitrary terminals.
4. **Live RGB requires a host bridge or vendor support.** The bundled private SDK proves the device can send vendor events and accept thread/general lighting RPCs, but Work Louder has not published this as a supported third-party API for Codex Micro.
5. **Avoid device contention.** Input `0.17.2` was released specifically to fix device communication interfering with Codex. Any custom HID/RPC bridge would need ownership/coordination so it does not fight Codex or Input. ([Input v0.17.2](https://github.com/worklouder/input-releases/releases/tag/v0.17.2))

Work Louder also warns that Karabiner-Elements and Logitech Options+ can interfere when they have Input Monitoring access. This is not a reason to rule out a remapper, but it makes direct Layer 2 HID bindings the lowest-risk first implementation and requires an explicit communication test if a global remapper is added. **Fact — public support guidance:** [Codex Micro setup](https://worklouder.cc/openai-micro-setup).

## Raw mappings and reproducibility

### Available now

- Human-readable cached device map:  
  `~/Library/Application Support/input/devices/33632/keymap.json`
- Host-side Input database containing profiles, layers, actions, linked apps, and Smart Actions:  
  `~/Library/Application Support/input/input_storage.json`
- Official Input installers:  
  [github.com/worklouder/input-releases](https://github.com/worklouder/input-releases)
- Official merged firmware binaries:  
  [github.com/worklouder/cm-v2-fw-releases](https://github.com/worklouder/cm-v2-fw-releases)

### Not available publicly

- Codex Micro / Creator Micro 2 firmware source.
- A supported public Codex Micro device protocol specification.
- Public source or npm packages for `@worklouder/device-kit-oai` and `@worklouder/wl-device-kit`.
- A first-party import/export format documented for hand-authored `keymap.json` files.
- A public list mapping the claimed 32 included physical icon caps one-for-one to the 38 current Codex desktop catalog entries.

## Facts versus inferences

### Confirmed facts

- The hardware has 13 switches, touch, encoder, joystick, Bluetooth/USB-C, and RGB. ([Work Louder](https://worklouder.cc/codex-micro))
- The received device's current cached map has only the Codex vendor Layer 1.
- Codex desktop has six agent slots, configurable action keys, four joystick directions, three encoder modes, voice control, and live lighting.
- Input provides the reserved Codex Layer 1 plus five user layers, with ordinary shortcuts, macros/multi-actions, app-linked switching, dial, and joystick mapping. Its 0.17.2 bundle contains Smart Action types but disables that feature for Codex Micro. ([Input](https://worklouder.cc/input), [official demonstration](https://www.youtube.com/watch?v=3-2OH6ReiPM&t=139s))
- Firmware explicitly isolates Codex lighting to Codex-enabled layers. ([firmware release](https://github.com/worklouder/cm-v2-fw-releases/releases/tag/v0.6.0-rc.6))

### Inferences

- Layer 2 is the safest place for Herdr because it is absent by default and firmware restores ordinary behavior when leaving the Codex layer.
- Generic Layer 2 controls can reach other agents through shortcuts/commands, but reproducing Codex's state-aware lighting needs a Herdr/agent host bridge or new Work Louder API support.
- QMK/VIA should not be treated as an integration option for this platform.
- Because the public QMK/VIA files describe an older 16-key, two-encoder Work Louder Micro, copying that device's “Layer 2” or firmware would be unsafe and irrelevant here.
- Directly reusing the reverse-observed vendor RPCs would be technically possible in principle, but it is unsupported, potentially fragile across firmware/app updates, and subject to the proprietary SDK's terms.

## Open questions

1. What firmware version is installed on this specific Codex Micro? The local cache did not retain it, and the public Codex-capable firmware is currently marked prerelease.
2. Does Input 0.18.0-rc.5 enable any Codex Micro features that its short public changelog does not mention, especially Smart Actions or a fully supported radial menu?
3. Can Work Louder provide a supported SDK/license for third-party event and RGB control on Codex Micro?
4. Will Work Louder document and support Input's observed device-matched layer/profile JSON export as a cross-user preset-sharing workflow?
5. Which exact 32 physical icons are in the Codex keyset, and how do they map to the current 38-entry Codex software catalog?

## Primary-source index

- [Work Louder — Codex Micro](https://worklouder.cc/codex-micro)
- [OpenAI Supply Co. — Codex Micro](https://openai.com/supply/co-lab/work-louder/)
- [Work Louder — Input](https://worklouder.cc/input)
- [Work Louder — Creator Micro 2 setup](https://worklouder.cc/micro-setup)
- [Work Louder — Creator Micro 2](https://worklouder.cc/creator-micro-2)
- [Work Louder — Input releases](https://github.com/worklouder/input-releases)
- [Work Louder — Input v0.17.2](https://github.com/worklouder/input-releases/releases/tag/v0.17.2)
- [Work Louder — Input releases, including 0.18 prereleases](https://github.com/worklouder/input-releases/releases)
- [Work Louder/OpenAI — official Codex Micro demonstration](https://www.youtube.com/watch?v=3-2OH6ReiPM)
- [Work Louder — Creator Micro 2 firmware releases](https://github.com/worklouder/cm-v2-fw-releases)
- [Work Louder — firmware v0.6.0-rc.6](https://github.com/worklouder/cm-v2-fw-releases/releases/tag/v0.6.0-rc.6)
- [Work Louder — firmware v0.5.0-rc.1](https://github.com/worklouder/cm-v2-fw-releases/releases/tag/v0.5.0-rc.1)
- [Work Louder — unofficial Input Linux port](https://github.com/worklouder/input-linux)

All web sources were accessed on 2026-07-25.
