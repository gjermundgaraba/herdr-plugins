# herdr-micro

Unofficial macOS [Herdr](https://herdr.dev/) integration for the Work Louder
Codex Micro. It shows Herdr agent state on the six Agent keys and routes the
configurable controls to the focused Codex, Claude Code, or Pi agent.

The device transport is a separate per-user service. It uses the stock Codex
Micro firmware and shared macOS IOHIDManager access over USB or Bluetooth Low
Energy; USB is preferred when both are present. It does not seize USB, install
a root helper, or use `sudo`.

## Requirements

- macOS and a Codex Micro; the current hardware baseline is stock firmware
  0.6.2
- The [Herdr fork build](../README.md#herdr-build) with frontend socket
  protocol 7 (Hub is not required)
- Rust 1.89 or newer when building from source
- An Apple Development code-signing identity when building from source
- Work Louder Input for the one-time Layer 2 setup
- macOS Input Monitoring permission for the installed Codex Micro service
- macOS Accessibility permission only for configured system `key` bindings
- Handy installed at `/Applications/Handy.app` for the reserved voice-control
  button
- [Hunk](https://www.hunk.dev/) on `PATH` for the optional `diff` popup pane

The bridge uses an unsupported proprietary device protocol. Read the
[compatibility and safety notes](docs/micro-bridge.md) before setup.

## Install

Install and authorize Micro:

```sh
herdr plugin install gjermundgaraba/herdr-plugins/herdr-micro
herdr plugin enable gjermundgaraba.herdr-micro
herdr plugin action invoke service-authorize \
  --plugin gjermundgaraba.herdr-micro
```

`herdr-micro start`, including the plugin startup hook, installs or refreshes
the per-user `dev.herdr.codex-micro` LaunchAgent automatically. The stable
service executable lives under
`~/Library/Application Support/dev.herdr.codex-micro/`; grant Input Monitoring
to that installed executable when macOS prompts. On a fresh install,
`service-authorize` installs the stable service first and asks the running
service process to request access. If no entry appears, open Input Monitoring
under Privacy & Security in System Settings, click the plus button,
authenticate, and add
`~/Library/Application Support/dev.herdr.codex-micro/codex-micro`; then rerun
`service-authorize`.

The build signs the service as `dev.herdr.codex-micro`, so one Input Monitoring
grant survives later builds signed by the same Apple Development identity.
Changing the signing identity requires one new grant.

### Local development

```sh
git clone https://github.com/gjermundgaraba/herdr-plugins.git
cd herdr-plugins/herdr-micro
cargo build --release --locked \
  --package herdr-micro --bin herdr-micro \
  --package codex-micro --bin codex-micro
mkdir -p bin
install -m 750 ../target/release/herdr-micro bin/.herdr-micro.new
install -m 750 ../target/release/codex-micro bin/.codex-micro.new
/usr/bin/codesign --force --timestamp=none --sign "Apple Development" \
  --identifier dev.herdr.codex-micro bin/.codex-micro.new
mv -f bin/.herdr-micro.new bin/herdr-micro
mv -f bin/.codex-micro.new bin/codex-micro
herdr plugin link . --enabled
herdr plugin action invoke service-authorize \
  --plugin gjermundgaraba.herdr-micro
```

## Set up the Micro

1. In Work Louder Input, create a blank Layer 2, then quit Work Louder Input
   and the Codex desktop app.

2. If the Micro bridge is running, stop it before changing the keymap. On a
   fresh install nothing is running yet; skip this step.

   ```sh
   herdr plugin action invoke micro-stop --plugin gjermundgaraba.herdr-micro
   ```

3. Clone the device's OAI controls into Layer 2. Setup accepts only a blank or
   previously managed layer, backs up the keymap, and verifies the write.

   ```sh
   herdr plugin action invoke micro-setup --plugin gjermundgaraba.herdr-micro
   ```

4. Start and check the integration.

   ```sh
   herdr plugin action invoke micro-start --plugin gjermundgaraba.herdr-micro
   herdr plugin action invoke doctor --plugin gjermundgaraba.herdr-micro
   ```

## Configuration and controls

Run **Configure Herdr Micro** or locate the live configuration directory:

```sh
herdr plugin config-dir gjermundgaraba.herdr-micro
```

`config.json` contains Herdr controls and lighting. The service reserves Button
5 (`ACT10`) for Handy and consumes its press directly by running
`/Applications/Handy.app/Contents/MacOS/handy --toggle-transcription`; a Handy
installed elsewhere is not found through `PATH`. This works without Herdr and
does not synthesize F19 or any other CGEvent, so Secure Input does not block
it. Keep `controls.buttons["5"]` set to `null`.

The other buttons, dial, joystick, gestures, routing, and lighting policy stay
in the Herdr client. The stock wide keycap spans switches 5 and 6, so Button 6
is also `null` by default. Changing which non-reserved buttons are enabled
requires stopping the bridge, rerunning `micro-setup`, and restarting it.

The two top-level fields, `controls` and `lighting`, are required. Besides the
Herdr actions in the default configuration, a binding can tap a system key from
any frontmost app (requires Accessibility):

```json
"2": { "action": "key", "key": "F19" }
```

`key` names cover F13 to F20; `keycode` accepts any macOS virtual keycode (0 to 127)
instead of `key`, and optional `modifiers` adds any of `cmd`, `shift`, `alt`,
`ctrl`, and `fn`.

The actions are `prompt`, `input`, `submit`, `fast`, `focus-pane`, `script`,
and `key`; the default configuration demonstrates most of them plus `byAgent`
variants. Buttons also accept gesture bindings:

```json
"1": { "tap": { "action": "submit" }, "hold": { "action": "fast" }, "holdMs": 400 }
```

`tap`, `doubleTap`, `hold`, and `release` each take an action. `holdMs`
defaults to 500 and `doubleTapMs` to 250; both must be between 50 and 5000.
Gesture bindings are accepted on buttons and the dial press only, not on dial
rotation or joystick directions, and a `byAgent` variant cannot hold a `key`
action.

Runtime files use the plugin state directory: `run/micro.sock`,
`logs/micro.log`, and `data/backups/`. The device service writes its own log to
`~/Library/Logs/dev.herdr.codex-micro/service.log`.

The manifest also declares a `diff` popup pane that runs `hunk diff --watch`
in the current workspace. No button is bound to it by default; open it with
`herdr plugin pane open --plugin gjermundgaraba.herdr-micro --entrypoint diff`.
`doctor` warns when Hunk is missing.

## Effort controls

Install the bundled Pi extension once, then run `/reload` in existing Pi
sessions:

```sh
herdr plugin action invoke setup-pi-effort \
  --plugin gjermundgaraba.herdr-micro
```

Codex uses its native `alt+.` and `alt+,` TUI defaults; if those bindings are
overridden in `~/.codex/config.toml`, update the adapter arguments in
`config.json`. Claude Code uses the same adapter with its `/effort` picker. See
[thinking-effort control](docs/micro-bridge.md#thinking-effort-control) for the
boundaries.

## Routing and operation

The bridge discovers owner-only per-TUI frontend sockets and subscribes directly.
Exactly one TUI must report `focused: true`; absent, unknown, or conflicting
focus blanks lights and disables Herdr routing. Quiet subscriptions stay live
until EOF. The six sticky agent slots use `(endpoint_id, pane_id)` identity;
server `agent.focused` highlights the active endpoint's focused slot.

Agent buttons use `navigate`, including SSH endpoints. Ordinary keys/text use
`input`, so they work in a plain shell or an open Navigator, even when a runtime
lease is unavailable. For configuration use `{"action":"input","keys":["enter"]}`
or `{"action":"input","text":"hello"}`. Targeted agent prompts require their
captured endpoint to already be active; they never activate implicitly. Calls
carry the endpoint and the server boot whose pane ids they use, so a restarted
server rejects them instead of acting on a reused id; cancelled or disconnected
mutations are never replayed. Gesture routes are captured at the first press.
Delayed work is rejected if that TUI is no longer the uniquely focused client;
it never retargets to a different TUI.

Two bindings work OS-globally. `action: "key"` bindings synthesize macOS
keycodes (named F13 to F20, or a configured keycode with modifiers) even
without a focused Herdr TUI, and the device service's reserved Handy button is
global too. Neither is a Herdr input route, and official device ownership still
gates all bridge work.

Scripts run locally from the plugin root and receive `HERDR_FRONTEND_SOCKET`,
`HERDR_PANE_ID`, and `HERDR_MICRO_BIN_PATH` (the plugin's own `bin/herdr-micro`).
Use `client input text TEXT` or `client input keys KEY...`. Input is
deliberately untargeted. External commands run normally, outside frontend
validation.

Useful actions:

```sh
herdr plugin action invoke micro-status --plugin gjermundgaraba.herdr-micro
herdr plugin action invoke micro-stop --plugin gjermundgaraba.herdr-micro
herdr plugin log list --plugin gjermundgaraba.herdr-micro --limit 20
```

Before unlinking or uninstalling the plugin, remove its per-user service:

```sh
HERDR_MICRO_ROOT="$(herdr plugin list --plugin gjermundgaraba.herdr-micro --json | plutil -extract result.plugins.0.plugin_root raw -o - -)"
"$HERDR_MICRO_ROOT/bin/codex-micro" uninstall
```

No administrator access is needed.

Architecture, ownership, and limitations are in [the bridge
guide](docs/micro-bridge.md). Hardware and version evidence is indexed in [the
research record](docs/research/README.md).

## License

Apache-2.0. See [`../LICENSE`](../LICENSE) and
[third-party notices](THIRD_PARTY_NOTICES.md).
