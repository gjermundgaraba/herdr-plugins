import assert from "node:assert/strict";
import test from "node:test";
import { diffPaneArgs } from "../src/diff-pane.mjs";
import {
  claimIdentity,
  configureAppSense,
  focusedAppForClaim,
  resolveLayerClaim,
} from "../src/layer-claims.mjs";
import {
  assignSlots,
  deviceOwner,
  encodeMessage,
  encoderEffortDirection,
  Reassembler,
  slotLighting,
} from "../src/micro-protocol.mjs";
import { reviewPrompt } from "../src/review-prompt.mjs";

const agent = (id, status, seq = 0) => ({
  terminal_id: id,
  agent_status: status,
  state_change_seq: seq,
});

test("keeps slots sticky, admits urgent agents, and frames device messages", () => {
  const initial = assignSlots([], [
    agent("idle", "idle"),
    agent("working", "working"),
  ]);
  assert.deepEqual(initial.slice(0, 2), ["working", "idle"]);

  const full = ["a", "b", "c", "d", "e", "f"];
  const agents = full.map((id) => agent(id, "idle"));
  const replaced = assignSlots(full, [...agents, agent("blocked", "blocked")]);
  assert.equal(replaced.includes("blocked"), true);

  const lights = slotLighting(replaced, [
    ...agents,
    agent("blocked", "blocked"),
  ]);
  assert.equal(lights.length, 6);
  assert.equal(lights.find((light) => light.c === 0xffaa00)?.e, 1);

  const reports = encodeMessage("test", { text: "x".repeat(100) });
  assert.equal(reports.length, 3);
  assert.equal(reports.every((report) => report.length === 64), true);
  const decoder = new Reassembler();
  const incoming = encodeMessage("event", { ok: true });
  assert.deepEqual(
    incoming.flatMap((report) => decoder.push(report)),
    [JSON.stringify({ m: "event", p: { ok: true } })],
  );
});

test("decodes IOKit reports with and without the report ID", () => {
  const report = encodeMessage("event", { ok: true })[0];
  assert.equal(new Reassembler().push(report).length, 1);
  assert.equal(new Reassembler().push(report.subarray(1)).length, 1);
});

test("maps the physical dial direction to effort direction", () => {
  assert.equal(encoderEffortDirection("ENC_CW"), "lower");
  assert.equal(encoderEffortDirection("ENC_CC"), "raise");
});

test("translates the review skill syntax for each focused agent", () => {
  assert.equal(
    reviewPrompt("codex"),
    "$deslop $ponytail:ponytail-review\nReview scope: uncomitted changes",
  );
  assert.equal(
    reviewPrompt("claude"),
    "/deslop /ponytail:ponytail-review\nReview scope: uncomitted changes",
  );
  assert.equal(
    reviewPrompt("pi"),
    "/skill:deslop /skill:ponytail:ponytail-review\nReview scope: uncomitted changes",
  );
  assert.throws(() => reviewPrompt("other"), /unsupported focused agent/);
});

test("opens Hunk in the focused agent's repository", () => {
  assert.deepEqual(
    diffPaneArgs({
      cwd: "/repo",
    }),
    [
      "plugin",
      "pane",
      "open",
      "--plugin",
      "gjermundgaraba.herdr-micro",
      "--entrypoint",
      "diff",
      "--cwd",
      "/repo",
      "--focus",
    ],
  );
  assert.throws(() => diffPaneArgs({}), /no repository context/);
});

test("Codex owns the device only while it is frontmost", () => {
  const codex = "/Applications/Codex.app/Contents/MacOS/ChatGPT";
  assert.equal(deviceOwner(codex, "com.openai.codex"), codex);
  assert.equal(deviceOwner(codex, "com.mitchellh.ghostty"), null);
});

test("the last matching frontmost-window claim wins", () => {
  const claims = [
    { id: "terminal", layer: 3, process: "dev.ghostty" },
    {
      id: "herdr",
      layer: 2,
      process: "dev.ghostty",
      titleIncludes: "herdr",
    },
  ];
  assert.deepEqual(
    resolveLayerClaim(claims, {
      process: "dev.ghostty",
      title: "Herdr workspace",
    }),
    claims[1],
  );
  assert.equal(
    resolveLayerClaim(claims, { process: "com.apple.Safari", title: "" }),
    null,
  );
  assert.deepEqual(claimIdentity(claims[1]), {
    appName: "Herdr Micro Layer 2",
    process: "gjermundgaraba.herdr-micro.layer-2",
  });
});

test("AppSense setup binds explicit default and claimed layers", () => {
  const keymap = {
    profiles: [
      {
        layers: [
          { id: 0, layout: { keymap: [["KV_OAI_AG05"]] } },
          { id: 1, layout: {} },
        ],
      },
    ],
    linkedApps: [],
  };
  configureAppSense(keymap);
  assert.deepEqual(
    keymap.profiles[0].layers.map(({ linkedAppId }) => linkedAppId),
    [0, 1],
  );
  assert.deepEqual(
    keymap.linkedApps.map(({ name, process }) => [name, process]),
    [
      [
        "Herdr Micro Layer 1",
        "gjermundgaraba.herdr-micro.layer-1",
      ],
      [
        "Herdr Micro Layer 2",
        "gjermundgaraba.herdr-micro.layer-2",
      ],
    ],
  );
});

test("unclaimed windows preserve the last applicable layer", () => {
  const claims = [
    { id: "herdr", layer: 2, process: "com.mitchellh.ghostty" },
    { id: "codex", layer: 1, process: "com.openai.codex" },
  ];
  const commands = [
    { process: "com.mitchellh.ghostty", title: "" },
    { process: "com.google.Chrome", title: "" },
    { process: "com.openai.codex", title: "" },
    { process: "com.google.Chrome", title: "" },
  ].map((frontmost) =>
    focusedAppForClaim(resolveLayerClaim(claims, frontmost)),
  );
  assert.deepEqual(
    commands.map((command) => command?.process ?? null),
    [
      "gjermundgaraba.herdr-micro.layer-2",
      null,
      "gjermundgaraba.herdr-micro.layer-1",
      null,
    ],
  );
});
