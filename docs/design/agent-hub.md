# Herdr agent hub: design exploration

Status: implemented. Written 2026-09-03 against Herdr 0.8.2 (protocol 20,
upstream checkout at `v0.8.2`) and revised for the production Herdr fork at
commit `85ad1d77`. That fork's `session.snapshot.client_focused` field is the
authoritative active-session signal.

Consumers today: `herdr-micro` (this repo), `clankerdeck`
(`/Users/gg/ws/pers/clankerdeck`), the `sdk/picker` live agent list, and one more
app coming. Goals: cut CPU spent on Herdr polling, share the Herdr-facing work,
support several local sessions and remote hosts, show agents from all sessions
(with "active session" still a first-class notion), and give every app the same
view of the agent set. Backwards compatibility and churn are explicitly not
concerns.

## 1. What was measured

Default session while measuring: 25 workspaces, 34 tabs, 48 panes, 29 agents.
Server CPU was measured as the delta of the server's cumulative CPU time while
adding a controlled extra caller.

| Call | Payload | Server CPU per call | Notes |
| --- | --- | --- | --- |
| `session.snapshot` | 67 KB | ~22 ms | Robust: +10 % CPU at 4 Hz. |
| `agent.list` | 17 KB | ~6.5 ms | +25 % CPU at 40 Hz. |
| `pane.get` (one pane) | ~0.6 KB | below noise, well under 1 ms | |
| `workspace.list` | ~10 KB | below noise, well under 1 ms | |
| `ping` | 0.1 KB | ~0 | |

Where the cost comes from: every `pane_info` resolves `foreground_cwd` live
(`proc_pidinfo` on the shell, the foreground process group, and the members of
that process group). A snapshot builds `pane_info` for all 48 panes plus 29
agent records on the server's App thread, which is also the render thread. There
is no caching of snapshot output.

What that meant for the pre-hub setup:

| Poller | Rate | Server CPU on the polled session |
| --- | --- | --- |
| clankerdeck (`session.snapshot`) | 4 Hz | ~9 % of a core |
| herdr-micro (`session.snapshot`, active session) | 4 Hz | ~9 % of a core |

The default session's server sits at ~22-29 % CPU. Two snapshot pollers explain
most of that. The apps themselves are cheap on the Herdr side: clankerdeck 2.5 %
(mostly its 30 fps animation timer and JPEG encoding), herdr-micro 1.1 %.

Two additional costs showed up while measuring:

- The removed app-side client-focus integration queried the terminal app every
  50 ms. A 5 s sample showed ~194 ms of terminal-app main-thread time, about 4%
  of a core, continuously, on top of herdr-micro's own side of that traffic.
  The final design has no terminal-app integration.
- herdr-micro re-runs a full `session.snapshot` (22 ms) before every action to
  revalidate the target (`dispatch.rs`). `agent.get` on the specific target
  costs a fraction of a millisecond.

## 2. What the socket API actually offers for push

Read from `src/api/server.rs`, `src/api/subscriptions.rs`,
`src/api/event_hub.rs`, `src/api/schema/events.rs`, and
`src/app/api/plugins/runtime.rs` at v0.8.2, plus the production fork's
`session.snapshot` shape at commit `85ad1d77`.

- **Lifecycle subscriptions are cheap.** `workspace.*`, `worktree.*`, `tab.*`,
  `pane.created/closed/updated/focused/moved/exited/agent_detected`, and
  `layout.updated` are served from an in-memory ring buffer (`EventHub`, 512
  events). Each subscription connection is one server thread that wakes every
  100 ms (`CONNECTION_POLL_INTERVAL`), locks the hub, scans events after its
  cursor, and writes at most one event per subscription kind per tick.
- **`pane.agent_status_changed` subscriptions are polling in disguise.** The
  subscription requires a `pane_id`. Its poll path falls through to a `pane.get`
  dispatched to the App thread on every 100 ms tick when nothing new arrived
  (`ActiveAgentStatusChangedSubscription::poll_result`). One subscription per
  agent pane means 10 `pane.get`/s per agent, on the order of 7-9 % of a core
  for 29 agents. This is what `sdk/picker` does today for every pane while the
  picker is open. It is not a way to reduce server CPU.
- **There is no unfiltered "any agent status changed" subscription.** A plain
  working→idle transition emits only `PaneAgentStatusChanged` into the hub, and
  no hub-based subscription kind exposes it.
