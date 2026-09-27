# Space Priority

Mark spaces as priority so that their agents waiting on you jump the queue.
In the priority agent order, blocked and done agents in a priority space come
before every other agent, in the agent list, the Agents sidebar, and
next/previous agent navigation.

Requires the [Herdr fork build](../README.md#herdr-build): stock Herdr's agent
order ignores the token this plugin sets.

## How it works

The plugin sets the `space_priority` workspace metadata token to `★` on
priority spaces. The fork reads it when sorting agents by priority:

1. Blocked agents in priority spaces, then done agents in priority spaces.
2. Everyone else in the usual priority order: blocked, done, working, idle.

Within each status, the fork queues blocked and done agents oldest state
change first, so the agent that has waited longest comes up next. Working and
idle agents show the most recent first.

Working, idle, and unknown agents in a priority space keep their normal
place, so a busy priority space does not crowd out an agent that needs you
elsewhere. Agents on a disconnected machine still sort last.

Only the priority order changes. The agent list always uses it; the Agents
sidebar uses it when `ui.agent_panel_sort = "priority"` (or after you click
its sort toggle), and keeps strict space order otherwise. An agent view set
through `agent.view.set` with its own sort replaces the priority order, and
with it this lift.

A worktree family shares its checkout's priority. Toggling a linked worktree
marks its checkout and every worktree beside it, and a worktree opened later
joins automatically.

Herdr keeps metadata tokens in memory only, so the plugin saves priority
spaces per session under its state directory and republishes them from its
`startup` hook, which also runs after a live handoff. The `workspace.created`
and `workspace.closed` hooks keep worktree families in sync and forget
closed spaces.

## Setup

```sh
herdr plugin install gjermundgaraba/herdr-plugins/space-priority
```

Bind the action in `~/.config/herdr/config.toml`:

```toml
[[keys.command]]
key = "prefix+shift+p"
type = "plugin_action"
command = "gjermundgaraba.herdr-space-priority.toggle"
description = "Toggle space priority"
```

The action flips the focused space and shows a notification with the result.
On the fork build, **Toggle space priority** also appears in a space's
right-click menu and flips the space you clicked.

To see which spaces are priority, place the token in your spaces sidebar
rows; it renders `★` on priority spaces and nothing elsewhere:

```toml
[ui.sidebar.spaces]
rows = [
  ["state_icon", { token = "$numbered_workspace", bold = true, dim = false }, { token = "$space_priority", fg = "#e5c07b" }],
]
```

## Notes

- Closing a family's checkout drops the family's priority; the remaining
  worktrees are no longer a family.
- `herdr workspace report-metadata` can set the `space_priority` token too,
  but such marks are not saved, and the next sync overwrites the token's
  value with `★` or clears it.

## Local development

Run these commands from the repository root:

```sh
cargo build --release --locked -p herdr-space-priority
mkdir -p space-priority/bin
install -m 750 target/release/herdr-space-priority space-priority/bin/.herdr-space-priority.new
mv -f space-priority/bin/.herdr-space-priority.new space-priority/bin/herdr-space-priority
herdr plugin link "$PWD/space-priority"
```

The plugin runs its `bin/` copy; `target/` is only used while building. A
linked plugin's startup hook first runs at the next server start or handoff.
