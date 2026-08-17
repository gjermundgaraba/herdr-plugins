# herdr-picker-herdr

Live agent and workspace sources for [`herdr-picker`](../herdr-picker). These
are separate optional executables, not a Herdr plugin and not built into the
generic picker. For a simpler script-based variant of the same pickers, see
[`herdr-picker-scripts`](../herdr-picker-scripts).

Linux and macOS with Herdr 0.8.0 or newer are supported.

## Try it from a checkout

Run this inside a Herdr-managed terminal:

```sh
cargo build --quiet -p herdr-picker -p herdr-picker-herdr && PATH="$PWD/target/debug:$PATH" HERDR_PICKER_CONFIG_DIR="$PWD/herdr-picker-herdr/examples" target/debug/herdr-picker run agents
```

Replace `agents` with `workspaces` to try the workspace picker. This builds both
executables and uses the bundled definitions without installing or copying
anything.

## Install

```sh
cargo install --locked --path herdr-picker
cargo install --locked --path herdr-picker-herdr
```

With Nix:

```sh
nix profile install github:gjermundgaraba/herdr-plugins#herdr-picker
nix profile install github:gjermundgaraba/herdr-plugins#herdr-picker-herdr
```

Copy the picker definitions:

```sh
mkdir -p ~/.config/herdr-picker/pickers
# From a checkout:
cp herdr-picker-herdr/examples/{agents,workspaces}.toml \
  ~/.config/herdr-picker/pickers/
# Or after the Nix profile install:
cp ~/.nix-profile/share/herdr-picker/examples/{agents,workspaces}.toml \
  ~/.config/herdr-picker/pickers/
herdr-picker check agents
herdr-picker check workspaces
```

Add either picker directly to `~/.config/herdr/config.toml`:

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

Reload Herdr after editing it:

```sh
herdr server reload-config
```

The sources take an initial Herdr snapshot, keep one event subscription open,
and stream replacement snapshots while the picker is open. Agent rows retain
the old attention-first ordering: blocked, done, working, idle, then unknown;
newer state changes sort first within a status. Selecting a row focuses its pane
or workspace after the popup closes.

## Executables

Each executable does one job and takes no arguments:

| Executable | Purpose |
|---|---|
| `herdr-picker-herdr-agents` | Stream live agent items |
| `herdr-picker-herdr-workspaces` | Stream live workspace items |
| `herdr-picker-herdr-focus-agent` | Focus the selected agent pane |
| `herdr-picker-herdr-focus-workspace` | Focus the selected workspace |

Source executables read one picker context from stdin and write full
`{"items":[...]}` snapshots. Focus executables read the selected value from the
picker's final `selections` map.
