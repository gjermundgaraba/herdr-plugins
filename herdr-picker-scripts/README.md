# One-shot picker script examples

`herdr-picker-agents.py` and `herdr-picker-workspaces.py` demonstrate one-shot
runtime inventory providers for the generic picker. These are examples: the
custom Herdr build owns History and the agent/workspace pickers natively.
`herdr-picker-move-pane.py` with `move-pane.toml` is the move-pane-to-tab picker;
bind it as a `type = "popup"` command on each host where it should run.

Copy `agents.toml` or `workspaces.toml` to your picker config directory and put
this directory on PATH. These scripts use the runtime `herdr` CLI, not frontend
routing, and do not aggregate SSH endpoints from a TUI.
