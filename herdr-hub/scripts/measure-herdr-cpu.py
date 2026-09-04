#!/usr/bin/env python3
"""Measure a process's CPU usage from the delta of cumulative CPU time."""

import argparse
import json
import subprocess
import time


def cpu_seconds(pid: int) -> float:
    value = subprocess.check_output(
        ["ps", "-o", "time=", "-p", str(pid)], text=True
    ).strip()
    if not value:
        raise SystemExit(f"process {pid} is not running")

    days = 0
    if "-" in value:
        day, value = value.split("-", 1)
        days = int(day)
    parts = [float(part) for part in value.split(":")]
    seconds = parts.pop()
    minutes = parts.pop() if parts else 0
    hours = parts.pop() if parts else 0
    return days * 86_400 + hours * 3_600 + minutes * 60 + seconds


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--pid", type=int, required=True)
    parser.add_argument("--baseline", type=float, default=30.0)
    args = parser.parse_args()

    started = time.monotonic()
    cpu_started = cpu_seconds(args.pid)
    time.sleep(args.baseline)
    elapsed = time.monotonic() - started
    cpu_elapsed = cpu_seconds(args.pid) - cpu_started
    print(
        json.dumps(
            {
                "pid": args.pid,
                "wall_seconds": round(elapsed, 3),
                "cpu_seconds": round(cpu_elapsed, 3),
                "cpu_percent": round(cpu_elapsed / elapsed * 100, 2),
            },
            sort_keys=True,
        )
    )


if __name__ == "__main__":
    main()
