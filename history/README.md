# herdr-history

Vim-style back/forward focus history for [Herdr](https://herdr.dev): jump through
previously focused panes across tabs and workspaces, like vim's `ctrl+o` / `ctrl+i`
jumplist or browser back/forward. New navigation after going back truncates the
forward branch, closed panes are pruned automatically, and hitting either end of
the history shows a toast.

Unlike the native `keys.last_pane` (a 1-deep toggle), this keeps a 100-entry stack.

## Install

```sh
herdr plugin install gjermundgaraba/herdr-plugins/history
```

Or for local development:

Run these commands from the repository root:

```sh
cargo build --release --locked -p herdr-history
mkdir -p history/bin
install -m 750 target/release/herdr-history history/bin/.herdr-history.new
mv -f history/bin/.herdr-history.new history/bin/herdr-history
herdr plugin link /path/to/herdr-plugins/history
```

Startup is automatic with a new Herdr server. When installing, linking, or
enabling the plugin on a server that is already running, start history before
changing focus:

```sh
herdr plugin action invoke gjermundgaraba.herdr-history.activate
```

Run that once in each active Herdr session. Back/forward deliberately refuse to
create a late, incomplete history.

Then bind keys in `~/.config/herdr/config.toml` (there is no plugin key registration
in Herdr's plugin API v1). `prefix+o` is Herdr's default `open_notification_target`,
so free it first — a conflicting `[[keys.command]]` is silently disabled otherwise:

```toml
[keys]
open_notification_target = ""

[[keys.command]]
key = "prefix+o"
type = "plugin_action"
command = "gjermundgaraba.herdr-history.back"
description = "History back"

[[keys.command]]
key = "prefix+i"
type = "plugin_action"
command = "gjermundgaraba.herdr-history.forward"
description = "History forward"
```

Reload with `herdr server reload-config`. Requires Herdr >= 0.8.0 (socket
protocol 19) and Rust >= 1.89 to install. The installed plugin has no runtime
language dependency.

## How it works

A small per-session daemon is the only history writer. It seeds from a
`session.snapshot`, then consumes Herdr's ordered retained `pane.focused`
stream. The snapshot's focused pane is used as the replay boundary when a
dropped stream reconnects. Herdr 0.8.0 exposes no exact snapshot cursor, so
activation on an already-running server intentionally starts at its current
pane; activate before changing focus as described above.

Back/forward commands are serialized through the same daemon and call
`pane.focus`, which switches workspace and tab automatically. Rebinding the
server socket resets history because pane ids may recycle; reconnect failures
are logged once per outage. Concurrent Herdr sessions use separate in-memory
histories and control sockets, keyed by Herdr socket path alone: one daemon per
Herdr server.

Every control request carries a hash of the client executable's bytes. A
daemon built from different bytes unlinks its socket, replies, and exits; the
client then starts the current build and retries once, so the first action
after any rebuild or upgrade completes against the new daemon (back/forward
report "at oldest", because history is in-memory only and does not survive
the swap). Identical bytes never swap, so no-op rebuilds leave the daemon and
its history alone. A daemon whose executable stays deleted for a few seconds
retires on its own. Every retirement is logged with its reason.

## Development

```sh
cargo test -p herdr-history                                  # pure history logic
cargo build --release --locked -p herdr-history
mkdir -p history/bin
install -m 750 target/release/herdr-history history/bin/.herdr-history.new
mv -f history/bin/.herdr-history.new history/bin/herdr-history
herdr plugin action invoke gjermundgaraba.herdr-history.activate  # optional: swap now
herdr plugin log list --plugin gjermundgaraba.herdr-history  # per-invocation logs
```

Build and stage the executable as above: the next action swaps the daemon to
the new binary automatically. `activate` forces the swap immediately; it retires a
daemon built from different bytes and leaves an identical one alone. A bare
workspace build (`cargo build --release` at the repo root) unifies features
differently and produces a different binary than `-p herdr-history`, which
just costs one automatic swap when alternating between the two.

Daemon-side events (retirements, reconnects, subscription failures) go to
`$HERDR_PLUGIN_STATE_DIR/logs/history.log`, not to the per-invocation logs.
