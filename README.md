# herdr-plugins

Independent plugins for [Herdr](https://herdr.dev/).

| Plugin | Description |
| --- | --- |
| [clanker-picker](clanker-picker) | Attention-ranked inbox for every agent in the session |
| [history](history) | Vim-style back/forward focus history |

Install only the plugin you want:

```sh
herdr plugin install gjermundgaraba/herdr-plugins/clanker-picker
herdr plugin install gjermundgaraba/herdr-plugins/history
```

For local development:

```sh
herdr plugin link "$PWD/clanker-picker"
herdr plugin link "$PWD/history"
```

Each subdirectory is an independent plugin with its own `herdr-plugin.toml`.
