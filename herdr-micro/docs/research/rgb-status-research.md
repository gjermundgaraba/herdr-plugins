# Critical per-agent RGB for Herdr

Research snapshot: **2026-07-25**

> **Historical decision record:** the proposed experiment is complete. The
> local `herdr-micro` plugin now drives Layer 2 over USB and BLE; see
> [hardware test log](./hardware-test-log.md) and
> [wireless bridge evidence](./wireless-bridge-evidence.md).

## Decision

**The six-agent RGB display can work on Layer 2 without new firmware.** It must be an **OAI-enabled layer** containing `KV_OAI_AG00` through `KV_OAI_AG05`; an ordinary Layer 2 containing only HID shortcuts still cannot address those six slots.

The gate is the active layer's keycodes, not its numeric index. [Pejman's hardware-tested procedure](https://gist.github.com/pejmanjohn/d8f1fb99698c1599a533e65514e24469) copied Layer 1's `layout` to Layer 2 and retained the live Agent keys and white-to-blue status changes on Input `0.17.2` / firmware `v0.4.1`. [`claude-micro-layer`](https://github.com/duolahypercho/claude-micro-layer/commit/3597273f98f5b458f9ab70d4a5ab2b7bcc212d10) independently put the six `KV_OAI_AG*` codes on Layer 2 and observed per-thread lighting and vendor events.

Firmware disassembly corroborates the tests: both official Creator Micro 2 `v0.4.0` and `v0.6.0-rc.6` binaries scan the active layer for vendor OAI keycode ranges and contain no hard-coded Layer 1 comparison. Installed Codex logs also show `v.oai.thstatus` accepted while Layer 2 is active. See [Layer 2 individual-RGB bypass](./layer2-individual-rgb-bypass-research.md).

[`house-of-herdr`](https://github.com/alasano/house-of-herdr/tree/7d8eadaed41a1bb4456565d6bcba8cdb7380b77e/packages/codex-micro) is an existing MIT Herdr plugin that:

- maps six Herdr agents and their `working`, `blocked`, `done`, `idle`, and `unknown` states to the six Agent Key LEDs;
- focuses the corresponding agent from each key;
- routes the dial, joystick, and command keys through Herdr; and
- directly sends `v.oai.thstatus` and `v.oai.rgbcfg` over vendor HID.

The author [demonstrates it working on the physical device](https://x.com/aljosa/status/2080765081001074906). Local source checks passed 64 tests and a TypeScript build. [`microd`](https://github.com/PlaneshiftDev/microd) is a second direct Herdr implementation; its 13 Rust tests also pass locally.

The remaining hard boundary is device ownership. `house-of-herdr`, Input, and Codex can all open the interface; there is no cross-process lease, and competing writers can repaint lights or duplicate input. OAI Agent key presses can also focus/open Codex when Codex is running. The Herdr experiment must therefore quit Input and Codex and use one bridge as the sole vendor-HID owner.

Therefore:

1. **Prove the gate safely:** clone Layer 1's live `layout` into disposable Layer 3, preserving all six Agent keycodes and initially disabling generic lighting.
2. **Run Herdr alone:** quit Input and Codex, then verify six distinct `v.oai.thstatus` colors and `v.oai.hid` events with a pinned Herdr bridge.
3. **Move the proven layout to Layer 2:** customize the seven other keys, dial, and joystick while keeping `AG00`–`AG05`.

The exact Herdr-plus-Layer-2 combination has not yet been publicly hardware-tested, but both halves are independently verified on shipping firmware. Do not copy the proprietary `UNLICENSED` package, run multiple RGB writers concurrently, or flash firmware.

## Creator Micro 2 Agent Mode

Creator Micro 2 Agent Mode is important evidence of capability, but it is not an open integration point.

| Verified behavior | Consequence for Herdr |
|---|---|
| Six Agent Keys show `idle`, `thinking`, `complete`, `needs input`, and `error`. | The hardware can present the required per-agent state model. |
| The product page describes those keys as monitoring **Codex agents**. | Agent Mode is not documented as provider-neutral. |
| Firmware `v0.6.0-rc.6` enables Codex actions and live Codex lighting only on a Codex-enabled layer. | Dynamic lighting can be isolated by layer. |
| Creator Mode supplies ordinary Input shortcuts, macros, AppSense, and app-color cues. | It can trigger Herdr controls, but it cannot ingest Herdr agent state. |
| No public Creator Micro 2/Codex Micro status API or applicable SDK was found. | Herdr cannot supportedly replace Codex as the RGB publisher. |

The exact provisioning path is not yet publicly documented. As of 2026-07-25, Codex-capable Creator Micro 2 firmware is prerelease, the setup page has no Agent Mode procedure, and the public artifacts do not establish that users can create an arbitrary or second agent-status layer.

For the existing Codex Micro, Creator Micro 2 Agent Mode is now corroborating evidence rather than the integration mechanism. The practical Codex Micro bypass is to provision Layer 2 with the same OAI keycodes and let a sole-owner Herdr bridge publish the six states.

Full evidence: [Creator Micro 2 Agent Mode](./creator-micro-2-agent-mode-evidence.md).

## Support and reliability gate

The community plugin is sufficient for an experiment, but critical status should not be treated as authoritative until these are satisfied:

| Gate | Required before depending on the lights |
|---|---|
| Supported access | Public SDK/API or explicit acceptance of an unsupported clean-room client |
| Device ownership | Exactly one logical writer, including Codex/Input handoff |
| Layer isolation | Herdr status appears only on the intended Herdr layer |
| Failure safety | Firmware lease/TTL/watchdog clears stale status |
| Compatibility | Supported Codex Micro firmware and USB/BLE matrix |

Until all five are satisfied, the lights are **advisory**, not authoritative. Herdr's sidebar, notifications, and sounds remain the reliable fallback.

## Why a normal Layer 2 is insufficient

Work Louder's current firmware notes say:

- Codex commands can be assigned to a keymap layer.
- Live Codex status lighting is active only on a Codex-enabled layer.
- Switching away restores that layer's normal lighting.
- A firmware fix stopped Codex lighting from leaking onto unrelated layers.

That gating is useful—it protects other layers. Input does not expose the OAI actions in its normal Layer 2 picker, but its JSON importer and serializer preserve them when copied from Layer 1.

The factory Codex layer contains private `KV_OAI_*` actions, and current Input locks Codex Micro Layer 1 from normal editing. Copying those actions into Layer 2 is an unsupported but hardware-verified solution:

- The firmware maps the six Agent positions and renders `thstatus` on that active layer.
- Codex still receives the vendor action if it is running.
- Codex and a Herdr process could overwrite the same global six-slot state.
- Switching layers does not create a documented ownership handoff.

## The capability that exists privately

Read-only inspection of the installed Codex app found:

| Property | Observed value |
|---|---|
| Device VID/PID | `0x303A` / `0x8360` |
| Vendor HID usage page | `0xFF00` |
| Agent slots | IDs `0`–`5` |
| Per-slot fields | color, brightness, effect, speed, key/ambient sync |
| Status write | complete six-slot model |
| Device events | vendor key/action/agent and joystick notifications |

Codex uses:

| Meaning | Color |
|---|---|
| idle | `#FFFFFF` |
| working | `#304FFE` |
| complete/unread | `#00FF4C` |
| awaiting input | `#FF6D00` |
| error | `#FF0033` |
| unassigned | `#000000` |

The exact observed framing and method schemas are recorded for interoperability research in [rgb-device-protocol-evidence.md](./rgb-device-protocol-evidence.md). They are implementation evidence, not a public API contract or permission to redistribute Work Louder code.

## Recommended production architecture

The cleanest design is an extension inside the existing Codex/Input device owner:

```text
Herdr snapshots and events
  → stable six-slot allocator
  → status renderer
  → vendor-supported status-provider API
  → existing single device owner
  → Codex Micro
```

If Work Louder instead provides a standalone SDK, use one signed per-user broker:

```text
Herdr source collectors
  → authoritative state cache
  → six-slot reconciler
  → one logical device lease
  → diff + atomic write + acknowledgement
  → Codex Micro
```

The broker must:

- read the active layer and render only on an authorized Herdr layer;
- serialize and deduplicate full-state writes;
- resnapshot after reconnect, transport handoff, or layer change;
- yield cleanly to Codex/Input rather than racing them;
- expose software health separately from agent state.

## Herdr state pipeline

Herdr provides enough information for a high-quality six-agent display, but its snapshot must remain authoritative.

Use this collector sequence:

1. Subscribe to topology/focus events.
2. Take `session.snapshot`.
3. Subscribe to unfiltered `pane.agent_status_changed` for every pane.
4. Take a second snapshot and publish it atomically.
5. Treat later events as invalidations that trigger a coalesced fresh snapshot.

This avoids several traps:

- general subscriptions can replay up to 512 retained events and expose no resume cursor;
- status subscriptions are pane-specific and have no revision;
- viewing one tab can turn several agents from `done` to `idle`;
- array order and the Agent-view sort are not durable slot identity;
- every named Herdr session has a separate socket.

Full event and reconnect semantics are in [rgb-herdr-state-evidence.md](./rgb-herdr-state-evidence.md).

## Stable six-slot assignment

Physical keys must preserve muscle memory. Never sort the visible slots directly by current status.

Use:

```text
SourceKey = configured host ID + Herdr session name
TargetKey = SourceKey + Herdr terminal_id
```

Allocator rules:

1. Keep every surviving target in its current slot.
2. Clear a slot only when its terminal disappears.
3. Fill empty slots deterministically.
4. Never evict a working, blocked, done, or focused incumbent automatically.
5. When more than six agents exist, use explicit pages/banks and a separate overflow alert.

`terminal_id` survives pane moves. Pane IDs, agent labels, names, and array indexes do not provide the same stable identity.

## Status presentation

Use Herdr's meanings, not merely Codex's color labels:

| Herdr state | RGB | Meaning |
|---|---|---|
| unassigned | off | no agent |
| `idle` | solid white | ready and already seen |
| `working` | blue breathing | active computation |
| `done` | solid green | completed result not yet seen |
| `blocked` | amber pulse | attention required |
| `unknown` | dim violet pulse | state cannot be proven |

Add a brief white accent or brightness lift for the selected agent without replacing its base state.

Red should not mean a Herdr agent error by default: Herdr has no generic `error` state. Reserve red for a proven agent-specific error adapter or an unmistakable whole-device bridge/transport fault. Also note that unusual approval prompts can sometimes be classified as `idle`; a hardware approve action must inspect the actual target prompt before sending input.

## The unresolved stale-light problem

No public or locally inspected contract documents a firmware TTL, host lease, or watchdog for dynamic lighting.

If the host bridge crashes after writing blue, green, or amber, the last state may remain visible. A graceful shutdown can clear the lights and `launchd` can restart the process, but neither covers force-kill, an OS hang, or power loss.

For critical status, require one of:

- host lease renewed by heartbeat;
- per-update firmware TTL;
- watchdog fallback to the stored layer lighting;
- transactional `begin dynamic session` / `end dynamic session`.

Without this, stale status cannot be bounded and the keyboard must not be the only alert surface.

## Community prototype

The clean-room protocol has already been implemented under community licenses; vendor permission is not a technical prerequisite to evaluate that code. The bundled Work Louder package remains private and must not be copied.

For the safest evaluation:

1. Pin and review `house-of-herdr`; first fix or verify its `Codex.app` ownership detector.
2. Close Codex and Input so exactly one process owns the device.
3. Use wired USB and do not flash firmware, write keymaps, or reset profiles.
4. Keep Herdr's on-screen status visible to catch stale or incorrect lights.
5. Test shutdown, reconnect, active-layer behavior, and six-slot repaint.

A test that opens a second non-exclusive HID handle beside Codex is not evidence of reliable coexistence. This research built and tested the software but could not exercise the user's keyboard because no Codex Micro was connected during the hardware check.

## Vendor request

Send Work Louder this scoped request:

> We want a signed macOS Herdr status provider for Codex Micro Layer 2. It must display six independent agent states while preserving native Codex Layer 1 and Work Louder Input configuration. Please provide a supported SDK/API or an extension point in Input/Codex, plus documented device ownership and failure semantics.

Ask for:

- whether Creator Micro 2 Agent Mode accepts a third-party status provider, and whether the same capability can be enabled on Codex Micro Layer 2;
- six Agent Key RGB/effect calls and active-layer notifications;
- Codex/Input/custom-client lease, handoff, and arbitration rules;
- atomic updates, acknowledgements, errors, heartbeat, and TTL;
- supported Codex Micro firmware plus USB/BLE parity and recovery;
- macOS signing/Input Monitoring requirements and redistribution terms.

Work Louder lists `hello@worklouder.cc` and directs support through its Discord ticket flow. OpenAI should also confirm whether Codex can yield device ownership or accept a third-party status provider.

## Evidence set

- [Device protocol and ownership evidence](./rgb-device-protocol-evidence.md)
- [Herdr state/event/slot evidence](./rgb-herdr-state-evidence.md)
- [Deployment, support, and licensing evidence](./rgb-integration-architecture-evidence.md)
- [Source code and community workarounds](./source-and-community-workarounds.md)
