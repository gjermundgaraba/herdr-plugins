# herdr-picker-workspaces

A live Herdr workspace picker implemented as a focused Rust example for
[`herdr-picker`](../herdr-picker). It streams connected workspaces from the
Herdr hub, keeps display-number order within each session, and focuses the
selected workspace on the correct session after the popup closes. Badges show
the local session name or the full remote session key.

For a shorter, one-shot Python example, see
[`herdr-picker-scripts`](../herdr-picker-scripts).

## Prerequisite

The live provider needs the macOS [`herdr-hub`](../herdr-hub) service. From
this checkout, follow the Hub README's
[local-development setup](../herdr-hub/README.md#local-development). For a
normal installation, follow its [Setup](../herdr-hub/README.md#setup).

There is no standalone Linux hub service, so this live picker is not available
as a local Linux setup.

## Try it

Run this inside a Herdr-managed terminal:

```sh
cargo build --quiet -p herdr-picker -p herdr-picker-workspaces
PATH="$PWD/target/debug:$PATH" \
  HERDR_PICKER_CONFIG_DIR="$PWD/herdr-picker-workspaces/examples" \
  target/debug/herdr-picker run workspaces
```

## Install

```sh
cargo install --locked --path herdr-picker
cargo install --locked --path herdr-picker-workspaces
mkdir -p ~/.config/herdr-picker/pickers
cp herdr-picker-workspaces/examples/workspaces.toml \
  ~/.config/herdr-picker/pickers/
```

With Nix:

```sh
nix profile install github:gjermundgaraba/herdr-plugins#herdr-picker
nix profile install github:gjermundgaraba/herdr-plugins#herdr-picker-workspaces
mkdir -p ~/.config/herdr-picker/pickers
cp ~/.nix-profile/share/herdr-picker/examples/workspaces.toml \
  ~/.config/herdr-picker/pickers/
```

Install the Hub using its [Nix instructions](../herdr-hub/README.md#setup)
before running the live provider.

Add a direct popup keybinding:

```toml
[[keys.command]]
key = "prefix+w"
type = "popup"
command = "herdr-picker run workspaces"
description = "workspaces"
width = "88%"
height = "80%"
```

The package contains two executables:

| Executable | Purpose |
|---|---|
| `herdr-picker-workspaces` | Stream live workspace snapshots |
| `herdr-picker-focus-workspace` | Focus the selected workspace |
