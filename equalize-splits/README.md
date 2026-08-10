# herdr-equalize-splits

Automatically equalizes pane sizes after every split, pane close, or pane exit in
[Herdr](https://herdr.dev/). Splitting the right pane in a 50/50 two-pane tab
therefore produces three equal-width panes instead of 50/25/25. Closing the
middle pane then restores a 50/50 layout.

New splits only equalize their connected directional group. Pane closes
equalize each directional group in the affected tab.

Herdr 0.8.0 does not include the split source or requested ratio in
`pane.created`, so explicit split ratios and panes created by `layout.apply`
can be equalized too.

## Install

```sh
herdr plugin install gjermundgaraba/herdr-plugins/equalize-splits
```

For local development:

```sh
cd /path/to/herdr-plugins/equalize-splits
cargo build --release --locked
herdr plugin link "$PWD"
```

No keybinding or configuration is required. Its startup hook seeds a pane-to-tab
cache so background pane exits equalize the affected tab. Runtime state lives
under `HERDR_PLUGIN_STATE_DIR`. Requires Herdr >= 0.8.0. Installing from GitHub
requires `cargo` to build the binary.

## Development

```sh
cargo test
herdr plugin log list --plugin gjermundgaraba.herdr-equalize-splits
```
