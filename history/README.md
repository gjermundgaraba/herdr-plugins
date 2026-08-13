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

```sh
cargo build --release --locked
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

Reload with `herdr server reload-config`. Requires Herdr socket protocol 20 and
Rust >= 1.89 to install. The installed plugin has no runtime language dependency.

## How it works

A small per-session daemon is the only history writer. It seeds from a
`session.snapshot`, then subscribes to ordered `pane.focused` events after that
snapshot's event cursor, so focus changes during startup cannot be lost or
reordered. Back/forward commands are serialized through the same daemon and call
`pane.focus`, which switches workspace and tab automatically. History resets
when the Herdr session epoch changes, because pane ids recycle across normal
server restarts; live handoff keeps the epoch and history. A dropped event
subscription reconnects and reconciles from a fresh snapshot without stopping
the daemon. Concurrent Herdr sessions use separate in-memory histories and
control sockets keyed by Herdr socket path and executable identity. Rebuilt
daemons retire themselves instead of continuing to run stale code.

## Development

```sh
cargo test                                                 # pure history logic
cargo build --release --locked                             # linked executable
herdr plugin action invoke gjermundgaraba.herdr-history.activate
herdr plugin log list --plugin gjermundgaraba.herdr-history  # per-invocation logs
```
