# herdr-picker-agents

A live Herdr agent picker implemented as a focused Rust example for
[`herdr-picker`](../herdr-picker). It streams agents from every session through
the Herdr hub, orders them by attention, shows their session as a badge, and
focuses the selected pane on its owning session after the popup closes.

For a shorter, one-shot Python example, see
[`herdr-picker-scripts`](../herdr-picker-scripts).

## Prerequisite

The live provider needs the macOS [`herdr-hub`](../herdr-hub) service. From
this checkout, build and start it once using its manifest commands:

```sh
cargo build --release --locked -p herdr-hub
mkdir -p herdr-hub/bin
install -m 750 target/release/herdr-hub herdr-hub/bin/.herdr-hub.new
mv -f herdr-hub/bin/.herdr-hub.new herdr-hub/bin/herdr-hub
herdr plugin link herdr-hub
herdr-hub/bin/herdr-hub install-service
```

There is no standalone Linux hub service, so this live picker is not available
as a local Linux setup.

## Try it

Run this inside a Herdr-managed terminal:

```sh
cargo build --quiet -p herdr-picker -p herdr-picker-agents
PATH="$PWD/target/debug:$PATH" \
  HERDR_PICKER_CONFIG_DIR="$PWD/herdr-picker-agents/examples" \
  target/debug/herdr-picker run agents
```

## Install

```sh
cargo install --locked --path herdr-picker
cargo install --locked --path herdr-picker-agents
mkdir -p ~/.config/herdr-picker/pickers
cp herdr-picker-agents/examples/agents.toml \
  ~/.config/herdr-picker/pickers/
```

With Nix:

```sh
nix profile install github:gjermundgaraba/herdr-plugins#herdr-hub
herdr plugin link ~/.nix-profile/herdr-hub
herdr-hub install-service
nix profile install github:gjermundgaraba/herdr-plugins#herdr-picker
nix profile install github:gjermundgaraba/herdr-plugins#herdr-picker-agents
mkdir -p ~/.config/herdr-picker/pickers
cp ~/.nix-profile/share/herdr-picker/examples/agents.toml \
  ~/.config/herdr-picker/pickers/
```

Add a direct popup keybinding:

```toml
[[keys.command]]
key = "prefix+a"
type = "popup"
command = "herdr-picker run agents"
description = "agents"
width = "88%"
height = "80%"
```

The package contains two executables:

| Executable | Purpose |
|---|---|
| `herdr-picker-agents` | Stream live agent snapshots |
| `herdr-picker-focus-agent` | Focus the selected agent pane |
