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
| [herdr-micro](herdr-micro) | Control Herdr from a Work Louder Codex Micro (macOS) | [Fork build](#herdr-build) |
| [herdr-deck](herdr-deck) | Stream Deck dashboard and controls for Herdr agents (macOS) | [Fork build](#herdr-build) |

## Herdr build

The plugins target the
[gjermundgaraba/herdr](https://github.com/gjermundgaraba/herdr) fork on the
`custom-v3` branch, currently based on upstream 0.9.3. The fork adds the
daemon's client API (`client.list`, the `client.changed` subscription, and
`client.jump`), the client-side `[keys]` actions that replaced the picker plugins,
native space groups (`workspace.set_group`) and persisted workspace tokens, the
priority agent order that lifts spaces marked with the `space_priority`
token, pinned spaces sidebar sections in the TUI, the
native `ui.pick` picker, one `plugin.invoke` path for manifest entrypoints,
a richer invocation context, and `tab_id` on pane close
events plus `pane.cwd_changed`. Stock `herdrdev/herdr` has none of these, so
only herdr-hub runs against it.

Micro and Deck use the client API on the ordinary daemon socket, through the
typed `client.*` calls in [sdk/rust](sdk/rust). Every attached client (the TUI
or the native app) reports whether its window has focus, the endpoints it is
attached to, and its Agents list in the order it shows them; the daemon serves
those reports and relays a jump to the client that should perform it. The fork
documents the contract in
`docs/next/website/src/content/docs/client-contract.md`. No plugin has a git
dependency on the fork, so nothing in `Cargo.lock` pins a fork commit: a
daemon that rejects the client API is logged at run time with its own error
and has no clients.

Each local session is its own daemon. The device tools list them with
`herdr session list --json`, follow all of them, and show and jump one client:
the one whose window most recently took focus, also while another app is in
front. Of several clients that take focus in one listing of a daemon, the one
with the highest client id wins. While no client is followed, a reporting
client is followed anyway: one that reports focus before one whose focus is
unknown before one that reports none, and of equals the last by socket path
and then client id. The followed client is forgotten when its daemon lists
its clients without it or its session is no longer listed as running. While
its daemon only fails to answer it is kept, and the devices show nothing from
Herdr until the daemon lists it again. Prompts and Enter are typed only while
that client reports that it has focus; a TUI reports that only after its
terminal has sent a focus change, so one started in an already focused
terminal is shown and jumped but not typed into until then.

The agent list, workspace list, Back/Forward history, and the unread hold are
`[keys]` actions native to the fork; see its configuration docs. Hub keeps only
the session/host inventory and relay. See the
[Micro bridge](herdr-micro/docs/micro-bridge.md) for how a key press becomes a
jump or a keyboard event.

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
herdr plugin install gjermundgaraba/herdr-plugins/space-groups
herdr plugin install gjermundgaraba/herdr-plugins/space-priority
```

Herdr Micro and Herdr Deck connect directly to the Herdr daemon sockets. Micro
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
herdr plugin link "$PWD/space-groups"
herdr plugin link "$PWD/space-priority"
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

The other Nix packages are `equalize-splits`, `fork-to-pane`, `move-pane`,
`space-meta`, `space-groups`, and `space-priority`. `herdr-micro` has no Nix
package because its service binary must be locally codesigned, and
`herdr-deck` has none because it links system `jpeg-turbo` and HID libraries.

Each package's filtered source contains its full local path-dependency closure,
so editing one plugin does not invalidate unrelated plugin outputs, while edits
to a shared SDK invalidate packages that include it.

Enter the repository development shell with the same pinned Rust toolchain using
`nix develop`.

## Plugin SDK and files

[`sdk/hub`](sdk/hub) provides the versioned hub protocol, streaming model
client, and action calls.

[`sdk/device`](sdk/device) is what Micro and Deck do on a press: the checks
against the followed client and its window focus, `client.jump`, and prompts
typed as macOS keyboard events.

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
