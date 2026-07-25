# herdr-micro

Herdr plugin for the Work Louder Codex Micro.

## Development

Requires Herdr 0.7.5 or newer and Node.js 22 or newer.

```sh
herdr plugin link .
herdr plugin action invoke status --plugin gjermundgaraba.herdr-micro
herdr plugin unlink gjermundgaraba.herdr-micro
```

Herdr injects the active session socket and plugin paths into each command.
Use the CLI at `HERDR_BIN_PATH` for short-lived actions; reserve the raw socket
API for the future long-running device event subscriber.

Plugin notes and primary-source links are in
[`docs/herdr-plugin-notes.md`](docs/herdr-plugin-notes.md).
