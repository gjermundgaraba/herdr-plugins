#!/usr/bin/env python3
"""One-shot agent source for herdr-picker, backed by `herdr api snapshot`."""

import json
import os
import subprocess
import sys

STYLE = {
    "blocked": (0, "◉", "danger", False),
    "done": (1, "●", "accent", False),
    "working": (2, "", "warning", True),
    "idle": (3, "✓", "success", False),
}
UNKNOWN = (4, "○", "muted", False)


def load_snapshot():
    command = [os.environ.get("HERDR_BIN_PATH", "herdr"), "api", "snapshot"]
    try:
        api = subprocess.run(command, check=True, capture_output=True, text=True)
    except FileNotFoundError:
        sys.exit(f"{command[0]} is not on PATH")
    except OSError as error:
        sys.exit(f"`{command[0]}` failed: {error}")
    except subprocess.CalledProcessError as error:
        sys.exit(f"`{' '.join(command)}` failed: {error.stderr.strip()}")
    return json.loads(api.stdout)["result"]["snapshot"]


def build_items(snapshot):
    labels = {w["workspace_id"]: w["label"] for w in snapshot["workspaces"]}

    items = []
    for agent in sorted(
        snapshot["agents"],
        key=lambda a: (
            STYLE.get(a["agent_status"], UNKNOWN)[0],
            -a.get("state_change_seq", 0),
        ),
    ):
        _, indicator, tone, spinning = STYLE.get(agent["agent_status"], UNKNOWN)
        workspace = labels.get(agent["workspace_id"], agent["workspace_id"])
        title = (
            agent.get("name")
            or agent.get("terminal_title_stripped")
            or agent.get("terminal_title")
            or agent["terminal_id"]
        )
        items.append(
            {
                "id": agent["pane_id"],
                "title": workspace if title == workspace else f"{workspace}: {title}",
                "subtitle": " · ".join(
                    value
                    for value in (
                        agent.get("foreground_cwd") or agent.get("cwd"),
                        agent.get("agent"),
                    )
                    if value
                ),
                "badge": agent.get("agent") or "",
                "indicator": indicator,
                "tone": tone,
                "spinning": spinning,
                "search": f"{agent['pane_id']} {agent['terminal_id']} {agent['agent_status']}",
                "value": {"pane_id": agent["pane_id"]},
            }
        )
    return items


def main():
    json.load(sys.stdin)  # the picker context is unused, but stdin must be drained
    print(json.dumps({"items": build_items(load_snapshot())}), flush=True)


if __name__ == "__main__":
    main()
