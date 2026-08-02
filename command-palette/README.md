# herdr-command-palette

A fuzzy command and navigation palette for [Herdr](https://herdr.dev).

It searches:

- native Herdr actions, with effective default/configured keybindings
- actions from every installed plugin, with their configured keybindings
- workspaces, tabs, panes/terminals, and agents

The **Agents** action opens the same palette filtered to agents. With an empty
search, agents are attention-ranked: blocked, done, working, idle, then
unknown; newer state changes appear first within each status. Type immediately
to search agents directly by fuzzy relevance.

Selecting an action executes it after explicitly closing the palette popup.
Only actions safely supported by Herdr's public API are shown; actions requiring
Herdr-owned prompts or modes remain omitted until Herdr exposes a host-action
invocation API.

## Install

```sh
herdr plugin install gjermundgaraba/herdr-plugins/command-palette
```

For local development:

```sh
cargo build --release --locked
herdr plugin link /path/to/herdr-plugins/command-palette
```

## Keybindings

Bind the full palette:

```toml
[[keys.command]]
key = "prefix+space"
type = "plugin_action"
command = "gjermundgaraba.herdr-command-palette.open"
description = "command palette"
```

Bind the attention-ranked Agents view:

```toml
[[keys.command]]
key = "prefix+a"
type = "plugin_action"
command = "gjermundgaraba.herdr-command-palette.agents"
description = "agents"
```

By default, the palette opens ready for direct search. Supported filters are `all`,
`actions`, `workspaces`, `tabs`, `panes`, and `agents`.

The `workspaces` action opens a palette filtered to workspaces:

```toml
[[keys.command]]
key = "prefix+w"
type = "plugin_action"
command = "gjermundgaraba.herdr-command-palette.workspaces"
description = "workspace palette"
```

Configure an action's input mode in the plugin config directory printed by:

```sh
herdr plugin config-dir gjermundgaraba.herdr-command-palette
```

Create `config.toml` there:

```toml
[actions.agents]
mode = "vim"

[actions.workspaces]
mode = "vim"
```

Supported modes are `direct` and `vim`; omitted actions and modes use `direct`.
Each `[actions.<id>]` table matches a manifest action ID exactly. Plugin actions
pass the resolved mode to the popup; opening the pane directly with
`--env HERDR_COMMAND_PALETTE_MODE=...` bypasses action configuration.

Reload Herdr after changing keybindings:

```sh
herdr server reload-config
```

## Palette controls

| Key | Action |
|---|---|
| Type | Fuzzy search |
| `Enter` / click | Run or focus |
| `Up` / `Down`, `Ctrl+N` / `Ctrl+P` | Move |
| `Tab` / `Shift+Tab` | Cycle source filter |
| `Ctrl+A/W/T/G` | Actions/workspaces/tabs/agents |
| `Alt+P` | Panes |
| `Ctrl+U` | Clear search |
| `Esc` | Close |

In Vim mode, use `j` / `k` to move and `/` to start searching. `Esc` leaves
search first, then closes the palette.

The plugin uses `nucleo-matcher` for fuzzy scoring and this repository's
`sdk/rust` client for Herdr socket calls.

## License

Apache-2.0. See `LICENSE`.
