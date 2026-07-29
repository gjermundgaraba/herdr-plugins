# Creator Micro 2 “Agent Mode”: evidence and implications

**Research date:** 2026-07-25  
**Scope:** Creator Micro 2 Agent Mode, its Codex integration, RGB/status ownership, and whether it offers a supported route for Herdr, Claude Code, Pi, or another status provider.

## Answer at a glance

| Question | Finding |
|---|---|
| What is Agent Mode? | A **Codex-enabled keymap layer** on Creator Micro 2. It is not a provider-neutral agent mode. |
| How is it enabled? | The only public implementation is prerelease firmware **v0.6.0-rc.6** plus the corresponding Codex host update. Work Louder has not documented how the Codex-enabled layer is provisioned. |
| Where is it configured? | Thread slots, Codex commands, joystick skills, dial behavior, and lighting preferences are configured in the Codex desktop app. Input manages ordinary Creator layers; its role in provisioning the Codex-enabled layer is undocumented. |
| Which agents supply status? | Codex/ChatGPT desktop threads only. No official source or current artifact exposes a Claude Code, Pi, Herdr, or generic status-provider selector. |
| Can Herdr use the controls? | Yes, through ordinary Input shortcuts/macros and a host-side bridge. Smart Actions are a possible command-trigger route, but are not enabled in the inspected stable Input 0.17.2 build. This is Creator Mode functionality, not Agent Mode’s native status path. |
| Can Herdr set Agent Key RGB through a supported API? | No public supported route was found. The current RGB/status transport is private and Codex-host-owned. |
| Does Creator Micro 2 provide something Codex Micro does not? | No supported third-party integration route. Creator Micro 2 is a general Creator product gaining Codex through an optional update; Codex Micro ships as a dedicated edition with Layer 1 reserved for Codex. Public evidence does not establish user-selectable placement of the Creator Micro 2 Codex layer. |

## 1. What Work Louder means by “Agent Mode”

