# herdr-picker-workspaces

A live Herdr workspace picker implemented as a focused Rust example for
[`herdr-picker`](../herdr-picker). It streams workspaces in display order and
focuses the selected workspace after the popup closes.

For a shorter, one-shot Python example, see
[`herdr-picker-scripts`](../herdr-picker-scripts).

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

