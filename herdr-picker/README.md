# herdr-picker

A fuzzy picker for [Herdr](https://herdr.dev) workspaces, tabs, panes, and
agents.

The **Agents** action opens a filtered, attention-ranked view: blocked, done,
working, idle, then unknown; recent state changes rank first within each
status.

## Install

```sh
herdr plugin install gjermundgaraba/herdr-plugins/herdr-picker
```

For local development:

```sh
cargo build --release --locked
herdr plugin link /path/to/herdr-plugins/herdr-picker
```

## Keybindings

```toml
[[keys.command]]
key = "prefix+space"
type = "plugin_action"
command = "gjermundgaraba.herdr-picker.open"
description = "resource picker"

[[keys.command]]
key = "prefix+a"
type = "plugin_action"
command = "gjermundgaraba.herdr-picker.agents"
description = "agents"
```

The `workspaces` action similarly opens a workspace-only view. Configure an
action's input mode in the plugin's `config.toml`:

```toml
[actions.agents]
mode = "vim"
```

Supported modes are `direct` (default) and `vim`.

## Controls

| Key | Action |
|---|---|
| Type | Fuzzy search |
| `Enter` / click | Focus |
| `Up` / `Down`, `Ctrl+N` / `Ctrl+P` | Move |
| `Tab` / `Shift+Tab` | Cycle filters |
| `Ctrl+W/T/G`, `Alt+P` | Workspaces/tabs/agents/panes |
| `Ctrl+U` | Clear search |
| `Esc` | Close |

In Vim mode, use `j` / `k` to move and `/` to search.

## License

Apache-2.0. Portions originated in
[Herdr](https://github.com/herdrdev/herdr)'s Apache-2.0-licensed picker code.
See [`../LICENSE`](../LICENSE).
