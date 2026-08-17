# herdr-picker-scripts

The same agent and workspace pickers as
[`herdr-picker-herdr`](../herdr-picker-herdr), implemented as four short Python
scripts instead of compiled binaries. Use these as a starting point for your
own pickers, or when you want the pickers without a Rust toolchain.

Each script does one job:

| Script | Purpose |
|---|---|
| `herdr-picker-agents.py` | List agents, attention-first |
| `herdr-picker-workspaces.py` | List workspaces in order |
| `herdr-picker-focus-agent.py` | Focus the selected agent pane |
| `herdr-picker-focus-workspace.py` | Focus the selected workspace |

The sources call `herdr api snapshot` once and print one item snapshot, so the
list is not live while the picker is open; the focus scripts call
`herdr agent focus` and `herdr workspace focus`. Requires `python3` and the
`herdr` CLI on `PATH`. For live-updating lists, use the compiled
`herdr-picker-herdr` binaries instead.

## Try it from a checkout

Run this inside a Herdr-managed terminal:

```sh
cargo build --quiet -p herdr-picker && PATH="$PWD/herdr-picker-scripts:$PWD/target/debug:$PATH" HERDR_PICKER_CONFIG_DIR="$PWD/herdr-picker-scripts" herdr-picker run agents
```

Replace `agents` with `workspaces` to try the workspace picker.

## Install

Install [`herdr-picker`](../herdr-picker), then copy the scripts to a directory
on `PATH` and the picker definitions to the config directory:

```sh
cp herdr-picker-scripts/*.py ~/.local/bin/
mkdir -p ~/.config/herdr-picker/pickers
cp herdr-picker-scripts/{agents,workspaces}.toml ~/.config/herdr-picker/pickers/
herdr-picker check agents
herdr-picker check workspaces
```

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
```
