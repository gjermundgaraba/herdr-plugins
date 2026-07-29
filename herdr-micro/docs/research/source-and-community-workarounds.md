# Codex/Creator Micro source and community workarounds

**Research date:** 2026-07-25

**Scope:** actual source and artifacts available for Creator Micro 2 / Codex Micro Agent Mode, the private Work Louder host integration, and community routes that could supply Herdr or other-agent controls/status.

> **Historical source review:** `house-of-herdr` informed the implementation,
> but the active solution is the separately built `herdr-micro` plugin.

## Bottom line

1. **There is no public first-party Agent Mode SDK or firmware source.** Work Louder publishes firmware and application binaries. The only public Work Louder SDK alpha is for Nomad v1.
2. **The installed Codex app contains a complete, inspectable host integration.** It includes compiled JavaScript, TypeScript declarations, and a README for `@worklouder/device-kit-oai`, including discovery, input events, and RGB methods. This is an artifact, not public source: both Work Louder packages are private, `UNLICENSED`, and marked proprietary/confidential.
3. **The wire behavior has been independently reproduced.** Several community projects implement the vendor HID framing and `v.oai.*` messages. This proves technical feasibility; it does not turn the private package or protocol into a supported API.
4. **An end-to-end Herdr integration exists.** The `house-of-herdr` author demonstrated live Herdr Agent Key status, focus, dial, joystick, and command controls on physical hardware. Its source and tests were reproduced here and used as a reference for the independently implemented `herdr-micro` plugin. [`FreeMicro`](https://github.com/eliBenven/freemicro/tree/1e78198c1b4bfe43b7e4aee3246c73314b9bcc0f) and [`codexpad`](https://github.com/shahcolate/codexpad) provide additional, more extensively documented physical-device implementations for Claude Code.
5. **A Layer 2 bypass is hardware-verified.** Copying Layer 1's `KV_OAI_*` layout into Layer 2 makes firmware expose the six per-Agent LEDs and vendor events there. [Pejman's procedure and versioned test](https://gist.github.com/pejmanjohn/d8f1fb99698c1599a533e65514e24469) and an [independent Layer 2 implementation](https://github.com/duolahypercho/claude-micro-layer/commit/3597273f98f5b458f9ab70d4a5ab2b7bcc212d10) agree. The exact Herdr combination is also physically verified on this keyboard through `herdr-micro`, with the bridge as sole device writer.

## Evidence classes

- **Public first-party source:** source published by Work Louder, OpenAI, QMK, VIA, or Herdr under an identifiable public repository/license.
- **First-party binary artifact:** code, declarations, metadata, or strings reproducibly extracted from installed vendor applications or released firmware. This establishes implementation facts, not reuse rights or support.
- **Community source:** independently published code. Its own license and validation status apply; Work Louder/OpenAI do not support it.
- **Inference:** a conclusion from those facts. It is explicitly labeled and must not be presented as an API guarantee.

## 1. What first-party source is actually public

| Surface | What is public | What is missing |
|---|---|---|
| Creator Micro 2 firmware | A release repository containing a one-line README and downloadable merged `.bin` files. The Codex-capable release is prerelease `v0.6.0-rc.6`. [Firmware repository](https://github.com/worklouder/cm-v2-fw-releases), [v0.6.0-rc.6](https://github.com/worklouder/cm-v2-fw-releases/releases/tag/v0.6.0-rc.6) | Firmware source, schematics, GPIO/pin map, protocol specification, build instructions, and a third-party license. |
| Work Louder Input | Installer release assets and changelogs. [Input releases](https://github.com/worklouder/input-releases) | Original Input application source and a public current-device SDK. |
| Work Louder SDK alpha | A public alpha release whose notes say **“ONLY FOR NOMAD v1.”** [SDK alpha](https://github.com/worklouder/input-releases-internal/releases/tag/sdk-alpha-0.1) | Creator Micro 2 and Codex Micro support. |
| OpenAI Codex CLI | The CLI/TUI and app-server source in [`openai/codex`](https://github.com/openai/codex) | The current Codex desktop Electron application and Work Louder hardware bridge are not published there as reusable source. |
| Herdr | CLI/socket/plugin source and documentation. [Herdr repository](https://github.com/ogulcancelik/herdr), [plugin documentation](https://herdr.dev/docs/plugins/) | A first-party Codex Micro plugin or hardware driver. |

Work Louder’s [`input-linux`](https://github.com/worklouder/input-linux) repository is not the missing source tree. Its own README calls it an unofficial, unsupported community port that downloads/extracts the Windows Input installer and applies Linux patches. It is useful evidence that the packaged app can be adapted, but not an original public Input SDK or license grant.

### The QMK/VIA source is for the wrong device

Public QMK and VIA definitions do exist for the older Creator Micro:

- [QMK `work_louder/micro`](https://github.com/qmk/qmk_firmware/tree/master/keyboards/work_louder/micro)
- [VIA Creator Micro definition](https://github.com/the-via/keyboards/blob/master/v3/work_louder/micro.json)

They identify a 16-position, two-encoder **ATmega32U4** board with VID:PID `0x574C:0xE6E3`. Creator Micro 2 and Codex Micro are 13-switch, one-encoder plus joystick **ESP32-S3** devices with different USB identities. The old matrix pins, QMK keymap, VIA JSON, and firmware must not be applied to either current device.

**Conclusion:** no current-device QMK/VIA/Vial path was found. Public source for the old Creator Micro is a false lead, not a starting firmware tree.

## 2. Reproducible first-party artifacts

### Installed applications

The machine inspected on 2026-07-25 contains:

| Artifact | Version | SHA-256 |
|---|---:|---|
| `/Applications/Codex.app/Contents/Resources/app.asar` | `26.721.41059` | `da39a51b06fb4c728d418b8f0f05fc8fd8c6b1f74c4fb4d47c20c7914a798f45` |
| `/Applications/input.app/Contents/Resources/app.asar` | `0.17.2` | `5ffc6ed367e8b823e516e3011e22fee256b820df6092b9c2f070f8ec4acf37cf` |
| `~/Library/Application Support/input/devices/33632/keymap.json` | received Codex Micro map | `c7930d448b78afb84e89f69a26f9a32c9ee088f3b6737b25e0ebfa2fc7648206` |

The keymap is a readable local cache and useful backup/evidence. It is not a documented hand-editing API; Input can overwrite it and also writes the device-side map.

### Private Work Louder packages in Codex

Extracting the Codex ASAR reveals:

```text
node_modules/@worklouder/device-kit-oai/
  package.json
  README.md
  dist/*.js
  dist/*.d.ts
  node_modules/@worklouder/wl-device-kit/

.vite/build/codex-micro-service-CY8ASf0t.js
webview/assets/codex-micro-*.js
```

Observed package metadata:

| Package | Version | Declared status |
|---|---:|---|
| `@worklouder/device-kit-oai` | `0.1.11` | `license: "UNLICENSED"`; README says private GitHub Package; files say proprietary/confidential. |
| `@worklouder/wl-device-kit` | `0.1.23` | Same private/`UNLICENSED` status. |

No `.map` files or original TypeScript source are shipped for `device-kit-oai`; the artifact contains readable compiled JavaScript and declaration files. Its README documents installation from GitHub Packages after authentication, but a token scope alone does not grant package access. Work Louder must authorize the account.

### The interface that exists inside the private package

The declarations and compiled implementation reproduce these methods:

```ts
class RPCApiOAI {
  onHidReceived(callback): () => void
  onJoystickMove(callback): () => void
  getFirmwareVersion(): Promise<string>
  getDeviceStatus(): Promise<WLDeviceStatus>
  sendLightingPreview(config): Promise<void>
  sendThreadsLighting(threads): Promise<boolean>
  sendLightingConfig(config): Promise<boolean>
}
```

The Codex host service uses that package to:

- recognize Codex Micro PID `0x8360` and Creator Micro 2 PIDs `0x8297`/`0x8298`;
- select vendor HID usage page `0xFF00`;
- subscribe to `v.oai.hid` and `v.oai.rad`;
- write six Agent Key slots through `v.oai.thstatus`; and
- write ambient/key lighting through `v.oai.rgbcfg`.

These facts are reproducible from the installed artifact and the public product behavior. They do **not** establish that `RPCApiOAI` is a public or redistributable API. [OpenAI’s product page](https://openai.com/supply/co-lab/work-louder/) describes RGB as coming from Codex; [Work Louder’s firmware notes](https://github.com/worklouder/cm-v2-fw-releases/releases/tag/v0.6.0-rc.6) confirm Codex status lighting and layer gating.

### Firmware binary

The official `firmware_v0.6.0-rc.6_merged.bin` has SHA-256:

```text
05d8d8ad6ecfb34fa20a62f6b1822e3d9b8b50a569f78af56b5200703c4066dc
```

A plain `strings` scan reproduces:

```text
v.oai.hid
v.oai.rad
v.oai.rgbcfg
v.oai.thstatus
OAI BRIDGE: init, v.oai.thstatus registered on all variants
/fs/keymap.json
```

**Artifact fact:** the OAI bridge is present in the released firmware binary and the string says it registers across variants.

**Not established:** how Creator Micro 2’s Codex-enabled layer is provisioned, whether every hardware/firmware combination behaves identically, or that these method names are a stable external contract.

## 3. Why “private package found” is not “SDK available”

The package is technically callable, but four blockers remain:

1. **Access:** its own README says it is private and requires authenticated GitHub Packages access.
2. **License:** both Work Louder packages declare `UNLICENSED`; source headers prohibit unauthorized copying.
3. **Compatibility:** native HID/serial dependencies are built for the Electron runtime shipped with the app, not promised for arbitrary Node versions.
4. **Ownership:** the observed protocol has no public lease/arbitration mechanism. Input 0.17.2 specifically fixed interference with Codex, and Work Louder warns that other Input Monitoring applications can interfere. [Input 0.17.2](https://github.com/worklouder/input-releases/releases/tag/v0.17.2), [setup warning](https://worklouder.cc/openai-micro-setup)

**Supported next step:** ask Work Louder for SDK/package access, a usable license, firmware compatibility guarantees, and explicit multi-client ownership rules.

**Unsupported shortcut:** copying the package out of Codex and shipping it in a Herdr bridge. The installed bytes are evidence, not permission.

## 4. Community implementations

All projects below are independent, unofficial, and very new. “Hardware verified” means the project author reports testing their own unit; this research did not independently exercise the keyboard.

### Best current candidates

| Project | What it actually provides | Reproduced here | Assessment |
|---|---|---|---|
| [`alasano/house-of-herdr`](https://github.com/alasano/house-of-herdr/tree/7d8eadaed41a1bb4456565d6bcba8cdb7380b77e/packages/codex-micro) | MIT macOS Herdr plugin: six Agent Key LEDs, agent focus, dial/joystick/command mappings, direct vendor HID. | Source build and 64 unit tests pass. No hardware run. | Closest exact Herdr match; use as the Herdr/control-layer reference. |
| [`eliBenven/FreeMicro`](https://github.com/eliBenven/freemicro/tree/1e78198c1b4bfe43b7e4aee3246c73314b9bcc0f) | MIT macOS daemon for Claude Code; reads all vendor inputs and drives all RGB over USB and BLE; stable project/TTY slots, focus, daemon, diagnostics, and source protocol implementation. | One local run: 1,170 tests pass. A parallel run had 1,169 pass and one subprocess timing-threshold failure. No hardware run. | Strongest macOS device-layer candidate. No Herdr adapter and PID `0x8360` only. |
| [`shahcolate/codexpad`](https://github.com/shahcolate/codexpad) | MIT Claude Code daemon/UI; hardware-tested keys/RGB, command bindings, automatic official-app handoff, Unix socket, and a generic MCP server with `pad_status`, `pad_set`, `pad_session`, ring, rainbow, and off tools. | Source/interface inspected. No included hardware or automated test was run here. | Best ready-made generic control surface. Herdr could target its socket/MCP layer, but no Herdr adapter exists. |
| [`PlaneshiftDev/microd`](https://github.com/PlaneshiftDev/microd/tree/7365e4578a3292ae6952c8bfc1621275703a7d9c) | Rust Codex Micro daemon, direct Herdr bridge, gestures, and macOS tray ownership switch. | Workspace test run: 13 tests pass. No hardware run. | Broadest prebuilt Herdr architecture, but only two commits, no release/CI record, and unclear repository-level license. |
| [`boopdotpng/work-louder-oai`](https://github.com/boopdotpng/work-louder-oai/tree/25e154146c343b9c58cb70943a29bfb6c719dde6) | MIT dependency-free Linux daemon/CLI, event socket, full input and RGB zones. | Source inspected; no hardware run. Its [coverage record](https://github.com/boopdotpng/work-louder-oai/blob/25e154146c343b9c58cb70943a29bfb6c719dde6/docs/COVERAGE.md) reports USB verification on firmware 0.4.1. | Strong clean-room protocol reference; Linux only and no Herdr adapter. |

### Partial or not ready

- [`DevVig/microbridge`](https://github.com/DevVig/microbridge/tree/fcd0aba7a4fa360ca8690681bd7b049fa3682f10) is a promising MIT cross-agent daemon/UI, but its own [HID status document](https://github.com/DevVig/microbridge/blob/fcd0aba7a4fa360ca8690681bd7b049fa3682f10/docs/device-hid.md) says physical validation, mapping, effects, and ownership still remain.
- [`jal-co/pi-codex-micro`](https://github.com/jal-co/pi-codex-micro/tree/1bdd74119a205bbfbde45ff71348f36295cdf899) has useful Pi keyboard/session logic and a simulator, but its physical LED transport is explicitly a scaffold/mock.

### Social search

ClankerSearch was used across X, Reddit, Bluesky, and Threads with exact product, protocol, RGB, Herdr, Claude, and Pi terms. GitHub repository searches were then used to validate claims against source instead of treating demos as proof.

Useful social findings:

- [`@aljosa` launched the direct Herdr plugin](https://x.com/aljosa/status/2080765081001074906), then documented the Layer 1-only behavior he had observed. Later Layer 2 tests identified the missing condition: the active layer must contain the private OAI keycodes. `house-of-herdr` and `microd` are the two exact Herdr bridges found.
- [Work Louder replied that it was forwarding the limitation internally](https://x.com/work_louder/status/2080794523899183179). No API, firmware change, or delivery date was promised.
- [`@viccsmind` reports Codex Agent Keys, live status LEDs, push-to-talk, and actions working on an older Work Louder Micro](https://x.com/i/web/status/2080001548277715060). This supports general hackability, but it is Codex-only and not Creator Micro 2 validation.
- [FreeMicro's Reddit launch](https://www.reddit.com/r/codex/comments/1v5hxnn/reverse_engineered_the_codex_micro_keypad_and/) points to the same source and physical-device claims checked above.

No useful independent solution was found on Bluesky or Threads. Most other Reddit results were substitute DIY controllers, not integrations for the shipping Codex Micro. These projects are only days old, so no mature third-party reliability reports were found.

### FreeMicro evidence and limits

FreeMicro is the strongest published macOS implementation of the physical protocol found in this sweep:

- author-reported verification on one shipping Codex Micro with firmware 0.4.1;
- input and lighting over both USB and Bluetooth LE;
- captured HID descriptors and a machine-readable hardware probe;
- direct IOKit access on macOS rather than assuming `hidapi.open_path()` works; and
- opt-in lighting, with `--coexist` limiting its writes to the ordinary key-backlight zone.

Its [protocol document](https://github.com/eliBenven/freemicro/blob/1e78198c1b4bfe43b7e4aee3246c73314b9bcc0f/docs/PROTOCOL.md) and [capability record](https://github.com/eliBenven/freemicro/blob/1e78198c1b4bfe43b7e4aee3246c73314b9bcc0f/hardware/capabilities.json) reproduce VID:PID `0x303A:0x8360`, usage page `0xFF00`, report ID 6, USB/BLE framing, input notifications, and RGB methods.

Two cautions matter:

1. The protocol document contains a stale contradictory block later in the same file: its current top-level findings and implementation use `v.oai.rgbcfg`, while the old block says `lights.preview`. The source, defaults, and capability record consistently use `rgbcfg`; treat the prose defect as an early-project maturity warning.
2. `--coexist` avoids the Agent Key LEDs rather than sharing them. It therefore cannot supply the requested cross-agent status lights while Codex owns those LEDs.

There is no Herdr reference in FreeMicro’s current source. Adapting its MIT device layer to consume Herdr state/events is a plausible clean implementation, but that combined route has not been built or validated.

### Replacement firmware: physical progress, still not ready

[`SilkePilon/OpenMicro`](https://github.com/SilkePilon/OpenMicro/tree/a030fe5309e3f925e33c62b123f50b7f663ed7ca) explores replacement ESP32-S3 firmware and a Rust multi-agent host stack. Its README and `main.rs` still say the firmware has never been flashed, but that wording is stale relative to the repository’s later [hardware bring-up log](https://github.com/SilkePilon/OpenMicro/blob/a030fe5309e3f925e33c62b123f50b7f663ed7ca/docs/hardware/creator-micro-2-pinout-findings.md): the author reports flashing a real Creator Micro 2, correcting the GPIO36/37/38 power-gate polarity, visibly lighting both chains, and confirming GRB order with camera measurements.

That successful LED bring-up does not make OpenMicro a usable replacement:

- BLE, matrix scanning, key-coordinate mapping, and battery I²C remain incomplete or unverified;
- the host daemon still defaults to a mock transport;
- the Codex Micro/Creator Micro 2 shared-PCB assumption is not proven; and
- its backup instructions say 4 MiB even though the analyzed vendor image declares 16 MiB flash and partitions beyond 4 MiB.

Do not flash OpenMicro as the integration workaround. It is valuable hardware research and a future clean-firmware path, not a recoverability-tested release.

## 5. Focused assessment: `house-of-herdr`

This plugin matches the requested outcome more closely than a new implementation:

- subscribes to Herdr agent state;
- maps `blocked`, `done`, `working`, `idle`, and `unknown` to six physical LEDs;
- focuses the corresponding agent when a slot key is pressed;
- routes other physical controls through Herdr’s socket API;
- uses only Codex Micro PID `0x8360`; and
- yields/reclaims the device using a host-process watcher.

Its manifest runs `npm install`, `npm run build`, and a detached Node daemon. Herdr plugins are ordinary unsandboxed executables; Herdr explicitly tells users to review install commands and source and supports pinning a Git ref. [Herdr plugin security model](https://herdr.dev/docs/plugins/)

### Reproduced source checks

At commit `7d8eadaed41a1bb4456565d6bcba8cdb7380b77e`:

```text
6 test files passed
64 tests passed
TypeScript build passed
```

The local verification installed dependencies with scripts disabled, so it did not execute or validate `node-hid`’s native install step and did not touch the keyboard.

### Material caveats

1. **No reproduced hardware run.** Unit tests cover framing, config, controls, slots, and text; they cannot prove macOS permission, native HID, firmware, or LED behavior.
2. **Current app-name mismatch.** [`src/chatgpt.ts`](https://github.com/alasano/house-of-herdr/blob/7d8eadaed41a1bb4456565d6bcba8cdb7380b77e/packages/codex-micro/src/chatgpt.ts) only detects a process path containing `ChatGPT.app/Contents/MacOS/ChatGPT`. This machine’s supported host is `/Applications/Codex.app`. Automatic yielding will not detect that process unless the plugin is updated.
3. **Ownership is heuristic.** The plugin polls for one application name; it does not negotiate a device lease with Codex or Input. Multiple readers can duplicate actions and multiple RGB writers can overwrite each other.
4. **Codex Micro only.** [`src/device.ts`](https://github.com/alasano/house-of-herdr/blob/7d8eadaed41a1bb4456565d6bcba8cdb7380b77e/packages/codex-micro/src/device.ts) hard-codes PID `0x8360`; it does not currently target Creator Micro 2 PIDs.
5. **Layer 2 needs a separate configuration step.** The plugin directly drives the Codex vendor interface/Agent LEDs but does not install OAI keycodes into Layer 2. Clone the OAI layout through Input first; keep `KV_OAI_AG00`–`AG05` on the six status keys.

### Safest experiment shape

Do not install it blindly or run it beside the official writers. If testing is desired:

1. Review/pin the exact source revision; do not use `-y` on first install.
2. Patch/confirm `Codex.app` ownership detection upstream.
3. Quit Codex and Work Louder Input before starting the plugin.
4. Use wired USB and grant Input Monitoring only to the exact terminal/Herdr host.
5. Stop/uninstall the plugin and reconnect the keyboard if input or lighting becomes inconsistent.

Candidate pinned install command, **not executed during this research**:

```sh
herdr plugin install \
  --ref 7d8eadaed41a1bb4456565d6bcba8cdb7380b77e \
  alasano/house-of-herdr/packages/codex-micro
```

## 6. Recommended paths

### Path A — dependable now

Use ordinary Input mappings on Codex Micro Layer 2:

```text
Layer 2 HID chord → Herdr binding/dispatcher → exact pane/agent adapter
```

Keep Agent Key RGB and the reserved vendor layer owned by Codex. This reproduces navigation, effort/model controls, approve/deny/interrupt, skills, and prompts without relying on the private protocol.

### Path B — smallest experimental Herdr RGB

Evaluate `house-of-herdr` in single-owner USB mode after fixing its `Codex.app` detector. It already contains the Herdr state/slot/control logic. Treat it as a prototype: pin the commit, close Codex/Input, avoid persistent filesystem/keymap RPCs, and expect firmware updates to break it.

### Path C — better community implementation

Keep Herdr as the agent/session source, but replace or harden `house-of-herdr`’s device layer with one of the more thoroughly exercised MIT implementations:

```text
Herdr socket/plugin events
  → stable six-slot state model
  → FreeMicro IOKit transport OR codexpad local socket
  → v.oai.thstatus / v.oai.rgbcfg
```

- **FreeMicro route:** best if USB + BLE and direct macOS device access matter. Add a Herdr event adapter and generalize the current Claude-specific focus/session resolver.
- **codexpad route:** best if a local daemon, generic MCP tools, command bindings, and automatic handoff are useful. Add a Herdr-to-`pad_session`/socket bridge.

This is the most credible unsupported route to a high-feature result, but it requires code and hardware validation. Do not mix source from the proprietary Work Louder packages into the community implementation.

### Path D — supported RGB eventually

Ask Work Louder for:

- authorized access to `@worklouder/device-kit-oai`;
- an explicit license for a Herdr/third-party integration;
- supported Creator Micro 2 and Codex Micro product IDs/firmware matrix;
- device ownership/coexistence semantics; and
- permission and guidance for publishing a Herdr adapter.

This is the only route that turns the observed private interface into a supported integration.

## 7. Reproduction commands

The following commands are read-only except for temporary extraction/download directories.

### Verify installed application artifacts

```sh
plutil -p /Applications/Codex.app/Contents/Info.plist |
  rg 'CFBundleShortVersionString|CFBundleVersion'
plutil -p /Applications/input.app/Contents/Info.plist |
  rg 'CFBundleShortVersionString|CFBundleVersion'

shasum -a 256 \
  /Applications/Codex.app/Contents/Resources/app.asar \
  /Applications/input.app/Contents/Resources/app.asar
```

### Extract and inspect Codex

Use a pinned ASAR tool and a new temporary directory:

```sh
artifact_dir="$(mktemp -d)"
npx --yes @electron/asar@3.4.1 extract \
  /Applications/Codex.app/Contents/Resources/app.asar \
  "$artifact_dir/codex"

jq '{name,version,license}' \
  "$artifact_dir/codex/node_modules/@worklouder/device-kit-oai/package.json"

rg -n \
  'sendThreadsLighting|sendLightingConfig|v\\.oai\\.|Proprietary|UNLICENSED' \
  "$artifact_dir/codex/node_modules/@worklouder/device-kit-oai" \
  "$artifact_dir/codex/.vite/build"
```

`npx` executes a downloaded package; verify/pin it according to local supply-chain policy before reproducing.

### Verify the public firmware binary

```sh
firmware_dir="$(mktemp -d)"
curl -fL \
  'https://github.com/worklouder/cm-v2-fw-releases/releases/download/v0.6.0-rc.6/firmware_v0.6.0-rc.6_merged.bin' \
  -o "$firmware_dir/firmware.bin"

shasum -a 256 "$firmware_dir/firmware.bin"
strings -a "$firmware_dir/firmware.bin" |
  rg 'v\\.oai|OAI BRIDGE|/fs/keymap.json'
```

### Reproduce the community plugin’s source-only checks

```sh
review_dir="$(mktemp -d)"
git clone https://github.com/alasano/house-of-herdr.git \
  "$review_dir/house-of-herdr"
git -C "$review_dir/house-of-herdr" checkout \
  7d8eadaed41a1bb4456565d6bcba8cdb7380b77e

cd "$review_dir/house-of-herdr"
pnpm install --ignore-scripts
pnpm --filter @house-of-herdr/codex-micro test
pnpm --filter @house-of-herdr/codex-micro build
```

This intentionally skips native install scripts and therefore validates source/tests only, not physical HID operation.

### Reproduce FreeMicro’s software checks

```sh
review_dir="$(mktemp -d)"
git clone https://github.com/eliBenven/freemicro.git \
  "$review_dir/freemicro"
git -C "$review_dir/freemicro" checkout \
  1e78198c1b4bfe43b7e4aee3246c73314b9bcc0f

cd "$review_dir/freemicro"
uv run --with pytest pytest -q
```

One local run produced `1170 passed`; a separate run produced `1169 passed` and one 3.3-second subprocess timing-threshold failure. It builds the package in an isolated temporary environment but does not install a daemon, change Claude hooks, request macOS permissions, or touch the keyboard.

## 8. Final fact/inference boundary

### Established

- First-party binaries implement a real bidirectional vendor HID Agent Mode.
- The installed private package exposes the needed input/status/RGB methods internally.
- The released firmware contains the corresponding OAI methods.
- Independent MIT community source implements the protocol for macOS and Linux.
- A Herdr-specific community plugin now exists.
- Community authors report real-hardware input/RGB verification for FreeMicro, codexpad, `work-louder-oai`, and `microd`.
- OpenMicro’s current tree contains a real Creator Micro 2 LED bring-up record, despite stale “never flashed” text elsewhere in the same revision.

### Not established

- A public or redistributable Work Louder API.
- Safe simultaneous Codex/Input/community RGB ownership.
- A current verified Creator Micro 2 community implementation.
- Hardware validation of the inspected `house-of-herdr` revision on this keyboard.
- An existing tested bridge that combines Herdr with FreeMicro or codexpad.
- Stable compatibility across firmware/app updates or Bluetooth.

**Historical decision:** prove the OAI-enabled layout on disposable Layer 3,
then put it on Layer 2 with a sole-owner bridge. That sequence was completed
with the local `herdr-micro` plugin. The rollback, licensing, and no-firmware
warnings still apply.
