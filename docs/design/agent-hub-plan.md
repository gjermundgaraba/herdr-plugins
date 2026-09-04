# Herdr agent hub: implementation plan

Companion to [agent-hub.md](agent-hub.md), which holds the measurements and the
reasoning. This document is the build order. Clean-break rules apply throughout:
one path, no fallbacks to direct polling, delete replaced code in the same
phase, every phase ends green with `cargo test --workspace`.

Revision 2 incorporates two rounds of review. The rulings that changed the plan
are marked "(rev 2)" where they land. The Phase 3 focus design was revised after
implementation to follow the Herdr fork used in production; that ruling is
marked "(focus revision)" and supersedes the earlier terminal-integration plan.

Decisions taken here (change them before Phase 1 if you disagree):

| Decision | Choice |
| --- | --- |
| Crate names | `herdr-hub` (daemon, plugin dir `herdr-hub/`), `herdr-hub-client` (crate in `sdk/hub/`) |
| Herdr minimum | protocol 20, `min_herdr_version = "0.8.2"` (rev 2; 20 landed in 0.8.2 with bell forwarding, nothing here depends on it, the floor is policy) |
| Hub socket | `/tmp/herdr-hub-<uid>.sock` + `/tmp/herdr-hub-<uid>.lock`, peer uid checked (same pattern as `codex-micro`) |
| Service binary | copied to `~/Library/Application Support/dev.herdr.hub/herdr-hub`; the LaunchAgent never points into a build tree (rev 2) |
| Linux | no user service; the hub runs on Linux only embedded in `herdr-hub relay` (Phase 4). The plugin still declares `linux` because hooks and `notify` must run there (rev 2) |
| Hub config | `~/.config/herdr-hub/config.toml`, hosts list only, parsed from Phase 4 (rev 2) |
| Session key | `"<host>/<session name>"`, local host key is `local` |
| Sessions in scope | the default session and named sessions under `~/.config/herdr/sessions/`. Servers bound to a custom `HERDR_SOCKET_PATH` are not sessions to the hub and are ignored everywhere, including `notify` (rev 2) |
| Agent identity | `(session key, terminal_id)`; workspaces are `(session key, workspace_id)` (rev 2) |
| LaunchAgent label | `dev.herdr.hub` (matches `dev.herdr.codex-micro`) |
| Plugin id | `gjermundgaraba.herdr-hub` |
| clankerdeck slots | Span all sessions by default; the encoder that cycled sessions now cycles a session filter (`all`, then each connected session) |
| clankerdeck Phase 2 actions | Until Phase 3 supplies an active session, focused-pane actions under `all` use `local/default` when present, otherwise the sole connected allowed session; with several non-default sessions the user must select a filter |
| Remote hosts | Require `herdr-hub` and the hook plugin on the remote host; no polling tier |
| Active session | The production Herdr fork at commit `85ad1d77` exposes `client_focused` through `session.snapshot`; the hub derives `model.active` from that field (focus revision) |
| Active-session trigger | The extension API has no client-focus event, so the hub owns one canonical 250 ms snapshot loop; consumers never poll Herdr themselves (focus revision) |
| Tabs in the model | Not before Phase 5; added there with a protocol bump for picker label parity (rev 2) |
| Runtime | std threads + mpsc, blocking I/O, `poll(2)` where a thread waits on several fds. No async runtime (none in the workspace today) |
| clankerdeck dependency | `herdr-hub-client = { path = "../herdr-plugins/sdk/hub" }` while both repos are co-developed; a pinned git dependency is a packaging decision for later |

## Phase 0: `herdr-hub-client` (sdk/hub)

Protocol types and the client every app links. No Herdr knowledge beyond
re-exporting `AgentInfo`/`WorkspaceInfo` from `herdr-client`.

Files:

- `sdk/hub/Cargo.toml`: deps `herdr-client`, `serde`, `serde_json`; `libc` on unix.
- `sdk/hub/src/protocol.rs`

```rust
pub const PROTOCOL: u32 = 1;

#[derive(Serialize, Deserialize)] pub struct Model {
    pub version: u64,
    pub active: Option<String>,          // session key
    pub hosts: Vec<HostState>,
    pub sessions: Vec<SessionState>,
}
pub struct HostState { pub key: String, pub connected: bool, pub error: Option<String> }
pub struct SessionState {
    pub key: String, pub host: String, pub name: String,
    pub connected: bool, pub error: Option<String>,
    pub protocol: u32,
    pub workspaces: Vec<WorkspaceInfo>,
    pub agents: Vec<AgentInfo>,          // focus comes from AgentInfo.focused; no separate focused pane (rev 2)
    pub socket_path: Option<PathBuf>,    // local sessions only; herdr-micro scripts need it
}

// Every connection opens with exactly one of these and keeps that role (rev 2).
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum ClientMessage {
    Subscribe { protocol: u32 },                              // streaming role: one writer, model then updates
    Call { protocol: u32, id: u64, session: String, method: String, params: Value },   // rpc role: one reply, then close
    Notify { protocol: u32, socket_path: PathBuf, event: Option<Value> },              // hook role: no reply
}
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum ServerMessage {
    Hello { protocol: u32, model: Model },       // first message on a Subscribe connection
    Session { version: u64, session: SessionState },
    SessionRemoved { version: u64, key: String },
    Host { version: u64, host: HostState },
    Active { version: u64, key: Option<String> },
    Reply { id: u64, result: Value },            // exactly one of Reply / ReplyError per Call (rev 2)
    ReplyError { id: u64, error: String },
    Error { error: String },                     // protocol mismatch or malformed first message, then close
}
```

