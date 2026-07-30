import assert from "node:assert/strict";
import test from "node:test";
import {
  DEFAULT_CONTROLS,
  keyBinding,
  resolveBinding,
  validateControls,
} from "../src/control-config.mjs";
import { diffPaneArgs } from "../src/diff-pane.mjs";
import { fastModePlan } from "../src/fast-mode.mjs";
import {
  claimIdentity,
  configureAppSense,
  configureMicro,
  focusedAppForClaim,
  resolveLayerClaim,
  validateLayerClaims,
} from "../src/layer-claims.mjs";
import {
  DEFAULT_LIGHTING,
  validateLightingConfig,
} from "../src/lighting-config.mjs";
import { deviceEvent } from "../src/micro-device.mjs";
import {
  aggregateLighting,
  assignSlots,
  deviceOwner,
  encodeMessage,
  joystickEvent,
  Reassembler,
  slotLighting,
} from "../src/micro-protocol.mjs";
import { scrollPlan } from "../src/scroll.mjs";
import { promptArgs, submitArgs } from "../src/submit.mjs";

const agent = (id, status, seq = 0) => ({
  terminal_id: id,
  agent_status: status,
  state_change_seq: seq,
});

test("validates controls and resolves device and agent bindings", () => {
  assert.equal(validateControls(DEFAULT_CONTROLS), DEFAULT_CONTROLS);
  assert.deepEqual(keyBinding(DEFAULT_CONTROLS, "ACT09", 1), {
    action: "prompt",
    prompt: "/copy",
    submit: true,
  });
  assert.deepEqual(keyBinding(DEFAULT_CONTROLS, "ENC_CC", 2), {
    action: "effort",
    direction: "raise",
  });
  assert.equal(keyBinding(DEFAULT_CONTROLS, "ENC_CC", 1), null);
  assert.deepEqual(
    resolveBinding(DEFAULT_CONTROLS.buttons[3], "codex"),
    { action: "fast" },
  );
  assert.equal(resolveBinding(DEFAULT_CONTROLS.buttons[3], "claude"), null);
  assert.throws(
    () =>
      validateControls({
        ...DEFAULT_CONTROLS,
        buttons: { 8: { action: "submit" } },
      }),
    /invalid button/,
  );
  assert.throws(
    () =>
      validateControls({
        ...DEFAULT_CONTROLS,
        dial: { press: { action: "prompt", prompt: "", submit: true } },
      }),
    /non-empty string/,
  );
  assert.equal(
    validateControls({
      ...DEFAULT_CONTROLS,
      joystick: {
        ...DEFAULT_CONTROLS.joystick,
        up: { action: "scroll", direction: "up", percent: 50 },
      },
    }).joystick.up.percent,
    50,
  );
  assert.throws(
    () =>
      validateControls({
        ...DEFAULT_CONTROLS,
        joystick: {
          ...DEFAULT_CONTROLS.joystick,
          up: { action: "scroll", direction: "up", percent: 0 },
        },
      }),
    /percent/,
  );
});

