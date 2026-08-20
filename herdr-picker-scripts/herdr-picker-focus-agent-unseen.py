#!/usr/bin/env python3
"""Focus the selected agent pane, then mark the pane you left as unseen.

Uses the fork-only `pane.mark_unseen` API method; the origin pane comes from
`HERDR_ACTIVE_PANE_ID`, captured when the picker popup opened. Leaving a
working, blocked, or non-agent pane is tolerated (`pane_not_idle`).
"""

import json
import os
import socket
import subprocess
import sys

context = json.load(sys.stdin)
selection = context["selections"][context["step"]]["value"]
subprocess.run(["herdr", "agent", "focus", selection["pane_id"]], check=True)

origin = os.environ.get("HERDR_ACTIVE_PANE_ID")
if not origin or origin == selection["pane_id"]:
    sys.exit(0)

request = {"id": "picker-unseen", "method": "pane.mark_unseen", "params": {"pane_id": origin}}
with socket.socket(socket.AF_UNIX) as conn:
    conn.connect(os.environ["HERDR_SOCKET_PATH"])
    conn.sendall((json.dumps(request) + "\n").encode())
    response = json.loads(conn.makefile().readline())

error = response.get("error", {})
if error and error.get("code") != "pane_not_idle":
    sys.exit(f"pane.mark_unseen failed: {response}")
