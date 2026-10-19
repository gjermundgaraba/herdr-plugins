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

`Environment::context` is the typed `HERDR_PLUGIN_CONTEXT_JSON`. Menu, key, and
API invocations describe their target pane, tab, and space, and actions, panes,
and startup hooks also get the `client_id` of the shell client, the TUI or Herdr
GPUI, that invoked them. Read anything else through the client:

```rust
let context = Environment::load()?.context.unwrap_or_default();
let client = Client::from_env()?;
let spaces = client.list_workspaces()?;
let tabs = client.list_tabs(context.workspace_id.as_deref())?;
if let Some(pane_id) = context.focused_pane_id.as_deref() {
    let session = client.get_pane(pane_id)?.agent_session;
}
```

`start_agent` returns once the daemon launched the agent; unlike `herdr agent
start`, it does not wait for the agent to become ready. It retries for up to
two seconds from the first busy answer while the target pane's shell is still
starting, as the CLI does, and then returns the daemon's `agent_pane_busy`. The
answers to the reads between attempts are awaited only within those two
seconds, where the platform supports socket timeouts, so a daemon that stalls
on one of them, or sends it piece by piece, also ends in `agent_pane_busy`. A
stall on `agent.start` itself blocks unless the client has a timeout
(`with_timeout`), and is then a transport error.

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

`Client::subscribe` opens a live lifecycle-event stream. Since Herdr 0.9.0 a
subscription starts when the request is accepted and does not replay retained
events, and the stable API has no snapshot cursor or server-generation token,
so consumers must refresh state after reconnecting.

Written against the [Herdr fork build](../../README.md#herdr-build) (0.9.3,
socket protocol 23).

`sessions`, which is Unix-only, lists the running local sessions with `herdr
session list --json`, killing it after `SESSION_LIST_TIMEOUT`. Each session is
its own daemon with its own socket. `resolve_herdr` finds the executable from
`HERDR_BIN_PATH`, then `PATH`.
