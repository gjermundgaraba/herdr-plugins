# Herdr → six RGB Agent Keys: state-pipeline evidence and design

Accessed: **2026-07-25**  
Target: Herdr **v0.7.5**, protocol **17**  
Scope: official Herdr documentation, the v0.7.5 schema/source, and the open upstream event-replay issue.

> **Historical design note:** the working `herdr-micro` implementation uses
> one-second `herdr agent list` polling and sticky slots. The snapshot/event
> design below was not needed; revisit it only if polling proves insufficient.

## Recommendation

Build the RGB companion as a **snapshot-authoritative cache with event-driven invalidation**:

1. One collector owns each configured `(host, Herdr session)` source.
2. It subscribes to topology/focus events and to one status subscription per recognized-agent pane.
3. Every event only marks the source dirty; it never mutates the LED model directly.
4. A coalesced `session.snapshot` refresh atomically replaces the source cache.
5. A persistent allocator keeps `(source, terminal_id)` in the same one of six slots until that terminal disappears or the user explicitly changes pages.

This is more reliable than applying events incrementally because:

- `pane.agent_status_changed` subscriptions require a specific `pane_id`; there is no documented wildcard. [Event subscription schema](https://github.com/ogulcancelik/herdr/blob/v0.7.5/src/api/schema/events.rs)
- focusing can turn several agents from `done` to `idle` without emitting a corresponding general status event;
- pushed events have no public sequence, timestamp, resume cursor, or snapshot generation;
- general lifecycle subscriptions currently replay retained history to new subscribers. [Issue #1270](https://github.com/ogulcancelik/herdr/issues/1270)

Herdr explicitly describes `session.snapshot` as the bootstrap for clients that keep a local runtime cache and says to call it again after reconnect or suspected staleness. [Socket API: snapshot](https://herdr.dev/docs/socket-api/#raw-methods)

## Evidence and version anchor

- The v0.7.5 release is protocol 17. The installed binary can return its matching schema through `herdr api schema --json`. [v0.7.5 release](https://github.com/ogulcancelik/herdr/releases/tag/v0.7.5), [schema documentation](https://herdr.dev/docs/socket-api/#schema)
- The claims below were checked against the tagged v0.7.5 schema and source. Later clients must validate `ping.version`, `ping.protocol`, and their own generated schema before relying on exact fields. [Protocol stability](https://herdr.dev/docs/socket-api/#protocol-stability)
- The transport is newline-delimited JSON over a Unix-domain socket or Windows named pipe. A subscription connection accepts one initial request, acknowledges it, and then remains a pushed-event stream. [Socket transport](https://herdr.dev/docs/socket-api/#socket-transport), [server implementation](https://github.com/ogulcancelik/herdr/blob/v0.7.5/src/api/server.rs)

## Authoritative bootstrap: `session.snapshot`

Request:

```json
{"id":"rgb_boot_1","method":"session.snapshot","params":{}}
```

Response shape:

```json
{
  "id": "rgb_boot_1",
  "result": {
    "type": "session_snapshot",
    "snapshot": {
      "version": "0.7.5",
      "protocol": 17,
      "focused_workspace_id": "w1",
      "focused_tab_id": "w1:t1",
      "focused_pane_id": "w1:p2",
      "workspaces": [],
      "tabs": [],
      "panes": [],
      "layouts": [],
      "agents": []
    }
  }
}
```

The three focused IDs are optional. The arrays contain full `WorkspaceInfo`, `TabInfo`, `PaneInfo`, `PaneLayoutSnapshot`, and `AgentInfo` records. [Session schema](https://github.com/ogulcancelik/herdr/blob/v0.7.5/src/api/schema/session.rs), [snapshot construction](https://github.com/ogulcancelik/herdr/blob/v0.7.5/src/app/api/session.rs)

### Agent fields relevant to RGB state

`snapshot.agents[]` supplies:

- identity/location: `terminal_id`, `workspace_id`, `tab_id`, `pane_id`;
- identity labels: optional `name`, `agent`, `display_agent`, `title`, and terminal-title fields;
- state: `agent_status`, `focused`, `launch_pending`, `interactive_ready`, `state_change_seq`;
- presentation: `state_labels`, `tokens`;
- optional session/cwd data;
- `revision`.

The exact record is defined by `AgentInfo`. [Agent schema](https://github.com/ogulcancelik/herdr/blob/v0.7.5/src/api/schema/agents.rs)

Important revision limits:

- There is **no snapshot-level revision or generation**.
- Event envelopes do **not** expose the server's internal event sequence.
- `AgentInfo.revision` is copied from its pane terminal revision. Source inspection shows that status and seen changes are not a general increment contract for this field, so it cannot order all agent changes. [Agent construction](https://github.com/ogulcancelik/herdr/blob/v0.7.5/src/app/agents.rs), [pane construction](https://github.com/ogulcancelik/herdr/blob/v0.7.5/src/app/creation.rs)
- `state_change_seq` is more useful for within-server recency: Herdr increments one session-global counter when the underlying semantic terminal state actually changes. It does not increment for the derived `done → idle` seen transition. It starts at zero with app state, so do not compare it across sessions, hosts, or server generations. [state-change update](https://github.com/ogulcancelik/herdr/blob/v0.7.5/src/app/actions.rs)

Use an RGB-side `connection_epoch` and an atomic snapshot replacement. Do not invent a total order from Herdr's `revision` fields.

## Exact event contracts

### Naming has two forms

Subscription selectors use dotted names such as:

```json
{"type":"pane.focused"}
```

General pushed lifecycle envelopes use snake-case enum values:

```json
{
  "event": "pane_focused",
  "data": {
    "type": "pane_focused",
    "pane_id": "w1:p2",
    "workspace_id": "w1"
  }
}
```

The dedicated pane-status subscription is different: its pushed `event` is literally `pane.agent_status_changed`, and its untagged `data` object has no `type` field. This follows the two separate `EventEnvelope` and `SubscriptionEventEnvelope` schemas. [Event schemas](https://github.com/ogulcancelik/herdr/blob/v0.7.5/src/api/schema/events.rs), [bundled JSON Schema](https://github.com/ogulcancelik/herdr/blob/v0.7.5/docs/next/api/herdr-api.schema.json)

### Events needed by the RGB collector

| Subscription selector | Relevant payload |
| --- | --- |
| `workspace.created`, `.updated`, `.metadata_updated` | full `workspace` |
| `workspace.renamed`, `.focused` | `workspace_id` plus label where relevant |
| `workspace.moved` | `workspace_id`, `insert_index`, full ordered `workspaces` |
| `workspace.closed` | `workspace_id`, optional final `workspace` |
| `tab.created` | full `tab` |
| `tab.closed`, `.renamed`, `.focused` | `tab_id`, `workspace_id`, plus label where relevant |
| `tab.moved` | `tab_id`, `workspace_id`, `insert_index`, full ordered `tabs` |
| `pane.created`, `.updated` | full `pane` |
| `pane.closed`, `.focused`, `.exited` | `pane_id`, `workspace_id` |
| `pane.moved` | old pane/workspace/tab IDs, full new `pane`, optional created/closed container IDs |
| `pane.agent_detected` | `pane_id`, `workspace_id`, optional `agent`, `released`, optional `final_status` |
| `layout.updated` | full layout for one workspace/tab |

The exact variants and optional fields are in `EventData`. [Event payload source](https://github.com/ogulcancelik/herdr/blob/v0.7.5/src/api/schema/events.rs)

The status selector is pane-scoped:

```json
{
  "type": "pane.agent_status_changed",
  "pane_id": "w1:p2"
}
```

Omit `agent_status` to receive every status or presentation change. If a filter is supplied, only that status is emitted; a matching current state can also produce an initial event. The pushed record is:

```json
{
  "event": "pane.agent_status_changed",
  "data": {
    "pane_id": "w1:p2",
    "workspace_id": "w1",
    "agent_status": "blocked",
    "agent": "claude",
    "title": "Reviewing auth",
    "display_agent": "Claude",
    "state_labels": {}
  }
}
```

Optional fields are omitted when absent. There is no `terminal_id`, `tab_id`, `focused`, `state_change_seq`, `revision`, or event cursor in this payload. It also fires when title/display/state-label presentation changes without a semantic status change. [Status subscription implementation](https://github.com/ogulcancelik/herdr/blob/v0.7.5/src/api/subscriptions.rs)

The first stream response is only an acknowledgement:

```json
{"id":"rgb_status_1","result":{"type":"subscription_started"}}
```

Later pushed events do not carry the request ID. The v0.7.5 implementation polls subscriptions at 100 ms intervals; treat that as an implementation detail, not a latency guarantee. [Subscription server loop](https://github.com/ogulcancelik/herdr/blob/v0.7.5/src/api/server.rs)

## Focus, seen, `done`, and `idle`

Herdr's public statuses are `blocked`, `working`, `done`, `idle`, and `unknown`. `done` means the agent is semantically idle but its completion has not been seen; `idle` is the seen form. [Concepts: Agent](https://herdr.dev/docs/concepts/#agent)

The v0.7.5 mapping is:

| Internal state and seen bit | Public `agent_status` |
| --- | --- |
| idle + unseen | `done` |
| idle + seen | `idle` |
| working + either | `working` |
| blocked + either | `blocked` |
| unknown + either | `unknown` |

[Status mapping source](https://github.com/ogulcancelik/herdr/blob/v0.7.5/src/app/api_helpers.rs)

The practical rules are:

1. A non-idle state sets the pane's internal seen bit to true.
2. A completion transition to idle becomes unseen unless it occurs in the active tab while the outer terminal is not known to be unfocused.
3. Focusing a workspace/tab/pane marks **every pane in the active tab** seen, not only the focused pane.
4. `agent.focus` and `pane.focus` perform that mark even when the target was already focused. Outer-terminal focus gained also marks the active tab seen.
5. `AgentInfo.focused` identifies the one logical active pane in that Herdr session. It is not proof that this session's window has global OS focus.

[Seen-state mutation](https://github.com/ogulcancelik/herdr/blob/v0.7.5/src/app/actions.rs), [agent focus](https://github.com/ogulcancelik/herdr/blob/v0.7.5/src/app/agents.rs), [pane focus](https://github.com/ogulcancelik/herdr/blob/v0.7.5/src/app/api/panes.rs), [outer-focus handling](https://github.com/ogulcancelik/herdr/blob/v0.7.5/src/app/runtime.rs)

The internal `seen` boolean is available as an Agent-view filter field but is not present in `AgentInfo`. A client can only observe unseen idle as `agent_status: "done"`. Do not try to infer seen for working, blocked, or unknown agents.

### Why focus must trigger a snapshot

The general focus event contains only IDs. Same-pane API focus and outer-terminal focus gained can mark done agents seen without changing logical focused IDs, and the regular focus-event emitter may therefore have no new lifecycle event to publish. The pane-scoped status subscription compensates by polling `pane.get` and emits when its last status/presentation snapshot differs. A collector still needs to re-read `session.snapshot` to update every affected agent in that tab. [Focus event source](https://github.com/ogulcancelik/herdr/blob/v0.7.5/src/app/api.rs), [status polling fallback](https://github.com/ogulcancelik/herdr/blob/v0.7.5/src/api/subscriptions.rs)

## Ordering and filter semantics

### `agent.list` and `session.snapshot.agents`

Both call the same `collect_agent_infos()` function. The current source traverses:

```text
workspace vector order
  → tab vector order
    → layout.pane_ids() order
      → recognized agent records only
```

[Agent collection source](https://github.com/ogulcancelik/herdr/blob/v0.7.5/src/app/agents.rs), [snapshot construction](https://github.com/ogulcancelik/herdr/blob/v0.7.5/src/app/api/session.rs)

Treat this as structural order for v0.7.5, not a durable identity. Pane moves, tab/workspace moves, layout changes, and server restoration can alter it.

### The Agent-view projection does not order API results

`agent.view.set` changes the built-in Agents UI, indexed focus, and previous/next Agent navigation. The docs explicitly say it does **not** change `agent.list`, detection, notifications, or global attention counts; `session.snapshot.agents` likewise continues to use raw collection order. [Agent view queries](https://herdr.dev/docs/socket-api/#agent-view-queries)

Agent-view filters support:

- boolean composition: `all`, `any`, `not`;
- comparisons: `eq`, `in`, `exists`;
- fields: status, workspace/tab/pane ID, agent, seen, state-change sequence, or a metadata token;
- current-workspace/current-tab context values.

Sorts are stable and evaluated in the declared order. Fields include workspace/tab/pane order, attention, status, agent, seen, state-change sequence, or a token; missing values sort after present values. Effective attention is `blocked > done > working > idle > unknown`. [Agent view documentation](https://herdr.dev/docs/socket-api/#agent-view-queries), [sort implementation](https://github.com/ogulcancelik/herdr/blob/v0.7.5/src/app/agent_view.rs)

The default UI setting is grouped/Space order; priority is optional. A companion must implement its own allocator instead of assuming the visible Herdr sidebar and `snapshot.agents` have the same order. [Agent panel defaults](https://github.com/ogulcancelik/herdr/blob/v0.7.5/src/config/model.rs)

## Reconnect- and replay-safe collector

### Streams

Use two socket streams per source:

**Topology stream:** one general subscription containing workspace/tab/pane lifecycle, focus, move, agent-detection, update, and layout events.

**Status stream:** one `pane.agent_status_changed` selector for every recognized-agent pane in the latest snapshot. A shell becoming an agent emits `pane.agent_detected`, which invalidates the cache and causes the next snapshot/status-stream rebuild. The subscription list is immutable after acknowledgement, so rebuild this stream when the snapshot's agent-pane-ID set changes.

Do not set a status filter.

### Race-tolerant bootstrap

```text
DISCONNECTED
  → connect topology stream and await subscription_started
  → snapshot S1 and validate version/protocol
  → connect status stream for every pane in S1 and await subscription_started
  → snapshot S2
  → atomically publish S2
  → LIVE
```

If the pane set changes:

1. Open and acknowledge a replacement status stream before closing the old one.
2. Take another snapshot.
3. Atomically publish it, then retire the old stream.

Duplicates during overlap are harmless because events are invalidations.

### Dirty/snapshot loop

Per source, maintain:

```text
dirty: boolean
snapshot_in_flight: boolean
connection_epoch: local integer
last_success_at: monotonic time
```

On any event, set `dirty = true`. A single worker:

1. clears `dirty`;
2. requests `session.snapshot` on a separate connection;
3. validates it still belongs to the same connection epoch;
4. atomically replaces the source cache;
5. repeats immediately when an event set `dirty` during the request.

Coalesce bursts for roughly 25–50 ms, but do not run concurrent snapshots for the same source. Add an unconditional reconciliation snapshot every 30 seconds; the interval is a design choice, not a Herdr guarantee.

### Replay and dedupe

The event hub retains at most 512 envelopes and assigns each an internal sequence. General `ActiveEventSubscription` instances currently start at sequence zero, so a new subscriber receives matching retained history. The internal sequence is not serialized, and `events.subscribe` has no cursor/resume parameter. Multiple subscription entries are polled in request-array order, with each entry yielding at most one event per 100 ms sweep, so the output is not a public globally ordered log. [Event hub](https://github.com/ogulcancelik/herdr/blob/v0.7.5/src/api/event_hub.rs), [subscription initialization and polling](https://github.com/ogulcancelik/herdr/blob/v0.7.5/src/api/subscriptions.rs), [subscription server loop](https://github.com/ogulcancelik/herdr/blob/v0.7.5/src/api/server.rs), [open replay bug](https://github.com/ogulcancelik/herdr/issues/1270)

Therefore:

- never perform work directly from a lifecycle event;
- never count events as transitions;
- never apply an old create/close/status envelope to the cache;
- use a replay burst only to request one fresh snapshot;
- emit an RGB/USB update only when the final six-slot render fingerprint changed.

A useful render fingerprint is:

```text
slot number
+ source key
+ terminal_id
+ agent_status
+ focused
+ launch_pending
+ interactive_ready
+ chosen colors/animation
```

Labels, `state_change_seq`, and Herdr `revision` can invalidate UI metadata but should not cause a keyboard write unless they affect the rendered result.

### Disconnect

On EOF, socket error, server stop, SSH loss, or protocol mismatch:

1. Increment `connection_epoch` so late snapshot responses are discarded.
2. Disable all agent-control key actions for that source immediately.
3. Mark its allocated slots stale: dim the last color for 2 seconds, then use a slow amber stale pulse; clear them after a configurable 15-second grace.
4. Retry with jittered exponential backoff, for example 250 ms up to 15 seconds.
5. Perform the full two-stream/two-snapshot bootstrap after reconnect; there is no resumable event cursor.

The exact grace/backoff values are companion policy.

## Deterministic six-slot allocator

### Identity

Define:

```text
SourceKey = configured-host-id + "\0" + Herdr-session-name
TargetKey = SourceKey + "\0" + terminal_id
```

Use a user-configured stable host ID or SSH alias. Never use `pane_id`, array index, agent kind, `display_agent`, or mutable agent name as identity. `terminal_id` follows the terminal across pane moves, while public pane IDs are topology addresses. Names can be changed and are cleared when an occupant exits or is replaced. [Agent identity and targets](https://herdr.dev/docs/agent-automation/#agent-identity-and-launch), [pane-move event](https://github.com/ogulcancelik/herdr/blob/v0.7.5/src/api/schema/events.rs)

This deliberately gives a replacement agent launched in the same surviving terminal the same physical key. Herdr does not expose a durable occupant-generation ID for every agent, so trying to distinguish same-kind replacements from snapshots alone would be unreliable. Reset the slot's rendered semantic state from the new snapshot, but retain its location.

For durable user intent, allow an optional pin by `(SourceKey, unique agent name)`. Resolve that name to the current `TargetKey` on every snapshot; if the named agent later appears in a new terminal, restore its pinned slot. Herdr agent names are unique while live, but they are mutable and cleared when the occupant exits or is replaced, so an unresolved pin must remain vacant rather than falling through to a different agent.

The full source namespace is mandatory: default and named sessions have independent servers/sockets and different hosts can reuse the same public IDs. Reset a source's identity generation after reconnect if the old terminal IDs are absent from the new snapshot.

### Recommended stable allocator

Persist six `TargetKey | null` values.

On each atomic aggregate snapshot:

1. Retain every incumbent whose `TargetKey` is still present in `snapshot.agents`.
2. Empty only slots whose incumbent disappeared.
3. Rank unassigned candidates deterministically.
4. Fill empty slots in ascending slot number.
5. Do not reorder or evict surviving incumbents merely because status changed.

Initial/admission rank:

```text
attention: blocked > done > working > idle > unknown
then configured SourceKey priority
then state_change_seq descending within that source
then structural workspace/tab/pane order
then TargetKey lexical
```

Never compare `state_change_seq` numerically across sources.

This preserves muscle memory. With more than six agents, keep overflow in the same deterministic order and expose an explicit next/previous page action. A hidden `blocked` or `done` agent should produce a brief all-six amber overflow heartbeat, but it should not silently steal another agent's key.

If automatic attention preemption is required, make it opt-in and exact: after an overflow agent remains `blocked` or `done` for 500 ms, replace the highest-numbered visible slot whose incumbent is `unknown` or `idle`; never evict focused, blocked, or done incumbents. The newcomer inherits that slot and remains there until it disappears. This is less stable than explicit paging.

### Optional Herdr UI mirroring

To make Herdr's Agent sidebar match keyboard slots, the companion can report a display-only token such as `rgb_slot=01` on allocated panes and install an Agent view filtered by token existence, sorted by that token ascending. Tokens and Agent views are transient and must be reapplied after server startup; only one Agent-view projection is active, so this should be opt-in. [Metadata tokens](https://herdr.dev/docs/socket-api/#agent-state-reporting), [Agent view lifetime](https://herdr.dev/docs/socket-api/#agent-view-queries)

## RGB state machine

Keep Herdr state and RGB presentation separate.

| Slot state | Suggested RGB |
| --- | --- |
| unallocated | off |
| `launch_pending` or not `interactive_ready` | cyan double blink |
| `blocked` | amber fast pulse |
| `done` | green slow pulse |
| `working` | blue breathing |
| `idle` | dim blue |
| `unknown` | dim violet |
| stale source | previous hue dimmed, then amber slow pulse |

Apply `focused` as a short white accent or brightness increase without replacing the base status color. This matters because focus is orthogonal to blocked/working/unknown, while `done` normally becomes `idle` when seen.

Renderer rules:

- publish all six keys atomically where the keyboard protocol permits;
- cap animations locally and send only changed frames;
- keep state transitions monotonic within the companion's current `connection_epoch`;
- never infer a completion from event counts or silence;
- use the latest accepted snapshot as the only source of semantic status.

## Named sessions and remote hosts

### Named sessions

The default socket is:

```text
~/.config/herdr/herdr.sock
```

A named session uses:

```text
~/.config/herdr/sessions/<name>/herdr.sock
```

Resolution is explicit `--session`, `HERDR_SOCKET_PATH`, `HERDR_SESSION`, then the default socket. Named sessions have separate panes, tabs, workspaces, sockets, and runtime state but share global config. [Socket paths](https://herdr.dev/docs/socket-api/#socket-paths), [named sessions](https://herdr.dev/docs/persistence-remote/#named-sessions)

`herdr session list --json` exposes `name`, `default`, `running`, `socket_path`, and `session_dir`; the current implementation returns the default first and valid named-session directories alphabetically. Prefer an explicit configured session allow-list rather than depending on that implementation order. Poll session discovery separately, and do not launch a stopped session merely to light LEDs. [Session discovery source](https://github.com/ogulcancelik/herdr/blob/v0.7.5/src/session.rs)

Every running session needs its own collector state, event streams, snapshot worker, connection epoch, and source namespace. Two sessions may both report one `focused` agent because focus is session-local.

### Remote hosts

The raw control API is local to the machine that owns the Herdr server. `herdr --remote` is an SSH thin UI client; the docs do not describe it as a local raw-API proxy. [Socket API overview](https://herdr.dev/docs/socket-api/), [remote attach](https://herdr.dev/docs/persistence-remote/#remote-attach-over-ssh)

Recommended remote architecture:

```text
local keyboard daemon
  ← authenticated NDJSON over SSH
remote lightweight collector
  ↔ remote Herdr Unix socket / named pipe
```

Run the same snapshot/event algorithm on the remote host and forward normalized source snapshots, not raw Herdr lifecycle events. Select a remote named session with `HERDR_SESSION=<name>` or the resolved session socket. The SSH transport must add its own connection epoch and stale policy.

An SSH Unix-socket forward can work on Unix, but a small remote collector is easier to make cross-session, schema-aware, and reconnect-safe. Do not merge hosts using `focused`; choose a current source explicitly or merge using fixed configured source priorities.

## Verification checklist

Before calling the pipeline production-ready, test:

1. working → idle in a background tab yields green `done`, then focusing any pane in that tab yields dim-blue `idle` for every completed agent there;
2. focusing an already-focused done agent and regaining outer-terminal focus both refresh LEDs without relying on a new focus event;
3. an agent replacement in a surviving terminal resets status/labels but deliberately retains that terminal's physical slot; closing the terminal removes the old `TargetKey`;
4. topology/status stream reconnects with historical replay cause one cache refresh and no duplicate animation;
5. server stop, named-session restart, SSH loss, protocol mismatch, more than six agents, pane move, and 513+ rapid lifecycle events all converge to the next authoritative snapshot.

Also verify agent-specific detection quality. Herdr can classify unfamiliar approval screens as `idle`; RGB correctness cannot exceed the installed agent manifest/integration's lifecycle accuracy. [Agent blocked-state limits](https://herdr.dev/docs/agents/#blocked-state)

## Primary sources

- [Herdr Socket API](https://herdr.dev/docs/socket-api/)
- [Herdr concepts and public agent states](https://herdr.dev/docs/concepts/)
- [Persistence, named sessions, and remote access](https://herdr.dev/docs/persistence-remote/)
- [v0.7.5 API schema](https://github.com/ogulcancelik/herdr/blob/v0.7.5/docs/next/api/herdr-api.schema.json)
- [v0.7.5 event schema](https://github.com/ogulcancelik/herdr/blob/v0.7.5/src/api/schema/events.rs)
- [v0.7.5 subscription implementation](https://github.com/ogulcancelik/herdr/blob/v0.7.5/src/api/subscriptions.rs)
- [v0.7.5 event hub](https://github.com/ogulcancelik/herdr/blob/v0.7.5/src/api/event_hub.rs)
- [v0.7.5 snapshot construction](https://github.com/ogulcancelik/herdr/blob/v0.7.5/src/app/api/session.rs)
- [v0.7.5 agent collection and records](https://github.com/ogulcancelik/herdr/blob/v0.7.5/src/app/agents.rs)
- [v0.7.5 seen/status transitions](https://github.com/ogulcancelik/herdr/blob/v0.7.5/src/app/actions.rs)
- [Upstream event replay bug #1270](https://github.com/ogulcancelik/herdr/issues/1270)
