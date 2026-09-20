# herdr-plugins

Independent plugins and tools for [Herdr](https://herdr.dev/).

## Herdr build

These plugins target the
[gjermundgaraba/herdr](https://github.com/gjermundgaraba/herdr) fork on the
`custom-v3` branch, currently based on upstream 0.9.1. The fork adds the
per-TUI frontend socket (protocol 7), the `agent.prompt` client command lane
method, and the client-side `[keys]` actions the pickers replaced. Stock
`herdrdev/herdr` has none of these, so Micro does not work against it. The
socket's Rust client is the `herdr-frontend` crate under `sdk/frontend` in the
fork; Micro depends on it as a git dependency, so `Cargo.lock` pins the exact
fork commit the plugins were built against and `cargo update -p herdr-frontend`
is how the plugins follow the fork. Everything else here runs on stock Herdr.
The fork documents the socket and the actions in
`docs/next/website/src/content/docs/frontend-api.md`.

| Package | Description |
| --- | --- |
| [herdr-hub](herdr-hub) | Runtime session/host inventory and relay |
| [equalize-splits](equalize-splits) | Automatically equalize pane sizes after splitting |
| [fork-to-pane](fork-to-pane) | Fork Pi, Codex, Claude Code, or OpenCode into a new pane; branch Amp with a thread reference |
| [move-pane](move-pane) | Move the focused pane to another tab or a new tab from one keybinding |
| [space-meta](space-meta) | Space numbers and PR badges in the spaces sidebar |
| [herdr-micro](herdr-micro) | Control Herdr from a Work Louder Codex Micro |

Install plugins as needed (Micro does not depend on Hub):

```sh
herdr plugin install gjermundgaraba/herdr-plugins/equalize-splits
herdr plugin install gjermundgaraba/herdr-plugins/fork-to-pane
herdr plugin install gjermundgaraba/herdr-plugins/herdr-hub
herdr plugin install gjermundgaraba/herdr-plugins/herdr-micro
herdr plugin install gjermundgaraba/herdr-plugins/move-pane
herdr plugin install gjermundgaraba/herdr-plugins/space-meta
```

Herdr Micro connects directly to per-TUI frontend sockets. See [Micro installation](herdr-micro/README.md#install).

For local development:

Build each plugin using its README before linking it. `herdr plugin link` only
registers the working tree; it does not run manifest `[[build]]` commands.
Each plugin runs its installed copy under its own `bin/`; Cargo `target/` is
only needed while building and staging.
Hub and Micro also require the staging and
service steps in their package READMEs.

```sh
herdr plugin link "$PWD/equalize-splits"
herdr plugin link "$PWD/fork-to-pane"
herdr plugin link "$PWD/move-pane"
herdr plugin link "$PWD/space-meta"
```

Use the [Hub local-development sequence](herdr-hub/README.md#local-development)
and [Micro local-development sequence](herdr-micro/README.md#local-development)
for those service-backed plugins.

Each plugin directory above has its own `herdr-plugin.toml`; Herdr does not
install cross-plugin dependencies.

## Nix

The flake builds each plugin independently with the Rust version in
`rust-toolchain.toml` and the committed `Cargo.lock`:

```sh
nix build .#herdr-hub
result/bin/herdr-hub --version
```

Every output is a linkable plugin root; the hub output is also a CLI package.
Follow the [Hub setup](herdr-hub/README.md#setup) to install and start it.

The agent list, Back/Forward history, and the unread hold are native to the
custom Herdr build as `[keys]` actions; see its configuration docs. Micro
connects directly to the TUI's protocol-7 frontend socket; Hub retains only
session/host inventory and relay. Frontend calls carry the endpoint ID and the
server boot ID whose pane ids they use. Input uses the ordinary TUI dispatcher,
including overlays. See the
[Micro bridge](herdr-micro/docs/micro-bridge.md) for routing and script behavior.

The other Nix packages are `equalize-splits`, `fork-to-pane`, `move-pane`, and
`space-meta`. `herdr-micro` has no Nix package: its service binary must be
locally codesigned.

Each package's filtered source contains its full local path-dependency closure,
so editing one plugin does not invalidate unrelated plugin outputs, while edits
to a shared SDK invalidate packages that include it.

Enter the repository development shell with the same pinned Rust toolchain using
`nix develop`.

## Plugin SDK and files

[`sdk/hub`](sdk/hub) provides the versioned hub protocol, streaming model
client, and action calls.

The reusable client under [`sdk/rust`](sdk/rust) also validates Herdr's plugin
environment and the repository file layout:

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
