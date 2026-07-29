import { execFile } from "node:child_process";
import { setTimeout as sleep } from "node:timers/promises";
import { fileURLToPath } from "node:url";
import { promisify } from "node:util";
import {
  DEFAULT_CONTROLS,
  ensureControlConfig,
  keyBinding,
  loadControls,
  resolveBinding,
} from "./control-config.mjs";
import { diffPaneArgs } from "./diff-pane.mjs";
import { changeEffort } from "./effort.mjs";
import { fastModePlan } from "./fast-mode.mjs";
import {
  focusedAppForClaim,
  loadLayerClaims,
  resolveLayerClaim,
} from "./layer-claims.mjs";
import { loadLightingConfig } from "./lighting-config.mjs";
import { listenForControl } from "./micro-control.mjs";
import { MicroDevice } from "./micro-device.mjs";
import {
  assignSlots,
  aggregateLighting,
  deviceOwner,
  joystickEvent,
  SLOT_COUNT,
  slotLighting,
} from "./micro-protocol.mjs";
import { promptArgs, submitArgs } from "./submit.mjs";

const run = promisify(execFile);
const herdrBin = process.env.HERDR_BIN_PATH ?? "herdr";
const frontmostBin = fileURLToPath(new URL("../bin/frontmost", import.meta.url));
const controlConfigFile = ensureControlConfig();
const liveHerdrEnv = { ...process.env };
for (const key of [
  "HERDR_PANE_ID",
  "HERDR_TAB_ID",
  "HERDR_WORKSPACE_ID",
  "HERDR_PLUGIN_CONTEXT_JSON",
]) {
  delete liveHerdrEnv[key];
}

let agents = [];
let slots = Array.from({ length: SLOT_COUNT }, () => null);
let device = null;
let deviceState = "starting";
let owner = null;
let stopping = false;
let controls = DEFAULT_CONTROLS;
let lastLighting = "";
let lastFocusedApp = "";
let lastOpenError = "";
let lastHerdrError = "";
let lastFrontmostError = "";
let lastControlConfigError = "";
let lastLightingConfigError = "";
let herdrLostAt = null;
let frontmost = null;
let layerClaim = null;
let lastJoystickSector = null;
let controlQueue = Promise.resolve();
let managedAggregateZones = new Set();

function log(message) {
  console.log(`${new Date().toISOString()} ${message}`);
}

function refreshControls() {
  try {
    controls = loadControls(controlConfigFile);
    lastControlConfigError = "";
  } catch (error) {
    if (error.message !== lastControlConfigError) {
      lastControlConfigError = error.message;
      log(`control configuration failed: ${error.message}`);
    }
  }
}

function status() {
  return {
    device: owner ? "yielded" : deviceState,
    owner,
    agents: agents.length,
    frontmost,
    layerClaim: layerClaim
      ? { id: layerClaim.id, layer: layerClaim.layer }
      : null,
    slots: slots.map((id) => {
      const agent = agents.find((candidate) => candidate.terminal_id === id);
      return agent
        ? {
            pane: agent.pane_id,
            agent: agent.agent,
            status: agent.agent_status,
          }
        : null;
    }),
  };
}

async function refreshLayerClaim() {
  try {
    const layerClaims = loadLayerClaims();
    const { stdout } = await run(frontmostBin, []);
    frontmost = JSON.parse(stdout);
    layerClaim = resolveLayerClaim(layerClaims, frontmost);
    lastFrontmostError = "";
    if (!device) return;
    const focusedApp = focusedAppForClaim(layerClaim);
    if (!focusedApp) return;
    const signature = JSON.stringify(focusedApp);
    if (signature === lastFocusedApp) return;
    await device.setFocusedApp(focusedApp);
    lastFocusedApp = signature;
    log(`layer ${layerClaim.layer} claimed by ${layerClaim.id}`);
  } catch (error) {
    if (error.message !== lastFrontmostError) {
      lastFrontmostError = error.message;
      log(`frontmost window unavailable: ${error.message}`);
    }
  }
}

