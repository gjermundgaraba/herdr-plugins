# herdr-command-palette

A fuzzy command and navigation palette for [Herdr](https://herdr.dev).

It searches:

- native Herdr actions, with effective default/configured keybindings
- actions from every installed plugin, with their configured keybindings
- workspaces, tabs, panes/terminals, and agents

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

The plugin uses `nucleo-matcher` for fuzzy scoring and this repository's
`sdk/rust` client for Herdr socket calls.

## License

Apache-2.0. See `LICENSE`.

