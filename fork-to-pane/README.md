# herdr-fork-to-pane

Fork the focused Pi, Codex, Claude Code, or OpenCode session into a new pane on the right
in [Herdr](https://herdr.dev/). The new agent receives the source conversation
history but writes to a new native session:

- Pi: `pi --fork <session>`
- Codex: `codex fork <session-id>`
- Claude Code: `claude --resume <session-id> --fork-session`
- OpenCode: `opencode --session <session-id> --fork`

Amp is also supported as a **branch with a reference**, not a full-history fork.
The new pane starts Amp with `@T-… ` prefilled in its input. Add your task, edit or
remove the reference, and submit when ready. Nothing is submitted automatically,
and no hidden context is added later. It does not resume or modify the original thread.

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

Requires Herdr >= 0.8.0 and a Pi, Codex, Claude Code, or OpenCode version with the fork
command shown above.

## Amp setup

Amp [removed native thread forking](https://ampcode.com/news/stick-a-fork-in-it).
For reference-based branching, install the bundled [Amp companion](amp-plugin.ts)
in Amp's user-local plugin directory on each machine running Herdr:

```sh
mkdir -p ~/.config/amp/plugins
cp /absolute/path/to/fork-to-pane/amp-plugin.ts \
  ~/.config/amp/plugins/herdr-fork-to-pane.ts
```

Copy from the installed Herdr plugin's directory or this checkout. Amp's plugin
loader rejects symlinks. To update the companion, copy the new version over this
same file rather than installing a second copy. Reload Amp plugins or restart Amp
after installing or updating.
Requires an Amp CLI with `activeThread` and `onDispose` plugin APIs.

The companion runs only inside Herdr. It tracks the active thread (including
thread switches) through a short-lived `amp_thread_id` pane token; it does not
take over agent lifecycle state. Open an existing Amp thread before invoking the
fork action. The action waits for Amp's input to be ready, pastes the reference
without pressing Enter, then focuses the new pane. If startup or paste fails,
the pane stays open and an error is reported rather than retrying the paste.
The companion does not intercept or modify user messages.

## Warp limitation

The [interactive `/fork` command](https://docs.warp.dev/agents/cli/reference/)
can copy the current conversation, but the CLI exposes no startup fork flag and
Herdr has no native Warp session integration. The plugin cannot reliably launch
that copy in another Herdr pane. `warp --resume` reopens the original conversation
and is not a substitute for forking.

## Development

Run these commands from the repository root:

```sh
cargo test -p herdr-fork-to-pane
bun test fork-to-pane/amp-plugin.test.ts
cargo build --release --locked -p herdr-fork-to-pane
mkdir -p fork-to-pane/bin
install -m 750 target/release/herdr-fork-to-pane fork-to-pane/bin/.herdr-fork-to-pane.new
mv -f fork-to-pane/bin/.herdr-fork-to-pane.new fork-to-pane/bin/herdr-fork-to-pane
herdr plugin link /path/to/herdr-plugins/fork-to-pane
herdr plugin log list --plugin gjermundgaraba.herdr-fork-to-pane
```
