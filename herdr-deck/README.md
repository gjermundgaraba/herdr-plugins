# herdr-deck

Native Stream Deck daemon for macOS with a Herdr agent dashboard. It owns the configured devices directly over HID and does not use Elgato plugins or profiles.

Configured and physically verified here:

- Stream Deck + `A00WA4411LI67S`: eight sticky Herdr agent keys, four dials, and touch strip.
- Stream Deck Pedal `A00YA4272193CD`: system-wide F19, Enter, and F19.

The CLI enumerates every model `elgato-streamdeck` recognizes. The daemon manages only Plus and Pedal, the two devices whose rendering and input paths are tested here.

## Safe takeover

Do not uninstall Elgato first. Quit it, verify Herdr Deck, then disable its login item. Both programs need exclusive access to the HID devices.

Build dependency: `brew install jpeg-turbo`. All commands below run from
this directory.

```sh
cargo test -p herdr-deck
cargo build --release --locked -p herdr-deck

# Quit Elgato Stream Deck from its menu-bar icon, then:
../target/release/herdr-deck devices
../target/release/herdr-deck push
../target/release/herdr-deck doctor
../target/release/herdr-deck run
```

Press `Ctrl-C` to stop. Herdr Deck closes HID handles cleanly; screens keep their last frame until another process takes over.

## Configuration

[`herdr-deck.json`](herdr-deck.json) is source controlled. Push it atomically to the hot-reloaded live path:

```sh
"$HOME/Library/Application Support/dev.herdr.deck/bin/herdr-deck" push
# ~/.config/herdr-deck/config.json
```

This uses the installed service executable. Before the first installation,
use `../target/release/herdr-deck push` after building.

Invalid replacements are rejected and the last working config remains active. Rules match exact `serial`, then `model`, then a role fallback. A disabled exact rule overrides broader fallbacks, so hot reload can release a device immediately. Duplicate selectors and unknown fields are rejected. Unmatched devices remain untouched.

`herdr.sessions` is an optional allow-list of endpoint IDs in the focused Herdr
frontend. Leave it out or set it to `[]` to show every available endpoint:

```json
"herdr": { "slotCount": 8, "sessions": [] }
```

Use frontend endpoint IDs in a non-empty allow-list, not runtime session keys.
`cycle-session` cycles the display filter through `all`, `active`, and each
available allowed endpoint.

The daemon connects directly to local Herdr TUIs using frontend protocol 7;
Herdr Hub is not used. It discovers private sockets in `HERDR_CLIENT_API_DIR`
(default `/tmp/herdr-clients-<uid>`). `install-service` preserves a non-empty
`HERDR_CLIENT_API_DIR` from its environment in the LaunchAgent.

The dashboard follows the uniquely focused TUI's endpoint snapshots, including
agent statuses and selected input target. Actions capture its socket, endpoint
and server boot ID, source pane, and target pane/agent identity.
Queued actions are invalidated by observed focus, selection, readiness, or
endpoint changes. Dispatch rechecks the live TUI and target agent before sending;
every runtime call carries the captured endpoint route. Slot focus uses terminal
navigation completion, and failed or unknown mutations are never replayed.

An explicit filter or slot belonging to another endpoint requires you to focus
that endpoint first. Missing or ambiguous focus, unavailable input, and unavailable
endpoints disable Herdr actions. Quiet subscriptions remain connected; EOF clears
the affected TUI and reconnects automatically. Other TUIs remain tracked.

System keyboard actions work independently of Herdr and send an unmodified
keypress to the active macOS application. F19 includes the native macOS function-key flag, which shortcut recorders may
display as `fn+f19`. Supported keys are `enter` and `f19`:

```json
"buttons": {
  "0": { "action": "system-key", "key": "f19" },
  "1": { "action": "system-key", "key": "enter" },
  "2": { "action": "system-key", "key": "f19" }
}
```

These actions require Accessibility access for
`~/Library/Application Support/dev.herdr.deck/bin/herdr-deck`, granted under
Privacy & Security in System Settings. The `system-enter` action is also
accepted as a shortcut for `system-key` with `key: "enter"`.

Replacing the executable can invalidate its Accessibility authorization even
while the switch remains on. If the daemon startup log reports access is not
granted after an upgrade, remove and re-add the installed executable in
Accessibility, then restart the service.

Actions are `system-key`, `system-enter`, `focus-slot`, `focus-pane`, `send-keys`,
`prompt`, `submit`, and `cycle-session`. `send-keys`, `prompt`, and `submit`
target Herdr. Dashboard keys default to `focus-slot` if no binding is supplied.
Use `prompt` with `submit: false` to insert text without sending.

## CLI

After installing the service, use its independent executable:

```sh
HERDR_DECK_BIN="$HOME/Library/Application Support/dev.herdr.deck/bin/herdr-deck"
"$HERDR_DECK_BIN" devices
"$HERDR_DECK_BIN" check herdr-deck.json
"$HERDR_DECK_BIN" doctor
"$HERDR_DECK_BIN" frames working /tmp/deck-frames
"$HERDR_DECK_BIN" push herdr-deck.json
"$HERDR_DECK_BIN" uninstall-service
```

Use `../target/release/herdr-deck run` for foreground development only when the
installed service is stopped, since both need exclusive device access. To
deploy source changes, rebuild and run the build's `install-service` command
below; running it from the existing installed copy does not deploy a new build.

The daemon uses bounded latest-frame mailboxes, subscribes directly to Herdr frontend snapshots, reconnects automatically, and hot-reloads configuration.

## Start at login

After a successful foreground test:

```sh
cargo build --release --locked -p herdr-deck
../target/release/herdr-deck install-service
```

`install-service` validates the configuration, atomically copies the executable to `~/Library/Application Support/dev.herdr.deck/bin/herdr-deck`, and starts or restarts the launchd service using that installed copy. It takes an optional plist path and an optional config path as its two arguments. The service logs to `~/Library/Logs/dev.herdr.deck/daemon.log`. The service also uses a stable working directory outside the checkout, so clearing Cargo build artifacts does not affect it. To upgrade, rebuild and run `install-service` again. `uninstall-service` removes the launchd service and leaves the installed binary and configuration in place.

Then turn off "Open automatically at login" in the Elgato Stream Deck app. Keep the app installed until the launchd service survives a login and a device reconnect; after that it can be removed without affecting Herdr Deck.

## Rendering

Key images are generated SVG with animated orbs ported from the MIT-licensed
thinking-orbs engine; see [docs/rendering.md](docs/rendering.md) for the
pipeline, where every constant comes from, off-device preview with `frames`,
and how to add an animation. Attribution is in `THIRD_PARTY_NOTICES.md`.

## Deploying changes

A build alone does not update the running installation. After changing the
source, run the tests, rebuild, and run the new build's `install-service`;
running `install-service` from the installed copy only reinstalls that copy.
Verify with the installed executable's `doctor`. Never point launchd at
`target/` or symlink the installed binary there, and keep local configuration
edits when reinstalling.

## Boundaries

- USB only; no Elgato Marketplace plugins or profiles.
- Plus and Pedal are the managed hardware targets in this build.
- Herdr Deck owns reconnect, sleep/wake, rendering, and error recovery.
- Firmware updates remain out of scope; keep a vendor recovery path before deleting the app.
