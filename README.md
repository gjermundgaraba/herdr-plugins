# herdr-plugins

Independent plugins and tools for [Herdr](https://herdr.dev/).

| Package | Description |
| --- | --- |
| [herdr-picker](herdr-picker) | Standalone declarative fuzzy picker and workflow runner |
| [herdr-picker-agents](herdr-picker-agents) | Live Herdr agent picker example in Rust |
| [herdr-picker-workspaces](herdr-picker-workspaces) | Live Herdr workspace picker example in Rust |
| [herdr-picker-scripts](herdr-picker-scripts) | One-shot agent, workspace, and pane-moving examples in Python |
| [equalize-splits](equalize-splits) | Automatically equalize pane sizes after splitting |
| [fork-to-pane](fork-to-pane) | Fork the focused Pi, Codex, or Claude Code session into a new pane |
| [history](history) | Vim-style back/forward focus history |
| [space-meta](space-meta) | Space numbers and PR badges in the spaces sidebar |
| [herdr-micro](herdr-micro) | Control Herdr from a Work Louder Codex Micro |

Install the standalone picker on `PATH`:

```sh
nix profile install github:gjermundgaraba/herdr-plugins#herdr-picker
```

Install only the plugin you want:

```sh
herdr plugin install gjermundgaraba/herdr-plugins/equalize-splits
herdr plugin install gjermundgaraba/herdr-plugins/fork-to-pane
herdr plugin install gjermundgaraba/herdr-plugins/history
herdr plugin install gjermundgaraba/herdr-plugins/herdr-micro
herdr plugin install gjermundgaraba/herdr-plugins/space-meta
```

For local development:

Build each plugin using its README before linking it. `herdr plugin link` only
registers the working tree; it does not run manifest `[[build]]` commands. The
picker runs directly from `PATH`.

```sh
herdr plugin link "$PWD/equalize-splits"
herdr plugin link "$PWD/fork-to-pane"
herdr plugin link "$PWD/history"
herdr plugin link "$PWD/herdr-micro"
herdr plugin link "$PWD/space-meta"
```

Each plugin directory above is independent and has its own `herdr-plugin.toml`.

## Nix

The flake builds each plugin independently with the Rust version in
`rust-toolchain.toml` and the committed `Cargo.lock`:

```sh
nix build .#herdr-picker
result/bin/herdr-picker --version
```

The other package names include `herdr-picker-agents`,
`herdr-picker-workspaces`, `equalize-splits`, `fork-to-pane`, `history`,
`space-meta`, and `herdr-micro`. The two picker examples are independent
optional binary packages. `herdr-micro` is exposed only on macOS, matching its
plugin manifest; every other package supports Linux and macOS. Plugin packages
contain a complete, prebuilt plugin root, so linking them never invokes Cargo.
The picker packages expose their executables under `bin/`.

For Home Manager, add the picker package to the profile:

```nix
inputs.herdr-plugins.url = "github:gjermundgaraba/herdr-plugins";
```

Then pass the flake inputs to this Home Manager module:

```nix
{ inputs, pkgs, ... }:
{
  home.packages = [
    inputs.herdr-plugins.packages.${pkgs.stdenv.hostPlatform.system}.herdr-picker
  ];
}
```

That exposes `herdr-picker` on `PATH`; no plugin registration is needed for
direct popup keybindings. Nix reuses an unchanged package from the local store
on later activations, so there is no activation-time Rust compilation and no
binary cache is required.

Each package's filtered source contains its full local path-dependency closure.
The picker and its Rust examples share `sdk/picker`; popup chrome lives in
`sdk/ratatui`; Herdr clients use `sdk/rust`; `herdr-micro` also includes
`codex-micro`. Consequently, editing one plugin does not invalidate unrelated
plugin outputs, while edits to a shared SDK invalidate packages that include
it.

Enter the repository development shell with the same pinned Rust toolchain using
`nix develop`.

## Plugin SDK and files

[`sdk/ratatui`](sdk/ratatui) provides shared search chrome, key hints,
and colors for Rust popup integrations, including external consumers such as
ClankerSnip. It intentionally does not own application state or event loops.

[`sdk/picker`](sdk/picker) provides the picker wire types and shared live Herdr
provider plumbing used by the independent agent and workspace examples.

The reusable client under [`sdk/rust`](sdk/rust) also validates Herdr's plugin
environment and supplies the repository file layout:

```text
HERDR_PLUGIN_CONFIG_DIR/   user-edited configuration
HERDR_PLUGIN_STATE_DIR/
  data/                    durable state and backups
  cache/                   disposable data
  run/                     sockets and locks
  logs/                    bounded detached-process logs
```

Plugins must use the injected directories rather than derive them from `HOME`.
They create only the state subdirectories they need. Config serialization stays
schema-specific and each plugin README names its file. Managed actions and
events log to stdout/stderr for `herdr plugin log list`; only detached workers
and daemons write under `logs/`.

## License

Apache-2.0. See [`LICENSE`](LICENSE).