- **Plugin event hooks cover it.** `PLUGIN_HOOK_EVENT_KINDS` includes
  `pane.agent_status_changed`, `pane.agent_detected`, `pane.created/closed/
  focused/moved/exited`, `workspace.*`, `tab.*`, `worktree.*`. Herdr spawns the
  manifest `[[events]]` command without blocking, with `HERDR_SOCKET_PATH`,
  `HERDR_PLUGIN_EVENT`, and `HERDR_PLUGIN_EVENT_JSON` (the full envelope:
  pane id, workspace id, status, agent, title, display agent, state labels) in
  the environment. The plugin registry is global (`~/.config/herdr/plugins.json`),
  so one linked plugin fires in every session server. Limits: 32 concurrent hook
  commands, 200 retained log records. `pane.updated`, `layout.updated`,
  `workspace.metadata_updated`, and `pane.output_changed` are not hookable.
- **Events carry no sequence numbers and a new subscription replays the whole
  ring buffer** (`last_sequence: 0`). Clients cannot detect gaps after overflow
  and will see stale events right after subscribing. Any client must be able to
  resync from an authoritative read.
- **Event payloads cannot rebuild `AgentInfo`.** `PaneInfo` (in
  `pane.created/updated/moved`) lacks `name`, `state_change_seq`,
  `launch_pending`, `interactive_ready`, `screen_detection_skipped`.
  `pane.agent_status_changed` carries status and presentation only. Nothing
  reports `foreground_cwd` changes. A purely incremental client is therefore
  both incomplete and unsafe.
- **Client focus is snapshot-only.** Fork commit `85ad1d77` exposes
  `client_focused` through `session.snapshot`, but the current extension API
  emits no event when that field changes. Active-session tracking therefore
  needs one canonical snapshot loop in the hub.
- **`herdr --remote` does not forward the API socket.** It bridges only the UI
  client socket over SSH (`src/remote/host_unix.rs`, `attach.rs`). Remote API
  access needs its own transport.
- **Discovery:** `herdr session list --json` returns name, running flag, session
  dir, and socket path. A plugin `[[startup]]` hook runs in every new session
  server once its API is ready, which is a push signal for new sessions.

## 3. Design space and verdicts

### 3.1 How to learn about agent state

| Option | Idle server cost | Latency | Verdict |
| --- | --- | --- | --- |
| Poll `session.snapshot` at 4 Hz per app (pre-hub) | ~9 % per app per session | ≤250 ms | Deleted. |
| Poll `agent.list` at 1-2 Hz from one process | ~0.7-1.3 % per session | ≤1 s | Reject: incomplete for client focus and unnecessary as a second path. |
| Per-pane `pane.agent_status_changed` subscriptions | ~7-9 % for 29 agents | ≤100 ms | Reject: server-side polling. |
| Incremental state from lifecycle events only | ~0 | ≤100 ms | Reject: payloads insufficient, no gap detection. |
| **One hub-owned `session.snapshot` poll per local session** | +0.20 CPU points on the paired default-session sample | ≤250 ms | **Required and selected:** the snapshot is the only complete state and client-focus read. |

The final loop, per local session:

1. Read `session.snapshot` every 250 ms. The snapshot supplies protocol,
   workspaces, tabs, agents, and the fork's `client_focused` value together.
2. Publish the complete session record only when it changes. A plugin event may
   wake the watcher early, but the resulting authoritative read is still
   `session.snapshot`; event payloads never form a second update path.
3. A snapshot error after a live connection marks the session disconnected
   immediately. An initial outage is reported after 30 seconds (or immediately
   for an incompatible protocol). Reconnect uses bounded backoff; discovery
   reconciliation handles servers that appear or disappear.

Only one update path exists: `session.snapshot` is the truth. Lifecycle
subscriptions and coalesced list refetches were an earlier design and were
deleted when the fork's snapshot-only focus signal became authoritative.

`state_change_seq` and `focused` come straight from `AgentInfo`, so slot
ordering in both apps keeps working unchanged.

### 3.2 Where the logic lives

| Option | Same view for all apps | Remote | Ops | Verdict |
| --- | --- | --- | --- | --- |
| Library only, each app embeds a watcher | Converges, never identical at an instant; N× refetches | Each app needs its own SSH per host | No new process | Reject as the deployment shape. |
| Shared daemon (hub) that apps subscribe to | Identical versioned model | One SSH per host | One LaunchAgent, one socket, one protocol | **Recommended.** |
| One app acts as hub for the others | Identical | One SSH | Asymmetric, fragile | Reject. |

