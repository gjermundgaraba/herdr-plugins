# Herdr Hub

Herdr Hub keeps a versioned inventory of local and remote Herdr sessions,
workspaces, tabs, and agents. It pushes changes and routes session API calls over
a private per-user socket. It has no TUI client model or focus selection.
Micro and local tools use each TUI's frontend socket directly.

## Setup

```sh
herdr plugin install gjermundgaraba/herdr-plugins/herdr-hub
```

In Herdr, run **Install Herdr Hub service**. It copies the current binary into
Application Support and runs it as the per-user `dev.herdr.hub` LaunchAgent.
When its action log reports success, run **Check Herdr Hub setup**. The plugin
startup hook keeps the installed copy current after rebuilds.

With Nix instead:

```sh
nix profile install github:gjermundgaraba/herdr-plugins#herdr-hub
herdr plugin link ~/.nix-profile/herdr-hub --enabled
herdr-hub install-service
herdr-hub doctor
```

Use **Show Herdr Hub status** for a compact summary. A source or Nix install
also exposes `herdr-hub dump` to print the complete model once.

Hub protocol **6** carries `hello`, `session`, `session_removed`, and `host`
updates, plus session-scoped `call` replies. It does not discover frontend
sockets, route client input, or activate endpoints. Rebuild Hub SDK consumers
and local/remote Hub binaries together when updating.

`doctor` checks the linked plugin, installed service, and configured remote hosts.
`HERDR_HUB_SOCKET_PATH` overrides the Hub socket and `HERDR_HUB_CONFIG_PATH`
overrides its configuration file (default `~/.config/herdr-hub/config.toml`).

## Local development

### Build, stage, and link

From a fresh repository checkout, build and stage the executable before
linking the plugin. These commands mirror the manifest's `[[build]]` commands;
`herdr plugin link` registers a working tree but does not run them.

```sh
cargo build --release --locked -p herdr-hub
mkdir -p herdr-hub/bin
install -m 750 target/release/herdr-hub herdr-hub/bin/.herdr-hub.new
mv -f herdr-hub/bin/.herdr-hub.new herdr-hub/bin/herdr-hub
herdr plugin link "$PWD/herdr-hub" --enabled
```

Run the block from the repository root. After source changes, repeat the build
and staging commands.

### Start the macOS service

```sh
herdr-hub/bin/herdr-hub install-service
herdr-hub/bin/herdr-hub doctor
```

`install-service` refreshes the installed copy after a rebuild.

## Remote hosts

Configure each remote in `~/.config/herdr-hub/config.toml`:

```toml
[[hosts]]
key = "workbox"
ssh = "workbox"
```

`key` becomes the host prefix in session identities such as
`workbox/default`; `ssh` is one SSH-config target. Restart the local service
with `herdr-hub install-service` after changing this file. `doctor` checks that
every configured target can run the same `herdr-hub` version.

The remote host needs both the CLI and the hook plugin. With Nix:

```sh
nix profile install github:gjermundgaraba/herdr-plugins#herdr-hub
herdr plugin link ~/.nix-profile/herdr-hub --enabled
```

Without Nix, clone this repository and follow
[Build, stage, and link](#build-stage-and-link). On the remote host, also
symlink the staged `herdr-hub/bin/herdr-hub` into a directory on `PATH`, such
as `~/.local/bin`. There is no `cargo install` path. Do not run the macOS
service commands on a Linux relay host.

Configured hosts retain one `ssh` relay each for the separate session inventory.
The remote hub owns one 250 ms snapshot loop per remote-local session and pushes
changed state through that relay. This inventory is independent of TUI frontend membership and focus.
On Linux there is no standalone user service: the plugin hooks and `notify`
command are supported and the hub runs in-process for `herdr-hub relay` when no
local hub is already listening.
