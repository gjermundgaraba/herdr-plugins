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

Build each plugin using its README before linking it. `herdr plugin link` only
registers the working tree; it does not run manifest `[[build]]` commands.

```sh
herdr plugin link "$PWD/command-palette"
herdr plugin link "$PWD/equalize-splits"
herdr plugin link "$PWD/history"
herdr plugin link "$PWD/herdr-micro"
herdr plugin link "$PWD/popup-terminal"
```

Each plugin directory above is independent and has its own `herdr-plugin.toml`.

## Plugin clients

A reusable typed socket client lives under [`sdk/rust`](sdk/rust).
