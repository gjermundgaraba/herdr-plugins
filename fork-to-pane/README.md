# herdr-fork-to-pane

Fork the focused Pi, Codex, or Claude Code session into a new pane on the right
in [Herdr](https://herdr.dev/). The new agent receives the source conversation
history but writes to a new native session:

- Pi: `pi --fork <session>`
- Codex: `codex fork <session-id>`
- Claude Code: `claude --resume <session-id> --fork-session`

Both agents share the same working directory and files. This plugin does not
create a Git worktree.

## Install

Install the Herdr integrations for the agents you use so their native session
references are available:

```sh
herdr integration install pi
herdr integration install codex
herdr integration install claude
herdr plugin install gjermundgaraba/herdr-plugins/fork-to-pane
```

Invoke **Fork agent into right pane** from the action menu or bind it in
`~/.config/herdr/config.toml`:

```toml
[[keys.command]]
key = "prefix+f"
type = "plugin_action"
command = "gjermundgaraba.herdr-fork-to-pane.fork"
description = "fork agent into right pane"
```

Reload keybindings with `herdr server reload-config`.

Requires Herdr >= 0.8.0 and a Pi, Codex, or Claude Code version with the fork
command shown above.

## Development

```sh
cargo test -p herdr-fork-to-pane
cargo build --release --locked
herdr plugin link /path/to/herdr-plugins/fork-to-pane
herdr plugin log list --plugin gjermundgaraba.herdr-fork-to-pane
```
