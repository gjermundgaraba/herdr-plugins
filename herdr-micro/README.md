# herdr-micro

Unofficial macOS [Herdr](https://herdr.dev/) plugin for the Work Louder Codex
Micro. It shows Herdr agent state on the six Agent keys and routes the keys,
dial, and joystick to the focused Codex, Claude Code, or Pi agent.

## Requirements

- macOS and a Codex Micro
- [Herdr](https://herdr.dev/docs/install/) 0.7.5 or newer
- Ghostty 1.3 or newer
- Rust 1.71 or newer when building from source
- Work Louder Input for the one-time Layer 2 setup
- macOS Input Monitoring permission for direct HID access
- macOS Automation permission for Ghostty inspection
- macOS Accessibility permission only for the optional `scroll` action
- [Hunk](https://www.hunk.dev/) only for the optional `diff` action

The bridge uses an unsupported proprietary device protocol. See the
[compatibility and safety notes](docs/micro-bridge.md) before setup.

## Install

```sh
herdr plugin install gjermundgaraba/herdr-plugins/herdr-micro
herdr plugin enable gjermundgaraba.herdr-micro
```

For local development:

```sh
git clone https://github.com/gjermundgaraba/herdr-plugins.git
cd herdr-plugins/herdr-micro
cargo build --release --locked
mkdir -p bin
install -m 750 target/release/herdr-micro bin/.herdr-micro.new
mv -f bin/.herdr-micro.new bin/herdr-micro
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

   - `controls.json`: HID keys, seven action switches, dial, joystick, gestures, and per-agent actions
   - `lighting.json`: state colors/effects and aggregate lighting zones
   - `effort.json`: user-configured Codex effort shortcuts

Run **Configure Micro controls** in Herdr to open `controls.json`. Action
changes are validated and reloaded while the bridge runs. `hidKeys` maps the
seven action switches to unique `F13` through `F24` keys, `null` disables a
switch, and an omitted switch retains its stock OAI code. It is empty by
default. For example, the stock wide keycap spans action switches 5 and 6, so
this emits one F19:

```json
"hidKeys": { "5": "F19", "6": null }
```

These are ordinary system-wide keyboard keys, so select keys that are not bound
by macOS or another application. HID-key changes are held pending by the running
bridge because they also alter the device keymap. Stop the bridge, run **Set up
Micro Layer 2** again, then restart the bridge. Every changed keymap is backed
up and verified before setup succeeds.

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

Quit Work Louder Input while the bridge runs. Other Input Monitoring clients
can also contend with direct HID access. Only one process may own the vendor
HID interface. The bridge blanks the LEDs on controlled shutdown and stops
after 60 seconds without a running Herdr session.

Herdr v1 has no plugin teardown hook. Run `micro-stop` before disabling,
uninstalling, unlinking, or updating the plugin. The next `micro-start`
replaces a daemon from a different plugin version.

Current architecture, compatibility, ownership, and limitations are in
[the bridge guide](docs/micro-bridge.md). Durable hardware and version evidence
is indexed in [the research record](docs/research/README.md).

## License

MIT. See [third-party notices](THIRD_PARTY_NOTICES.md).
