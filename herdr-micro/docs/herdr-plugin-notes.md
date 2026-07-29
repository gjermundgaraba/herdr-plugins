# Herdr plugin notes

Checked on 2026-07-26 against the official Herdr documentation, Herdr source, and the installed `herdr 0.7.5` binary.

## Minimal package contract

Herdr plugins are ordinary executable packages, not SDK modules. Herdr validates `herdr-plugin.toml`, launches its declared argv commands, injects context, and records command logs. The plugin chooses its own language and dependencies. There is no separate plugin SDK. [Official plugin overview](https://herdr.dev/docs/plugins/#overview)

A minimal useful macOS manifest is:

```toml
id = "gjermundgaraba.herdr-micro"
name = "Herdr Micro"
version = "0.1.0"
min_herdr_version = "0.7.5"
description = "Control Herdr from a Work Louder Codex Micro."
platforms = ["macos"]

[[actions]]
id = "status"
title = "Show Herdr status"
command = ["node", "src/status.mjs"]
```

- `id`, `name`, `version`, and `min_herdr_version` are required. `description` is optional. Omitting top-level `platforms` is accepted for local development but produces a warning. [Manifest reference](https://herdr.dev/docs/plugins/#manifest)
- `command` is an argv array, not a shell command. There is no shell expansion unless the first argv item explicitly launches a shell. Commands run with the plugin root as their working directory. [Manifest reference](https://herdr.dev/docs/plugins/#manifest), [runtime environment](https://herdr.dev/docs/plugins/#commands-and-environment)
- `package.json`, Node, TypeScript, and build tooling are optional from Herdr's perspective. Keep only what this plugin actually needs.
- Actions, event hooks, startup hooks, panes, and link handlers are manifest-declared in plugin v1; runtime action registration is unavailable. [Official plugin overview](https://herdr.dev/docs/plugins/#overview)

`min_herdr_version = "0.7.5"` is the correct conservative baseline here: this machine runs 0.7.5, and 0.7.5 introduced one-shot startup hooks plus user-global plugin registration. [Herdr 0.7.5 changelog](https://github.com/ogulcancelik/herdr/blob/master/CHANGELOG.md#075---2026-07-21)

## Lifecycle

### Registration

```sh
herdr plugin link .
herdr plugin list --plugin gjermundgaraba.herdr-micro --json
herdr plugin unlink gjermundgaraba.herdr-micro
```

`link` accepts either the plugin directory or the manifest path, validates it, creates the plugin config/state directories, and registers the local checkout without copying it. It does **not** run `[[build]]`; local development builds are the author's responsibility. `unlink` unregisters the plugin but leaves the checkout alone. GitHub installation instead uses `herdr plugin install owner/repo[/subdir]`, runs declared builds, and stores a managed checkout. [Install and link](https://herdr.dev/docs/plugins/#install-and-link), [build commands](https://herdr.dev/docs/plugins/#build-commands)

As of 0.7.5, registration and enabled state are global to the current user, not scoped to one Herdr session. [CLI reference](https://herdr.dev/docs/cli-reference/#plugins)

### Actions and event hooks

- `plugin action invoke` starts the fixed manifest command asynchronously and returns its invocation context and initial command-log record. Herdr fills missing context from the active workspace, tab, focused pane, worktree, and request. [Plugin API](https://herdr.dev/docs/socket-api/#plugin-apis)
- `[[events]]` launches the declared command once per matching Herdr event while the plugin is enabled. `pane.agent_status_changed` is a supported hook and is the simplest event-driven entrypoint for RGB refreshes. Unknown event names link with a warning rather than failing. [Plugin API](https://herdr.dev/docs/socket-api/#plugin-apis)
- Use `herdr plugin log list` to inspect captured stdout, stderr, exit status, and startup/action/event failures. [CLI reference](https://herdr.dev/docs/cli-reference/#plugins)

### Startup is not daemon supervision

`[[startup]]` commands run once after session restore when the API socket is ready, and again after live handoff. They do not run when a client attaches, config reloads, or a plugin is linked or enabled. Herdr starts them asynchronously and logs failures, but explicitly defines them as one-shot initialization—not supervised daemons. A startup command should restore state or ensure an independently managed daemon exists, then exit. [Startup hooks](https://herdr.dev/docs/plugins/#startup-hooks)

Consequences for `herdr-micro`:

1. Keep a manual `start` action while developing; linking alone will not start the bridge.
2. If the final plugin needs a persistent HID/event process, give it an explicit idempotent start/stop protocol.
3. Do not rely on disable or unlink to terminate that process. The public lifecycle promises no teardown, and the current handlers only mutate registration/enabled state. [Current Herdr source: plugin handlers](https://github.com/ogulcancelik/herdr/blob/d4e0dd3d903c50d2edb8c3cec71952a83989b310/src/app/api/plugins/mod.rs#L89-L159)

## Runtime environment and storage

Every runtime command receives:

```text
HERDR_SOCKET_PATH
HERDR_BIN_PATH
HERDR_ENV=1
HERDR_PLUGIN_ID
HERDR_PLUGIN_ROOT
HERDR_PLUGIN_CONFIG_DIR
HERDR_PLUGIN_STATE_DIR
HERDR_PLUGIN_CONTEXT_JSON
HERDR_WORKSPACE_ID       when available
HERDR_TAB_ID             when available
HERDR_PANE_ID            when available
```

Actions additionally receive `HERDR_PLUGIN_ACTION_ID`; startup/event hooks receive `HERDR_PLUGIN_EVENT`; event hooks also receive `HERDR_PLUGIN_EVENT_JSON`; pane commands receive `HERDR_PLUGIN_ENTRYPOINT_ID`. [Runtime environment](https://herdr.dev/docs/plugins/#commands-and-environment)

Use:

- `HERDR_PLUGIN_ROOT` for shipped source/read-only assets only.
- `HERDR_PLUGIN_CONFIG_DIR` for user-editable configuration and secrets.
- `HERDR_PLUGIN_STATE_DIR` for plugin-owned runtime/durable state.

Herdr creates the config and state directories but provides no storage API, schema, migrations, cleanup, or process supervision. [Runtime environment](https://herdr.dev/docs/plugins/#commands-and-environment), [storage](https://herdr.dev/docs/plugins/#storage)

On this machine for this plugin:

```text
/Users/gg/.config/herdr/plugins/config/gjermundgaraba.herdr-micro
/Users/gg/.local/state/herdr/plugins/gjermundgaraba.herdr-micro
```

Code should consume the injected paths rather than reconstructing them. Humans/scripts can discover config with:

```sh
herdr plugin config-dir gjermundgaraba.herdr-micro
```

The current source derives these from XDG config/state roots and keeps the global registry at `~/.config/herdr/plugins.json` by default. [Current path implementation](https://github.com/ogulcancelik/herdr/blob/d4e0dd3d903c50d2edb8c3cec71952a83989b310/src/plugin_paths.rs), [registry implementation](https://github.com/ogulcancelik/herdr/blob/d4e0dd3d903c50d2edb8c3cec71952a83989b310/src/persist/plugin_registry.rs)

## CLI versus socket API

Prefer `HERDR_BIN_PATH` plus Herdr CLI commands for one-shot operations. It is the portable route across Unix sockets and Windows named pipes. Use the raw socket for a long-lived RGB status subscriber. [Choosing an API layer](https://herdr.dev/docs/socket-api/#choose-an-integration-layer), [runtime environment](https://herdr.dev/docs/plugins/#commands-and-environment)

The raw transport is newline-delimited JSON. On macOS it is a Unix-domain socket at `HERDR_SOCKET_PATH`; each request is one JSON line, responses repeat the request `id`, and an `events.subscribe` connection stays open for pushed events. [Socket transport](https://herdr.dev/docs/socket-api/#socket-transport)

A stateful bridge should:

1. Call `session.snapshot` once to bootstrap workspaces, panes, agents, focus, and layouts.
2. Subscribe to pane/agent lifecycle events, including `pane.agent_status_changed`.
3. Rebuild from a fresh snapshot after reconnecting or suspected cache drift.

`session.snapshot` is explicitly a bootstrap, not a subscription. The event API acknowledges first, then pushes later lines. [Snapshot behavior](https://herdr.dev/docs/socket-api/#raw-methods), [event subscriptions](https://herdr.dev/docs/socket-api/#event-subscriptions)

Generate the exact protocol schema from the installed binary instead of hand-maintaining request types:

```sh
herdr api schema --json
herdr api schema --output herdr-api.schema.json
```

The generated schema covers requests, responses, errors, emitted events, and subscription events. [Socket schema](https://herdr.dev/docs/socket-api/#schema)

## Development check

```sh
npm test
herdr plugin link .
herdr plugin list --plugin gjermundgaraba.herdr-micro --json
herdr plugin action list --plugin gjermundgaraba.herdr-micro
herdr plugin action invoke status --plugin gjermundgaraba.herdr-micro
herdr plugin log list --plugin gjermundgaraba.herdr-micro --limit 10
```

The installed 0.7.5 CLI successfully linked the current scaffold with no warnings, exposed its `status` action, and created both plugin-owned directories. Re-run `plugin link .` after manifest edits so the validation result is explicit. Use `plugin unlink` for cleanup; do not use `uninstall` for a linked checkout.
