# herdr-fork-to-pane

Fork the focused Pi, Codex, Claude Code, or OpenCode session into a new pane on the right
in [Herdr](https://herdr.dev/). The new agent receives the source conversation
history but writes to a new native session:

- Pi: `pi --fork <session>`
- Codex: `codex fork <session-id>`
- Claude Code: `claude --resume <session-id> --fork-session`
- OpenCode: `opencode --session <session-id> --fork`

Both agents share the same working directory and files. This plugin does not
create a Git worktree.

## Install

Install the Herdr integrations for the agents you use so their native session
references are available:

```sh
herdr integration install pi
herdr integration install codex
herdr integration install claude
herdr integration install opencode
herdr plugin install gjermundgaraba/herdr-plugins/fork-to-pane
```

Restart OpenCode after installing its integration. Its native session reference
becomes available after a session-bearing event.

Bind the fork action in `~/.config/herdr/config.toml`. It also appears in a
pane's right-click menu, where it forks the clicked pane's session, and from a
shell `herdr plugin invoke gjermundgaraba.herdr-fork-to-pane.fork` does the
same:

```toml
[[keys.command]]
key = "prefix+f"
type = "plugin"
command = "gjermundgaraba.herdr-fork-to-pane.fork"
description = "fork agent into right pane"
```

Reload keybindings with `herdr server reload-config`.

Requires stock Herdr >= 0.8.0 (the fork build is not needed) and a Pi, Codex, Claude Code, or OpenCode version with the fork
command shown above.

## Development

Run these commands from the repository root:

```sh
cargo test -p herdr-fork-to-pane
cargo build --release --locked -p herdr-fork-to-pane
mkdir -p fork-to-pane/bin
install -m 750 target/release/herdr-fork-to-pane fork-to-pane/bin/.herdr-fork-to-pane.new
mv -f fork-to-pane/bin/.herdr-fork-to-pane.new fork-to-pane/bin/herdr-fork-to-pane
herdr plugin link /path/to/herdr-plugins/fork-to-pane
herdr plugin log list --plugin gjermundgaraba.herdr-fork-to-pane
```
