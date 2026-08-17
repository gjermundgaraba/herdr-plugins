#!/usr/bin/env python3
"""One-shot workspace source for herdr-picker, backed by `herdr api snapshot`."""

import json
import subprocess
import sys

STYLE = {
    "blocked": ("◉", "danger", False),
    "done": ("●", "accent", False),
    "working": ("", "warning", True),
    "idle": ("✓", "success", False),
}
UNKNOWN = ("○", "muted", False)

json.load(sys.stdin)  # the picker context is unused, but stdin must be drained
api = subprocess.run(
    ["herdr", "api", "snapshot"], check=True, capture_output=True, text=True
)
snapshot = json.loads(api.stdout)["result"]["snapshot"]

items = []
for workspace in sorted(snapshot["workspaces"], key=lambda w: w["number"]):
    indicator, tone, spinning = STYLE.get(workspace["agent_status"], UNKNOWN)
    items.append(
        {
            "id": workspace["workspace_id"],
            "title": workspace["label"],
            "subtitle": "{} tabs · {} panes · {}".format(
                workspace["tab_count"], workspace["pane_count"], workspace["agent_status"]
            ),
            "badge": str(workspace["number"]),
            "indicator": indicator,
            "tone": tone,
            "spinning": spinning,
            "search": f"{workspace['workspace_id']} {workspace['agent_status']}",
            "value": {"workspace_id": workspace["workspace_id"]},
        }
    )

print(json.dumps({"items": items}), flush=True)
