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
import { GestureDispatcher } from "./gestures.mjs";
import {
  focusedSession,
  inspectGhostty,
  probeSessionTerminals,
} from "./ghostty-routing.mjs";
import {
  automaticLayer,
  focusedAppForLayer,
  GHOSTTY_PROCESS,
} from "./layer-routing.mjs";
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
import { scrollPlan } from "./scroll.mjs";
import {
  discoverSessions,
  sessionEnvironment,
} from "./session-routing.mjs";
import { promptArgs, submitArgs } from "./submit.mjs";

const run = promisify(execFile);
const herdrBin = process.env.HERDR_BIN_PATH ?? "herdr";
const frontmostBin = fileURLToPath(new URL("../bin/frontmost", import.meta.url));
const controlConfigFile = ensureControlConfig();

let sessions = [];
let sessionMappings = [];
let selectedSession = null;
let routingReady = false;
const sessionSlots = new Map();
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
let noSessionsAt = null;
let lastRoutingError = "";
let nextMappingProbeAt = 0;
let frontmost = null;
let ghosttyState = null;
let activeLayer = null;
let lastJoystickSector = null;
let controlQueue = Promise.resolve();
let managedAggregateZones = new Set();
const gestures = new GestureDispatcher(dispatchControl);

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
    session: selectedSession,
    routing: routingReady ? "ready" : selectedSession ? "unavailable" : "none",
    sessions,
    sessionMappings: sessionMappings.map((mapping) => ({
      session: mapping.sessionName,
      terminal: mapping.terminalId,
    })),
    layer: activeLayer,
    agents: agents.length,
    frontmost,
    focusedTerminal: ghosttyState?.focusedTerminalId ?? null,
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

async function refreshFrontmost() {
  try {
    const { stdout } = await run(frontmostBin, []);
    frontmost = JSON.parse(stdout);
    lastFrontmostError = "";
  } catch (error) {
    if (error.message !== lastFrontmostError) {
      lastFrontmostError = error.message;
      log(`frontmost window unavailable: ${error.message}`);
    }
  }
}

async function selectLayer(layer) {
  if (!layer) return;
  activeLayer = layer;
  if (!device) return;
  const focusedApp = focusedAppForLayer(layer);
  const signature = JSON.stringify(focusedApp);
  if (signature === lastFocusedApp) return;
  await device.setFocusedApp(focusedApp);
  lastFocusedApp = signature;
  log(`layer ${layer} selected`);
}