Framing is `herdr_client::ndjson::read_frame`, which already bounds a frame at
1 MiB; a full model for the current eight sessions is about 200 KB, so no new
bound and no size test (rev 2).

- `sdk/hub/src/client.rs`: `HubClient`
  - `subscribe(timeout) -> Result<(Stream, Model)>`: sends `Subscribe`, reads
    `Hello`, exact protocol match, returns the initial model and a `Stream`
    whose `next()` yields `ServerMessage`s and whose `as_fd()` lets apps
    `poll(2)`.
  - `apply(&mut Model, &ServerMessage)`: replace-by-key merge so apps hold a
    `Model` and never merge themselves. Versions are strictly increasing on a
    stream; there is no gap handling because the hub never drops a message for
    a live subscriber (it drops the subscriber, see Phase 1) (rev 2).
  - `call(session, method, params, timeout) -> Result<Value>`: one short-lived
    connection per call: `Call`, then `Reply` or `ReplyError`, then close.
  - `run(on_message: impl FnMut(Result<ServerMessage>))` helper: subscribe,
    stream, reconnect with backoff 250 ms → 5 s, delivers `Err` once per outage
    so apps can enter their offline state immediately.
- `sdk/hub/src/presentation.rs`: `attention_rank(status) -> u8`
  (blocked > done > working > idle > unknown) and
  `attention_order(a, b)` (rank, then `state_change_seq` desc, then
  `terminal_id`). This replaces the three copies in clankerdeck, herdr-micro
  `protocol.rs`, and `herdr-picker-agents`.
- `sdk/hub/src/lib.rs`: re-exports plus `pub fn socket_path() -> PathBuf`.
- Tests: protocol round trip, `apply` replace/remove/active semantics, client
  handshake against a fake listener (same style as `sdk/rust/src/client.rs`
  tests), a `Call` connection that receives no broadcast.

Add `sdk/hub` to workspace members and `[workspace.dependencies]`.

Size: ~400 lines.

## Phase 1: `herdr-hub` daemon

Plugin directory `herdr-hub/` with `Cargo.toml` (lib + bin), `herdr-plugin.toml`,
`src/`. Depends on `herdr-client`, `herdr-hub-client`, `anyhow`, `serde`,
`serde_json`, `libc`, `signal-hook`. No `toml` until Phase 4 (rev 2).

### 1.1 Modules

`src/discover.rs`

- `fn list_sessions(herdr: &Path) -> Result<Vec<LocalSession>>`: runs
  `<herdr> session list --json` with the absolute Herdr path the daemon was
  given (see 1.3), keeps `running == true`, returns `{ name, socket_path }`.
- `fn session_name(socket_path) -> Option<String>`: `…/herdr/herdr.sock` is
  `default`, `…/sessions/<name>/herdr.sock` is `<name>`, anything else is
  `None` and is ignored by every caller (rev 2).
- Reconciliation (rev 2): the hub reruns `list_sessions` every 60 s and on
  every `notify startup`. Sessions that appear get a watcher; sessions that
  are gone and have no live watcher are removed. This is authoritative; hooks
  only make it fast.

`src/watch.rs`: one thread per local session, tagged with a generation.

> **Superseded in Phase 3:** this Phase 1 lifecycle-subscription, debounce, and
> list-refetch loop was shipped as an intermediate architecture. Phase 3 deletes
> it and replaces it with the one canonical 250 ms `session.snapshot` loop
> required by the fork's snapshot-only `client_focused` field.

```rust
const LIFECYCLE: &[&str] = &[
    "pane.created", "pane.closed", "pane.updated", "pane.focused", "pane.moved",
    "pane.exited", "pane.agent_detected",
    "tab.closed",                      // tab.close emits no pane.closed for its panes (rev 2)
    "tab.focused",
    "workspace.created", "workspace.updated", "workspace.renamed", "workspace.moved",
    "workspace.reordered", "workspace.closed", "workspace.focused", "workspace.metadata_updated",
];
// tab.created is omitted: tab creation always emits pane.created.
// worktree.* are omitted: provenance changes emit workspace.updated,
// creation and removal emit workspace.created / workspace.closed.
const DEBOUNCE: Duration = 50 ms;      // quiet period before a refetch
const FLOOR: Duration = 250 ms;        // minimum spacing between refetches
const RESYNC: Duration = 60 s;
const RECONNECT: 250 ms doubling to 5 s; report failure after 30 s (removal is discover's call, see below)
const MIN_PROTOCOL: u32 = 20;
```

Loop per connection attempt:

1. `Client::new(socket).with_timeout(2 s)`; `ping`; reject protocol < 20 with
   an error state on the session.
2. `subscribe(LIFECYCLE)` on one connection (replayed history is harmless: it
   only marks dirty).
3. `session.snapshot()` once → publish `SessionState` (workspaces, agents,
   protocol, connected = true).
