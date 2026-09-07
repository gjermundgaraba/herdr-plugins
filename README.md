# herdr-plugins

Independent plugins and tools for [Herdr](https://herdr.dev/).

| Package | Description |
| --- | --- |
| [herdr-hub](herdr-hub) | Shared agent model, active-session state, and action relay for every Herdr dashboard |
| [herdr-picker](herdr-picker) | Standalone declarative fuzzy picker and workflow runner |
| [herdr-picker-agents](herdr-picker-agents) | Live Herdr agent picker example in Rust |
| [herdr-picker-workspaces](herdr-picker-workspaces) | Live Herdr workspace picker example in Rust |
| [herdr-picker-scripts](herdr-picker-scripts) | One-shot agent, workspace, and pane-moving examples in Python |
| [equalize-splits](equalize-splits) | Automatically equalize pane sizes after splitting |
| [fork-to-pane](fork-to-pane) | Fork Pi, Codex, Claude Code, or OpenCode into a new pane; branch Amp with a thread reference |
| [history](history) | Vim-style back/forward focus history |
| [space-meta](space-meta) | Space numbers and PR badges in the spaces sidebar |
| [herdr-micro](herdr-micro) | Control Herdr from a Work Louder Codex Micro |

Install the standalone picker on `PATH`:

```sh
nix profile install github:gjermundgaraba/herdr-plugins#herdr-picker
```

Install plugins as needed. When installing Micro, run the Hub line first:

```sh
herdr plugin install gjermundgaraba/herdr-plugins/equalize-splits
herdr plugin install gjermundgaraba/herdr-plugins/fork-to-pane
herdr plugin install gjermundgaraba/herdr-plugins/history
herdr plugin install gjermundgaraba/herdr-plugins/herdr-hub
herdr plugin install gjermundgaraba/herdr-plugins/herdr-micro
herdr plugin install gjermundgaraba/herdr-plugins/space-meta
```

Herdr Micro depends on Herdr Hub. Install Hub first and verify its service as
described in the [Hub setup](herdr-hub/README.md#setup), then continue with the
[Micro installation](herdr-micro/README.md#install).

For local development:

Build each plugin using its README before linking it. `herdr plugin link` only
registers the working tree; it does not run manifest `[[build]]` commands. The
picker runs directly from `PATH`. Each plugin runs its installed copy under
its own `bin/`; Cargo `target/` is only needed while building and staging.
Hub and Micro also require the staging and
service steps in their package READMEs.

```sh
herdr plugin link "$PWD/equalize-splits"
herdr plugin link "$PWD/fork-to-pane"
herdr plugin link "$PWD/history"
herdr plugin link "$PWD/space-meta"
```

Use the [Hub local-development sequence](herdr-hub/README.md#local-development)
and [Micro local-development sequence](herdr-micro/README.md#local-development)
for those service-backed plugins.

Each plugin directory above has its own `herdr-plugin.toml`; Herdr 0.8.2 does
not install cross-plugin dependencies.

## Nix

The flake builds each plugin independently with the Rust version in
`rust-toolchain.toml` and the committed `Cargo.lock`:

```sh
nix build .#herdr-picker
result/bin/herdr-picker --version
```

The hub output is both a linkable plugin root and a CLI package; follow the
[Hub setup](herdr-hub/README.md#setup) to install and start it.

The hub targets the production Herdr fork at commit `85ad1d77`. It reads the
fork's `session.snapshot.client_focused` field in one canonical 250 ms loop and
pushes active-session changes to every consumer; consumers do not poll Herdr or
integrate with a terminal app directly.

The other package names include `herdr-hub`, `herdr-picker-agents`,
`herdr-picker-workspaces`, `equalize-splits`, `fork-to-pane`, `history`, and
`space-meta`. The two picker examples are independent optional binary packages.
Every package builds on Linux and macOS, but the hub-backed live pickers require
the macOS hub service; Linux supports the hub's remote hooks and relay, not a
standalone local service. Plugin packages contain a complete, prebuilt plugin
root, so linking them never invokes Cargo. The picker packages expose their
executables under `bin/`; `herdr-hub` exposes both
`herdr-hub/bin/herdr-hub` for plugin commands and `bin/herdr-hub` for `PATH`.
`herdr-micro` has no Nix package: its service binary must be codesigned with a
local Apple Development identity.

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
The picker and its Rust examples share `sdk/picker` and `sdk/hub`; popup chrome
lives in `sdk/ratatui`; plugin-environment clients use `sdk/rust`.
Consequently, editing one plugin does not invalidate unrelated plugin outputs,
while edits to a shared SDK invalidate packages that include it.

Enter the repository development shell with the same pinned Rust toolchain using
`nix develop`.

## Plugin SDK and files

[`sdk/ratatui`](sdk/ratatui) provides shared search chrome, key hints,
and colors for Rust popup integrations, including external consumers such as
ClankerSnip. It intentionally does not own application state or event loops.

[`sdk/picker`](sdk/picker) provides the picker wire types and shared live Herdr
provider plumbing used by the independent agent and workspace examples.

[`sdk/hub`](sdk/hub) provides the versioned hub protocol, streaming model
client, action calls, and shared agent attention ordering.

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