test("plans screen-relative scrolling without resizing the pane", () => {
  const plan = scrollPlan(
    { pane_id: "w1:p1", scroll: { viewport_rows: 71 } },
    {
      area: { x: 32, y: 1, width: 250, height: 73 },
      panes: [
        {
          pane_id: "w1:p1",
          rect: { x: 32, y: 1, width: 125, height: 73 },
        },
      ],
    },
    "up",
    50,
  );
  assert.deepEqual(
    plan,
    {
      paneId: "w1:p1",
      notches: 12,
      x: 94.5 / 282,
      y: 37.5 / 74,
    },
  );
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

test("decodes key and joystick device notifications", () => {
  assert.deepEqual(deviceEvent("v.oai.hid", { k: "ENC_CLK", act: 1 }), {
    type: "key",
    key: "ENC_CLK",
    action: 1,
  });
  assert.deepEqual(deviceEvent("v.oai.rad", { a: 0.75, d: 0.9 }), {
    type: "joystick",
    angle: 0.75,
    distance: 0.9,
  });
  assert.equal(deviceEvent("other", {}), null);
});

test("maps joystick vectors once per sector and rearms at center", () => {
  assert.deepEqual(joystickEvent(0, 0.5, null), {
    sector: null,
    direction: null,
  });
  assert.deepEqual(joystickEvent(0, 0.8, null), {
    sector: 0,
    direction: "right",
  });
  assert.deepEqual(joystickEvent(0, 0.9, 0), {
    sector: 0,
    direction: null,
  });
  assert.deepEqual(joystickEvent(0.25, 0.9, 0), {
    sector: 1,
    direction: "down",
  });
  assert.deepEqual(joystickEvent(0.25, 0.1, 1), {
    sector: null,
    direction: null,
  });
  assert.deepEqual(
    joystickEvent(0, 0.65, null, {
      engageDistance: 0.6,
      releaseDistance: 0.2,
    }),
    { sector: 0, direction: "right" },
  );
});

test("configures per-agent and aggregate status lighting", () => {
  assert.equal(validateLightingConfig(DEFAULT_LIGHTING), DEFAULT_LIGHTING);
  assert.throws(
    () =>
      validateLightingConfig({
        ...DEFAULT_LIGHTING,
        ambient: "rainbow",
      }),
    /ambient/,
  );
  const config = {
    ...DEFAULT_LIGHTING,
    states: {
      blocked: { c: 0xffaa00, b: 1, e: 1, s: 0 },
      done: { c: 0x22cc55, b: 1, e: 1, s: 0 },
      working: { c: 0x2277ff, b: 1, e: 4, s: 0.35 },
      idle: { c: 0xffffff, b: 0.25, e: 1, s: 0 },
      unknown: { c: 0xffffff, b: 0.08, e: 1, s: 0 },
    },
  };
  const agents = [
    { ...agent("idle", "idle"), focused: true },
    agent("blocked", "blocked"),
  ];
  assert.equal(slotLighting(["idle"], agents, config)[0].b, 1);
  assert.deepEqual(
    aggregateLighting(["idle", "blocked"], agents, config),
    {
      ambient: { e: 1, b: 1, s: 0, c: 0xffaa00 },
    },
  );
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

test("submits one Enter to the focused agent", () => {
  assert.deepEqual(
    promptArgs(
      { action: "prompt", prompt: "/model" },
      { pane_id: "w1:p2" },
    ),
    ["agent", "prompt", "w1:p2", "/model"],
  );
  assert.deepEqual(
    promptArgs(
      { action: "prompt", prompt: "/model", submit: false },
      { pane_id: "w1:p2" },
    ),
    ["pane", "send-text", "w1:p2", "/model"],
  );
  assert.deepEqual(submitArgs({ pane_id: "w1:p2" }), [
    "agent",
    "send-keys",
    "w1:p2",
    "enter",
  ]);
  assert.throws(() => submitArgs({}), /no pane/);
});

test("toggles fast mode in Codex and Pi", () => {
  assert.deepEqual(fastModePlan({ agent: "codex", pane_id: "w1:p1" }), [
    ["agent", "prompt", "w1:p1", "/fast"],
  ]);
  assert.deepEqual(fastModePlan({ agent: "pi", pane_id: "w1:p2" }), [
    ["pane", "send-text", "w1:p2", "/fast"],
    ["agent", "send-keys", "w1:p2", "enter"],
  ]);
  assert.throws(
    () => fastModePlan({ agent: "claude", pane_id: "w1:p3" }),
    /unsupported focused agent/,
  );
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

test("setup clones a blank target layer and preserves its metadata", () => {
  const oaiKeys = [
    "KV_OAI_AG00",
    "KV_OAI_AG01",
    "KV_OAI_AG02",
    "KV_OAI_AG03",
    "KV_OAI_AG04",
    "KV_OAI_AG05",
    "KV_OAI_ACT06",
    "KV_OAI_ACT07",
    "KV_OAI_ACT08",
    "KV_OAI_ACT09",
    "KV_OAI_ACT10",
    "KV_OAI_ACT11",
    "KV_OAI_ACT12",
    "KV_OAI_ENC_CC",
    "KV_OAI_ENC_CW",
    "KV_OAI_ENC_CLK",
  ];
  const keymap = {
    profiles: [
      {
        layers: [
          { layout: { keymap: [oaiKeys] } },
          {
            name: "Herdr",
            lights: { color: "blue" },
            layout: { keymap: [["KC_NONE"]] },
          },
        ],
      },
    ],
    linkedApps: [],
  };
  configureMicro(keymap);
  assert.deepEqual(keymap.profiles[0].layers[1].layout, {
    keymap: [oaiKeys],
  });
  assert.deepEqual(keymap.profiles[0].layers[1].lights, { color: "blue" });
  assert.deepEqual(
    keymap.profiles[0].layers.map(({ linkedAppId }) => linkedAppId),
    [0, 1],
  );

  keymap.profiles[0].layers[1].layout = { keymap: [["KC_A"]] };
  assert.throws(() => configureMicro(keymap), /not blank/);
  assert.equal(
    validateLayerClaims([{ id: "six", layer: 6, process: "app" }]).length,
    1,
  );
  assert.throws(() => validateLayerClaims([null]), /valid array/);
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
