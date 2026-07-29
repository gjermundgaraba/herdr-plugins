import assert from "node:assert/strict";
import test from "node:test";
import { nextThinkingLevel } from "../integrations/pi/herdr-effort.js";
import { changeEffort, planEffortChange } from "../src/effort.mjs";

test("plans the native operation for each agent", () => {
  assert.deepEqual(planEffortChange("codex", "raise", "p1"), [
    { args: ["pane", "send-keys", "p1", "ctrl+shift+t"] },
  ]);
  assert.deepEqual(planEffortChange("claude", "lower", "p2"), [
    { args: ["pane", "send-text", "p2", "/effort"] },
    {
      args: ["pane", "send-keys", "p2", "enter"],
      waitAfterMs: 150,
    },
    {
      args: ["pane", "send-keys", "p2", "left"],
      waitAfterMs: 100,
    },
    { args: ["pane", "send-keys", "p2", "enter"] },
  ]);
  assert.deepEqual(planEffortChange("pi", "raise", "p3"), [
    { args: ["pane", "send-keys", "p3", "ctrl+shift+right"] },
  ]);
});

test("executes a frozen plan in order", async () => {
  const calls = [];
  await changeEffort({
    herdrBin: "/bin/herdr",
    agent: "claude",
    direction: "raise",
    paneId: "p2",
    run: async (bin, args) => calls.push([bin, args]),
    wait: async (ms) => calls.push(["wait", ms]),
  });
  assert.deepEqual(calls, [
    ["/bin/herdr", ["pane", "send-text", "p2", "/effort"]],
    ["/bin/herdr", ["pane", "send-keys", "p2", "enter"]],
    ["wait", 150],
    ["/bin/herdr", ["pane", "send-keys", "p2", "right"]],
    ["wait", 100],
    ["/bin/herdr", ["pane", "send-keys", "p2", "enter"]],
  ]);
});

test("Pi effort is bidirectional and clamps at the ends", () => {
  assert.equal(nextThinkingLevel("medium", "raise"), "high");
  assert.equal(nextThinkingLevel("medium", "lower"), "low");
  assert.equal(nextThinkingLevel("max", "raise"), "max");
  assert.equal(nextThinkingLevel("off", "lower"), "off");
});
