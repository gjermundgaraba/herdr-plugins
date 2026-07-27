# herdr-micro

Herdr plugin for the Work Louder Codex Micro.

## Development

Requires Herdr 0.7.5 or newer, Node.js 22 or newer, and Hunk 0.17 or newer.

```sh
npm install
mkdir -p bin
/usr/bin/swiftc native/frontmost.swift -o bin/frontmost
/usr/bin/swiftc native/micro-hid.swift -o bin/micro-hid -framework IOKit
herdr plugin link . --enabled
herdr plugin action invoke status --plugin gjermundgaraba.herdr-micro
node src/micro-action.mjs status
node src/micro-action.mjs stop
herdr plugin unlink gjermundgaraba.herdr-micro
```

Herdr injects the active session socket and plugin paths into each command.
The device daemon polls through the injected `HERDR_BIN_PATH`; its own Unix
socket provides single-instance status and shutdown control.

Plugin notes and primary-source links are in
[`docs/herdr-plugin-notes.md`](docs/herdr-plugin-notes.md).

## Thinking effort control

The plugin exposes `effort-raise` and `effort-lower` actions. It
freezes Herdr's focused pane from the invocation context, then dispatches:

- Codex: the locally configured reasoning-effort shortcuts;
- Claude: the native `/effort` picker;
- Pi: two extension-owned shortcuts from
  [`integrations/pi/herdr-effort.js`](integrations/pi/herdr-effort.js).

See [`docs/effort-control.md`](docs/effort-control.md) for the test boundary and
known limitations.

## Micro bridge

The bridge owns the Codex Micro vendor HID interface over USB or
Bluetooth LE, mirrors six Herdr Agent states onto the RGB keys, focuses those
panes, and maps the dial to the effort actions. See
[`docs/micro-bridge.md`](docs/micro-bridge.md) for its process and safety
boundaries.

## Automatic Layer 2

The bridge polls the native macOS frontmost app and window once per second.
Layer claims live in Herdr's plugin config directory as `claims.json`:

```json
[
  {
    "id": "herdr",
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
last matching rule wins. No match sends no command, preserving the last
applicable layer.

One-time device setup binds synthetic AppSense identities to Layers 1 and 2:

```sh
node src/micro-action.mjs stop
node src/micro-setup-appsense.mjs
node src/micro-start.mjs
```

The setup command refuses to run beside the bridge, Input, Codex, or ChatGPT,
backs up `keymap.json`, writes only the two AppSense bindings, and verifies a
full device read-back.
