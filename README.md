# herdr-plugins

Independent plugins for [Herdr](https://herdr.dev/).

| Plugin | Description |
| --- | --- |
| [herdr-picker](herdr-picker) | Fuzzy workspaces, tabs, panes, and agents, plus an attention-ranked Agents view |
| [equalize-splits](equalize-splits) | Automatically equalize pane sizes after splitting |
| [fork-to-pane](fork-to-pane) | Fork the focused Pi, Codex, or Claude Code session into a new pane |
| [history](history) | Vim-style back/forward focus history |
| [herdr-micro](herdr-micro) | Control Herdr from a Work Louder Codex Micro |

Install only the plugin you want:

```sh
herdr plugin install gjermundgaraba/herdr-plugins/herdr-picker
herdr plugin install gjermundgaraba/herdr-plugins/equalize-splits
herdr plugin install gjermundgaraba/herdr-plugins/fork-to-pane
herdr plugin install gjermundgaraba/herdr-plugins/history
herdr plugin install gjermundgaraba/herdr-plugins/herdr-micro
```

For local development:

Build each plugin using its README before linking it. `herdr plugin link` only
registers the working tree; it does not run manifest `[[build]]` commands.

```sh
herdr plugin link "$PWD/herdr-picker"
herdr plugin link "$PWD/equalize-splits"
herdr plugin link "$PWD/fork-to-pane"
herdr plugin link "$PWD/history"
herdr plugin link "$PWD/herdr-micro"
```

Each plugin directory above is independent and has its own `herdr-plugin.toml`.

## Nix

The flake builds each plugin independently with the Rust version in
`rust-toolchain.toml` and the committed `Cargo.lock`:

```sh
nix build .#command-palette
herdr plugin link "$(nix path-info .#command-palette)/command-palette"
```

The other package names are `equalize-splits`, `history`, `popup-terminal`, and
`herdr-micro`. The last package is exposed only on macOS, matching its plugin
manifest; the others support Linux and macOS. Every package contains a complete,
prebuilt plugin root, so linking it never invokes Cargo.

For Home Manager, keep the package in the profile so its Nix store path remains
live, then register that immutable plugin root after the profile is written:

```nix
inputs.herdr-plugins.url = "github:gjermundgaraba/herdr-plugins";
```

Then pass the flake inputs to this Home Manager module:

```nix
{ config, inputs, lib, pkgs, ... }:
let
  plugin =
    inputs.herdr-plugins.packages.${pkgs.stdenv.hostPlatform.system}.command-palette;
in
{
  home.packages = [ plugin ];

  home.activation.linkHerdrCommandPalette =
    lib.hm.dag.entryAfter [ "writeBoundary" ] ''
      run ${config.home.profileDirectory}/bin/herdr plugin link \
        ${plugin}/command-palette
    '';
}
```

The snippet assumes Herdr is already installed in the Home Manager profile.
Keeping `plugin` in `home.packages` prevents garbage collection of the registered
path. Nix reuses an unchanged package from the local store on later activations,
so there is no activation-time Rust compilation and no binary cache is required.

Each package's filtered source contains its full local path-dependency closure:
the plugin crate and `sdk/rust`, plus `sdk/ratatui` for `command-palette` and the
entire `herdr-micro` tree (including `codex-micro`) for `herdr-micro`. Consequently,
editing one plugin does not invalidate unrelated plugin outputs, while edits to a
shared SDK invalidate packages that include it.

Enter the repository development shell with the same pinned Rust toolchain using
`nix develop`.

## Plugin SDK and files

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

Rust popup plugins share search chrome, key hints, separators, and colors through
[`herdr-ratatui`](sdk/ratatui) without sharing application state or event loops.

## License

Apache-2.0. See [`LICENSE`](LICENSE).
