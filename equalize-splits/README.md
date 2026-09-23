# herdr-equalize-splits

Automatically equalizes pane sizes after every split, pane close, or pane exit in
[Herdr](https://herdr.dev/). Splitting the right pane in a 50/50 two-pane tab
therefore produces three equal-width panes instead of 50/25/25. Closing the
middle pane then restores a 50/50 layout.

New splits only equalize their connected directional group. Pane closes
equalize each directional group in the affected tab.

Herdr's `pane.created` event carries no split source or requested ratio (still
true in 0.9.1), so the plugin cannot tell an explicit-ratio split or a
`layout.apply` pane from an ordinary split. Those get equalized too.

## Install

```sh
herdr plugin install gjermundgaraba/herdr-plugins/equalize-splits
```

For local development:

```sh
cd /path/to/herdr-plugins/equalize-splits
cargo build --release --locked -p herdr-equalize-splits
mkdir -p bin
install -m 750 ../target/release/herdr-equalize-splits bin/.herdr-equalize-splits.new
mv -f bin/.herdr-equalize-splits.new bin/herdr-equalize-splits
herdr plugin link "$PWD"
```

No keybinding or configuration is required. Its startup hook seeds a pane-to-tab
cache so background pane exits equalize the affected tab. Cache and locking are
isolated by Herdr socket, and each server startup replaces the session-local
pane map. The `pane.moved` trigger refreshes this cache; it does not resize the
destination tab. Runtime state lives under `HERDR_PLUGIN_STATE_DIR`. Requires
stock Herdr >= 0.8.0; the fork build is not needed. Installing from GitHub requires `cargo` to build the binary.

## Development

```sh
cargo test
herdr plugin log list --plugin gjermundgaraba.herdr-equalize-splits
```