4. Wait with `poll(2)` on the subscription fd and a wake pipe. Timeout (rev 2):
   when dirty, the later of the debounce deadline and the floor deadline;
   otherwise the resync deadline. Taking the earlier deadline would spin with
   a zero timeout until the floor passed.
   - Subscription readable: drain all buffered events; mark `dirty`. No event
     payload is read beyond its kind (rev 2).
   - Wake pipe: hub-side notifications (hook events, resync request); mark dirty.
   - Deadline reached while dirty: refetch `agent.list` and `workspace.list`,
     publish, clear dirty. Both lists every time (rev 2): `WorkspaceInfo`
     carries the agent-status rollup, pane and tab counts, the active tab, and
     a label derived from a pane's cwd, none of which emit `workspace.*`
     events, and `workspace.list` costs well under a millisecond.
   - Resync due: refetch both regardless.
   - EOF/error: publish `connected = false`, break to reconnect.
5. Every message to the hub carries the watcher's generation. The hub drops
   messages whose generation is not the current one for that session key, so a
   retiring watcher can never remove or overwrite a replacement (rev 2).
   `Drop` stops the thread through an `AtomicBool` plus wake pipe.

Refetch results replace the session's `agents`/`workspaces` wholesale. No
event payload is ever applied to state.

`src/model.rs`: the hub's authoritative `Model` behind the main thread;
`version` increments per publish; `publish_session`, `remove_session`,
`set_active`, `set_host`, each returning the `ServerMessage` to broadcast.

`src/server.rs`: hub socket.

- `bind_private_socket` + lock file; refuse to run as root; peer uid check.
- Accept thread → per-connection reader thread. The first message fixes the
  connection's role (rev 2):
  - `Subscribe`: the reader hands the connection to the main thread, which in
    one step captures the current model, writes `Hello { model }`, and
    registers the subscriber. Nothing else ever writes to that socket except
    the subscriber's writer thread, which drains a bounded channel (64
    messages). A full channel disconnects that client; it reconnects and gets
    a fresh model. Anything received after `Subscribe` closes the connection.
  - `Call`: the reader forwards the raw method to the session's socket with a
    fresh `herdr_client::Client` (2 s timeout) and writes exactly one `Reply`
    or `ReplyError`, then closes. Unknown session key → `ReplyError`. Remote
    sessions are Phase 4.
  - `Notify`: `startup` → run discovery reconciliation now; `event` → look up
    the session by socket path, create the watcher if the path names a known
    session shape and no watcher exists, wake it. Paths outside the session
    layout are ignored. No reply; the hub closes the connection.

`src/hub.rs`: main loop owning `Model`, the watcher map with generations, the
reconciliation timer, and the server handles. Messages from watchers and the
server arrive on one mpsc channel.

`src/notify.rs`: `herdr-hub notify startup|event`. Reads `HERDR_SOCKET_PATH`
and `HERDR_PLUGIN_EVENT_JSON` from the environment, connects to the hub socket,
sends one `Notify`, exits 0 even if the hub is absent or the path is not a
session. Must finish in well under 100 ms; no logging beyond stderr on error.

`src/service.rs` (macOS only; rev 2):

- `install-service`: copies the running executable to
  `~/Library/Application Support/dev.herdr.hub/herdr-hub.new`, renames it over
  `herdr-hub`, resolves the absolute Herdr binary (from `HERDR_BIN_PATH` when
  invoked by a hook, otherwise from `PATH`; fails if neither resolves), writes
  `~/Library/LaunchAgents/dev.herdr.hub.plist` with `ProgramArguments:
  [<copied path>, serve]`, `EnvironmentVariables: { HERDR_BIN_PATH: <absolute> }`,
  `RunAtLoad`, `KeepAlive`, stdout/stderr to `~/Library/Logs/herdr-hub/`, then
  `launchctl bootout` (if loaded) and `bootstrap`. The daemon reads Herdr's
  path only from that variable.
- `ensure`: idempotent. Compares the installed copy's bytes directly with the
  invoking executable and runs
  `install-service` only when they differ or nothing is installed. This is what
  the plugin `[[startup]]` hook runs, so a rebuilt plugin replaces the service
  on the next session start; `herdr-hub install-service` does it immediately.
- `uninstall-service`: `bootout`, remove the plist, the copied executable, and
  the Application Support directory (logs stay).
- On Linux these subcommands exit with an explanatory error; `ensure` is a
  no-op there.

`src/main.rs` subcommands: `serve` (foreground), `status` (subscribe, print
model summary as JSON, exit 1 if unreachable), `doctor` (Herdr binary
resolvable, plugin linked and enabled via `plugin.list`, hub reachable,
LaunchAgent loaded and pointing at the installed copy), `notify`, `ensure`,
`install-service`, `uninstall-service`, `dump` (print the full model once).

Logging: `open_rotating_log` from `herdr-client`, file
`~/Library/Logs/herdr-hub/hub.log`.

### 1.2 Plugin manifest

