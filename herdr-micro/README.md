# herdr-micro

Unofficial macOS [Herdr](https://herdr.dev/) integration for the Work Louder
Codex Micro. It shows the agents of the Herdr client that last took focus on
the six Agent keys, jumps to an agent when its key is pressed, and types
configured prompts and Enter into that client while it has focus.

The device transport is a separate per-user service. It uses the stock Codex
Micro firmware and shared macOS IOHIDManager access over USB or Bluetooth Low
Energy; USB is preferred when both are present. It does not seize USB, install
a root helper, or use `sudo`.

## Requirements

- macOS and a Codex Micro; the current hardware baseline is stock firmware
  0.6.2
- The [Herdr fork build](../README.md#herdr-build) with the daemon's client
  API (`client.list`, `client.changed`, `client.jump`); Hub is not required
- `herdr` on `PATH`, or `HERDR_BIN_PATH` set, in the environment the bridge
  is started from, to list the local sessions
- Rust 1.89 or newer when building from source
- An Apple Development code-signing identity when building from source
- Work Louder Input for the one-time Layer 2 setup
- macOS Input Monitoring permission for the installed Codex Micro service
- macOS Accessibility permission for every bound control: prompts, Enter, and
  system `key` taps are all sent as macOS keyboard events
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
herdr plugin invoke gjermundgaraba.herdr-micro.service-authorize
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
herdr plugin invoke gjermundgaraba.herdr-micro.service-authorize
```

## Set up the Micro

1. In Work Louder Input, create a blank Layer 2, then quit Work Louder Input
   and the Codex desktop app.

2. If the Micro bridge is running, stop it before changing the keymap. On a
   fresh install nothing is running yet; skip this step.

   ```sh
   herdr plugin invoke gjermundgaraba.herdr-micro.micro-stop
   ```

3. Clone the device's OAI controls into Layer 2. Setup accepts only a blank or
   previously managed layer, backs up the keymap, and verifies the write.

   ```sh
   herdr plugin invoke gjermundgaraba.herdr-micro.micro-setup
   ```

4. Start and check the integration.

   ```sh
   herdr plugin invoke gjermundgaraba.herdr-micro.micro-start
   herdr plugin invoke gjermundgaraba.herdr-micro.doctor
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

The two top-level fields, `controls` and `lighting`, are required. There are
three actions, and each one is a macOS keyboard event:

```json
"4": { "action": "prompt", "prompt": "/copy" },
"3": { "action": "prompt", "prompt": "draft: ", "submit": false },
"7": { "action": "submit" },
"2": { "action": "key", "key": "F19" }
```

- `prompt` types its text into the frontmost app, then presses Enter unless
  `submit` is `false`. The text must not contain newlines or other control
  characters.
- `submit` presses Enter.
- `key` taps a system key. `key` names cover F13 to F20; `keycode` accepts any
  macOS virtual keycode (0 to 127) instead of `key`, and optional `modifiers`
  adds any of `cmd`, `shift`, `alt`, `ctrl`, and `fn`.

`prompt` and `submit` are sent only while the followed Herdr client (see
[Routing and operation](#routing-and-operation)) reports that its window has
focus, so the frontmost app is that client and the text lands where its
keyboard focus is. `key` is sent from any frontmost app.

A binding can depend on the kind of agent the client has focused:

```json
"3": { "byAgent": { "codex": { "action": "prompt", "prompt": "/fast" }, "default": null } }
```

Buttons also accept gesture bindings:

```json
"1": { "tap": { "action": "submit" }, "hold": { "action": "prompt", "prompt": "/model" }, "holdMs": 400 }
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

## Routing and operation

The bridge lists the running local sessions with `herdr session list --json`
and follows every one of them: each session is its own daemon, and the client
the keypad follows may be attached to any. From each daemon it reads the attached clients'
reports (`client.list`) and subscribes to their changes (`client.changed`).

The keypad follows one client, across all daemons. That client supplies
everything the keypad shows. It is the client whose window most recently took
focus, which is the client whose report changed to `focused: true` last, and
it stays followed while another app is in front. Of several clients that take
focus in one listing of a daemon, the one with the highest client id wins.

While no client is followed, the keypad follows a reporting client anyway:
one that reports focus before one whose focus is unknown before one that
reports none, and of equals the last by socket path and then client id. A TUI
reports unknown focus until its terminal sends a focus change, so a TUI
started in an already focused terminal is followed this way. It then stays
followed until another client takes focus.

The followed client is forgotten when its daemon lists its clients without it
(it detached, or the daemon restarted) or when `herdr session list` no longer
lists its session as running. A daemon that only fails to answer, for example
one listing that times out, does not lose it: the lights are blank and
nothing is sent until the daemon lists the client again, and no other client
is followed in the meantime unless one takes focus. With no reporting client
anywhere, the lights are blank as well. `doctor` names the client the running
bridge follows and says whether its window has focus.

The six Agent keys are sticky slots for the followed client's Agents list.
Free slots fill in the daemon's priority order (`priority_key`), and an agent
keeps its key while it stays in the list, also while the client's window is
not in front. When every slot is taken, a new agent replaces the last one in
priority order only if the daemon ranks it ahead and it is in a louder state
(blocked over done over working over idle). Slots use `(endpoint_id, pane_id)`
identity, so the same pane id on two machines is two agents. The brighter key
is the agent the client has focused, and the aggregate lights follow the first
lit slotted agent in priority order.

A client goes on listing the agents of an endpoint that is not online, for
example one that is reconnecting. Such an agent keeps its key as any other
agent does, so a louder agent can still replace it when every slot is taken.
The key is dark, and a press on it is logged as `ignored: no ready Agent
slot`. An agent that cannot be reached takes no key it does not already hold.

Pressing an Agent key sends one `client.jump` to the daemon the followed
client is attached to, with the agent's endpoint, that endpoint's boot ID, and
the pane. It works while another app is in front. The client switches
endpoint when it has to. The daemon holds the request until the client
answers, for up to twenty seconds. A press never waits for that answer: a
second press during that time sends a new jump, which replaces the first, and
nothing is logged for the replaced one. One thread writes the jumps to the
daemon, one at a time and in the order the presses arrived, and it only ever
holds the newest press: a press that is followed by another before it is
written is not sent at all. So an older press is never written after a newer
one. A second thread reads the answers, also one at a time, and of the jumps
waiting to be read only the newest is kept: the connection of an older one is
closed unread. However fast keys are pressed, the bridge therefore has two
threads and at most three connections for jumps. Of the jumps written, the
daemon lets the one it takes up last win; it reads each on a connection of
its own, so two written at the same instant can still be taken up in the
other order. Presses on another device, such as a Stream Deck, are not
ordered against these. A jump is never retried: after a timeout, a client
that left, or an endpoint the client could not reach, it may still have
happened. The outcome is written to `logs/micro.log`: `jumped to
<endpoint>/<pane>`, or `jump to <endpoint>/<pane>:` and the reason in words,
such as `the endpoint now runs another boot`, `the client knows no such
endpoint or pane`, `the client could not reach the endpoint; the jump may
still have happened`, or `the client did not confirm the jump in time; it may
still have happened`. A press that was not
sent because its client is no longer the followed one is logged as `ignored:`
and that reason. The API's error codes (`stale_boot`,
`jump_target_not_found`, `jump_endpoint_unavailable`, `jump_timeout`) do not
appear in the log.

Prompts and Enter do not go through Herdr. They are typed into the frontmost
app as keyboard events, so they are sent only while the followed client
reports that its window has focus; with another app in front they are refused
and logged. So are they while the client's focus is unknown: a TUI cannot be
typed into until its terminal has sent it a focus change, which a terminal
that already had focus when the TUI started does only once you leave it and
come back. A gesture captures the followed client at its first press; if it
is no longer the followed client, or its window does not have focus, when the
action fires or before any later keystroke (each chunk of text, and the
Enter), nothing more is sent. Text is typed as Unicode keyboard events,
so the keyboard layout does not matter and the pasteboard is not touched.

A daemon whose `client.list` or subscription fails is logged with its own
error, once per distinct error, and has no clients until it answers; it is
asked again with a backoff of up to five seconds.

`action: "key"` bindings and the device service's reserved Handy button work
without any Herdr client. Official device ownership still gates all
bridge work.

Useful actions:

```sh
herdr plugin invoke gjermundgaraba.herdr-micro.micro-status
herdr plugin invoke gjermundgaraba.herdr-micro.micro-stop
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
