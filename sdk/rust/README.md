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
socket protocol 23). The client API below is checked against the fork's
canonical fixtures, not against a running daemon.

## Attached clients

The [Herdr fork build](../../README.md#herdr-build) adds a client API to the
daemon socket. Every attached client (the TUI or the native app) reports
whether its window has focus, its endpoints, and its Agents list; a tool reads
those reports and asks a client to jump to an agent. The fork's
`docs/next/website/src/content/docs/client-contract.md` is the contract, and
`tests/fixtures` holds verbatim copies of its JSON fixtures, which
`tests/client_contract.rs` decodes.

```rust
use herdr_client::{Client, ClientChanged, ClientList};

fn show(list: &ClientList) {
    for attached in &list.clients {
        if let Some(report) = &attached.report {
            println!("{} focused: {:?}", attached.client_id, report.focused);
        }
    }
}

let client = Client::from_env()?;
// Subscribe first, so no change falls between the subscription and the list.
let mut changes = client.subscribe_client_changes()?; // client.changed
show(&client.list_clients()?); // client.list
while let Some(event) = changes.next_event()? {
    if let Some(change) = ClientChanged::from_event(&event) {
        // The event names the client only; its state is in client.list.
        let _client_id = change?.client_id;
        show(&client.list_clients()?);
    }
}
```

`Client::start_jump` writes `client.jump` and returns a `PendingJump`, whose
`wait` reads the answer: the jump, or a typed
`ClientJumpError` (`ClientNotFound`, `ClientNotReporting`, `Superseded`,
`ClientDeparted`, `StaleBoot`, `TargetNotFound`, `EndpointUnavailable`,
`Timeout`). The two are separate so that a caller can write a
second jump while the first is unanswered. The daemon holds a jump for up to twenty seconds while
the client performs it. The newest jump to a client wins: a second jump
replaces the pending one, whose caller gets `Superseded`, so that answer to an
earlier request is expected and not a failure. A client that is attached but
has never sent a report answers no jumps, and fails at once with
`ClientNotReporting`.
`Client::new` sets no socket timeout; when you set one with `with_timeout`,
use `CLIENT_JUMP_TIMEOUT` for jumps so the daemon's typed answer arrives
before the client gives up. After
`Timeout`, `ClientDeparted` or `EndpointUnavailable` the jump may still have
happened: do not repeat it without the user asking. Dropping a `PendingJump` closes its connection
and withdraws nothing.

Three modules build on these calls for tools that drive a device; `sessions`
and `follow` are Unix-only:

- `sessions` lists the running local sessions with `herdr session list --json`,
  killing it after `SESSION_LIST_TIMEOUT`. Each session is its own daemon with
  its own socket. `resolve_herdr` finds the executable from `HERDR_BIN_PATH`,
  then `PATH`.
- `follow` keeps one subscription per session, reads `client.list` after it
  starts and once per burst of `client.changed` events, and hands the caller a
  `Directory` of every daemon's clients whenever it changes. Sessions are
  rediscovered on a thread of their own, so a slow listing never delays a
  published change.
  `Directory::followed_client` is the client to show and to jump: the one,
  across all daemons, that most recently started reporting `focused: true`.
  It stays followed while another app is in front. Listings are taken one at
  a time, so the latest wins; of several clients that take focus in one
  listing, the one with the highest client id. While no client is followed,
  a reporting client is followed anyway: one that reports `true` before one
  that reports `null` before one that reports `false`, and of equals the last
  by socket path and then client id. The followed client is forgotten when a
  listing of its daemon lacks it or a discovery no longer lists its daemon.
  While its daemon is only offline (a failed subscription or listing) it is
  kept and `followed_client` is `None`, as it is while no client has sent a
  report; a tool must then show nothing and send nothing.
  `FollowedClient::has_focus` says whether its window reports focus right
  now, which keyboard events for the frontmost app must check; it is false
  while the focus is unknown.
  `Directory::updated` applies the rule to a new set of daemons.
  `FollowedClient::jump_to` builds the jump for one of its agents from the row's
  `endpoint_id`, that endpoint's `boot_id`, and the `pane_id`;
  `JumpTarget::start` writes it. A daemon whose subscription or listing fails
  is logged with its own error, once per distinct error, and has no clients
  until it answers.
- `slots` assigns a fixed number of sticky agent keys. New agents fill free
  slots in the daemon's `priority_key` order, compared as bytes; an agent keeps
  its slot while it is in the report, also while a jump cannot reach it
  (`FollowedClient::can_reach`), and only an agent that can be reached takes
  a slot it does not hold. With every slot taken, such an agent replaces the
  slotted agent that is last in that order when it comes before it and has a
  louder status, whether or not a jump can reach the one it replaces.

`priority_key` is opaque. Compare it, never parse it. It is empty when the
daemon sent none, and equal keys keep the report's order.
