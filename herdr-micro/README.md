# herdr-micro

Unofficial macOS [Herdr](https://herdr.dev/) plugin for the Work Louder Codex
Micro. It shows Herdr agent state on the six Agent keys and routes the keys,
dial, and joystick to the focused Codex, Claude Code, or Pi agent.

## Requirements

- macOS and a Codex Micro
- [Herdr](https://herdr.dev/docs/install/) 0.7.5 or newer
- Ghostty 1.3 or newer
- Rust 1.85 or newer when building from source
- Work Louder Input for the one-time Layer 2 setup
- Administrator access for the one-time privileged USB-helper installation
- macOS Automation permission for Ghostty inspection
- macOS Accessibility permission for configured F-key output and optional scrolling
- [Hunk](https://www.hunk.dev/) only for the optional `diff` action

The bridge uses an unsupported proprietary device protocol. See the
[compatibility and safety notes](docs/micro-bridge.md) before setup.

## Install

```sh
herdr plugin install gjermundgaraba/herdr-plugins/herdr-micro
HERDR_MICRO_ROOT="$(herdr plugin list --plugin gjermundgaraba.herdr-micro --json | plutil -extract result.plugins.0.plugin_root raw -o - -)"
sudo "$HERDR_MICRO_ROOT/bin/herdr-micro" install-helper
herdr plugin enable gjermundgaraba.herdr-micro
```

The explicit `sudo` step installs a root-owned, USB-only launchd helper. While
Herdr owns the Micro, the helper captures its USB interface so macOS and
ChatGPT cannot receive the same Agent-key reports.

For local development:

```sh
git clone https://github.com/gjermundgaraba/herdr-plugins.git
cd herdr-plugins/herdr-micro
cargo build --release --locked
mkdir -p bin
install -m 750 target/release/herdr-micro bin/.herdr-micro.new
mv -f bin/.herdr-micro.new bin/herdr-micro
install -m 750 target/release/herdr-micro-hid bin/.herdr-micro-hid.new
mv -f bin/.herdr-micro-hid.new bin/herdr-micro-hid
sudo ./bin/herdr-micro install-helper
herdr plugin link . --enabled
```

## Set up the Micro

1. In Work Louder Input, create a blank Layer 2. Connect by USB, then quit
   Input and the Codex desktop app.

2. Clone the device's OAI controls into Layer 2. The guarded setup accepts only
   a blank or previously managed layer, backs up the keymap, and verifies the
   write.

   ```sh
   herdr plugin action invoke micro-setup --plugin gjermundgaraba.herdr-micro
   ```

3. Start and check the bridge.

   ```sh
   herdr plugin action invoke micro-start --plugin gjermundgaraba.herdr-micro
   herdr plugin action invoke doctor --plugin gjermundgaraba.herdr-micro
   ```

4. Find the live configuration directory.

   ```sh
   herdr plugin config-dir gjermundgaraba.herdr-micro
   ```

   - `controls.json`: Agent-key output, action-key mappings, dial, joystick, gestures, and per-agent actions
   - `lighting.json`: state colors/effects and aggregate lighting zones
   - `effort.json`: user-configured Codex effort shortcuts

Run **Configure Micro controls** in Herdr to open `controls.json`. Action
changes are validated and reloaded while the bridge runs. `agentMacosKeys` maps
the six independently lit OAI Agent keys to macOS F13–F20 events. `actionDeviceKeys`
maps the seven action switches to on-device F13–F24 codes; `null` disables an
action switch. `actionMacosKeys` explicitly chooses which action switches also emit
macOS F13–F20 events. Synthetic macOS outputs must be unique across both macOS
maps; action-device outputs must also be unique.
The stock wide keycap spans switches 5 and 6, so switch 6 is disabled:

```json
{
  "version": 1,
  "buttons": {},
  "agentMacosKeys": { "1": "F13", "2": "F14", "3": "F15", "4": "F16", "5": "F17", "6": "F18" },
  "actionDeviceKeys": { "1": "F20", "2": "F21", "3": "F22", "4": "F23", "5": "F19", "6": null, "7": "F24" },
  "actionMacosKeys": { "1": null, "2": null, "3": null, "4": null, "5": "F19", "6": null, "7": null },
  "dial": {},
  "joystick": { "engageDistance": 0.75, "releaseDistance": 0.3 }
}
```

The privileged helper captures the USB-connected Micro so ChatGPT cannot also
receive Layer 2 events. The unprivileged bridge mirrors Agent F-keys and
only the action keys selected by `actionMacosKeys` back into macOS. `agentMacosKeys`
and `actionMacosKeys` reload live. Changes to `actionDeviceKeys` alter the device keymap: stop
the bridge, run **Set up Micro Layer 2** again, then restart it. Every changed
keymap is backed up and verified before setup succeeds.

## Effort controls

Install the bundled Pi extension once, then run `/reload` in existing Pi
sessions:

```sh
herdr plugin action invoke setup-pi-effort \
  --plugin gjermundgaraba.herdr-micro
```

For Codex, add shortcuts to `~/.codex/config.toml`:

```toml
[tui.keymap.chat]
increase_reasoning_effort = "ctrl-shift-t"
decrease_reasoning_effort = "ctrl-t"
```

Match those shortcuts in the plugin's `effort.json`; it uses `+`, not Codex's
`-` syntax:

```json
{"codex":{"raise":"ctrl+shift+t","lower":"ctrl+t"}}
```

Claude Code uses its native `/effort` picker. See [effort control](docs/effort-control.md)
for operational boundaries.

## Routing and operation

One bridge serves the default and named Herdr sessions. It maps each running
session to its Ghostty terminal UUID and routes all lighting and controls only
through the focused mapped session.

- Codex desktop frontmost: Layer 1 and device ownership yielded to Codex
- Mapped Ghostty terminal frontmost: Layer 2 and the matching Herdr session
- Other app frontmost: preserve the last applicable layer; dispatch nothing

Useful actions:

```sh
herdr plugin action invoke micro-status --plugin gjermundgaraba.herdr-micro
herdr plugin action invoke micro-stop --plugin gjermundgaraba.herdr-micro
herdr plugin log list --plugin gjermundgaraba.herdr-micro --limit 20
```

Quit Work Louder Input while the bridge runs and keep the Micro connected by
USB. The helper owns the device while Layer 2 is active, then restores the
normal macOS HID driver when the bridge yields to ChatGPT/Codex. It blanks the
LEDs on controlled shutdown and stops after 60 seconds without a running Herdr
session.

Herdr v1 has no plugin teardown hook. Run `micro-stop` before updating. Ordinary
plugin updates keep using the installed helper; rerun `install-helper` only
when the helper build changes or `doctor` reports a version mismatch. Before
uninstalling or unlinking, stop the bridge and run:

```sh
sudo "$HERDR_MICRO_ROOT/bin/herdr-micro" uninstall-helper
```

Current architecture, compatibility, ownership, and limitations are in
[the bridge guide](docs/micro-bridge.md). Durable hardware and version evidence
is indexed in [the research record](docs/research/README.md).

## License

MIT. See [third-party notices](THIRD_PARTY_NOTICES.md).