async function listAgents(session) {
  const { stdout } = await run(
    herdrBin,
    ["agent", "list"],
    { env: sessionEnvironment(session) },
  );
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

async function paintLighting() {
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
}

async function selectSession(next) {
  if (selectedSession === next) return;
  routingReady = false;
  selectedSession = next;
  agents = [];
  slots = Array(SLOT_COUNT).fill(null);
  lastLighting = "";
  await paintLighting();
  log(next ? `Herdr session selected: ${next}` : "Herdr session unselected");
}

function routingFailure(message) {
  if (message === lastRoutingError) return;
  lastRoutingError = message;
  log(`Herdr session routing failed: ${message}`);
}

async function refreshSessionMappings() {
  nextMappingProbeAt = Date.now() + 5_000;
  if (sessions.length === 0) {
    sessionMappings = [];
    return;
  }
  try {
    sessionMappings = await probeSessionTerminals(sessions);
    lastRoutingError = "";
    log(
      "Herdr sessions mapped: " +
        sessionMappings
          .map((mapping) => `${mapping.sessionName}=${mapping.terminalId}`)
          .join(", "),
    );
  } catch (error) {
    routingFailure(error.message);
  }
}

async function refreshSessions() {
  let discovered;
  try {
    discovered = await discoverSessions();
    if (discovered.length > 0) {
      noSessionsAt = null;
    } else {
      noSessionsAt ??= Date.now();
    }
  } catch (error) {
    routingFailure(error.message);
    return;
  }

  const changed = JSON.stringify(discovered) !== JSON.stringify(sessions);
  sessions = discovered;
  if (selectedSession && !sessions.includes(selectedSession)) {
    await selectSession(null);
  }
  if (changed) {
    sessionMappings = [];
    await refreshSessionMappings();
  }

  if (sessions.length === 0 && Date.now() - noSessionsAt >= 60_000) {
    log("No Herdr sessions for 60 seconds; releasing device");
    await shutdown();
  }
}

async function refreshRouting() {
  if (!frontmost) return;
  if (frontmost.process !== GHOSTTY_PROCESS) {
    ghosttyState = null;
    await selectLayer(automaticLayer(frontmost, null));
    return;
  }

  try {
    ghosttyState = await inspectGhostty();
    let sessionName = focusedSession(sessionMappings, ghosttyState);
    if (!sessionName && Date.now() >= nextMappingProbeAt) {
      await refreshSessionMappings();
      ghosttyState = await inspectGhostty();
      sessionName = focusedSession(sessionMappings, ghosttyState);
    }
    if (!sessionName) return;
    if (!sessions.includes(sessionName)) return;
    await selectSession(sessionName);
    await selectLayer(automaticLayer(frontmost, sessionName));
    lastRoutingError = "";
  } catch (error) {
    routingFailure(error.message);
  }
}

async function refreshAgents() {
  const route = selectedSession;
  if (!route) return;
  try {
    const nextAgents = await listAgents(route);
    if (selectedSession !== route) return;
    const nextSlots = assignSlots(sessionSlots.get(route) ?? [], nextAgents);
    sessionSlots.set(route, nextSlots);
    agents = nextAgents;
    slots = nextSlots;
    routingReady = true;
    lastHerdrError = "";
    await paintLighting();
  } catch (error) {
    if (selectedSession !== route) return;
    const hadState =
      routingReady || agents.length > 0 || slots.some((slot) => slot !== null);
    routingReady = false;
    agents = [];
    slots = Array(SLOT_COUNT).fill(null);
    if (hadState) {
      lastLighting = "";
      await paintLighting();
    }
    if (error.message !== lastHerdrError) {
      lastHerdrError = error.message;
      log(`Herdr session ${route} unavailable: ${error.message}`);
    }
  }
}

function requireAgent(current) {
  if (!current) throw new Error("no focused Herdr agent");
  return current;
}

async function runPrompt(action, current, session) {
  requireAgent(current);
  if (!["idle", "done"].includes(current.agent_status)) {
    throw new Error(`focused agent is ${current.agent_status}`);
  }
  await run(
    herdrBin,
    promptArgs(action, current),
    { env: sessionEnvironment(session) },
  );
}

async function focusAdjacentPane(direction, session) {
  const env = sessionEnvironment(session);
  const { stdout } = await run(
    herdrBin,
    ["pane", "current"],
    { env },
  );
  const paneId = JSON.parse(stdout).result?.pane?.pane_id;
  if (!paneId) throw new Error("no focused Herdr pane");
  await run(
    herdrBin,
    ["pane", "focus", "--direction", direction, "--pane", paneId],
    { env },
  );
  log(`joystick focus ${direction}: ${session}/${paneId}`);
}

async function scrollPane(action, session) {
  const { stdout: frontmostOutput } = await run(frontmostBin, []);
  const currentFrontmost = JSON.parse(frontmostOutput);
  const currentGhostty =
    currentFrontmost.process === GHOSTTY_PROCESS
      ? await inspectGhostty()
      : null;
  if (
    !currentGhostty ||
    focusedSession(sessionMappings, currentGhostty) !== session
  ) {
    log(`scroll ignored: ${session} is not frontmost`);
    return false;
  }
  const env = sessionEnvironment(session);
  const [{ stdout: paneOutput }, { stdout: layoutOutput }] = await Promise.all([
    run(herdrBin, ["pane", "current"], { env }),
    run(herdrBin, ["pane", "layout"], { env }),
  ]);
  const pane = JSON.parse(paneOutput).result?.pane;
  const layout = JSON.parse(layoutOutput).result?.layout;
  const plan = scrollPlan(pane, layout, action.direction, action.percent);
  await run(frontmostBin, [
    "scroll",
    String(plan.notches),
    String(plan.x),
    String(plan.y),
    currentFrontmost.process,
  ]);
  log(
    `scrolled ${action.direction} ${action.percent}% in ` +
      `${session}/${plan.paneId}`,
  );
  return true;
}

async function executeAction(action, current, session) {
  const env = sessionEnvironment(session);
  switch (action.action) {
    case "prompt":
      await runPrompt(action, current, session);
      break;
    case "diff":
      await run(
        herdrBin,
        diffPaneArgs(requireAgent(current)),
        { env },
      );
      break;
    case "fast":
      requireAgent(current);
      if (!["idle", "done"].includes(current.agent_status)) {
        throw new Error(`focused agent is ${current.agent_status}`);
      }
      for (const args of fastModePlan(current)) {
        await run(herdrBin, args, { env });
      }
      break;
    case "submit":
      await run(
        herdrBin,
        submitArgs(requireAgent(current)),
        { env },
      );
      break;
    case "effort":
      requireAgent(current);
      await changeEffort({
        herdrBin,
        agent: current.agent,
        direction: action.direction,
        paneId: current.pane_id,
        env,
      });
      break;
    case "focus-pane":
      await focusAdjacentPane(action.direction, session);
      break;
    case "scroll":
      return scrollPane(action, session);
  }
  return true;
}

function captureSession() {
  return routingReady ? selectedSession : null;
}

function queueControl(source, action) {
  controlQueue = controlQueue
    .then(action)
    .catch((error) => log(`${source} failed: ${error.message}`));
}

function dispatchControl(binding, source, capturedSession) {
  if (!capturedSession) {
    log(`${source} ignored: no ready Herdr session`);
    return;
  }
  queueControl(source, async () => {
    const current = (await listAgents(capturedSession)).find(
      (agent) => agent.focused,
    );
    const action = resolveBinding(binding, current?.agent);
    if (!action) return;
    if ((await executeAction(action, current, capturedSession)) === false) {
      return;
    }
    log(
      `${source}: ${action.action} in ${capturedSession}` +
        (current ? ` for ${current.agent} in ${current.pane_id}` : ""),
    );
  });
}

function dispatchAgentFocus(index, source) {
  const session = captureSession();
  const id = slots[index];
  const agent = agents.find((candidate) => candidate.terminal_id === id);
  if (!session || !agent) {
    log(`${source} ignored: no ready Agent slot`);
    return;
  }
  const paneId = agent.pane_id;
  queueControl(source, async () => {
    await run(
      herdrBin,
      ["agent", "focus", paneId],
      { env: sessionEnvironment(session) },
    );
    log(`${source}: focused ${session}/${paneId}`);
  });
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
        captureSession(),
      );
    }
    return;
  }
  if (event.type !== "key") return;
  const match = /^AG0([0-5])$/.exec(event.key);
  if (match) {
    if (event.action === 1) {
      dispatchAgentFocus(Number(match[1]), event.key);
    }
    return;
  }
  const binding = keyBinding(controls, event.key, event.action);
  if (event.action === 2) {
    if (binding) dispatchControl(binding, event.key, captureSession());
  } else if ([0, 1].includes(event.action)) {
    gestures.handle(
      event.key,
      binding,
      event.action === 1,
      captureSession(),
    );
  }
}

async function closeDevice(blank = true) {
  gestures.clear();
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
    await paintLighting();
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
    await refreshFrontmost();
    await refreshSessions();
    await refreshRouting();
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