```toml
id = "gjermundgaraba.herdr-hub"
name = "Herdr Hub"
version = "0.1.0"
min_herdr_version = "0.8.2"
description = "One agent model for every Herdr dashboard"
platforms = ["macos", "linux"]

[[build]]
command = ["cargo", "build", "--release", "--locked", "-p", "herdr-hub"]

[[build]]
command = ["mkdir", "-p", "bin"]

[[build]]
command = ["install", "-m", "750", "../target/release/herdr-hub", "bin/.herdr-hub.new"]

[[build]]
command = ["mv", "-f", "bin/.herdr-hub.new", "bin/herdr-hub"]

[[startup]]
command = ["bin/herdr-hub", "ensure"]     # installs or refreshes the service, then notifies

[[events]]
on = "pane.agent_status_changed"
command = ["bin/herdr-hub", "notify", "event"]

[[actions]]
id = "status"
title = "Show Herdr Hub status"
command = ["bin/herdr-hub", "status"]

[[actions]]
id = "doctor"
title = "Check Herdr Hub setup"
command = ["bin/herdr-hub", "doctor"]
```

`ensure` ends by sending `notify startup`. The staged `bin/` copy follows the
herdr-micro manifest so a `cargo build` never truncates a binary the hooks are
about to execute.

`pane.agent_detected`, `pane.created`, `pane.closed` arrive through the socket
subscription; the hook is only for the one event the socket cannot deliver
cheaply.

The preceding subscription behavior is the Phase 1 checkpoint. Phase 3 removes
the subscription and uses the hook only to wake the authoritative snapshot loop
early.

### 1.3 Nix and workspace

- `flake.nix`: add `herdr-hub = { sourceRoots = [ "herdr-hub" "sdk/hub" "sdk/rust" ]; alsoBin = true; }`
  and teach `buildPlugin` that `alsoBin` installs the plugin root as today
  plus `bin/<binary>` symlinks. Today a package is either `binOnly` or a plugin
  root, and the relay in Phase 4 needs `herdr-hub` on `PATH` on the remote host
  as well as a linkable plugin root (rev 2).
- Root `Cargo.toml`: add `herdr-hub` and `sdk/hub` members.
- `README.md` table row.

### 1.4 Tests

- `herdr-hub/src/watch.rs` unit tests against a fake Herdr listener (as
  `herdr-micro/src/herdr.rs` does today): snapshot then event → exactly one
  refetch of both lists after debounce; burst of 50 events → refetches respect
  the floor and the loop never wakes with a zero timeout (assert on the
  computed timeouts); EOF → `connected = false` then reconnect; protocol 19 →
  error state; replayed `pane.focused` history changes nothing but `dirty`.
- `hub.rs`: a stale-generation removal is dropped; reconciliation adds a
  session missed by hooks and removes one whose socket vanished.
- `server.rs`: `Subscribe` gets `Hello` then strictly increasing versions with
  no gap across a concurrent publish; a `Call` connection never receives a
  broadcast; slow subscriber disconnect; `Call` forwarding to a fake session
  socket with one `Reply`; `Notify` waking the right watcher and creating a
  missing one; a `Notify` with a custom socket path is ignored.
- `herdr-hub/scripts/measure-herdr-cpu.py`: the cumulative-CPU-delta script
  used for the measurements in the design doc, so the before/after numbers are
  reproducible.

### 1.5 Verification

```sh
cargo build --release --locked -p herdr-hub
cargo test --workspace
herdr plugin link herdr-hub
herdr-hub/bin/herdr-hub install-service
herdr-hub/bin/herdr-hub doctor
herdr-hub/bin/herdr-hub dump | python3 -m json.tool | head
launchctl print gui/$(id -u)/dev.herdr.hub | grep -A2 environment
python3 herdr-hub/scripts/measure-herdr-cpu.py --pid <default server pid> --baseline 30
```

Expected: `dump` lists every running session with agents; changing an agent's
state in a pane updates `dump` within ~150 ms; stopping and restarting a named
session removes and re-adds it without a hub restart; the default server's
baseline CPU is unchanged by the hub alone (the pollers are still running
until Phase 2).

Size: ~1,600 lines including tests.

## Phase 2: apps onto the hub

Both apps switch in the same phase because consumer-owned direct polling only
disappears when both are done. The required hub-owned snapshot loop arrives in
the revised Phase 3. Order: clankerdeck first (smaller), then herdr-micro.

### 2.1 clankerdeck

Add `herdr-hub-client` dependency.

Delete:

- `src/herdr.rs` entirely except `assign_slots`, `resize_slots`, `agent_label`,
  which move to `src/slots.rs` and take `&AgentInfo` from `herdr-hub-client`
  and `attention_order` from the sdk. `session_socket`, `rpc`, `rpc_frame`,
  `parse_snapshot_response`, `parse_sessions`, `HerdrClient`, `read_output`,
  `priority`, `compare`: gone.
- `daemon.rs`: `SnapshotCommand`, `snapshot_worker`, `MAX_SNAPSHOT_BACKOFF`,
  `Event::Snapshot`, `Daemon::snapshot`, `Daemon::session`,
  `cycle_session` over `herdr session list`.

Add:

- `Daemon::model: Option<Model>` and `Daemon::filter: SessionFilter`
  (`All | Session(key)`), `Event::Hub(Result<ServerMessage>)`.
