# herdr-plugins

Independent plugins for [Herdr](https://herdr.dev/).

| Plugin | Description |
| --- | --- |
| [command-palette](command-palette) | Fuzzy actions, keybindings, workspaces, tabs, panes, and agents, plus an attention-ranked Agents view |
| [equalize-splits](equalize-splits) | Automatically equalize pane sizes after splitting |
| [history](history) | Vim-style back/forward focus history |
| [herdr-micro](herdr-micro) | Control Herdr from a Work Louder Codex Micro |
| [popup-terminal](popup-terminal) | Native popup shell in the focused pane's working directory |

Install only the plugin you want:

```sh
herdr plugin install gjermundgaraba/herdr-plugins/command-palette
herdr plugin install gjermundgaraba/herdr-plugins/equalize-splits
herdr plugin install gjermundgaraba/herdr-plugins/history
herdr plugin install gjermundgaraba/herdr-plugins/herdr-micro
herdr plugin install gjermundgaraba/herdr-plugins/popup-terminal
```

For local development:

```sh
herdr plugin link "$PWD/command-palette"
herdr plugin link "$PWD/equalize-splits"
herdr plugin link "$PWD/history"
herdr plugin link "$PWD/herdr-micro"
herdr plugin link "$PWD/popup-terminal"
```

Each plugin directory above is independent and has its own `herdr-plugin.toml`.

## Plugin clients

Reusable typed socket clients live under [`sdk/`](sdk):

- [`sdk/rust`](sdk/rust) — synchronous Rust client
- [`sdk/go`](sdk/go) — context-aware Go client
