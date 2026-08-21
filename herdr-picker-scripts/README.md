# herdr-picker-scripts

One-shot picker examples implemented as short Python scripts. Use these as
starting points for your own pickers, or when you want examples without
compiling additional Rust binaries. The corresponding
[`agent`](../herdr-picker-agents) and
[`workspace`](../herdr-picker-workspaces) Rust examples demonstrate live
streaming and richer item metadata instead.

The scripts cover these jobs:

| Script | Purpose |
|---|---|
| `herdr-picker-agents.py` | List agents, attention-first |
| `herdr-picker-workspaces.py` | List workspaces in order |
| `herdr-picker-focus-agent.py` | Focus the selected agent pane |
| `herdr-picker-focus-workspace.py` | Focus the selected workspace |
| `herdr-picker-move-pane.py` | Move the active pane to a new or existing tab |

The agent and workspace sources call `herdr api snapshot` once, while the
move-pane source calls `herdr tab list`. Each prints one item snapshot, so its
list is not live while the picker is open. Requires `python3` and the `herdr`
CLI on `PATH`.

The move-pane launcher lists tabs in the active workspace and puts **New tab**
first. When the workspace has only one tab, it moves the pane into a new tab
immediately instead of opening the picker. Existing-tab moves place the pane
to the right of that tab's focused pane and focus the moved pane.

## Try it from a checkout

Run this inside a Herdr-managed terminal:

```sh
cargo build --quiet -p herdr-picker && PATH="$PWD/herdr-picker-scripts:$PWD/target/debug:$PATH" HERDR_PICKER_CONFIG_DIR="$PWD/herdr-picker-scripts" herdr-picker run agents
```

Replace `agents` with `workspaces` to try the workspace picker. The move-pane
launcher requires the active-pane context Herdr supplies to popup commands;
install it and use the popup keybinding below instead of invoking it directly
from a managed terminal.

## Install

Install [`herdr-picker`](../herdr-picker), then copy the scripts to a directory
on `PATH` and the picker definitions to the config directory:

```sh
cp herdr-picker-scripts/*.py ~/.local/bin/
mkdir -p ~/.config/herdr-picker/pickers
cp herdr-picker-scripts/{agents,workspaces,move-pane}.toml ~/.config/herdr-picker/pickers/
herdr-picker check agents
herdr-picker check workspaces
herdr-picker check move-pane
```

These definitions intentionally use the same picker names as the Rust
examples. Choose one implementation, or rename a copied definition when
comparing both.

Add keybindings to `~/.config/herdr/config.toml` and reload with
`herdr server reload-config`:

```toml
[[keys.command]]
key = "prefix+a"
type = "popup"
command = "herdr-picker run agents"
description = "agents"
width = "88%"
height = "80%"

[[keys.command]]
key = "prefix+w"
type = "popup"
command = "herdr-picker run workspaces"
description = "workspaces"
width = "88%"
height = "80%"

[[keys.command]]
key = "prefix+m"
type = "popup"
command = "herdr-picker-move-pane.py"
description = "move pane to tab"
width = "88%"
height = "80%"
```
