# herdr-move-pane

Move the focused pane to another tab in its workspace, or into a new tab, from
one keybinding in [Herdr](https://herdr.dev/).

When the workspace has a single tab the pane moves straight into a new tab.
Otherwise a small popup lists **New tab** and every other tab: `j`/`k`, the
arrow keys, or `ctrl+n`/`ctrl+p` move the highlight, `Enter` moves the pane to
the highlighted entry, `1`-`9` move it to that entry at once, and `Esc`, `q`,
or `ctrl+c` cancel. Moving into an existing tab splits to the right of its
focused pane and follows the pane.

Requires Herdr >= 0.8.0.

## Install

```sh
herdr plugin install gjermundgaraba/herdr-plugins/move-pane
```

Bind the action in `~/.config/herdr/config.toml`:

```toml
[[keys.command]]
key = "prefix+m"
type = "plugin_action"
command = "gjermundgaraba.herdr-move-pane.move"
description = "Move pane to tab"
```

Plugin actions have no menu in Herdr, so the keybinding is the way in; from a
shell, `herdr plugin action invoke gjermundgaraba.herdr-move-pane.move` moves
the focused pane.

## Local development

```sh
cd move-pane
cargo build --release --locked -p herdr-move-pane
mkdir -p bin
install -m 750 ../target/release/herdr-move-pane bin/.herdr-move-pane.new
mv -f bin/.herdr-move-pane.new bin/herdr-move-pane
herdr plugin link "$PWD"
```

The plugin runs its `bin/` copy; `target/` is only used while building.
