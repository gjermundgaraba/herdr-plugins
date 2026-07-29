# herdr-micro

Unofficial [Herdr](https://herdr.dev/) plugin for the Work Louder Codex
Micro. It shows six agent states on the RGB keys and controls focused Codex,
Claude Code, and Pi agents.

Tested on macOS with Codex Micro firmware `v0.4.1` over USB and Bluetooth LE.
The plugin talks directly to the vendor HID interface, so firmware changes may
break it.

## Requirements

- macOS, Herdr 0.7.5 or newer, and Node.js 20 or newer
- A Codex Micro and Xcode Command Line Tools (`swiftc`)
- Work Louder Input for the initial keyboard profile only
- [Hunk](https://hunk.sh/) only if you map the optional `diff` action

## Install

From GitHub:

```sh
herdr plugin install gjermundgaraba/herdr-plugins/herdr-micro
herdr plugin enable gjermundgaraba.herdr-micro
```

For local development:

```sh
git clone https://github.com/gjermundgaraba/herdr-plugins.git
cd herdr-plugins/herdr-micro
mkdir -p bin
/usr/bin/swiftc native/frontmost.swift -o bin/frontmost
/usr/bin/swiftc native/micro-hid.swift -o bin/micro-hid -framework IOKit
herdr plugin link . --enabled
```

## First run

1. In Input, create a blank Layer 2. Connect by USB, then quit Input and the
   Codex desktop app.
2. Run the one-time guarded setup. It clones the private OAI layout from Layer
   1, creates AppSense bindings, backs up the keymap, and verifies the write.

   ```sh
   herdr plugin action invoke micro-setup --plugin gjermundgaraba.herdr-micro
   ```

3. Start the bridge and check the installation.

   ```sh
   herdr plugin action invoke micro-start --plugin gjermundgaraba.herdr-micro
   herdr plugin action invoke doctor --plugin gjermundgaraba.herdr-micro
   ```

4. Edit `buttons.json`, `claims.json`, `effort.json`, and `lighting.json` in:

   ```sh
   herdr plugin config-dir gjermundgaraba.herdr-micro
   ```

The setup refuses to overwrite a nonblank target layer or run while another
known device owner is active. Pass layers 2–6 when running the script directly:
`node src/micro-setup.mjs 3`.

## Buttons

`buttons.json` maps the seven physical action events. It is validated and
reloaded on every press:

```json
{
  "1": {
    "codex": "$review",
    "claude": "/review",
    "pi": "/skill:review",
    "default": "Review the current changes"
  },
  "2": "diff",
  "3": "fast",
  "4": "copy",
  "5": null,
  "6": null,
  "7": "submit"
}
```

Built-ins are `diff`, `fast`, `copy`, and `submit`; `null` disables a button.
A prompt object may use any lowercase Herdr agent name and an optional
`default` fallback. `fast` supports Codex and Pi. Buttons mapped to ordinary
keys such as F19 in Input bypass the plugin.

Use **Configure Micro buttons** in Herdr or:

```sh
npm run buttons
```

## Automatic layers

`claims.json` maps macOS bundle IDs to layers:

```json
[
  {
    "id": "terminal",
    "layer": 2,
    "process": "com.mitchellh.ghostty"
  },
  {
    "id": "codex",
    "layer": 1,
    "process": "com.openai.codex"
  }
]
```

An optional `titleIncludes` narrows a claim to matching window titles. The
last matching rule wins. An unclaimed app sends no command, preserving the
last applicable layer. Claims are reloaded while the bridge runs.

## Thinking effort

Claude Code uses its native `/effort` picker. Pi needs the bundled extension:

```sh
herdr plugin action invoke setup-pi-effort \
  --plugin gjermundgaraba.herdr-micro
```

Run `/reload` in existing Pi sessions.

Codex needs two key bindings in `~/.codex/config.toml`:

```toml
[tui.keymap.chat]
increase_reasoning_effort = "ctrl-shift-t"
decrease_reasoning_effort = "ctrl-t"
```

Match them in the plugin's `effort.json`:

```json
{
  "codex": {
    "raise": "ctrl+shift+t",
    "lower": "ctrl+t"
  }
}
```

## Dial, joystick, and lighting

Pressing the dial submits `/model` to the focused Codex, Claude Code, or Pi
agent. The four joystick directions focus the adjacent Herdr pane. A direction
fires once when the stick is pushed outward and rearms after it returns to
center.

`lighting.json` controls the six individual Agent keys and the optional
aggregate zones:

```json
{
  "states": {
    "blocked": { "color": "#ffaa00", "brightness": 1, "effect": "solid", "speed": 0 },
    "done": { "color": "#22cc55", "brightness": 1, "effect": "solid", "speed": 0 },
    "working": { "color": "#2277ff", "brightness": 1, "effect": "breath", "speed": 0.35 },
    "idle": { "color": "#ffffff", "brightness": 0.25, "effect": "solid", "speed": 0 },
    "unknown": { "color": "#ffffff", "brightness": 0.08, "effect": "solid", "speed": 0 }
  },
  "focusedBrightness": 1,
  "ambient": "status",
  "keys": null
}
```

Effects are `off`, `solid`, `snake`, `rainbow`, `breath`, `gradient`, and
`shallow-breath`. Set `ambient` or `keys` to `"status"` to make that aggregate
zone follow the highest-priority slotted agent; use `null` to leave it alone.
The configuration reloads while the bridge runs.

## Operations

```sh
herdr plugin action invoke micro-status --plugin gjermundgaraba.herdr-micro
herdr plugin action invoke micro-stop --plugin gjermundgaraba.herdr-micro
herdr plugin log list --plugin gjermundgaraba.herdr-micro --limit 20
```

Only one process should own the vendor HID interface. Quit Input while using
the bridge. The bridge yields to the frontmost Codex desktop app, stops after
60 seconds without Herdr, and blanks the LEDs on a controlled shutdown.

Design and compatibility details are in
[`docs/micro-bridge.md`](docs/micro-bridge.md) and
[`docs/effort-control.md`](docs/effort-control.md). Archived protocol research
is in [`docs/research/`](docs/research/).

## License

MIT. See [third-party notices](THIRD_PARTY_NOTICES.md).
