# herdr-deck

Native Stream Deck daemon for macOS with a Herdr agent dashboard. It owns the configured devices directly over HID and does not use Elgato plugins or profiles.

Configured and physically verified here:

- Stream Deck + `A00WA4411LI67S`: eight sticky Herdr agent keys, four dials, and touch strip.
- Stream Deck Pedal `A00YA4272193CD`: system-wide F19, Enter, and F19.

The CLI enumerates every model `elgato-streamdeck` recognizes. The daemon manages only Plus and Pedal, the two devices whose rendering and input paths are tested here.

## Requirements

- macOS and a Stream Deck + (the Pedal is optional)
- The [Herdr fork build](../README.md#herdr-build) with the daemon's client
  API (`client.list`, `client.changed`, `client.jump`); stock Herdr does not
  have it (Hub is not required)
- `herdr` on `PATH`, or `HERDR_BIN_PATH` set, when running `install-service`
  or `run`, to list the local sessions
- `jpeg-turbo` (`brew install jpeg-turbo`) to build

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

`herdr.sessions` is an optional allow-list of the endpoint IDs to show, as the
followed Herdr client reports them (`local`, `ssh:...`). Leave it out or set it
to `[]` to show every endpoint:

```json
"herdr": { "slotCount": 8, "sessions": [] }
```

`cycle-session` cycles the display filter through `all`, `active` (the
endpoint the client currently presents), and each allowed endpoint that is
online. A filter on one endpoint stays while that endpoint is not online and
while the client's daemon does not answer; `cycle-session` then goes to
`all`. The filter falls back to `all` by itself when the followed client no
longer reports the endpoint or `herdr.sessions` no longer allows it.

The daemon lists the running local sessions with `herdr session list --json`
and follows every one of them over the ordinary Herdr daemon socket; Herdr Hub
is not used. Each daemon reports its attached clients (`client.list`) and
their changes (`client.changed`). A launchd service has neither the user's
`PATH` nor `HERDR_BIN_PATH`, so `install-service` resolves the `herdr`
executable when it runs and writes it into the LaunchAgent as
`HERDR_BIN_PATH`. Run `install-service` again if that executable moves.

The dashboard follows one Herdr client, across all daemons, and shows that
client's report: its endpoints and its Agents list, in the order it shows
them. It follows the client whose window most recently took focus, which is
the client whose report changed to `focused: true` last, and keeps following
it while another app is in front. Of several clients that take focus in one
listing of a daemon, the one with the highest client id wins.

While no client is followed, the dashboard follows a reporting client anyway:
one that reports focus before one whose focus is unknown before one that
reports none, and of equals the last by socket path and then client id. A TUI
reports unknown focus until its terminal sends a focus change, so a TUI
started in an already focused terminal is followed this way. It then stays
followed until another client takes focus.

The followed client is forgotten when its daemon lists its clients without it
(it detached, or the daemon restarted) or when `herdr session list` no longer
lists its session as running. A daemon that only fails to answer, for example
one listing that times out, does not lose it: the dashboard shows nothing from
Herdr and sends no Herdr action until the daemon lists the client again, and
follows no other client in the meantime unless one takes focus. With no
reporting client anywhere, the keys and the touch strip are empty; with no
client attached anywhere, the keys read offline.

The keys are sticky slots. Free slots fill in the daemon's priority order
(`priority_key`), and an agent keeps its key while the client reports it and
the display filter selects its endpoint, also while the client's window is
not in front. When every slot is taken, a new agent replaces the last one in
priority order only if the daemon ranks it ahead and it is in a louder state
(blocked over done over working over idle). The yellow border marks the agent
the client has focused.

A client goes on reporting the agents of an endpoint that is not online, for
example one that is reconnecting. Such an agent keeps its key as any other
agent does, so a louder agent can still replace it when every slot is taken.
The key is drawn in the `unknown` colors with no animation, and the agent is
left out of the touch strip's counts; a press on the key is refused and
logged. An agent that cannot be reached takes no key it does not already
hold.

`focus-slot` sends one `client.jump` to the daemon the followed client is
attached to, with the agent's endpoint, that endpoint's boot ID, and the pane.
It works while another app is in front. The client switches endpoint when it
has to, so the slot's endpoint need not be the one on screen. The daemon
holds the request until the client answers, for up to twenty seconds.
A press never waits for that answer: a second press during that time sends a
new jump, which replaces the first, and nothing is logged for the replaced
one. One thread writes the jumps to the daemon, one at a time and in the
order the presses arrived, and it only ever holds the newest press: a press
that is followed by another before it is written is not sent at all. So an
older press is never written after a newer one. A second thread reads the
answers, also one at a time, and of the jumps waiting to be read only the
newest is kept: the connection of an older one is closed unread. However
fast keys are pressed, the dashboard therefore has two threads and at most
three connections for jumps. Of the jumps written, the daemon lets the one it
takes up last win; it reads each on a connection of its own, so two written
at the same instant can still be taken up in the other order. Presses on
another device, such as the Micro keypad, are not ordered against these. A
jump is never retried: after a timeout, a client that left, or an endpoint the
client could not reach, it may still have happened. A failed jump is written
to the daemon log as `focus-slot failed: jump to <endpoint>/<pane>:` and the
reason in words, such as `the endpoint now runs another boot`, `the client
knows no such endpoint or pane`, `the client could not reach the endpoint; the
jump may still have happened`, or `the client did not confirm the jump in
time; it may still have happened`. A press that was not sent because its
client is no longer the followed one is logged as `focus-slot failed:` and
that reason. The API's error codes (`stale_boot`, `jump_target_not_found`,
`jump_endpoint_unavailable`, `jump_timeout`) do not appear in the log.

