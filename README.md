# herdr-plugins

Independent plugins for [Herdr](https://herdr.dev/).

| Plugin | Description |
| --- | --- |
| [clanker-picker](clanker-picker) | Attention-ranked inbox for every agent in the session |
| [equalize-splits](equalize-splits) | Automatically equalize pane sizes after splitting |
| [history](history) | Vim-style back/forward focus history |

Install only the plugin you want:

```sh
herdr plugin install gjermundgaraba/herdr-plugins/clanker-picker
herdr plugin install gjermundgaraba/herdr-plugins/equalize-splits
herdr plugin install gjermundgaraba/herdr-plugins/history
```

For local development:

```sh
herdr plugin link "$PWD/clanker-picker"
herdr plugin link "$PWD/equalize-splits"
herdr plugin link "$PWD/history"
```

Each plugin directory above is independent and has its own `herdr-plugin.toml`.

## Plugin clients

Reusable typed socket clients live under [`sdk/`](sdk):

- [`sdk/rust`](sdk/rust) — synchronous Rust client
- [`sdk/go`](sdk/go) — context-aware Go client
