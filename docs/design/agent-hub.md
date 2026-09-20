# Herdr Hub: session inventory and relay

Hub owns the shared session inventory, not TUI presentation. Navigation,
history, and the unread hold are native TUI client actions; Herdr owns
selection, navigation completion, and choice dialogs. Micro and local tools
connect directly to the per-TUI frontend socket. Hub does not discover
frontends, choose a focused client, route client input, or activate
endpoints.

## State ownership

Hub's versioned model contains `hosts` and `sessions`. Session records contain
workspaces, tabs, and agents from the runtime's `session.snapshot`. Fleet agent
identity is `(session key, terminal_id)`. There is no `active`, `current_client`,
`clients`, or session-level `client_focused` field.

One watcher per local session reads `session.snapshot` every 250 ms and publishes
a complete replacement only when it changes. Plugin status hooks wake that
watcher early; event payloads are not a second state-authority path. A live
snapshot error marks the session disconnected. Initial outages use the existing
startup grace period; reconnect uses bounded backoff.

Discovery runs `herdr session list --json` at startup and periodically. It admits
only supported default/named-session socket layouts under the discovered session
root. Startup hooks accelerate reconciliation. Event notifications only wake
already admitted socket paths; they cannot create session identities.

## Protocol 6

NDJSON travels over a private per-user Unix socket with same-user peer checks,
bounded frames, bounded handshakes/writes, and bounded subscriber queues.

```jsonc
{"type":"subscribe","protocol":6}
{"type":"hello","protocol":6,"model":{"version":1,"hosts":[],"sessions":[]}}
{"type":"session","version":2,"session":{/* complete SessionState */}}
{"type":"session_removed","version":3,"key":"local/default"}
{"type":"host","version":4,"host":{"key":"workbox","connected":false,"error":"offline"}}
```

A subscriber receives an atomic initial model then ordered updates. Slow
subscribers disconnect instead of silently dropping updates; reconnect supplies
a fresh model. EOF removes the subscriber without waiting for another broadcast.

Session-scoped calls use a dedicated one-reply connection:

```jsonc
{"type":"call","protocol":6,"id":7,"session":"workbox/agents","method":"pane.focus","params":{"pane_id":"pane_3"}}
{"type":"reply","id":7,"result":{/* runtime result */}}
```

Hub routes raw API methods to the session's local socket or remote relay. It
does not interpret TUI state or replace per-TUI `navigate`, `input`, and `call`.
The unpublished client-facing Hub protocol and model were removed, not retained
as a compatibility path. Hub binaries and SDK consumers must agree on protocol 6.

## Remote hosts

The local Hub maintains one `ssh host herdr-hub relay` process per configured
host. The relay connects to that host's existing Hub, or runs the Hub library
in-process for the SSH connection's lifetime. It forwards only remote-local
sessions, preventing nested remote inventories from leaking through.

Remote session keys are qualified by the configured host key. Connection epochs
prevent stale updates and replies from a retired transport affecting a replacement.
Pending calls fail when a transport is retired and are not replayed. The same
relay carries session updates and call replies without conflating their roles.

The remote host requires the Hub binary and hook plugin. SSH establishment and
version checks are noninteractive and bounded. There is no per-call SSH spawn
or socket-forward fallback. See [the Hub README](../../herdr-hub/README.md) for
configuration and installation.

## Deployment and boundaries

On macOS Hub runs as a per-user LaunchAgent, independent of session lifetimes.
On Linux relay mode can own the in-process Hub; plugin notifications remain
supported. Build/install the plugin's independent `bin/` executable before
linking. Updating an installed service is a separate, explicit operation.

The Hub SDK exposes only the protocol, the model, and the streaming client.
Attention ordering, slot assignment, and rendering are consumer policy. No Hub
model fact claims which TUI is focused or where a user's next keystroke will
go.
