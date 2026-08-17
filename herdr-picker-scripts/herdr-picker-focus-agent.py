#!/usr/bin/env python3
"""Focus the agent pane selected in herdr-picker."""

import json
import subprocess
import sys

context = json.load(sys.stdin)
selection = context["selections"][context["step"]]["value"]
subprocess.run(["herdr", "agent", "focus", selection["pane_id"]], check=True)
