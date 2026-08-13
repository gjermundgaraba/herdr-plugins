# herdr-plugins

Independent plugins for [Herdr](https://herdr.dev/).

| Plugin | Description |
| --- | --- |
| [herdr-picker](herdr-picker) | Fuzzy workspaces, tabs, panes, and agents, plus an attention-ranked Agents view |
| [equalize-splits](equalize-splits) | Automatically equalize pane sizes after splitting |
| [fork-to-pane](fork-to-pane) | Fork the focused Pi, Codex, or Claude Code session into a new pane |
| [history](history) | Vim-style back/forward focus history |
| [herdr-micro](herdr-micro) | Control Herdr from a Work Louder Codex Micro |

Install only the plugin you want:

```sh
herdr plugin install gjermundgaraba/herdr-plugins/herdr-picker
herdr plugin install gjermundgaraba/herdr-plugins/equalize-splits
herdr plugin install gjermundgaraba/herdr-plugins/fork-to-pane
herdr plugin install gjermundgaraba/herdr-plugins/history
herdr plugin install gjermundgaraba/herdr-plugins/herdr-micro
```

For local development:

Build each plugin using its README before linking it. `herdr plugin link` only
registers the working tree; it does not run manifest `[[build]]` commands.

```sh
herdr plugin link "$PWD/herdr-picker"
herdr plugin link "$PWD/equalize-splits"
herdr plugin link "$PWD/fork-to-pane"
herdr plugin link "$PWD/history"
herdr plugin link "$PWD/herdr-micro"
```

Each plugin directory above is independent and has its own `herdr-plugin.toml`.

## Plugin SDK and files

The reusable client under [`sdk/rust`](sdk/rust) also validates Herdr's plugin
environment and supplies the repository file layout:

```text
HERDR_PLUGIN_CONFIG_DIR/   user-edited configuration
HERDR_PLUGIN_STATE_DIR/
  data/                    durable state and backups
  cache/                   disposable data
  run/                     sockets and locks
  logs/                    bounded detached-process logs
```

Plugins must use the injected directories rather than derive them from `HOME`.
They create only the state subdirectories they need. Config serialization stays
schema-specific and each plugin README names its file. Managed actions and
events log to stdout/stderr for `herdr plugin log list`; only detached workers
and daemons write under `logs/`.

Rust popup plugins share search chrome, key hints, separators, and colors through
[`herdr-ratatui`](sdk/ratatui) without sharing application state or event loops.

## License

Apache-2.0. See [`LICENSE`](LICENSE).