- `hub_worker` thread: `HubClient::run` forwarding messages to the daemon
  channel; the daemon applies them with `HubClient::apply`. Every hub message,
  including the `Err` that marks an outage, sets `render_dirty`; the existing
  per-key and touch-strip signatures already suppress redundant frames, and
  render runs at animation cadence whenever an agent is animated anyway (rev 2).
- `visible_agents()` = all agents across `model.sessions` matching the filter,
  keyed by `(session key, terminal_id)`; `slots` store that key. Workspace
  labels are looked up through the agent's own `SessionState`, never through a
  flat map of workspace ids, which collide across sessions (rev 2).
- Actions: `action_worker` calls `HubClient::call(session_key, method, params)`;
  `Action::CycleSession` cycles `filter` through `All` then each connected
  session key; `focus-slot` resolves the slot's session key.
- Config: `herdr.session` and `slotCount` become
  `"herdr": { "slotCount": 8, "sessions": [] }` where `sessions` is an optional
  allow-list of session keys (empty means all). `doctor` checks the hub.
- Render: `ButtonSignature.session` → the agent's session key;
  `TouchSignature.session` → filter label; the touch strip shows the filter name
  and counts across the filtered set; key subtitle keeps workspace label, so add
  a short session badge when the filter is `All` and more than one session is
  connected.
- README and `clankerdeck.json` updated.

Verification: `cargo test`, `cargo build --release`, `clankerdeck push`,
restart the LaunchAgent, keys show agents from several sessions, dial cycles the
filter, pedal actions still reach the right pane, stopping the hub blanks the
deck to offline within a second, and the default server's CPU drops by ~9
points on the measurement script.

### 2.2 herdr-micro

Delete:

- `herdr.rs`: `SessionWorker`, `SessionUpdate`, `spawn_session_worker`,
  `follow_session`, `current_snapshot`, `validate_snapshot`,
  `discover_sessions`, `parse_sessions`, `SNAPSHOT_POLL_INTERVAL`,
  `MIN_HERDR_PROTOCOL`. `run_command_with_timeout` and its helpers move to
  `process.rs` (still used by script actions).
- `daemon/reconcile.rs`: `refresh_sessions`, `sync_selected_worker`,
  `apply_session_update`, `Selected.pending_agents`, `Selected.pending_slots`,
  `Selected.generation`, `State.no_sessions_at`, `NO_SESSIONS_SHUTDOWN`.
- `daemon/mod.rs`: `SESSION_REFRESH_INTERVAL`, `SESSION_RETRY_INTERVAL`,
  `session_workers`, `worker_generation`, `sessions_due`, the
  `session_update_rx` drain.
- `dispatch.rs`: the `current_snapshot` before every action.

Add:

- `hub.rs`: a thread running `HubClient::run`, forwarding messages on a channel
  drained by the daemon loop; `State.model: Model`. An `Err` from the stream
  (hub outage) does what `SessionUpdate::Unavailable` does today: revoke
  routing, clear agents and slots, blank lighting, until the next successful
  model arrives (rev 2).
- `State.sessions` derives from `model.sessions` (`name`, `key`). Phase 3 makes
  the hub's active-session value authoritative, so Micro needs no independent
  client-focus integration.
- `apply_selected_agents` reads the selected session's `agents` from the model.
- Actions: `actions.rs` functions take a `Caller` closure (`method, params ->
  Result<Value>`) backed by `HubClient::call(session_key, …)` instead of
  `herdr_client::Client`. `execute_work` revalidates a binding's target with
  `agent.get` (`{"target": pane_id}`) and requires both the captured
  `terminal_id`/`pane_id`/`agent` identity and `focused == true` on the
  returned record, which is the same guarantee the focused-agent comparison
  gives today (rev 2). `FocusSlot` checks identity only, as today.
  Scripts still receive `HERDR_SOCKET_PATH` from
  `SessionState.socket_path`; remote sessions leave it empty and script actions
  refuse them.
- `doctor.rs`: checks the hub instead of `herdr session list`.
- `docs/micro-bridge.md`: replace the "Herdr action scheduling" paragraph.

Verification: `cargo test`, rebuild through the manifest, `herdr-micro start`,
lighting follows the client-focused session's agents, prompt/submit/diff
actions work, a prompt queued while focus moves to another pane is ignored,
stopping the hub blanks the lighting, and the default server's CPU drops by the
second ~9 points.

Size: ~-900 lines net across both apps.

## Phase 3: active session from Herdr

The production Herdr fork at commit `85ad1d77` adds `client_focused` to
`session.snapshot`. That is the authoritative client-focus signal. The current
extension API does not emit an event when this field changes, so the hub runs
one canonical 250 ms snapshot loop and publishes `Active { key }` when the
focused session changes. This is the single production path, not a compatibility
fallback. It replaces the two consumer-owned 4 Hz snapshot loops while keeping
the same maximum focus-detection latency.

Then:

- Bump the hub protocol to 3 and add required
  `SessionState.client_focused: Option<bool>`. Local sessions publish the fork's
  snapshot value; imported remote sessions publish `None` because only local
  client focus may drive the local hub's `active` value.
- The local watcher reads `client_focused` from each snapshot and reports it to
  the hub. The hub chooses the focused local session, or `None` when no Herdr
  client is focused, and publishes only changes.
