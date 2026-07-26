import { execFile } from "node:child_process";
import { setTimeout as sleep } from "node:timers/promises";
import { fileURLToPath } from "node:url";
import { promisify } from "node:util";
import { changeEffort } from "./effort.mjs";
import {
  focusedAppForClaim,
  loadLayerClaims,
  resolveLayerClaim,
} from "./layer-claims.mjs";
import { listenForControl } from "./micro-control.mjs";
import { MicroDevice } from "./micro-device.mjs";
import {
  assignSlots,
  deviceOwner,
  encoderEffortDirection,
  SLOT_COUNT,
  slotLighting,
} from "./micro-protocol.mjs";

const run = promisify(execFile);
const herdrBin = process.env.HERDR_BIN_PATH ?? "herdr";
const frontmostBin = fileURLToPath(new URL("../bin/frontmost", import.meta.url));
const layerClaims = loadLayerClaims();

let agents = [];
let slots = Array.from({ length: SLOT_COUNT }, () => null);
let device = null;
let deviceState = "starting";
let owner = null;
let stopping = false;
let effortBusy = false;
let lastLighting = "";
let lastFocusedApp = "";
let lastOpenError = "";
let lastHerdrError = "";
let lastFrontmostError = "";
let herdrLostAt = null;
let frontmost = null;
let layerClaim = null;

function log(message) {
  console.log(`${new Date().toISOString()} ${message}`);
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
    const lighting = slotLighting(slots, agents);
    const signature = JSON.stringify(lighting);
    if (signature === lastLighting) return;
    await device.setLighting(lighting);
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

async function adjustEffort(direction) {
  if (effortBusy) return;
  effortBusy = true;
  try {
    const current = (await listAgents()).find((agent) => agent.focused);
    if (!current) throw new Error("no focused Herdr agent");
    await changeEffort({
      herdrBin,
      agent: current.agent,
      direction,
      paneId: current.pane_id,
    });
    log(`effort ${direction}: ${current.agent} in ${current.pane_id}`);
  } catch (error) {
    log(`effort ${direction} failed: ${error.message}`);
  } finally {
    effortBusy = false;
  }
}

function onDeviceEvent(event) {
  if (event.type !== "key") return;
  const match = /^AG0([0-5])$/.exec(event.key);
  if (match && event.action === 1) {
    void focusSlot(Number(match[1])).catch((error) =>
      log(`agent focus failed: ${error.message}`),
    );
  } else if (event.action === 2) {
    const direction = encoderEffortDirection(event.key);
    if (direction) void adjustEffort(direction);
  }
}

async function closeDevice(blank = true) {
  const current = device;
  device = null;
  lastLighting = "";
  lastFocusedApp = "";
  if (!current) return;
  if (blank) {
    await current
      .setLighting(slotLighting(Array(SLOT_COUNT).fill(null), []))
      .catch(() => {});
  }
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
log("bridge started");

let ownerScanDue = 0;
while (!stopping) {
  try {
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
