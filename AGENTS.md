# herdr-plugins agent instructions

This is a repo with herdr plugins.

For changes to plugin manifests, actions, or Herdr APIs, consult the relevant sections of the [Herdr agent guide](https://herdr.dev/agent-guide.md).

## general instructions

- Before reporting a plugin source change done, run all applicable manifest `[[build]]` commands in order from that plugin's directory, including the install and atomic replacement steps. Tests and bare `cargo build` do not update the installed executable used by a locally linked plugin. `herdr plugin link` only registers the plugin; it does not build or install its executable.
- Runtime commands must use the plugin's independent `bin/` copy, never a binary or symlink into `target/`. Keep Cargo artifacts separate from installed executables so build-cache cleanup cannot break plugins. Follow the package README for the build and install sequence.
- For other daemons and installed services, follow the plugin’s lifecycle documentation: `herdr-hub/README.md` for Hub, `herdr-micro/docs/micro-bridge.md` for Micro, and `herdr-deck/README.md` for Deck. Hub’s installed service copy can be refreshed after rebuilding with `herdr-hub/bin/herdr-hub install-service`.
- Never suggest "upstreaming a change to herdr itself" (`herdrdev/herdr`). If a plugin needs something the Herdr fork build does not offer, say so plainly; changing the fork is a separate task in that repository, not part of plugin work.


## herdr source references

use herdr source references whenever you need to understand how herdr actually works in a way that the agent guide above does not easily provide.

These plugins run against the `gjermundgaraba/herdr` fork on branch `custom-v3` (see the root README's "Herdr build" section), not stock `herdrdev/herdr`. For source references, use a reusable, read-only checkout of that fork at `~/.cache/checkouts/github.com/gjermundgaraba/herdr` on `custom-v3`; clone it there if absent and never edit the shared checkout. The frontend socket lives in `src/client/frontend_api.rs`, its Rust client is the `herdr-frontend` crate in `sdk/frontend` (Micro's git dependency), and both are documented in `docs/next/website/src/content/docs/frontend-api.md`; the client command lane list is `CLIENT_SHELL_METHODS` in `src/server/client_commands.rs`. Consult upstream `herdrdev/herdr` only to tell fork additions apart from stock behavior.
