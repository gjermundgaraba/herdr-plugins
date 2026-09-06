# herdr-plugins agent instructions

This is a repo with herdr plugins.

For changes to plugin manifests, actions, or Herdr APIs, consult the relevant sections of the [Herdr agent guide](https://herdr.dev/agent-guide.md).

## general instructions

- Before reporting a plugin source change done, rebuild any generated executable it affects
using the applicable manifest `[[build]]` command(s). Tests and debug builds may not update the artifact used by a locally linked plugin.
- Plugins with a long-running daemon (currently `history`) swap in a rebuilt
executable automatically on the next action; in-memory daemon state (focus
history) resets when the binary actually changed. To verify a change
immediately instead of waiting for the next keypress, invoke any action after
rebuilding, e.g. `herdr plugin action invoke gjermundgaraba.herdr-history.activate`.
- Never suggest "upstreaming a change to herdr itself". If we can't do something in an extension today, we can't do it today. Just plainly say that if this is the case.


## upstream herdr source references

use herdr source references whenever you need to understand how actually herdr works in a way that the agent guide above does not easily provide.

For upstream source references, use a reusable, read-only checkout of `herdrdev/herdr` at `~/.cache/checkouts/github.com/herdrdev/herdr`; clone it there if absent and never edit the shared checkout. Check out the current version of herdr we use.