Build it as a library crate plus a thin daemon binary. The library owns
discovery, per-session watching (section 3.1), the multi-session model, and the
remote relay client. The daemon binds the hub socket, runs the library, and
serves clients. Apps are hub clients only; they stop linking `herdr-client` for
state. The library boundary is for tests and for the relay, not for apps to
embed a second copy.

Process model on macOS: a per-user LaunchAgent (`KeepAlive`), like
`dev.herdr.codex-micro` and `net.garaba.clankerdeck` already are. It is
independent of any Herdr session, so dashboards show "offline" instead of
disappearing when the last session stops. A companion `herdr-plugin.toml`
provides the hooks:

```toml
[[startup]]
command = ["bin/herdr-hub", "ensure"]                # refresh service, then notify startup

[[events]]
on = "pane.agent_status_changed"
command = ["bin/herdr-hub", "notify", "event"]       # forwards HERDR_PLUGIN_EVENT_JSON
```

`notify` connects to the hub socket, writes one line, exits 0 (also when the hub
is not running). Herdr already knows the emitting session through
`HERDR_SOCKET_PATH`. Startup notifications accelerate discovery; status
notifications may wake the snapshot watcher before its next 250 ms deadline.

Discovery remains separate from state polling: `session list` runs at hub start
and during periodic reconciliation, while the startup hook makes new sessions
appear promptly.

### 3.3 Hub to client protocol

NDJSON over a private Unix socket (`bind_private_socket`,
`peer_is_current_user` from `sdk/rust`), exact protocol version in the hello,
as `codex-micro`'s service does. Server pushes:

```jsonc
{"type":"hello","protocol":3,"model":{...}}
{"type":"session","version":812,"session":{...}}
{"type":"session_removed","version":813,"key":"local/besu"}
{"type":"active","version":814,"key":"local/default"}
```

`version` is a single monotonic counter across all pushes. Every client sees the
same sequence, which is the strongest "same view" guarantee available. Sessions
are replaced whole (a session record is ~1 KB per agent), so there is no diff
machinery and no client-side merge logic beyond "replace by key".

Model shape:

```jsonc
{
  "version": 812,
  "active": "local/default",            // resolved by the hub, may be null
  "hosts": [{"key": "local", "connected": true, "error": null}, {"key": "workbox", "connected": false, "error": "..."}],
  "sessions": [{
    "key": "local/default", "host": "local", "name": "default",
    "connected": true, "error": null, "protocol": 20,
    "workspaces": [ /* WorkspaceInfo values */ ],
    "tabs": [ /* TabInfo verbatim */ ],
    "agents": [ /* AgentInfo verbatim */ ],
    "socket_path": "/tmp/herdr/herdr.sock",
    "client_focused": true
  }]
}
```

Agent identity across the fleet is `(session key, terminal_id)`. Both apps'
sticky slot logic keys on `terminal_id` already.

Actions: the hub exposes a passthrough
`{"type":"call","protocol":3,"id":7,"session":"workbox/agents","method":"pane.focus","params":{"pane_id":"w3:p1"}}`
that forwards the raw Herdr method to the right socket (local) or relay (remote)
and returns the raw result. Apps use it for every action, so local and remote
sessions have one code path and apps never learn socket paths. The hub does not
interpret actions.

Shared presentation policy (attention ordering: status priority, then
`state_change_seq`) goes into the client library so both apps sort identically.
Slot assignment and rendering stay in the apps.

### 3.4 Remote hosts

| Option | Needs our binary remotely | Push | Verdict |
| --- | --- | --- | --- |
| `ssh host herdr api snapshot` per poll | No | No | Reject: a process and an SSH exec per poll. |
| SSH Unix-socket forward per remote session socket (`-L local.sock:remote.sock`) | No | Socket subscriptions only; hooks fire on the remote host and cannot reach the local hub | Fallback only; would reintroduce `agent.list` polling for status. |
| **`ssh host herdr-hub relay`**: the same binary runs the library remotely and speaks the hub protocol over stdio | Yes | Yes, everything | **Recommended.** |

The local hub spawns one `ssh` per configured host (user SSH config, ControlMaster
allowed), consumes `model`/`session` messages from the relay, prefixes session
keys with the host key, and forwards `call` requests over the same stream. The
relay connects to an already running hub on that host if one exists, else runs
the library in-process for the life of the SSH session and binds the hub socket
so that host's hooks reach it. Reconnect with backoff on SSH exit. This is the
same shape Herdr uses for its own UI bridge (`run_remote_client_bridge`).

