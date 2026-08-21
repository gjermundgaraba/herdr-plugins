#!/usr/bin/env python3
"""Move the active Herdr pane to a new or existing tab in its workspace.

With no arguments, skip the picker when the workspace has only one tab and
otherwise launch the `move-pane` workflow. The workflow uses the `source` and
`submit` subcommands.
"""

import json
import os
import subprocess
import sys


def required_env(name):
    value = os.environ.get(name)
    if not value:
        sys.exit(f"{name} is unavailable; run this from a Herdr popup command")
    return value


def herdr_command(*args, capture_stdout=False):
    command = [os.environ.get("HERDR_BIN_PATH", "herdr"), *args]
    options = {"check": True, "text": True}
    if capture_stdout:
        options["stdout"] = subprocess.PIPE
    else:
        options["stdout"] = subprocess.DEVNULL
    return subprocess.run(command, **options)


def list_tabs():
    workspace_id = required_env("HERDR_ACTIVE_WORKSPACE_ID")
    result = herdr_command(
        "tab", "list", "--workspace", workspace_id, capture_stdout=True
    )
    return json.loads(result.stdout)["result"]["tabs"]


def move_to_new_tab():
    herdr_command(
        "pane",
        "move",
        required_env("HERDR_ACTIVE_PANE_ID"),
        "--new-tab",
        "--workspace",
        required_env("HERDR_ACTIVE_WORKSPACE_ID"),
        "--focus",
    )


def launch():
    tabs = list_tabs()
    if not tabs:
        sys.exit("the active workspace has no tabs")
    if len(tabs) == 1:
        move_to_new_tab()
        return
    os.execvp("herdr-picker", ["herdr-picker", "run", "move-pane"])


def source():
    json.load(sys.stdin)  # drain the picker context
    active_tab_id = required_env("HERDR_ACTIVE_TAB_ID")
    tabs = list_tabs()

    items = [
        {
            "id": "new-tab",
            "title": "New tab",
            "subtitle": "Move this pane into a new tab",
            "badge": "+",
            "tone": "accent",
            "value": {"type": "new_tab"},
        }
    ]
    for tab in tabs:
        if tab["tab_id"] == active_tab_id:
            continue
        pane_count = tab["pane_count"]
        items.append(
            {
                "id": tab["tab_id"],
                "title": tab["label"],
                "subtitle": f"{pane_count} {'pane' if pane_count == 1 else 'panes'}"
                f" · {tab['agent_status']}",
                "value": {"type": "tab", "tab_id": tab["tab_id"]},
            }
        )

    print(json.dumps({"items": items}), flush=True)


def submit():
    context = json.load(sys.stdin)
    selection = context["selections"][context["step"]]["value"]
    if selection["type"] == "new_tab":
        move_to_new_tab()
        return
    if selection["type"] != "tab":
        sys.exit(f"unknown move-pane destination: {selection['type']}")

    herdr_command(
        "pane",
        "move",
        required_env("HERDR_ACTIVE_PANE_ID"),
        "--tab",
        selection["tab_id"],
        "--split",
        "right",
        "--focus",
    )


def main():
    command = sys.argv[1:] or ["launch"]
    if command == ["launch"]:
        launch()
    elif command == ["source"]:
        source()
    elif command == ["submit"]:
        submit()
    else:
        sys.exit("usage: herdr-picker-move-pane.py [launch|source|submit]")


if __name__ == "__main__":
    main()
