# herdr-plugins agent instructions

This is a repo with herdr plugins.

For changes to plugin manifests, actions, or Herdr APIs, consult the relevant sections of the [Herdr agent guide](https://herdr.dev/agent-guide.md).

## general instructions

- Before reporting a plugin source change done, run all applicable manifest `[[build]]` commands in order from that plugin's directory, including the install and atomic replacement steps. Tests and bare `cargo build` do not update the installed executable used by a locally linked plugin. `herdr plugin link` only registers the plugin; it does not build or install its executable.
- Runtime commands must use the plugin's independent `bin/` copy, never a binary or symlink into `target/`. Keep Cargo artifacts separate from installed executables so build-cache cleanup cannot break plugins. Follow the package README for the build and install sequence.
- For other daemons and installed services, follow the plugin’s lifecycle documentation: `herdr-hub/README.md` for Hub and `herdr-micro/docs/micro-bridge.md` for Micro. Hub’s installed service copy can be refreshed after rebuilding with `herdr-hub/bin/herdr-hub install-service`.
- Never suggest "upstreaming a change to herdr itself". If we can't do something in an extension today, we can't do it today. Just plainly say that if this is the case.


## upstream herdr source references

use herdr source references whenever you need to understand how actually herdr works in a way that the agent guide above does not easily provide.

For upstream source references, use a reusable, read-only checkout of `herdrdev/herdr` at `~/.cache/checkouts/github.com/herdrdev/herdr`; clone it there if absent and never edit the shared checkout. Check out the current version of herdr we use.
