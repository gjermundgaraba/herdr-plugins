# herdr-picker

A standalone fuzzy picker and declarative workflow runner for
[Herdr](https://herdr.dev). Install the binary once, then create as many
pickers and direct keybindings as you want without writing a Herdr plugin.

`herdr-picker` provides the terminal UI, fuzzy matching, linear workflow
history, and JSON command boundary. It has no built-in data sources or domain
actions.

Linux and macOS with Herdr 0.8.0 or newer are supported.

## Install

With Nix:

```sh
nix profile install github:gjermundgaraba/herdr-plugins#herdr-picker
```

From a checkout:

```sh
cargo install --locked --path herdr-picker
```

The `herdr-picker` executable must be on `PATH`.

## Create a picker

Definitions live under:

```text
$HERDR_PICKER_CONFIG_DIR/
$XDG_CONFIG_HOME/herdr-picker/pickers/
$HOME/.config/herdr-picker/pickers/
```

For example, save this as
`~/.config/herdr-picker/pickers/new-agent.toml`:

```toml
title = "New agent"
mode = "direct"
submit = ["python3", "/absolute/path/launch-agent.py"]

[[steps]]
id = "model"
title = "Choose model"

[[steps.items]]
id = "opus"
title = "Claude Opus"
subtitle = "Anthropic"
badge = "model"
value = { model = "claude-opus" }

[[steps.items]]
id = "gpt"
title = "GPT"
subtitle = "OpenAI"
badge = "model"
value = { model = "gpt" }

[[steps]]
id = "harness"
title = "Choose harness"
source = ["python3", "/absolute/path/list-harnesses.py"]
```

Run or validate it:

```sh
herdr-picker check new-agent
herdr-picker run new-agent
herdr-picker list
```

Picker names and step IDs may contain letters, numbers, `-`, and `_`. Set
`mode = "vim"` for Vim-style input.

## Direct keybindings

Add one popup command per picker to `~/.config/herdr/config.toml`:

```toml
[[keys.command]]
key = "prefix+n"
type = "popup"
command = "herdr-picker run new-agent"
description = "new agent"
width = "88%"
height = "80%"
```

Apply the change with:

```sh
herdr server reload-config
```

There is no launcher step. Herdr supplies the active workspace, tab, pane, cwd,
and socket environment to the picker and its commands.

## Sources

A step defines either inline `items` or a source command:

```toml
[[steps]]
id = "issue"
title = "Choose issue"
source = ["python3", "/absolute/path/search-issues.py"]
search = "provider"
```

Commands are argv arrays and do not implicitly run through a shell. The picker
writes one JSON context to source stdin and then closes stdin:

```json
{"workflow":"open-issue","step":"issue","query":"auth","selections":{"repo":{"id":"api","value":{"repo":"api"}}}}
```

The source writes one complete replacement snapshot per stdout line:

```json
{"items":[{"id":"42","title":"Authentication fails","subtitle":"api","value":{"number":42}}]}
```

A minimal Python source is:

```python
import json
import sys

context = json.load(sys.stdin)
items = search(context["query"], context["selections"])
print(json.dumps({"items": items}), flush=True)
```

Complete worked examples live in
[`herdr-picker-scripts`](../herdr-picker-scripts) (plain Python scripts) and
the live Rust packages
the one-shot [`herdr-picker-scripts`](../herdr-picker-scripts) examples.
Herdr agent/workspace navigation now lives in the TUI Navigator.

`id` and `title` are required and IDs must be unique. Put domain data under
`value`. Optional UI fields are `subtitle`, `detail`, `badge`, `indicator`,
`tone`, `spinning`, and `search`; `tone` may be `muted`, `accent`, `success`,
`warning`, or `danger`.

A source may emit more snapshots while it remains open. Each snapshot replaces
all rows and preserves the selected ID when possible. Frames are limited to 1
MiB and delivery uses capacity-one FIFO backpressure.

### Local and provider search

`search = "local"` is the default. The picker starts the source once and fuzzy
filters every snapshot itself. This suits live Herdr, filesystem, and process
updates.

`search = "provider"` delegates ordering and matching to the source. When the
query changes, the picker stops the previous source immediately, clears its
rows, waits for the 60 ms debounce, and starts a new source with the current
query. Only one source and one pending query can exist.

### Completion and errors

- `{"items":[]}` is a successful empty result.
- Exit zero after at least one snapshot is successful one-shot completion.
- A source may stay open and continue streaming snapshots.
- `{"error":"Service unavailable; reconnecting…"}` clears rows and displays an
  error while the source stays open. The next item snapshot clears the error.
- Exit zero before a snapshot, exit nonzero, malformed output, and invalid items
  are errors. Failed-source rows are cleared.
- The final 64 KiB of stderr is captured for failures and shown as wrapped popup
  diagnostics when space permits.

Source processes run in their own process groups. Leaving a step or replacing a
remote search terminates the whole group.

## Workflow context

Sources and the final submit command receive the same context shape. Selections
contain only stable item IDs and values:

```json
{
  "workflow": "new-agent",
  "step": "harness",
  "query": "claude",
  "selections": {
    "model": {
      "id": "opus",
      "value": {"model": "claude-opus"}
    },
    "harness": {
      "id": "claude",
      "value": {"command": "claude"}
    }
  }
}
```

Steps run in declaration order. Back returns to the previous step with its query
and highlighted item restored, then restarts that step's source. From a popup,
the final submit runs in a detached worker after the popup closes. Direct
terminal invocations wait for submit and report its exit status.

Detached popup-submit failures are logged to
`$XDG_STATE_HOME/herdr-picker/picker.log` or
`~/.local/state/herdr-picker/picker.log` and sent to Herdr through a best-effort
notification. Direct invocations report submit failures on stderr and exit
unsuccessfully. Successful submissions stay silent.

## Controls

| Control | Action |
|---|---|
| Type | Fuzzy search |
| `Enter` / click row | Choose and advance |
| `Up` / `Down`, `Ctrl+N` / `Ctrl+P` | Move |
| `PageUp` / `PageDown`, `Home` / `End` | Move farther |
| `Ctrl+U` | Clear search |
| `Esc` / click Back or Close | Back or close |
| `Ctrl+C` | Cancel the workflow |

In Vim mode, use `j` / `k` to move, `/` to search, and `Esc` to return to
normal mode before going back. The visible Back/Close button always performs
that action directly.

## License

Apache-2.0. Portions originated in
[Herdr](https://github.com/herdrdev/herdr)'s Apache-2.0-licensed picker code.
See [`../LICENSE`](../LICENSE).