Work Louder’s product page visually contrasts **Agent mode** with **Creator mode**, then immediately describes the Agent Keys as reflecting the state of “your Codex agents.” The same page calls the device “directly integrated into Codex,” says commands and layouts are changed in Codex, and lists only **ChatGPT Codex** and **Work Louder Input** under software. Separately, Creator Mode is described as Input-driven app shortcuts, macros, AppSense, a radial menu, app-colored underglow, and six layers. [Creator Micro 2 product page](https://worklouder.cc/creator-micro-2)

The firmware release makes the boundary even clearer:

- Codex commands are assigned to a keymap layer.
- Keys, encoder, and joystick emit supported Codex actions.
- Lighting shows live Codex activity and status.
- Codex lighting is active only while a **Codex-enabled layer** is active; changing layers restores ordinary lighting.

These statements define Agent Mode operationally as the active Codex-enabled layer. They do not define a generic agent protocol, plugin point, or provider registry. [Creator Micro v2 firmware v0.6.0-rc.6](https://github.com/worklouder/cm-v2-fw-releases/releases/tag/v0.6.0-rc.6)

### What it is not

Agent Mode is not documented as:

- an integration with Codex CLI processes;
- a Herdr, Claude Code, or Pi integration;
- an API for publishing arbitrary agent states;
- an RGB SDK; or
- a general-purpose layer whose `idle/thinking/complete/needs input/error` values can be supplied by another application.

The “agents” exposed in the current Codex configuration UI are Codex/ChatGPT **threads**. The six Agent Keys can follow pinned, recent, priority, or explicitly assigned chats. That is useful multi-thread control, but it is not discovery of arbitrary terminal agents.

## 2. Current enablement and configuration path

### Public release state

The latest stable Creator Micro 2 firmware is **v0.4.0**, released 2026-06-21. Its notes cover communications, charging, battery, sleep, and reliability; they do not include Codex. [Creator Micro v2 firmware v0.4.0](https://github.com/worklouder/cm-v2-fw-releases/releases/tag/v0.4.0)

Codex support first appears in the current public **v0.6.0-rc.6 prerelease**, released 2026-07-23. Its notes say the firmware adds support for the upcoming Codex integration and requires the corresponding Codex host update. [Creator Micro v2 firmware v0.6.0-rc.6](https://github.com/worklouder/cm-v2-fw-releases/releases/tag/v0.6.0-rc.6)

Work Louder separately announced that existing Creator Micro 2 and Framer Micro owners would receive this as a free, optional public update on **2026-08-01**, with beta access beginning 2026-07-23. Therefore the code is downloadable but the general rollout is still future-dated and prerelease as of 2026-07-25. Work Louder has not published an exact firmware/Input/Codex compatibility matrix. [Work Louder update announcement](https://www.youtube.com/shorts/t63MSgObnQU)

### What current public artifacts verify

The current Work Louder Input 0.17.2 application recognizes Creator Micro 2 and Codex Micro as separate device types. Its packaged configuration data includes a Codex profile populated with proprietary `KV_OAI_*` keycodes:

```text
Encoder: KV_OAI_ENC_CC, KV_OAI_ENC_CW, KV_OAI_ENC_CLK
Agents:  KV_OAI_AG00 … KV_OAI_AG05
Actions: KV_OAI_ACT06 … KV_OAI_ACT12
Joystick mode: VENDOR
```

The Input UI expressly locks Layer 1 only when the detected device type is **Codex Micro**, directing that device’s semantic configuration to Codex. Creator Micro 2 does not match that particular lock condition.

This does **not** prove that Input can create, append, or position a Codex layer on Creator Micro 2. No matching public Input control, firmware gate, or provisioning procedure was found in the current artifact. The reliable boundary is:

- firmware v0.6.0-rc.6 proves that a “Codex-enabled layer” exists and gates Codex lighting to it;
- the product page says commands and layouts are remapped in Codex; and
- Work Louder has not yet documented how that layer first reaches an existing Creator Micro 2.

[Creator Micro 2 product page](https://worklouder.cc/creator-micro-2), [Creator Micro v2 firmware v0.6.0-rc.6](https://github.com/worklouder/cm-v2-fw-releases/releases/tag/v0.6.0-rc.6)

The Creator Micro 2 setup page documents Input, touch-to-cycle through a maximum of six layers, and AppSense-based automatic layer selection. It does **not** yet document an Agent Mode toggle or third-party provider setup. [Creator Micro 2 setup](https://worklouder.cc/micro-setup)

### Codex-host configuration

The current Codex desktop application recognizes Creator Micro 2 as a first-class device and presents the same onboarding/settings family used for Codex Micro. Its settings let the user:

- choose which Codex threads the six Agent Keys follow;
- map supported Codex commands and custom commands/skills;
- configure joystick directions;
- choose dial behavior, including reasoning depth;
- configure microphone behavior, brightness, and auto-off.

Long-pressing the dial opens the device configuration page in Codex. Work Louder’s official Codex Micro walkthrough demonstrates the same Agent Key, command, dial, joystick, and configuration model; Creator Micro 2’s product page explicitly advertises that direct Codex integration. [Official Work Louder Codex walkthrough](https://www.youtube.com/watch?v=3-2OH6ReiPM)

On macOS, Codex needs Input Monitoring permission for the integration to respond to device input. Work Louder also warns that other Input Monitoring applications can interfere with Codex–device communication. [Codex Micro setup](https://worklouder.cc/openai-micro-setup)

## 3. RGB and status semantics

The public UI uses this Agent Key legend:

| State | Color |
|---|---|
| Idle | White |
| Thinking | Blue |
| Complete | Green |
| Needs input | Amber |
| Error | Red |
| Unassigned | Off |

The product page publishes the five named active states. The official walkthrough additionally documents unassigned/off and selection behavior. [Creator Micro 2 product page](https://worklouder.cc/creator-micro-2), [official Work Louder Codex walkthrough](https://www.youtube.com/watch?v=3-2OH6ReiPM)

The current Codex host translates its own thread state into six RGB slots and sends the resulting lighting state to the device through Work Louder’s private integration package. The firmware, not Input/AppSense, gates that lighting to the Codex-enabled layer. Switching to another layer restores its normal lighting; v0.6.0-rc.6 specifically fixes Codex lighting appearing on unrelated layers. [Creator Micro v2 firmware v0.6.0-rc.6](https://github.com/worklouder/cm-v2-fw-releases/releases/tag/v0.6.0-rc.6)

OpenAI’s own product page describes the RGB as coming “from Codex,” the joystick as launching Codex workflows, and the dial as adjusting reasoning level. This independently supports the Codex-host-fed interpretation. [OpenAI Supply Co. × Work Louder](https://openai.com/supply/co-lab/work-louder/)

This distinction matters:

- **Input/AppSense feedback:** focused-app layer selection and app-colored underglow.
- **Agent Mode feedback:** per-thread Codex status on the six Agent Keys.

Input’s app-awareness is not an inbound agent-status interface. It also cannot distinguish Codex, Claude Code, and Pi sessions that share one terminal application without help from a host process.

## 4. Arbitrary agents and Herdr

No official or current public artifact exposes a supported way to replace the Codex status source. The evidence consistently names Codex:

- the product page says “Codex agents” and “directly integrated into Codex”;
- the firmware calls the feature “OpenAI Codex integration” and “Codex lighting”;
- the packaged Codex profile uses `KV_OAI_*` vendor actions;
- the Codex desktop host derives the six states from its own threads.

Work Louder’s public SDK alpha does not help: it is expressly **“ONLY FOR NOMAD v1.”** No Creator Micro 2 or Codex Micro SDK/protocol repository appears in Work Louder’s public repository list. [SDK alpha release](https://github.com/worklouder/input-releases-internal/releases/tag/sdk-alpha-0.1), [Work Louder public repositories](https://github.com/orgs/worklouder/repositories)

### What is supported for Herdr

Creator Mode can trigger a Herdr integration through documented outbound mechanisms:

1. Map distinctive HID shortcuts/macros in Input.
2. Use AppSense when application-level focus is sufficient.
3. When enabled in a compatible Input release, use Smart Actions to run a command, open an app, paste text, or open a URL.
4. Have a host-side Herdr adapter translate the trigger into pane/agent-specific behavior.

Firmware v0.5.0-rc.1 documents Smart Actions as outbound triggers, but the inspected stable Input 0.17.2 artifact returns false for the device’s Smart Action feature gate. Treat this route as version-dependent and test it before relying on it. Neither the release notes nor product claims describe inbound state or RGB control. [Creator Micro v2 firmware v0.5.0-rc.1](https://github.com/worklouder/cm-v2-fw-releases/releases/tag/v0.5.0-rc.1)

### What remains unsupported

A Herdr adapter can aggregate Claude Code, Codex CLI, and Pi state and can implement controls such as focus, approve, interrupt, model, or thinking effort. However, publishing that state to the six Agent Key LEDs requires either:

- a future public Work Louder API;
- explicit access to and permission for Work Louder’s private device integration; or
- unsupported protocol reimplementation.

Creator Micro 2 Agent Mode does not remove this requirement. It describes a Codex vendor layer whose semantic state is owned by the Codex desktop host.

## 5. Device ownership and coexistence

Input 0.17.2’s changelog says it fixed device communication interfering with the Codex application. This is good evidence that Work Louder intends **Input and Codex** to coexist, but it is a targeted vendor fix rather than a general multi-client contract. [Input 0.17.2 release](https://github.com/worklouder/input-releases/releases/tag/v0.17.2)

No public document specifies:

- a device-lock or writer-arbitration protocol;
- priority among two RGB/status writers;
- how a third process should share the HID connection;
- atomic ownership handoff on layer changes; or
- compatibility guarantees across USB, Bluetooth, macOS, and Windows.

Consequently, a Herdr bridge should safely use normal keyboard output without also writing device lighting. Concurrent direct RGB control should be treated as experimental until Work Louder documents a supported API and coexistence semantics.

## 6. Exact Creator Micro 2 versus Codex Micro difference

There is a supported difference, but it is smaller than “open Agent Mode.”

| Creator Micro 2 | Codex Micro |
|---|---|
| General Creator product; Codex support was announced as a free, optional update for existing devices. | Dedicated Codex edition; the official walkthrough says Layer 1 is reserved for Codex and Input unlocks five additional layers. |
| Firmware v0.6.0-rc.6 says Codex commands are assignable to a keymap layer. It does not specify the layer index or provisioning process. | The shipped/default Codex profile and Input UI both identify Layer 1 as the protected Codex layer. |
| Current host artifacts recognize product IDs `0x8297` and `0x8298`. | Current host artifacts recognize product ID `0x8360`. |

The supported difference is product delivery: Creator Micro 2 remains a general Creator device that can gain Codex through an optional update, while Codex Micro provides the dedicated Codex layout out of the box. Public evidence does not support a stronger claim about movable Codex-layer placement.

It is **not** a route unavailable on Codex Micro for arbitrary agent status. Both products can send ordinary shortcuts to a Herdr bridge; neither exposes a public Herdr/Claude/Pi status writer. Buying or switching to Creator Micro 2 would therefore not solve the missing RGB/provider API.

## 7. Practical conclusion for the Codex Micro + Herdr project

Treat Creator Micro 2 Agent Mode as useful evidence that the hardware/firmware can gate semantic lighting to a designated vendor layer, but not as a reusable integration surface.

The defensible design remains:

1. Use Codex Micro Layer 2 for distinctive ordinary HID shortcuts.
2. Let a host-side adapter use Herdr for pane/agent targeting and state aggregation.
3. Implement Codex CLI, Claude Code, and Pi controls behind agent-specific adapters.
4. Keep RGB writeback optional and disabled by default until Work Louder provides a public SDK or grants access.

Creator Micro 2’s new Codex layer does not materially change that architecture.

## Artifact baseline and confidence

Local artifact inspection was used to verify behavior not yet documented on a public setup page:

- Work Louder Input **0.17.2**, released 2026-07-22.
- Codex desktop for macOS **26.721.41059**.
- Bundled Work Louder OAI integration package **0.1.11**.
- Creator Micro 2 firmware release notes through **v0.6.0-rc.6**, released 2026-07-23.

**High confidence:** Agent Mode is Codex-specific; firmware layer gating; current RGB legend; proprietary Codex vendor actions; no applicable public SDK; no official arbitrary-provider support.

**Unknown:** the exact user-facing provisioning flow for the Creator Micro 2 Codex-enabled layer. Neither current public documentation nor inspected Input 0.17.2 provides a reproducible creation procedure or complete version matrix.

**Unknown:** whether Work Louder will later expose the private status/RGB transport, broaden Agent Mode to other providers, or make firmware 0.6.0 stable without changing the flow.