Requirement stated plainly: the binary and the hook plugin must be installed on
the remote host, either from the Nix flake output or by building/linking the
plugin and putting its staged binary on `PATH`. Herdr's own remote bootstrap
does not install third-party binaries.

### 3.5 Active session

The production Herdr fork at commit `85ad1d77` puts `client_focused` in
`session.snapshot`. The hub treats it as authoritative and publishes the
matching local session as `model.active`, or `null` when no Herdr client is
focused.

The extension API has no event for changes to this field. The hub therefore
runs one canonical 250 ms snapshot loop. This is a revised final decision, not
a compatibility fallback: every consumer uses the pushed hub model and no
consumer opens its own focus-polling path. The 250 ms cadence matches the old
consumer refresh bound while replacing two separate snapshot pollers with one
owner.

## 4. Resulting architecture

```text
herdr session servers (local)         remote host
  │ snapshot every 250 ms              ssh host herdr-hub relay ─┐
  │ startup/status hook → wake                                   │ NDJSON stdio
  ▼                                                                ▼
herdr-hub (LaunchAgent) ── library: discover, snapshot, merge, active ──
  │ private socket, versioned model pushes, call passthrough
  ├── herdr-micro (lighting from active session, actions via call)
  ├── clankerdeck (slots across sessions, actions via call)
  ├── picker live agents
  └── next app
```

Expected steady state on the Herdr side: one hub-owned snapshot read every
250 ms for each local session. A remote relay owns the equivalent loop for its
host. Consumers do no direct Herdr polling; they receive versioned hub pushes.
The paired post-cutover sample recorded in the implementation plan was 3.17%
before and 3.37% after (+0.20 percentage points) on the default-session server.

## 5. Clean break: what each codebase loses

herdr-micro

- `herdr.rs`: `SessionWorker`, `follow_session`, `discover_sessions`,
  `run_command_with_timeout` and the process-spawning session discovery.
- `daemon/reconcile.rs`: `refresh_sessions`, `sync_selected_worker`,
  `apply_session_update`, `pending_agents` plumbing, the generation dance around
  session workers.
- The independent client-focus routing loop in `daemon/mod.rs` and
  `refresh_frontmost` / `refresh_routing` cadence, replaced by hub `active`
  pushes.
  (`external_owner` stays in `codex-micro` where it gates the device.)
- `current_snapshot` before every action in `dispatch.rs`, replaced by
  `agent.get` through the hub passthrough.
- `docs/micro-bridge.md` "Herdr action scheduling" paragraph about 4 Hz polling.

clankerdeck

- `src/herdr.rs` entirely: hand-rolled RPC framing, `session_socket`,
  `HerdrClient::sessions` (process spawn), `snapshot`, `parse_snapshot_response`.
- `snapshot_worker` and `SnapshotCommand` in `daemon.rs`; `action_worker`
  becomes a thin caller of the hub passthrough.
- `herdr.session` in config becomes a filter over hub session keys, or goes away
  if slots span all sessions.

sdk/picker

- `subscribe_events` with its per-pane `pane.agent_status_changed` fan-out.

sdk/rust

- Keep `Client`, `Subscription`, types, `unix`, `ndjson`. Only the hub uses them.

No consumer or fallback polling remains in any app. The required hub-owned
snapshot poll is the sole state path. If the hub is down, apps show offline and
reconnect.

## 6. Limits that no extension can change

- Server-side subscription delivery is a 100 ms poll with at most one event per
  subscription kind per tick; bursts are throttled and can overflow the 512-slot
  ring buffer. This is why subscriptions are not the authoritative state path.
- No cheap unfiltered agent-status subscription exists. The status hook can
  wake the watcher early, but every published update still comes from a fresh
  snapshot.
- `foreground_cwd` is resolved live on every read and is not event-driven.
- Client focus is available only on the fork's `session.snapshot`; there is no
  corresponding extension event, so the hub's 250 ms loop is required.
- Remote API access requires our own transport and our binary on the remote
  host.

## 7. Final decisions

1. Clankerdeck spans all sessions by default, ordered by attention, and marks
   agents from the active session.
2. Remote hosts require the hub binary and hook plugin. There is no polling
   fallback tier.
3. The hub alone derives active session from
   `session.snapshot.client_focused`; there is no terminal-app integration.