- Delete `LIFECYCLE`, the subscription connection, debounce/floor/resync
  deadlines, and the `agent.list`/`workspace.list` refetch path from Phase 1.
  Each watcher reads and validates one complete `session.snapshot` every 250 ms;
  an event notification may wake that same loop early.
- herdr-micro deletes its independent focus and session-mapping state. Selected
  session = `model.active`; layer selection and routing readiness follow
  `Active` messages; `DispatchLease::ensure` compares the lease's session key
  against the current `model.active`. `refresh_owner` stays (device gate,
  cheap).
- clankerdeck marks the active session's agents (a corner badge) and gains a
  filter value `Active`.

Verification: moving client focus between two Herdr sessions updates
`herdr-hub dump` `active` within 250 ms and herdr-micro lighting follows. Hub
startup and `doctor` require no macOS privacy grants.

## Phase 4: remote hosts

- `herdr-hub/src/config.rs` and the `toml` dependency arrive here:
  `~/.config/herdr-hub/config.toml` with `[[hosts]] key = "workbox", ssh = "workbox"`.
- `herdr-hub relay`: connects to the local hub socket if present, otherwise
  runs `hub::run` in-process with the hub socket bound (so that host's hooks
  reach it). Either way it bridges `ServerMessage`s to stdout and `Call`s from
  stdin, NDJSON, framed with `herdr_client::ndjson`. One writer thread owns
  stdout (rev 2).
- `herdr-hub/src/remote.rs` in the local hub, one connection per `[[hosts]]`
  entry: spawn `ssh -o BatchMode=yes -o ServerAliveInterval=15 <ssh> herdr-hub relay`
  and apply these rules to what comes back (rev 2):
  - Accept only `Session` and `SessionRemoved` messages whose session `host`
    is `local`; rewrite `host` and `key` to this host's key and clear
    `socket_path`. Drop everything else from the relay, including `Active`,
    `Host`, and sessions the remote hub itself imported from further hosts.
    The local hub owns `active` and `hosts`.
  - `Call` ids on the relay stream are allocated by the local hub as
    `(connection epoch, counter)`; the epoch increments on every reconnect so a
    late reply from a dead connection can never match a live request. Client
    ids are mapped back on reply.
  - Reconnect with backoff 1 s → 30 s. On loss: mark the host disconnected,
    remove its sessions, fail every in-flight call for that host with
    `ReplyError`.
- `herdr-hub doctor` runs `ssh <host> herdr-hub --version` for each host.
- Install on the remote host, documented in `herdr-hub/README.md`:
  `nix profile install github:gjermundgaraba/herdr-plugins#herdr-hub`, which
  puts `herdr-hub` on `PATH` and a plugin root at `~/.nix-profile/herdr-hub`
  (the `alsoBin` package from 1.3), then `herdr plugin link ~/.nix-profile/herdr-hub`.
  Without Nix: clone, `cargo build --release -p herdr-hub`, `herdr plugin link
  herdr-hub` (runs the manifest build, which stages `bin/`), and symlink
  `herdr-hub/bin/herdr-hub` into `~/.local/bin`. No `cargo install` path (rev 2).
- Apps need no change: sessions simply appear with another host key; scripts
  in herdr-micro refuse sessions without `socket_path`.

Verification: with one remote host configured, `dump` shows both hosts,
clankerdeck slots include remote agents, `focus-slot` on a remote agent focuses
the pane on the remote server, killing the SSH connection marks the host
disconnected and the sessions disappear, restoring it brings them back, and a
remote hub with its own hosts configured does not leak them into the local
model.

Size: ~600 lines.

## Phase 5: picker onto the hub

- Protocol bump: `SessionState.tabs: Vec<TabInfo>`. At the Phase 5 checkpoint it
  was kept current through lifecycle invalidation and `tab.list`; the revised
  Phase 3 path instead replaces tabs from every authoritative snapshot.
- `sdk/picker`: `serve(items: Fn(&Model) -> Vec<Item>)` streams from
  `HubClient::run`; delete `subscribe_events`, `lifecycle_subscriptions`,
  `pane_ids`, and the `herdr-client` subscription path. Item ids become
  `"<session key>:<pane id>"` and `"<session key>:<workspace id>"`, and every
  item `value` carries `session`. `submit` routes through `HubClient::call`
  with that session; the `HERDR_SOCKET_PATH` path is deleted (rev 2).
- `herdr-picker-agents`: items across sessions with a session badge, ordered by
  `attention_order`.
- `herdr-picker-workspaces`: workspaces from the model.
- `flake.nix` sourceRoots for the three picker packages gain `sdk/hub`.

Verification: picker opens with the same list as before, updates live, selecting
an agent in another session focuses the right server, and the default server
shows no `pane.get` load while the picker is open.

Size: ~-100 lines net.

## Cross-cutting

- Protocol changes between phases bump `PROTOCOL`; clients refuse mismatches
  and reconnect after the hub restarts. No compatibility handling.
- Server restart of a Herdr session: the watcher sees a snapshot error and reconnects, the
  new server's `[[startup]]` hook triggers reconciliation, and the periodic
  reconciliation covers a lost hook; generations make these paths converge
  on one watcher per socket path.
- Herdr upgrade that raises the protocol above 20 without breaking the used
  methods: nothing to do. If the `session.snapshot` shape changes,
  `MIN_PROTOCOL` moves and the types follow.