async function listAgents() {
  const { stdout } = await run(herdrBin, ["agent", "list"]);
  const envelope = JSON.parse(stdout);
  return (envelope.result?.agents ?? []).map((agent) => ({
    terminal_id: String(agent.terminal_id),
    pane_id: String(agent.pane_id),
    agent: String(agent.agent ?? ""),
    agent_status: ["idle", "working", "blocked", "done"].includes(
      agent.agent_status,
    )
      ? agent.agent_status
      : "unknown",
    state_change_seq: Number(agent.state_change_seq ?? 0),
    focused: agent.focused === true,
    cwd: String(agent.foreground_cwd ?? agent.cwd ?? ""),
  }));
}

async function findOwner() {
  const { stdout } = await run("/bin/ps", ["-axo", "command="]);
  return deviceOwner(stdout, frontmost?.process);
}

async function refreshAgents() {
  try {
    agents = await listAgents();
    herdrLostAt = null;
    lastHerdrError = "";
    slots = assignSlots(slots, agents);
    if (!device) return;
    let config;
    try {
      config = loadLightingConfig();
      lastLightingConfigError = "";
    } catch (error) {
      if (error.message !== lastLightingConfigError) {
        lastLightingConfigError = error.message;
        log(`lighting configuration failed: ${error.message}`);
      }
      return;
    }
    const lighting = {
      slots: slotLighting(slots, agents, config),
      aggregate: aggregateLighting(slots, agents, config),
    };
    const nextZones = new Set(Object.keys(lighting.aggregate));
    for (const zone of managedAggregateZones) {
      if (!nextZones.has(zone)) {
        lighting.aggregate[zone] = { e: 0, b: 0, s: 0, c: 0 };
      }
    }
    const signature = JSON.stringify(lighting);
    if (signature === lastLighting) return;
    if (Object.keys(lighting.aggregate).length > 0) {
      await device.setAggregateLighting(lighting.aggregate);
    }
    await device.setLighting(lighting.slots);
    managedAggregateZones = nextZones;
    lastLighting = signature;
  } catch (error) {
    herdrLostAt ??= Date.now();
    if (error.message !== lastHerdrError) {
      lastHerdrError = error.message;
      log(`Herdr unavailable: ${error.message}`);
    }
    if (Date.now() - herdrLostAt >= 60_000) {
      log("Herdr unavailable for 60 seconds; releasing device");
      await shutdown();
    }
  }
}

async function focusSlot(index) {
  const id = slots[index];
  const agent = agents.find((candidate) => candidate.terminal_id === id);
  if (!agent) return;
  await run(herdrBin, ["agent", "focus", agent.pane_id]);
}

function requireAgent(current) {
  if (!current) throw new Error("no focused Herdr agent");
  return current;
}

async function runPrompt(action, current) {
  requireAgent(current);
  if (!["idle", "done"].includes(current.agent_status)) {
    throw new Error(`focused agent is ${current.agent_status}`);
  }
  await run(herdrBin, promptArgs(action, current));
}

async function focusAdjacentPane(direction) {
  const { stdout } = await run(
    herdrBin,
    ["pane", "current"],
    { env: liveHerdrEnv },
  );
  const paneId = JSON.parse(stdout).result?.pane?.pane_id;
  if (!paneId) throw new Error("no focused Herdr pane");
  await run(
    herdrBin,
    ["pane", "focus", "--direction", direction, "--pane", paneId],
    { env: liveHerdrEnv },
  );
  log(`joystick focus ${direction}: ${paneId}`);
}

async function executeAction(action, current) {
  switch (action.action) {
    case "prompt":
      await runPrompt(action, current);
      break;
    case "diff":
      await run(herdrBin, diffPaneArgs(requireAgent(current)));
      break;
    case "fast":
      requireAgent(current);
      if (!["idle", "done"].includes(current.agent_status)) {
        throw new Error(`focused agent is ${current.agent_status}`);
      }
      for (const args of fastModePlan(current)) await run(herdrBin, args);
      break;
    case "submit":
      await run(herdrBin, submitArgs(requireAgent(current)));
      break;
    case "effort":
      requireAgent(current);
      await changeEffort({
        herdrBin,
        agent: current.agent,
        direction: action.direction,
        paneId: current.pane_id,
      });
      break;
    case "focus-pane":
      await focusAdjacentPane(action.direction);
      break;
  }
}

