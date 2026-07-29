import { execFile } from "node:child_process";
import { setTimeout as sleep } from "node:timers/promises";
import { promisify } from "node:util";

const runFile = promisify(execFile);
const DIRECTIONS = new Set(["raise", "lower"]);

export function planEffortChange(agent, direction, paneId) {
  if (!DIRECTIONS.has(direction)) {
    throw new Error(`direction must be raise or lower, got ${direction}`);
  }
  if (!paneId) throw new Error("focused Herdr pane is required");

  switch (agent) {
    case "codex":
      return [
        {
          args: [
            "pane",
            "send-keys",
            paneId,
            direction === "raise" ? "ctrl+shift+t" : "ctrl+t",
          ],
        },
      ];
    case "claude":
      return [
        { args: ["pane", "send-text", paneId, "/effort"] },
        {
          args: ["pane", "send-keys", paneId, "enter"],
          waitAfterMs: 150,
        },
        {
          args: [
            "pane",
            "send-keys",
            paneId,
            direction === "raise" ? "right" : "left",
          ],
          waitAfterMs: 100,
        },
        { args: ["pane", "send-keys", paneId, "enter"] },
      ];
    case "pi":
      return [
        {
          args: [
            "pane",
            "send-keys",
            paneId,
            direction === "raise"
              ? "ctrl+shift+right"
              : "ctrl+shift+left",
          ],
        },
      ];
    default:
      throw new Error(`unsupported focused agent: ${agent || "none"}`);
  }
}

export async function changeEffort({
  herdrBin,
  agent,
  direction,
  paneId,
  run = runFile,
  wait = sleep,
}) {
  const plan = planEffortChange(agent, direction, paneId);
  for (const step of plan) {
    await run(herdrBin, step.args);
    if (step.waitAfterMs) await wait(step.waitAfterMs);
  }
  return { agent, direction, paneId };
}