- Line-count expectation at the end: roughly +3,100 (hub, sdk, tests, relay,
  active) and -2,000 (both apps, picker sdk), with two consumer-owned 4 Hz
  snapshot loops replaced by the hub's single canonical loop.

## Order and checkpoints

1. Phase 0 and Phase 1 together (one PR): hub running as a LaunchAgent next to
   the existing pollers, `dump` verified, CPU script committed.
2. Phase 2 (one PR per app): measured CPU drop on the default server after
   each.
3. Phase 3 after the fork's `client_focused` contract is recorded here.
4. Phase 4 when a remote host exists to test against.
5. Phase 5 last; at that checkpoint its old direct polling ran only while open.

## Implementation status

### 2026-09-03 — Phases 0 and 1

- Shipped `sdk/hub`, the `herdr-hub` daemon and plugin, LaunchAgent lifecycle,
  flake packaging, documentation, and `scripts/measure-herdr-cpu.py`.
- Default-session baseline before implementation: PID 3329 used 2.66 CPU seconds
  over 30.012 seconds (8.86%). With the hub installed alongside both existing
  pollers: 2.51 CPU seconds over 30.013 seconds (8.36%). This run was below the
  earlier observed 22–29% range, so later cutover comparisons use these saved
  measurements rather than that historical range.
- Live verification passed: plugin linked, installed-copy LaunchAgent running,
  `doctor` green, and `dump` reported 7 connected sessions and 78 agents.
  Workspace tests, the release build, and strict Clippy passed. Nix evaluation
  was not available on this host because `nix` is not installed.
- Deviations: none.

### 2026-09-03 — Phase 2a (clankerdeck)

- Shipped the hub-only clankerdeck path in its separate repository as commit
  `3ea6004`: multi-session composite slots and filters, hub-routed calls,
  hub-aware rendering/doctor, and deletion of the direct snapshot poller.
- Live verification passed after `push` and `install-service`: the new
  `net.garaba.clankerdeck` process claimed both configured devices, observed a
  forced hub disconnect, and reconnected after the hub returned with 7
  sessions and 78 agents. All 25 tests, strict Clippy, and the release build
  passed.
- Default-session CPU after this cutover: PID 3329 used 2.18 CPU seconds over
  30.010 seconds (7.26%), down 1.10 percentage points from the 8.36% Phase 1
  alongside-poller checkpoint and 1.60 points from the 8.86% initial baseline.
- Deviation/clarification: Phase 2 did not define which session should receive
  non-slot actions while the display filter is `all`; focus is session-local,
  so it cannot identify the frontmost session before Phase 3. The decisions
  table now records the deterministic interim `local/default` rule. Slot focus
  always uses its composite session/terminal identity.

### 2026-09-03 — Phase 2b (herdr-micro)

- Shipped the hub-only Micro bridge path: one pushed model, hub-routed calls,
  `agent.get` identity/focus revalidation, local-only script sockets, and
  deletion of session discovery and the 4 Hz snapshot workers. At this
  historical checkpoint Micro still carried its previous client-focus
  integration; the revised Phase 3 removes it. The daemon protocol is 6 so neither protocol-4 nor
  protocol-5 pollers can survive the cutover.
- Workspace tests, strict Clippy, the release build, and the exact manifest
  staging/codesigning flow passed. Live verification passed with the worktree
  plugin linked: doctor was green, 7 hub sessions were visible, 6 interactive
  sessions were mapped, and the headless `gcul` session was isolated without
  disabling the others. `cliamp` selected Layer 2 and rendered 13 agents.
- A forced hub outage blanked routing, agents, and all 6 slots within one
  second. The disconnected Micro daemon used 0.08 CPU seconds over 5.017
  seconds (1.59%), then remapped and returned to ready after the hub was
  restored without a Micro restart.
- Default-session CPU after this cutover: PID 3329 used 0.78 CPU seconds over
  30.017 seconds (2.60%), down 4.66 percentage points from the 7.26% Phase 2a
  checkpoint and 6.26 points from the 8.86% initial baseline.
- Deviation: the planned second ~9-point drop could not occur from the saved
  7.26% checkpoint; the measured drop was 4.66 points. Live testing also
  exposed one running headless session. The revised Phase 3 no longer needs
  consumer-side session mapping.

### 2026-09-04 — Phase 3 (active session)

- Revised to make the production Herdr fork's `session.snapshot.client_focused`
  field authoritative. Fork commit `85ad1d77` exposes the field only through
  snapshots; the current extension API has no corresponding focus event.
- The hub therefore owns one canonical 250 ms snapshot loop and publishes
  `model.active`; Micro and clankerdeck consume that pushed identity and have no
  independent focus integration. This is the sole path, not a compatibility
  fallback.
- Removed the superseded platform-specific focus path and its privacy-permission
  checks. The earlier implementation checkpoint was never the final design.
- Live verification passed with the worktree plugin linked and the copied
  `dev.herdr.hub` LaunchAgent running hub 0.2.0. `doctor` was green; `dump`
  reported 8 connected local sessions and 66 agents, included the required
  `client_focused` field, and contained no legacy `terminal` field. Successive
  live dumps followed the sole focus claim across `werk`, `default`, and
  `remote` at the hub's 250 ms sampling cadence.