`prompt` and `submit` do not go through Herdr. `prompt` types its text into
the frontmost app and then presses Enter unless `submit` is `false`; `submit`
presses Enter. They are sent only while the followed client reports that its
window has focus, so the frontmost app is that client and the text lands
where its keyboard focus is. With another app in front they are refused and
logged. So are they while the client's focus is unknown: a TUI cannot be
typed into until its terminal has sent it a focus change, which a terminal
that already had focus when the TUI started does only once you leave it and
come back. The client is captured at the press and checked again before each
keystroke: every chunk of at most 20 UTF-16 units of text, and the Enter
after a 200 ms pause. If it lost focus or is no longer the followed client,
the rest is dropped. The check reads the last report
the daemon published, not macOS, so a keystroke sent after the window lost
focus but before its report arrived can reach the app that took focus.
Stopping the daemon drops queued prompts, submits, and system keys. At most
16 of them wait for the one worker that types; a control pressed while the
queue is full is ignored and logged as `action queue full`. Text is typed as Unicode keyboard events, so the keyboard layout does
not matter and the pasteboard is not touched. The text must not contain
newlines or other control characters.

A daemon whose `client.list` or subscription fails is logged with its own
error, once per distinct error, and has no clients until it answers; it is
asked again with a backoff of up to five seconds.

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

These actions, and `prompt` and `submit`, require Accessibility access for
`~/Library/Application Support/dev.herdr.deck/bin/herdr-deck`, granted under
Privacy & Security in System Settings. The `system-enter` action is also
accepted as a shortcut for `system-key` with `key: "enter"`.

Replacing the executable can invalidate its Accessibility authorization even
while the switch remains on. If the daemon startup log reports access is not
granted after an upgrade, remove and re-add the installed executable in
Accessibility, then restart the service.

Actions are `system-key`, `system-enter`, `focus-slot`, `prompt`, `submit`,
and `cycle-session`. Dashboard keys default to `focus-slot` if no binding is
supplied. Use `prompt` with `submit: false` to insert text without sending.

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

The daemon uses bounded latest-frame mailboxes, subscribes directly to each Herdr daemon's client changes, reconnects automatically, and hot-reloads configuration.

## Start at login

After a successful foreground test:

```sh
cargo build --release --locked -p herdr-deck
../target/release/herdr-deck install-service
```

`install-service` validates the configuration, resolves the `herdr` executable, atomically copies the executable to `~/Library/Application Support/dev.herdr.deck/bin/herdr-deck`, and starts or restarts the launchd service using that installed copy. It takes an optional plist path and an optional config path as its two arguments. The service logs to `~/Library/Logs/dev.herdr.deck/daemon.log`. The service also uses a stable working directory outside the checkout, so clearing Cargo build artifacts does not affect it. To upgrade, rebuild and run `install-service` again. `uninstall-service` removes the launchd service and leaves the installed binary and configuration in place.

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
