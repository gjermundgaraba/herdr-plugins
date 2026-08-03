# Popup Terminal

Opens Herdr's configured `[terminal].default_shell` in a native popup, starting
in the focused pane's working directory. The popup closes when the shell exits.

Requires Herdr 0.7.4+ and Rust 1.85+ to build. The resulting native binary has no
runtime dependencies beyond your configured shell.

## Install

```sh
herdr plugin install gjermundgaraba/herdr-plugins/popup-terminal
```

For local development:

```sh
herdr plugin link "$PWD/popup-terminal"
(cd popup-terminal && cargo build --release --locked && cargo test --locked)
```

## Keybinding

Add this to `~/.config/herdr/config.toml`:

```toml
[[keys.command]]
key = "prefix+t"
type = "plugin_action"
command = "gjermundgaraba.herdr-popup-terminal.open"
description = "open popup terminal"
```

Then apply it:

```sh
herdr server reload-config
```