function dispatchControl(binding, source) {
  controlQueue = controlQueue
    .then(async () => {
      const current = (await listAgents()).find((agent) => agent.focused);
      const action = resolveBinding(binding, current?.agent);
      if (!action) return;
      await executeAction(action, current);
      log(
        `${source}: ${action.action}` +
          (current ? ` for ${current.agent} in ${current.pane_id}` : ""),
      );
    })
    .catch((error) => log(`${source} failed: ${error.message}`));
}

function onDeviceEvent(event) {
  if (event.type === "joystick") {
    const next = joystickEvent(
      event.angle,
      event.distance,
      lastJoystickSector,
      controls.joystick,
    );
    lastJoystickSector = next.sector;
    if (next.direction) {
      dispatchControl(
        controls.joystick[next.direction] ?? null,
        `joystick ${next.direction}`,
      );
    }
    return;
  }
  if (event.type !== "key") return;
  const match = /^AG0([0-5])$/.exec(event.key);
  if (match && event.action === 1) {
    void focusSlot(Number(match[1])).catch((error) =>
      log(`agent focus failed: ${error.message}`),
    );
  } else {
    const binding = keyBinding(controls, event.key, event.action);
    if (binding) dispatchControl(binding, event.key);
  }
}

async function closeDevice(blank = true) {
  const current = device;
  device = null;
  lastLighting = "";
  lastFocusedApp = "";
  lastJoystickSector = null;
  if (!current) return;
  if (blank) {
    if (managedAggregateZones.size > 0) {
      await current
        .setAggregateLighting(
          Object.fromEntries(
            [...managedAggregateZones].map((zone) => [
              zone,
              { e: 0, b: 0, s: 0, c: 0 },
            ]),
          ),
        )
        .catch(() => {});
    }
    await current
      .setLighting(slotLighting(Array(SLOT_COUNT).fill(null), []))
      .catch(() => {});
  }
  managedAggregateZones = new Set();
  await current.close();
}

async function openDevice() {
  if (device || owner || stopping) return;
  try {
    const opened = await MicroDevice.open(onDeviceEvent, (error) => {
      log(`device error: ${error.message}`);
      void closeDevice(false);
    });
    if (stopping || owner) {
      await opened.close();
      return;
    }
    device = opened;
    deviceState = "connected";
    lastOpenError = "";
    lastLighting = "";
    log("device connected");
    await refreshAgents();
    await refreshLayerClaim();
  } catch (error) {
    deviceState = error.message.includes("not found") ? "absent" : "busy";
    if (error.message !== lastOpenError) {
      lastOpenError = error.message;
      log(`device open failed: ${error.message}`);
    }
  }
}

async function shutdown() {
  if (stopping) return;
  stopping = true;
  log("stopping");
  await closeDevice();
  closeControl();
}

const closeControl = await listenForControl(status, () => void shutdown());
process.on("SIGINT", () => void shutdown());
process.on("SIGTERM", () => void shutdown());
refreshControls();
log("bridge started");

let ownerScanDue = 0;
while (!stopping) {
  try {
    refreshControls();
    await refreshLayerClaim();
    if (Date.now() >= ownerScanDue) {
      ownerScanDue = Date.now() + 1_000;
      const nextOwner = await findOwner();
      if (nextOwner !== owner) {
        owner = nextOwner;
        if (owner) {
          log(`yielding to ${owner}`);
          await closeDevice();
        } else {
          log("device owner cleared");
        }
      }
    }
    if (!owner) {
      await openDevice();
      await refreshAgents();
    }
  } catch (error) {
    log(`refresh failed: ${error.message}`);
  }
  // ponytail: polling keeps this bridge dependency-free; replace with a Herdr
  // event subscription only if one-second status latency becomes a problem.
  await sleep(1_000);
}
