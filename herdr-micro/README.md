# herdr-micro

Unofficial [Herdr](https://herdr.dev/) plugin for the Work Louder Codex
Micro. It shows six agent states on the RGB keys and controls focused Codex,
Claude Code, and Pi agents.

Historical Node/Swift builds were physically tested on macOS with Codex Micro
firmware `v0.4.1` over USB and Bluetooth LE. The Rust binary's connected-device
canary mapped `default` and `werk`, discovered 23 agents, and selected Layer 2.
It is not a physical USB/BLE transport matrix; firmware changes may break HID
access.

## Requirements

- macOS, Ghostty 1.3 or newer, Herdr 0.7.5 or newer, and Rust 1.71 or newer
- A Codex Micro
- macOS Automation permission for Herdr or its terminal host to inspect Ghostty
- macOS Accessibility permission for the optional `scroll` action
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
cargo build --release --locked
mkdir -p bin
install -m 750 target/release/herdr-micro bin/.herdr-micro.new
mv -f bin/.herdr-micro.new bin/herdr-micro
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

4. Edit `controls.json`, `effort.json`, and `lighting.json` in:

   ```sh
   herdr plugin config-dir gjermundgaraba.herdr-micro
   ```

The setup refuses to overwrite a nonblank Layer 2 or run while another known
device owner is active.

## Controls

`controls.json` maps the seven action buttons, dial, and joystick. It is
validated and reloaded while the bridge runs:

```json
{
  "version": 1,
  "buttons": {
    "1": {
      "byAgent": {
        "codex": { "action": "prompt", "prompt": "$review", "submit": true },
        "claude": { "action": "prompt", "prompt": "/review", "submit": true },
        "pi": { "action": "prompt", "prompt": "/skill:review", "submit": true },
        "default": null
      }
    },
    "2": { "action": "diff" },
    "3": {
      "byAgent": {
        "codex": { "action": "fast" },
        "pi": { "action": "fast" },
        "default": null
      }
    },
    "4": { "action": "prompt", "prompt": "/copy", "submit": true },
    "5": null,
    "6": null,
    "7": { "action": "submit" }
  },
  "dial": {
    "clockwise": { "action": "effort", "direction": "raise" },
    "counterclockwise": { "action": "effort", "direction": "lower" },
    "press": { "action": "prompt", "prompt": "/model", "submit": true }
  },
  "joystick": {
    "engageDistance": 0.75,
    "releaseDistance": 0.3,
    "up": { "action": "scroll", "direction": "up", "percent": 50 },
    "down": { "action": "scroll", "direction": "down", "percent": 50 },
    "left": { "action": "focus-pane", "direction": "left" },
    "right": { "action": "focus-pane", "direction": "right" }
  }
}
```

Every control accepts a direct action, `null`, or a `byAgent` map. An exact
focused-agent match wins, followed by `default`; missing and `null` actions do
nothing. Agent entries are complete actions and do not merge with defaults.
Buttons and dial press also accept gesture bindings:

```json
{
  "tap": { "action": "submit" },
  "doubleTap": { "action": "diff" },
  "hold": { "action": "prompt", "prompt": "/model", "submit": true },
  "release": null,
  "holdMs": 500,
  "doubleTapMs": 250
}
```

Each gesture accepts the same actions and `byAgent` maps. Direct bindings fire
on press. A gesture `tap` fires on release, but waits for the double-tap window
when `doubleTap` is configured. A successful hold suppresses tap and
double-tap; `release`, when configured, fires after either. Timing defaults to
500 ms for hold and 250 ms for double-tap and accepts 50–5000 ms.

Actions are `prompt`, `diff`, `fast`, `submit`, `effort`, `focus-pane`, and
`scroll`.
`prompt` requires a nonempty `prompt`; `submit` defaults to `true`, while
`false` types without pressing Enter. `fast` supports Codex and Pi. Dial and
joystick bindings can use any action, and the joystick distances are hardware
calibration values between zero and one. Buttons mapped to ordinary keys such
as F19 in Input bypass the plugin.

`scroll` requires `direction` (`up` or `down`) and `percent` greater than zero
and at most 100. It scrolls the focused pane by approximately that fraction of
its visible rows. On macOS it briefly moves the event cursor to the pane,
posts wheel events, and restores the original cursor position. Run the doctor
if it does nothing; macOS event posting requires Accessibility permission.

Use **Configure Micro controls** in Herdr or:

```sh
bin/herdr-micro configure-controls
```

## Automatic layers and Herdr sessions

There is no layer or session routing configuration. The bridge selects:

- Layer 1 while the Codex desktop app is frontmost;
- Layer 2 when the focused Ghostty terminal belongs to a running Herdr
  session; and
- no new layer or session for unrelated applications, preserving the last
  applicable selection.

One global bridge discovers the default and named Herdr sessions. It briefly
sets a unique title through each named session, reads Ghostty's stable
terminal UUID through its AppleScript API, and restores the original title.
The resulting UUID-to-session map stays in memory and is rebuilt when sessions
or terminal clients change. User-visible tab titles are not used.

All six Agent slots, RGB, buttons, dial, joystick, and popups route through
the focused terminal's session via Herdr's `HERDR_SESSION` selector; agents
from sessions are never mixed. `scroll` rechecks that same UUID immediately
before posting wheel events, preventing input from reaching Chrome or another
terminal.

`micro-status` reports the active layer, selected session, terminal UUID, and
in-memory mappings. Run `bin/herdr-micro probe-sessions --watch` to inspect
focus switches without starting the hardware bridge.

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

## Lighting

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
60 seconds without a running Herdr session, and blanks the LEDs on a
controlled shutdown.

Design and compatibility details are in
[`docs/micro-bridge.md`](docs/micro-bridge.md) and
[`docs/effort-control.md`](docs/effort-control.md). Archived protocol research
is in [`docs/research/`](docs/research/).

## License

MIT. See [third-party notices](THIRD_PARTY_NOTICES.md).
