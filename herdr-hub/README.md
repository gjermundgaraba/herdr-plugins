# Herdr Hub

Herdr Hub keeps one versioned agent model for all running Herdr sessions and
pushes it to local dashboards over a private per-user socket.

## Setup

```sh
herdr plugin link herdr-hub
herdr-hub/bin/herdr-hub install-service
herdr-hub/bin/herdr-hub doctor
```

`install-service` copies the current binary into Application Support and runs
it as the per-user `dev.herdr.hub` LaunchAgent. The plugin startup hook keeps
the installed copy current after rebuilds.

Use `herdr-hub/bin/herdr-hub dump` to print the complete model once, or
`herdr-hub/bin/herdr-hub status` for a compact summary.

The hub publishes `active` from the forked Herdr server's
`session.snapshot.client_focused` field. It polls all local session snapshots
centrally every 250 ms because Herdr does not emit an event when client focus
changes. No Ghostty integration or macOS privacy permission is required.

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
herdr plugin link ~/.nix-profile/herdr-hub
```

Without Nix, clone this repository, run `cargo build --release -p herdr-hub`,
run `herdr plugin link herdr-hub` from the repository root, and symlink
`herdr-hub/bin/herdr-hub` into `~/.local/bin`. There is no `cargo install`
path.

The local hub keeps one `ssh` relay per configured host. The remote hub owns one
250 ms snapshot loop per remote-local session and pushes changed state through
that relay; consumers never poll Herdr, and there is no direct-socket fallback.
On Linux there is no standalone user service: the plugin hooks and `notify`
command are supported and the hub runs in-process for `herdr-hub relay` when no
local hub is already listening.
