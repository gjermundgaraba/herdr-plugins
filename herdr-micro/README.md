# herdr-micro

Unofficial macOS [Herdr](https://herdr.dev/) plugin for the Work Louder Codex
Micro. It shows Herdr agent state on the six Agent keys and routes the keys,
dial, and joystick to the focused Codex, Claude Code, or Pi agent.

## Requirements

- macOS and a Codex Micro
- [Herdr](https://herdr.dev/docs/install/) socket protocol 20
- Herdr `[experimental].kitty_graphics = true` for exact pane scrolling
- Ghostty 1.3 or newer
- Rust 1.89 or newer when building from source
- Work Louder Input for the one-time Layer 2 setup
- Administrator access for the one-time privileged USB-helper installation
- macOS Automation permission for Ghostty inspection
- macOS Accessibility permission for configured `key` bindings
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
install -m 750 ../target/release/herdr-micro bin/.herdr-micro.new
mv -f bin/.herdr-micro.new bin/herdr-micro
install -m 750 ../target/release/herdr-micro-hid bin/.herdr-micro-hid.new
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

   `config.json` contains controls, effort shortcuts, and lighting. Runtime files
use the plugin state directory: `run/micro.sock`, `logs/micro.log`, and
`data/backups/` for verified keymap backups. The daemon keeps three 10 MiB log
files.

Run **Configure Herdr Micro** in Herdr to open `config.json`. Binding
changes are validated and reloaded while the bridge runs. `buttons` maps the
seven action switches to bindings; `null` disables a switch. Each bound switch
uses a fixed internal HID code that macOS maps to no virtual keycode, so an
uncaptured Micro cannot type anything. Agent presses focus their slot directly
through Herdr. The only way a button reaches macOS is an explicit `key`
binding, which taps a configured key system-wide from any frontmost app while
the bridge owns the device (useful for app hotkeys such as dictation):

```json
"5": { "action": "key", "key": "F19" }
```

Names cover F13–F20; `keycode` accepts any macOS virtual keycode (0–127)
instead of `key`, and optional `modifiers` adds any of `cmd`, `shift`, `alt`,
`ctrl`, and `fn`.
The stock wide keycap spans switches 5 and 6, so `controls.buttons["6"]`
stays `null` by default. The three top-level fields—`controls`, `effort`, and
`lighting`—are required.

The privileged helper captures the USB-connected Micro so ChatGPT cannot also
receive Layer 2 events. Binding a previously unbound switch (or the reverse)
alters the device keymap: stop the bridge, run **Set up Micro Layer 2** again,
then restart it. Every changed keymap is backed up and verified before setup
succeeds.

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

Match those shortcuts in `config.json` under `effort`; it uses `+`, not
Codex's `-` syntax:

```json
{"effort":{"codex":{"raise":"ctrl+shift+t","lower":"ctrl+t"}}}
```

Claude Code uses its native `/effort` picker. See [effort control](docs/micro-bridge.md#thinking-effort-control)
for operational boundaries.

## Routing and operation

One bridge serves the default and named Herdr sessions. Native ScriptingBridge
queries map each running session to its Ghostty terminal UUID. Herdr snapshots
and subscriptions drive routing and lighting, and actions use direct socket
requests to the focused mapped session.

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
normal macOS HID driver when the bridge yields to ChatGPT/Codex. The user
daemon blanks the LEDs and stops after 60 seconds without a running Herdr
session; the disconnected helper restores native HID ownership and exits.
launchd starts a fresh helper for the next device lease.

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

Apache-2.0. See [`../LICENSE`](../LICENSE) and
[third-party notices](THIRD_PARTY_NOTICES.md).
