# herdr-plugins

Independent plugins and tools for [Herdr](https://herdr.dev/).

Everything here except [herdr-hub](herdr-hub) needs the
[Herdr fork build](#herdr-build); the **Requires** column below says which is
which.

| Package | Description | Requires |
| --- | --- | --- |
| [equalize-splits](equalize-splits) | Automatically equalize pane sizes after splits and closes | [Fork build](#herdr-build) |
| [fork-to-pane](fork-to-pane) | Fork Pi, Codex, Claude Code, or OpenCode into a new pane | [Fork build](#herdr-build) |
| [move-pane](move-pane) | Move a pane to another tab or a new tab from Herdr's native picker | [Fork build](#herdr-build) |
| [space-meta](space-meta) | Space numbers, branch names, git-dirty markers, and PR badges in the spaces sidebar (macOS) | [Fork build](#herdr-build) |
| [space-groups](space-groups) | Named space groups under collapsible sidebar headers | [Fork build](#herdr-build) |
| [space-priority](space-priority) | Priority spaces whose blocked and done agents sort first, optionally pinned in the sidebar | [Fork build](#herdr-build) |
| [herdr-hub](herdr-hub) | Background inventory and relay for local and remote Herdr sessions | Stock Herdr >= 0.9.0 |

## Herdr build

The plugins target the
[gjermundgaraba/herdr](https://github.com/gjermundgaraba/herdr) fork on the
`custom` branch, currently based on upstream 0.9.5. The fork adds the
client-side `[keys]` actions that replaced the picker plugins,
native space groups (`workspace.set_group`) and persisted workspace tokens, the
priority agent order that lifts spaces marked with the `space_priority`
token, pinned spaces sidebar sections in the TUI, the
native `ui.pick` picker, one `plugin.invoke` path for manifest entrypoints,
the invoking client's id in the invocation context, and `tab_id` on pane close
events plus `pane.cwd_changed`. Stock `herdrdev/herdr` has none of these, so
only herdr-hub runs against it. No plugin has a git dependency on the fork,
so nothing in `Cargo.lock` pins a fork commit.

The agent list, workspace list, Back/Forward history, and the unread hold are
`[keys]` actions native to the fork; see its configuration docs. Hub keeps only
the session/host inventory and relay.

## Install

Install plugins as needed. Each plugin is built
from source when installed, so you need a Rust toolchain with cargo.

```sh
herdr plugin install gjermundgaraba/herdr-plugins/equalize-splits
herdr plugin install gjermundgaraba/herdr-plugins/fork-to-pane
herdr plugin install gjermundgaraba/herdr-plugins/herdr-hub
herdr plugin install gjermundgaraba/herdr-plugins/move-pane
herdr plugin install gjermundgaraba/herdr-plugins/space-meta
herdr plugin install gjermundgaraba/herdr-plugins/space-groups
herdr plugin install gjermundgaraba/herdr-plugins/space-priority
```

For local development, build each plugin using its README before linking it.
`herdr plugin link` only registers the working tree; it does not run manifest
`[[build]]` commands. Each plugin runs its installed copy under its own `bin/`;
Cargo `target/` is only needed while building and staging. Hub also needs the
staging and service steps in its package README.

```sh
herdr plugin link "$PWD/equalize-splits"
herdr plugin link "$PWD/fork-to-pane"
herdr plugin link "$PWD/move-pane"
herdr plugin link "$PWD/space-meta"
herdr plugin link "$PWD/space-groups"
herdr plugin link "$PWD/space-priority"
```

Use the [Hub local-development sequence](herdr-hub/README.md#local-development)
for that service-backed plugin.

Each plugin directory above has its own `herdr-plugin.toml`. Herdr does not
install cross-plugin dependencies.

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

The other Nix packages are `equalize-splits`, `fork-to-pane`, `move-pane`,
`space-meta`, `space-groups`, and `space-priority`.

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
