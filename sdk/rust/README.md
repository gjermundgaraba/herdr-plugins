# herdr-client for Rust

Small synchronous client for Herdr's public plugin socket API. It provides:

- typed session, workspace, tab, pane, agent, and layout models
- generic typed calls for every socket method
- typed helpers for operations used by these plugins
- long-lived event subscriptions
- Unix socket and Windows named-pipe transport

```rust
use herdr_client::Client;

fn main() -> Result<(), herdr_client::Error> {
    let client = Client::from_env()?;
    let snapshot = client.snapshot()?;
    println!("{} panes", snapshot.panes.len());
    Ok(())
}
```

Plugin binaries can classify their Herdr launch without comparing raw environment
variables:

```rust
use herdr_client::{Environment, PluginInvocation};

match Environment::load()?.invocation() {
    Some(PluginInvocation::Action("open")) => { /* open the plugin UI */ }
    Some(PluginInvocation::Pane("palette")) => { /* run the UI */ }
    _ => {}
}
```

Methods added after this crate's tested Herdr version remain usable:

```rust
let value = client.call_value("some.future.method", &serde_json::json!({}))?;
```

Methods that retain the socket for a protocol-specific stream, currently
`pane.graphics.stream`, require dedicated support rather than `call_value`.

Plugin entrypoints can require Herdr's managed directories instead of deriving
paths from `HOME` or XDG variables:

```rust
use herdr_client::Environment;

let plugin = Environment::load()?.require_plugin()?;
let config = plugin.config_file("config.toml");
let state = plugin.data_dir().join("state.json");
let lock = plugin.run_dir().join("plugin.lock");
let log = plugin.logs_dir().join("plugin.log");
```

`data`, `cache`, `run`, and `logs` are repository conventions beneath
`HERDR_PLUGIN_STATE_DIR`; create only the directories an entrypoint uses.
`open_rotating_log` creates private append-only logs and rotates them on the
next open after the active file reaches its size limit. The final write can
cross that limit.

`Client::subscribe` uses Herdr 0.8.0's retained lifecycle-event stream. The
stable API has no snapshot cursor or server-generation token, so consumers
must tolerate replayed events and refresh state after reconnecting.

Validated against the [Herdr fork build](../../README.md#herdr-build)
(0.9.1, socket protocol 22).

## Local frontend protocol 7 (Unix)

`frontend::FrontendClient` connects directly to one TUI's socket, independently
of the runtime `Client`. `from_env()` reads `HERDR_FRONTEND_SOCKET`;
`connect(path)` takes an explicit socket. `discover(&directory())` lists the
owner-only sockets in the TUI socket directory; a socket whose TUI has exited
refuses connections. Older protocols are rejected at the hello; there is no
fallback.

The methods are `snapshot`, `subscribe`, `select`, `input`, and `call`.
`Subscription::next_snapshot(timeout)` delivers pushed snapshots; the timeout
only lets the caller check for cancellation and does not expire a quiet TUI.
EOF ends the subscription.

`Snapshot::route(endpoint_id)` yields a `Route` of endpoint ID and server boot
ID for an available endpoint. `select` and `call` take a route; the TUI
rejects a route whose boot no longer matches with `stale_route` rather than
acting on a reused pane id. `call` runs any method the endpoint advertises on
its client command lane, with the runtime API's parameter shape, and requires
the endpoint to already be active. `Input::Text` and `Input::Keys` carry no
route: they follow the TUI's own focus and overlays.

`select` returns the focus call's result on the active endpoint, or `{"ok":true}`
once another endpoint is observed focused. Any `Ok` means the target is focused.
`cancelled`, `timeout`, transport timeouts, malformed replies, and EOF mean the
mutation outcome is unknown; nothing is retried automatically.

Pickers are ordinary plugin-owned terminal applications, hosted through a
`local_terminal` keybinding on the TUI host. There are no picker wire types,
requests, events, or presentation capabilities in this SDK.