- Micro replaced its protocol-8 daemon automatically with protocol 9, consumed
  all 8 sessions, selected `werk`, and reported ready routing with 6 agents.
  Its private live config was migrated by setting the removed joystick scroll
  bindings to `null`; all other bindings were preserved.
  Clankerdeck's protocol-3 build was deployed through its normal `push` and
  `install-service` flow; doctor reported 8 sessions and 2/2 devices. Its
  clean-break fixture and documentation update is commit `dcb4e33` in the
  separate repository.
- A same-workload default-server sample immediately before the new hub was
  installed used 0.95 CPU seconds over 30.014 seconds (3.17%); after cutover it
  used 1.01 seconds over 30.013 seconds (3.37%), an increase of 0.20 percentage
  points for the hub's canonical default-session poll. This workload had 33
  panes and 21 agents in the default session, so the older 2.60% and 2.70%
  checkpoints are not treated as a paired comparison.
- The complete serialized workspace tests, strict workspace Clippy, locked
  release build, and exact affected manifest build/staging/codesigning flows
  passed. Two independent clean-break reviews found and closed ambiguity
  recovery on focus loss and session removal; final review was clean.
- Deviation from the earlier branch implementation: after rebasing onto main,
  the superseded terminal-specific Phase 3 path was deleted in favor of the
  production fork's `client_focused` contract. There is no deviation from the
  revised decisions table and no compatibility path remains.

### 2026-09-04 — Phase 4 (remote hosts)

- Shipped strict `[[hosts]]` configuration, one event-driven SSH supervisor per
  host, the filtered NDJSON relay, epoch-scoped routed calls, host/session
  teardown on disconnect, capped reconnect backoff, and remote version checks.
  Relay stdin and every cross-thread event path are bounded; a stopped hub can
  cancel a backpressured relay write and join all workers.
- Remote Login is disabled on this Mac (localhost port 22 refused the
  connection), so the requested localhost fallback used a temporary
  SSH-equivalent wrapper that checked the exact production arguments before
  launching the staged `herdr-hub relay`. `dump` showed 7 local plus 7
  `loopback` sessions, no recursively imported sessions, and a routed
  `agent.list` call returned 20 agents. Killing the relay marked `loopback`
  disconnected and removed all 7 imported sessions; restoring it reconnected
  after backoff and restored all 7. A relay against the healthy installed hub
  also emitted its 7-session `Hello` with `herdr` deliberately absent from
  `PATH`, proving connect-first startup.
- At this pre-focus-revision checkpoint, both the foreground hub and relay
  sampled at 0.0% CPU while idle. All 67 hub tests, the complete workspace test
  suite, strict workspace Clippy, the release build, and the exact manifest
  staging flow passed. The rebuilt signed
  LaunchAgent is running from its installed copy with 7 local sessions and 79
  agents; no temporary host configuration remains.
- Deviations: no code or design deviations. A true remote display and SSH
  server were unavailable, so live verification used the explicitly requested
  localhost subprocess path; exact SSH construction and version checking are
  covered by tests.

### 2026-09-04 — Phase 5 (picker)

- Shipped hub protocol 2 with required tab state and event-driven tab refetch,
  then moved the picker SDK, agent provider, and workspace provider onto the
  pushed multi-session hub model. Item ids are composite, every submit carries
  its owning session, routed calls use the hub, and the direct Herdr snapshot
  and subscription path is deleted.
- Live config checks passed. The providers reported 79 agents and 41 workspaces
  across all 7 connected sessions with composite ids and routed values. A
  submit for focused pane `w7:p7` through `local/cliamp` succeeded and the hub
  confirmed that pane on the named session. With both providers held open, the
  default server (PID 3329) used 0.81 CPU seconds over 30.025 seconds (2.70%),
  effectively unchanged from the 2.60% post-Phase-2b checkpoint and with no
  picker runtime references to `pane.get`, snapshots, direct Herdr sockets, or
  event subscriptions. At that pre-focus-revision checkpoint, a simultaneous
  30-second stack sample found none of the Herdr 0.8.2 `pane.get` or
  `session.snapshot` execution paths. The revised hub now intentionally owns the
  required snapshot path, so final CPU is recorded separately in Phase 3.
- Rebuilt and reinstalled the hub through its manifest flow. Micro daemon
  protocol 8 forced an automatic replacement of the running protocol-7 client,
  then repopulated all 7 sessions without a manual stop. Clankerdeck rebuilt,
  pushed its config, reinstalled `net.garaba.clankerdeck`, passed doctor with
  2/2 devices, and committed its protocol-2 fixture as `c6f6645`.
- The complete serialized workspace test suite, strict workspace Clippy, full
  release build, and exact affected manifest staging/codesigning flows passed.
  Ordinary parallel runs hit the existing one-second process timing tests in
  Micro and `herdr-picker`; the Micro test passed immediately in isolation and
  every affected test passed in the serialized complete run. Nix validation
  remains unavailable because `nix` is not installed.
- Deviations: none. The unsupported `agents-unseen` variant was deleted in the
  clean break: Herdr 0.8.2 has no `pane.mark_unseen`, and its local Python submit
  could not safely act on the new multi-session hub results.
