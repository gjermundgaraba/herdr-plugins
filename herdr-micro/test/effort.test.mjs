import assert from "node:assert/strict";
import fs from "node:fs/promises";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { nextThinkingLevel } from "../integrations/pi/herdr-effort.js";
import {
  validateEffortConfig,
} from "../src/effort-config.mjs";
import { changeEffort, planEffortChange } from "../src/effort.mjs";
import { installPiEffort } from "../src/pi-effort-install.mjs";

const effort = {
  codex: { raise: "ctrl+shift+t", lower: "ctrl+t" },
};

test("plans the native operation for each agent", () => {
  assert.deepEqual(planEffortChange("codex", "raise", "p1", effort), [
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
  assert.throws(
    () => planEffortChange("codex", "raise", "p1"),
    /not configured/,
  );
  assert.equal(validateEffortConfig(effort), effort);
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

test("Pi installer backs up a changed extension", async () => {
  const directory = await fs.mkdtemp(path.join(os.tmpdir(), "herdr-micro-"));
  const source = path.join(directory, "source.js");
  const target = path.join(directory, "extensions", "effort.ts");
  await fs.writeFile(source, "new\n");
  await fs.mkdir(path.dirname(target));
  await fs.writeFile(target, "old\n");

  const result = await installPiEffort(source, target, 123);
  assert.equal(await fs.readFile(target, "utf8"), "new\n");
  assert.equal(await fs.readFile(`${target}.bak-123`, "utf8"), "old\n");
  assert.equal(result.backup, `${target}.bak-123`);
  assert.equal((await installPiEffort(source, target)).unchanged, true);
});
