# herdr-plugins

Independent plugins and tools for [Herdr](https://herdr.dev/).

Most of these run on stock Herdr. Only [herdr-micro](herdr-micro) and
[herdr-deck](herdr-deck) need the [Herdr fork build](#herdr-build); the
**Requires** column below says which is which.

| Package | Description | Requires |
| --- | --- | --- |
| [equalize-splits](equalize-splits) | Automatically equalize pane sizes after splits and closes | Stock Herdr >= 0.8.0 |
| [fork-to-pane](fork-to-pane) | Fork Pi, Codex, Claude Code, or OpenCode into a new pane; branch Amp with a thread reference | Stock Herdr >= 0.8.0 |
| [move-pane](move-pane) | Move the focused pane to another tab or a new tab from one keybinding | Stock Herdr >= 0.8.0 |
| [space-meta](space-meta) | Space numbers, branch names, git-dirty markers, and PR badges in the spaces sidebar (macOS) | Stock Herdr >= 0.8.0 |
| [herdr-hub](herdr-hub) | Background inventory and relay for local and remote Herdr sessions | Stock Herdr >= 0.9.0 |
| [herdr-micro](herdr-micro) | Control Herdr from a Work Louder Codex Micro (macOS) | [Fork build](#herdr-build) |
| [herdr-deck](herdr-deck) | Stream Deck dashboard and controls for Herdr agents (macOS) | [Fork build](#herdr-build) |

## Herdr build

Micro and Deck target the
[gjermundgaraba/herdr](https://github.com/gjermundgaraba/herdr) fork on the
`custom-v3` branch, currently based on upstream 0.9.1. The fork adds the
per-TUI frontend socket (protocol 7), the `agent.prompt` client command lane
method, and the client-side `[keys]` actions that replaced the picker
plugins. Stock
`herdrdev/herdr` has none of these, so Micro and Deck do not work against it.
Everything else here runs on stock Herdr.

The socket's Rust client is the `herdr-frontend` crate under `sdk/frontend` in
the fork. Micro and Deck pull it in as a git dependency, so `Cargo.lock` pins
the exact fork commit the plugins were built against, and
`cargo update -p herdr-frontend` moves them to a newer one. The fork documents
the socket and the actions in
`docs/next/website/src/content/docs/frontend-api.md`.

The agent list, workspace list, Back/Forward history, and the unread hold are
`[keys]` actions native to the fork; see its configuration docs. Frontend calls carry the
endpoint ID and the server boot ID whose pane ids they use, and input goes
through the ordinary TUI dispatcher, including overlays. Hub keeps only the
session/host inventory and relay. See the
[Micro bridge](herdr-micro/docs/micro-bridge.md) for routing and script
behavior.

## Install

Install plugins as needed (Micro does not depend on Hub). Each plugin is built
from source when installed, so you need a Rust toolchain with cargo.

```sh
herdr plugin install gjermundgaraba/herdr-plugins/equalize-splits
herdr plugin install gjermundgaraba/herdr-plugins/fork-to-pane
herdr plugin install gjermundgaraba/herdr-plugins/herdr-hub
herdr plugin install gjermundgaraba/herdr-plugins/herdr-micro
herdr plugin install gjermundgaraba/herdr-plugins/move-pane
herdr plugin install gjermundgaraba/herdr-plugins/space-meta
```

Herdr Micro and Herdr Deck connect directly to per-TUI frontend sockets. Micro
is a plugin whose Codex Micro device service runs as a per-user LaunchAgent;
Deck is not a plugin at all and runs only as a LaunchAgent. See
[Micro installation](herdr-micro/README.md#install) and
[Deck setup](herdr-deck/README.md).

For local development, build each plugin using its README before linking it.
`herdr plugin link` only registers the working tree; it does not run manifest
`[[build]]` commands. Each plugin runs its installed copy under its own `bin/`;
Cargo `target/` is only needed while building and staging. Hub and Micro also
need the staging and service steps in their package READMEs.

```sh
herdr plugin link "$PWD/equalize-splits"
herdr plugin link "$PWD/fork-to-pane"
herdr plugin link "$PWD/move-pane"
herdr plugin link "$PWD/space-meta"
```

Use the [Hub local-development sequence](herdr-hub/README.md#local-development)
and [Micro local-development sequence](herdr-micro/README.md#local-development)
for those service-backed plugins.

Each plugin directory above has its own `herdr-plugin.toml`, except
`herdr-deck`, which is not a Herdr plugin. Herdr does not install
cross-plugin dependencies.

## Nix

The flake builds each plugin independently with the Rust version in
`rust-toolchain.toml` and the committed `Cargo.lock`:

```sh
nix build .#herdr-hub
result/bin/herdr-hub --version
```

Every output contains a linkable plugin root at `<out>/<name>`; the hub output
is also a CLI package.
Follow the [Hub setup](herdr-hub/README.md#setup) to install and start it.

The other Nix packages are `equalize-splits`, `fork-to-pane`, `move-pane`, and
`space-meta`. `herdr-micro` has no Nix package because its service binary must
be locally codesigned, and `herdr-deck` has none because it links system
`jpeg-turbo` and HID libraries.

Each package's filtered source contains its full local path-dependency closure,
so editing one plugin does not invalidate unrelated plugin outputs, while edits
to a shared SDK invalidate packages that include it.

Enter the repository development shell with the same pinned Rust toolchain using
`nix develop`.

## Plugin SDK and files

[`sdk/hub`](sdk/hub) provides the versioned hub protocol, streaming model
client, and action calls.

The reusable client under [`sdk/rust`](sdk/rust) also validates Herdr's plugin
environment, and its tests keep every plugin manifest version in step with its
crate. Plugins use this state layout:

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
