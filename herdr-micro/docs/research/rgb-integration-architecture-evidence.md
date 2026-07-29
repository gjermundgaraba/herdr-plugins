# Critical per-agent RGB: deployment architecture and evidence

Research date: **2026-07-25**. This note focuses narrowly on reliable dynamic Agent Key lighting for Herdr. Public claims use first-party sources. Observations from the locally installed signed applications are identified separately and are **implementation evidence, not a public or stable API contract**.

> **Historical production-support design:** the personal `herdr-micro`
> integration is now implemented and physically verified. The vendor support
> and licensing cautions below still apply; the broker proposal is not the
> current local plan.

## Decision

There is no presently documented, licensed, production-supported way for a third-party Herdr bridge to drive Codex Micro's per-agent RGB.

There is, however, a working unofficial route: [`house-of-herdr`](https://github.com/alasano/house-of-herdr/tree/7d8eadaed41a1bb4456565d6bcba8cdb7380b77e/packages/codex-micro) implements the Herdr bridge directly and its author demonstrates physical RGB/control. It takes over reserved Layer 1; it does not provide simultaneous Herdr RGB on Layer 2. Full source validation and caveats are in [source-and-community-workarounds.md](./source-and-community-workarounds.md).

The hardware and firmware clearly can do it. The locally installed Codex app contains a private Work Louder package with per-thread lighting calls, while current Work Louder firmware release notes describe live Codex activity/status lighting. The remaining production blockers are supported access, ownership arbitration, crash semantics, and licensing.

For RGB that is genuinely critical:

1. **Ask Work Louder for a supported API and ownership contract before building the device writer.**
2. **Require exactly one process to own lighting writes.** Prefer an extension point in Codex or Input. If that is unavailable, use one signed per-user broker that is the sole device writer.
3. **Do not ship a concurrent raw-HID sidecar.** It can be a disposable proof of concept only.
4. **Keep Layer 2 shortcuts independent of RGB.** Work Louder Input emits ordinary HID chords; Herdr and the agent adapters remain useful even when RGB is offline.
5. **Treat the lights as advisory until firmware supplies a lease/TTL or watchdog.** A crashed host process otherwise leaves the last displayed state looking current.

### Architecture ranking

| Rank | Architecture | Codex Layer 1 coexistence | Reliability | Support status | Decision |
|---:|---|---:|---:|---|---|
| 1 | Work Louder/OpenAI-supported status-provider extension inside Codex or Input | Preserved by the existing owner | Highest | Not publicly offered | Request this |
| 2 | Vendor-licensed, signed per-user broker as the sole hardware writer | Requires an official handoff or Codex status feed | High after protocol guarantees | Not currently licensed | Viable with vendor agreement |
| 3 | Pinned `house-of-herdr` as sole Layer 1 owner | Replaces native Codex while active | Medium, still young | MIT community source; vendor-unsupported | Best experiment now |
| 4 | Custom process opens HID beside Codex and Input | Possible, but writes are not arbitrated | Low | Unsupported | Do not run concurrently |
| 5 | Extract private package or flash guessed firmware | Fragile, unlicensed, or wrong target | Unacceptable | Unsupported | Do not do this |

## What is public and supported

Work Louder documents the intended product split:

- Codex owns native Agent and Command behavior, including six live Agent Key states.
- Work Louder Input maps custom shortcuts to keys, dial, and joystick across six programmable layers.
- RGB states are white idle, blue thinking, green complete, amber requires input, red error, and off when unassigned.

Sources: [Codex Micro setup](https://worklouder.cc/openai-micro-setup), [Codex Micro product](https://worklouder.cc/codex-micro), [Work Louder Input](https://worklouder.cc/input).

Work Louder's Creator Micro v2 firmware `v0.6.0-rc.6` supplies especially relevant first-party evidence:

- Codex commands can be assigned to a keymap layer.
- Lighting can show live Codex activity and status.
- Codex lighting appears only on a Codex-enabled layer; changing layer restores normal lighting.
- The release fixed Codex lighting leaking onto unrelated layers.

This establishes that layer-aware lighting arbitration exists in the current firmware family. It does **not** establish that a third-party host may call it, nor that this separately named Creator Micro v2 image should be flashed onto Codex Micro. The release is also a prerelease. See [Creator Micro v2 firmware `v0.6.0-rc.6`](https://github.com/worklouder/cm-v2-fw-releases/releases/tag/v0.6.0-rc.6).

No public Codex Micro SDK, protocol reference, source package, or third-party status-provider interface was found. Work Louder's only public SDK alpha is explicitly for Nomad v1 and requires special Nomad firmware; it is not applicable here. See [Nomad-v1-only SDK alpha](https://github.com/worklouder/input-releases-internal/releases/tag/sdk-alpha-0.1), [Work Louder repositories](https://github.com/orgs/worklouder/repositories).

## Local artifact evidence

The following signed applications were inspected read-only:

| Application | Bundle ID | Version inspected | Relevant finding |
|---|---|---:|---|
| `/Applications/Codex.app` | `com.openai.codex` | `26.721.41059` | Contains Codex Micro service and private Work Louder OpenAI device kit |
| `/Applications/input.app` | `it.focusense.input-app` | `0.17.2` | Contains the same base Work Louder device kit version |

### A private per-thread lighting API exists

Codex's bundled `@worklouder/device-kit-oai` version `0.1.11` exposes typed capabilities for:

- device discovery and connection;
- firmware/device status, including selected layer and connection state;
- generic lighting preview;
- per-thread lighting by thread ID;
- per-thread color, brightness, effect, speed, and optional key/ambient synchronization;
- key and ambient lighting configuration;
- custom HID and joystick notifications.

The base `@worklouder/wl-device-kit` version `0.1.23` handles Work Louder USB HID/serial discovery, transport, JSON-RPC, notifications, lighting, device files, and firmware operations. Input `0.17.2` bundles the same base version.

This is strong proof that a clean implementation is technically possible. It is not permission to depend on or redistribute these packages.

### The package is explicitly private and unlicensed

Both bundled package manifests declare:

```json
{
  "license": "UNLICENSED",
  "publishConfig": {
    "registry": "https://npm.pkg.github.com"
  }
}
```

Their bundled READMEs call them private GitHub Packages. The OpenAI-specific README marks the material proprietary/confidential and describes it as an OpenAI integration package. Both names return `404` from the public npm registry as of the research date.

Consequences:

- Do not copy the packages out of Codex or Input into this project.
- Do not dynamically load them from inside either signed application bundle.
- Do not hard-code their private RPC strings from an extracted application.
- Do not redistribute a bridge containing their code, typings, documentation, or protocol details without written permission.
- A public npm `404` does not prove nobody can get access; the package metadata itself says authenticated GitHub Packages access is required.

The Work Louder website terms are not an SDK license and do not grant reuse or redistribution rights. See [Work Louder terms](https://worklouder.cc/terms-of-services). This is an engineering boundary, not legal advice.

### Current Codex behavior is useful design evidence

The inspected Codex Micro service:

- recognizes both USB and Bluetooth HID paths;
- orders USB before Bluetooth and hands off to USB when it becomes available;
- watches HID topology, with a fallback scan;
- reconnects with a bounded `1 s → 2 s → 5 s → 10 s` backoff;
- serializes lighting updates inside the service;
- deduplicates unchanged lighting models;
- handles six agent slots;
- sends an all-off lighting state during a normal stop.

The base Work Louder transport:

- opens HID in non-exclusive mode on macOS;
- serializes requests in a FIFO with a documented 50 ms inter-request cooldown;
- rejects pending calls on disconnect and clears its internal queue.

These are good patterns to request from a supported SDK. They are not stable ABI/API promises and may change on any Codex update.

## Device ownership and contention

### Non-exclusive access is not multi-writer arbitration

`node-hid` documents a macOS `nonExclusive` open mode. It allows a keyboard interface to remain available to the OS and other processes after the user grants permission. It does **not** provide ordering, transactions, ownership leases, or last-writer conflict resolution across processes. See the first-party [node-hid README](https://github.com/node-hid/node-hid#opening-a-device).

The Work Louder base library's FIFO protects only calls made through one `WLDeviceCommImpl` instance. Two processes each have their own queue. If Codex, Input, and a Herdr sidecar all write lighting:

```text
Codex queue ─┐
Input queue ─┼── same HID RPC channel ── device lighting state
Herdr queue ─┘
```

Nothing public says those queues are coordinated. Even if each HID report is written intact, higher-level JSON-RPC fragments or complete lighting updates can interleave or overwrite each other. “The handle opened successfully” is therefore not a reliability test.

### Public history confirms that interference is real

- Work Louder warns that Karabiner and Logitech Options+ with Input Monitoring permission can interfere with Codex–Micro communication.
- Input `0.17.2` specifically says it fixed “device communication interfering with Codex application.”

Sources: [Codex Micro setup](https://worklouder.cc/openai-micro-setup), [Input `0.17.2` release](https://github.com/worklouder/input-releases/releases/tag/v0.17.2).

On the inspected Mac, Codex and Input `0.17.2` were simultaneously running while a Codex Micro was connected over Bluetooth. That shows process-level coexistence, not proof of concurrent lighting ownership. Input may avoid or yield the live channel after the `0.17.2` fix.

### Required ownership rule

For a production bridge, exactly one component must write the live lighting/control RPC channel:

```text
Herdr events ──────┐
Codex status* ─────┼── RGB broker ── one device connection ── Codex Micro
Input config* ─────┘

* only through vendor-supported feeds or handoff
```

The broker must have:

- an exclusive logical lease even if the HID handle is technically non-exclusive;
- a documented way to know which layer is active;
- a documented rule for when Codex, Input, or the broker may write;
- atomic or revisioned lighting updates;
- acknowledgements and protocol error reporting;
- a safe release/handoff operation.

If Work Louder exposes a status-provider plugin inside the existing Codex/Input owner, that is preferable to a standalone broker. It eliminates a second device process while preserving native Layer 1.

## USB versus Bluetooth

The product officially supports both USB-C and Bluetooth. The current Codex setup page says:

- hold the touch control to enter communication selection;
- there are BLE channels 1–3 and a wired mode;
- plugging USB in while BLE is selected leaves the device on BLE and only charges it.

Source: [Codex Micro setup](https://worklouder.cc/openai-micro-setup).

The Creator Micro v2 `v0.6.0-rc.6` prerelease changes that behavior so connecting USB selects USB automatically. This means connection selection is firmware-version-dependent. Do not infer transport from the presence of a cable. See [firmware release](https://github.com/worklouder/cm-v2-fw-releases/releases/tag/v0.6.0-rc.6).

For initial critical-RGB testing:

1. Explicitly select wired mode and verify it on the device.
2. Record device model, firmware, selected layer, transport, and stable hardware identity at connection.
3. Test Bluetooth separately only after wired recovery, hot-plug, sleep, and restart pass.
4. Never key persistent state to an OS HID path; paths can change after reconnect.

Wired mode is recommended because the current Codex implementation prefers it and it avoids Bluetooth reconnection and battery-state variables. That is an engineering inference, not a Work Louder reliability guarantee.

Required USB/BLE test cases:

| Case | Required result |
|---|---|
| Cable inserted while BLE selected | Broker reports actual BLE; it must not label the device wired merely because it is charging |
| Bluetooth disconnect/reconnect | No stale “working” state; automatic resubscribe and full-state repaint |
| USB appears while BLE is active | Handoff is serialized; old handle closes before the new writer starts |
| Sleep/wake | Snapshot is reconciled before any success color is shown |
| Layer changes | Herdr RGB appears only on the authorized Herdr layer |
| Two matching devices | Refuse ambiguous selection or require a configured serial/identity |

## Recommended broker design, conditional on vendor support

Use a small state-reconciliation service, not a chain of per-key scripts:

```text
Herdr local socket
  │ session.snapshot + subscribed events
  ▼
state cache
  │ stable pane/session → physical slot mapping
  ▼
status normalizer
  │ desired six-slot lighting model + revision
  ▼
single-writer Work Louder adapter
  │ diff, serialize, acknowledge, retry
  ▼
Codex Micro
```

### Herdr is the state authority

Bootstrap with a Herdr session snapshot, then subscribe to pane/agent lifecycle events. Periodically reconcile with a fresh snapshot rather than assuming every event was received exactly once.

Use stable pane or native session identity for slot assignment. A status-sorted Agents view can reorder while agents work, so it must not silently move colors between physical keys. Either:

- configure a stable Herdr ordering that is also used by `focus_agent 1…6`; or
- maintain explicit slot assignments and show the same assignments in Herdr metadata/sidebar.

Capture the Herdr session/socket explicitly. Named Herdr sessions use separate local sockets, so a broker must not accidentally aggregate or control the wrong session.

### Status mapping

Herdr's normalized states are `idle`, `working`, `blocked`, `done`, and `unknown`. A conservative mapping is:

| Herdr state | Agent Key | Meaning |
|---|---|---|
| no assigned pane | off | no agent |
| `idle` | solid white | available/seen |
| `working` | blue animation | active computation |
| `done` | solid green | unseen completed result |
| `blocked` | amber pulse | requires attention |
| `unknown` | dim white pulse | state cannot be proven |

Do not map `unknown` to idle or done. Herdr has no generic semantic `error` state, so Codex's red-per-agent error meaning cannot be recreated faithfully from Herdr alone. Reserve an unmistakable whole-device red/offline pattern for bridge or transport failure, or add agent-specific error adapters.

Coalesce bursts and write the complete desired six-slot state with a monotonically increasing revision. Repaint the full state after reconnect, transport handoff, layer activation, or Herdr resubscription.

## Failure-safe behavior

### The hard limitation: no documented lighting lease or TTL

The locally observed API sets lighting, but no public or local type contract inspected here documents a time-to-live, watchdog, or “revert unless renewed” lease. Therefore:

- graceful shutdown can set neutral/static lighting;
- launchd can restart a crashed broker;
- neither protects the interval after a crash, force-kill, OS hang, or power failure;
- the device may retain the last blue/green/amber state and make stale information look current.

If RGB is critical, ask Work Louder for one of:

- a host lease renewed by heartbeat;
- a firmware TTL on status lighting;
- a watchdog that restores the layer's stored static lighting when the host disappears;
- a transactional “begin dynamic session / end dynamic session” API.

Without one of those, the correct user-facing statement is **“near-real-time advisory status,” not “authoritative status.”**

### Failure matrix

| Failure | Safe response |
|---|---|
| Herdr socket unavailable | Stop semantic updates; set neutral/offline pattern if communication remains available |
| Event stream ends | Mark cache stale immediately; reconnect, snapshot, then repaint |
| Agent identity changes in a pane | Clear the slot before assigning the new identity |
| Unknown/unsupported agent state | Use explicit unknown pattern, never success |
| Device disconnects | Close handle, reject queued writes, retain desired state only in memory |
| Device reconnects | Re-read identity/layer, reacquire lease, repaint full snapshot |
| Codex or Input acquires ownership | Yield or fail closed; never race to “win” lighting |
| Broker crashes | `launchd` restarts it; firmware TTL must clear stale state meanwhile |
| Repeated protocol errors | Open a circuit breaker; stop writes and show software notification |
| Wrong/ambiguous device | Do not write |

The broker must never flash firmware, write device files, reset profiles, or enter bootloader mode. Those capabilities are unrelated to status lighting and enlarge the recovery risk.

### Static fallback

Configure a recognizable static Layer 2 lighting scheme in Input before enabling dynamic RGB. On a supported graceful handoff, the broker should restore that scheme. Do not make the fallback depend on the broker reading an undocumented value from Codex.

Because unexpected process death cannot currently guarantee restoration, retain a software status surface in Herdr's sidebar/metadata and notifications. The keyboard is an additional display, not the sole alarm path.

## macOS supervision and packaging

### Run as a per-user agent

The Herdr socket, terminal agents, and Input Monitoring grant all belong to the logged-in user. Run the broker in that user's GUI session, not as root and not as a system LaunchDaemon.

Apple describes per-user background processes as LaunchAgents and loads them for the logged-in user's session. For a distributable macOS application on macOS 13+, package the helper inside a signed/notarized app and register it with `SMAppService`; Apple calls this the modern replacement for directly installing `~/Library/LaunchAgents` entries. Sources: [Creating Launch Daemons and Agents](https://developer.apple.com/library/archive/documentation/MacOSX/Conceptual/BPSystemStartup/Chapters/CreatingLaunchdJobs.html), [`SMAppService`](https://developer.apple.com/documentation/servicemanagement/smappservice).

For a personal prototype, a conventional `~/Library/LaunchAgents` plist is acceptable. Use:

- one unique `Label`;
- absolute `Program`/`ProgramArguments`;
- the exact named Herdr socket or session argument;
- `RunAtLoad = true`;
- `KeepAlive = true` only if the process stays alive and handles device/Herdr absence internally;
- default or at least 10-second `ThrottleInterval`;
- `LimitLoadToSessionType = Aqua`;
- `Umask = 0077`;
- private `StandardOutPath` and `StandardErrorPath` with rotation;
- no shell interpolation and no secrets in the plist environment.

Apple's `launchd.plist(5)` manual says `KeepAlive` implies `RunAtLoad` and jobs that exit quickly and repeatedly are throttled. The broker should therefore remain alive, wait for hot-plug/socket availability, and use bounded backoff rather than exiting every time the keyboard or Herdr is absent.

### Do not poll HID enumeration aggressively

`node-hid` warns that device enumeration is relatively expensive and can slow parallel HID/USB activity. Use OS hot-plug notification when available, with a slow fallback reconciliation scan. The inspected Codex service follows that pattern. See [node-hid general notes](https://github.com/node-hid/node-hid#cost-of-hiddevices-hiddevicesasync-new-hidhid-and-hidasyncopen-for-detecting-device-plugunplug).

### Single-instance and local IPC

The broker must reject a second instance before either opens the device. A vendor lease is best; otherwise use both:

- a per-user single-instance lock; and
- a private Unix socket for commands/status.

Create the socket and state directory with user-only permissions. Herdr's control socket can inject input and restructure sessions, so passing its path gives the broker meaningful control. The RGB-only worker should consume state/events but should not expose arbitrary Herdr command execution.

## macOS permissions

The private Work Louder README says macOS Input Monitoring is required because the library opens the keyboard HID interface in non-exclusive mode. The upstream `node-hid` README likewise says macOS may request user permission when a keyboard is opened non-exclusively.

Expected permissions:

| Permission | Needed? | Why |
|---|---:|---|
| Input Monitoring | Yes for direct Work Louder HID client | Raw non-exclusive keyboard HID |
| Accessibility | No | The broker uses Herdr's socket and the device API, not GUI scripting |
| Microphone | No for RGB | Needed only by a separate cross-agent push-to-talk feature |
| Full Disk Access | No | Herdr socket and private app state do not require it |
| Administrator/root | No | Per-user agent and HID permission are sufficient |

The locally inspected Codex app is signed by OpenAI and explicitly has App Sandbox disabled. Input is signed by Focusense and is also not App-Sandboxed. A new bridge receives neither application's identity nor its TCC grant; it needs its own stable signed identity and separate Input Monitoring approval.

For a personal unsigned CLI prototype launched by Terminal, permission may attach to Terminal rather than provide a durable production identity. Packaging the helper with a signed GUI app makes permission instructions, upgrades, and launch registration tractable. Verify permission persistence across an app update before calling the deployment reliable.

Do not add Accessibility merely to work around a missing device API. That substitutes broad GUI control for a narrow unsupported hardware dependency and still does not solve writer contention.

## Shipping and licensing boundary

A redistributable implementation needs all of the following:

1. A written license for the Work Louder SDK/protocol and redistribution of any runtime.
2. A supported Codex Micro firmware/version matrix and compatibility policy.
3. A documented coexistence and ownership contract with Codex and Input.
4. A recovery image/procedure specific to Codex Micro.
5. A signed/notarized application and registered per-user helper.

Do not patch either installed application, alter its code signature, import modules from its `app.asar`, or depend on bundle-internal filenames. Codex's service filename is content-hashed and its internal behavior is not a compatibility promise.

Herdr licensing is a separate concern from the device library. A bridge can communicate with Herdr through its documented CLI/local socket without copying Herdr code. Evaluate the license of the exact Herdr version if distributing linked or derived code; stable `v0.7.5` and the post-release branch do not currently carry the same license metadata.

## Required vendor support request

Send one concrete request to Work Louder before implementing production RGB:

> We want a signed macOS Herdr status-provider for Codex Micro Layer 2. It must display six independent agent states while preserving native Codex Layer 1 and Work Louder Input configuration. Please provide a supported SDK/API or an extension point in Input/Codex, plus the ownership, transport, firmware, recovery, and redistribution terms below.

Ask for:

- Codex Micro public SDK/API or protocol documentation;
- six Agent Key IDs and per-key/thread RGB/effect calls;
- layer-scoped behavior and active-layer notifications;
- Codex/Input/custom-client ownership, lease, and handoff rules;
- atomic update, acknowledgement, timeout, and error semantics;
- host heartbeat/TTL and static-lighting restoration;
- USB/Bluetooth feature parity, identifiers, and handoff rules;
- minimum/supported firmware and exact Codex Micro recovery image;
- macOS Input Monitoring and signing requirements;
- permission to use and redistribute the device runtime;
- compatibility/versioning and support policy.

Work Louder directs support requests to its Discord ticket system and lists `hello@worklouder.cc` for direct contact in its support/return material: [Work Louder support policy](https://worklouder.cc/return-policy).

OpenAI also needs to answer one narrower question if native Codex Layer 1 must coexist: whether Codex can yield device ownership or accept a third-party status provider. No public Codex Micro extension API was found in the current Codex documentation: [OpenAI Codex Micro documentation](https://learn.chatgpt.com/docs/features/codex-micro).

## Prototype gate

Only start a direct-RGB prototype after written vendor approval or an explicitly published SDK. The prototype passes the gate to production only if all five checks succeed:

1. **Ownership:** Codex, Input, and the broker cannot issue unordered competing writes.
2. **Layer isolation:** Herdr colors never alter Layer 1; Codex colors never leak into Layer 2.
3. **Recovery:** crash, force-kill, sleep, cable pull, BLE drop, and Herdr restart cannot leave a believable stale status beyond the agreed TTL.
4. **Transport:** wired and Bluetooth behavior is explicit and correctly reported.
5. **Upgrade:** a Codex, Input, Herdr, or firmware update fails closed and reports incompatibility.

Until then, the supported deployable solution is:

```text
Codex Micro Layer 2
  └─ Work Louder Input HID chords
       └─ Herdr + per-agent control adapters
            ├─ Herdr sidebar/metadata and notifications for reliable status
            └─ static Layer 2 keyboard lighting
```

That delivers the controls now without making unsupported RGB a hidden single point of failure.

## Reproducibility notes

Local artifact claims were checked without modifying either application:

```sh
defaults read /Applications/Codex.app/Contents/Info CFBundleShortVersionString
defaults read /Applications/input.app/Contents/Info CFBundleShortVersionString
codesign -d --entitlements :- /Applications/Codex.app
codesign -d --entitlements :- /Applications/input.app
npx @electron/asar list /Applications/Codex.app/Contents/Resources/app.asar
npx @electron/asar list /Applications/input.app/Contents/Resources/app.asar
npm view @worklouder/device-kit-oai
npm view @worklouder/wl-device-kit
```

The inspection intentionally did not connect a new client, send an RPC, change lighting, modify firmware, or reset device state.
