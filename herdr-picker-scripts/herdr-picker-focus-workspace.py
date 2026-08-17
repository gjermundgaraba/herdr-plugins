#!/usr/bin/env python3
"""Focus the workspace selected in herdr-picker."""

import json
import subprocess
import sys

context = json.load(sys.stdin)
selection = context["selections"][context["step"]]["value"]
subprocess.run(["herdr", "workspace", "focus", selection["workspace_id"]], check=True)
