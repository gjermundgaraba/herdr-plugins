export const SLOT_COUNT = 6;

const OWNER_APPS = [
  {
    path: "/Applications/input.app/Contents/MacOS/input",
    process: null,
  },
  {
    path: "/Applications/Codex.app/Contents/MacOS/ChatGPT",
    process: "com.openai.codex",
  },
  {
    path: "/Applications/ChatGPT.app/Contents/MacOS/ChatGPT",
    process: "com.openai.chat",
  },
];

export function deviceOwner(processes, frontmostProcess) {
  return (
    OWNER_APPS.find(
      ({ path, process }) =>
        processes.includes(path) &&
        (process === null || process === frontmostProcess),
    )?.path ?? null
  );
}

const PRIORITY = {
  unknown: 0,
  idle: 1,
  working: 2,
  done: 3,
  blocked: 4,
};

const DEFAULT_LIGHTS = {
  blocked: { c: 0xffaa00, b: 1, e: 1, s: 0 },
  done: { c: 0x22cc55, b: 1, e: 1, s: 0 },
  working: { c: 0x2277ff, b: 1, e: 4, s: 0.35 },
  idle: { c: 0xffffff, b: 0.25, e: 1, s: 0 },
  unknown: { c: 0xffffff, b: 0.08, e: 1, s: 0 },
};

const OFF = { c: 0, b: 0, e: 0, s: 0 };
const REPORT_ID = 6;
const CHANNEL_RPC = 2;
const REPORT_SIZE = 64;
const MAX_PAYLOAD = 61;

const JOYSTICK_DIRECTIONS = ["right", "down", "left", "up"];

export function joystickEvent(
  angle,
  distance,
  lastSector,
  { engageDistance = 0.75, releaseDistance = 0.3 } = {},
) {
  if (!Number.isFinite(angle) || !Number.isFinite(distance)) {
    return { sector: lastSector, direction: null };
  }
  if (distance <= releaseDistance) return { sector: null, direction: null };
  if (lastSector === null && distance < engageDistance) {
    return { sector: null, direction: null };
  }
  const sector = Math.round(angle * 4) % 4;
  return {
    sector,
    direction:
      sector === lastSector ? null : JOYSTICK_DIRECTIONS[sector],
  };
}

function comparePriority(a, b) {
  return (
    PRIORITY[b.agent_status] - PRIORITY[a.agent_status] ||
    b.state_change_seq - a.state_change_seq
  );
}

export function assignSlots(previous, agents) {
  const sorted = [...agents].sort(comparePriority);
  const byId = new Map(agents.map((agent) => [agent.terminal_id, agent]));
  const slots = Array.from({ length: SLOT_COUNT }, (_, index) => {
    const id = previous[index];
    return id && byId.has(id) ? id : null;
  });
  const slotted = new Set(slots.filter(Boolean));

  for (const candidate of sorted) {
    if (slotted.has(candidate.terminal_id)) continue;
    const empty = slots.indexOf(null);
    if (empty >= 0) {
      slots[empty] = candidate.terminal_id;
      slotted.add(candidate.terminal_id);
      continue;
    }

    let victim = 0;
    for (let index = 1; index < SLOT_COUNT; index += 1) {
      if (comparePriority(byId.get(slots[index]), byId.get(slots[victim])) > 0) {
        victim = index;
      }
    }
    const displaced = byId.get(slots[victim]);
    if (PRIORITY[candidate.agent_status] <= PRIORITY[displaced.agent_status]) {
      break;
    }
    slotted.delete(displaced.terminal_id);
    slots[victim] = candidate.terminal_id;
    slotted.add(candidate.terminal_id);
  }

  return slots;
}

export function slotLighting(slots, agents, config = {}) {
  const byId = new Map(agents.map((agent) => [agent.terminal_id, agent]));
  const lights = config.states ?? DEFAULT_LIGHTS;
  return slots.map((id, index) => {
    const agent = id ? byId.get(id) : null;
    const light = agent ? lights[agent.agent_status] : OFF;
    return {
      id: index,
      ...light,
      ...(agent?.focused && config.focusedBrightness !== undefined
        ? { b: Math.max(light.b, config.focusedBrightness) }
        : {}),
    };
  });
}

export function aggregateLighting(slots, agents, config) {
  const byId = new Map(agents.map((agent) => [agent.terminal_id, agent]));
  const status = slots
    .map((id) => (id ? byId.get(id) : null))
    .filter(Boolean)
    .sort(comparePriority)[0]?.agent_status;
  const light = status ? config.states[status] : OFF;
  const side = { e: light.e, b: light.b, s: light.s, c: light.c };
  return Object.fromEntries(
    ["ambient", "keys"]
      .filter((zone) => config[zone] === "status")
      .map((zone) => [zone, side]),
  );
}

export function encodeMessage(method, params, id) {
  const envelope = { m: method };
  if (params !== undefined) envelope.p = params;
  if (id !== undefined) envelope.id = id;
  const bytes = Buffer.from(`${JSON.stringify(envelope)}\r\n`, "utf8");
  const reports = [];
  for (let offset = 0; offset < bytes.length; offset += MAX_PAYLOAD) {
    const chunk = bytes.subarray(offset, offset + MAX_PAYLOAD);
    const report = Buffer.alloc(REPORT_SIZE);
    report[0] = REPORT_ID;
    report[1] = CHANNEL_RPC;
    report[2] = chunk.length;
    chunk.copy(report, 3);
    reports.push(report);
  }
  return reports;
}

export class Reassembler {
  #buffer = Buffer.alloc(0);

  push(report) {
    const offset =
      report[0] === REPORT_ID && report[1] === CHANNEL_RPC
        ? 1
        : report[0] === CHANNEL_RPC
          ? 0
          : -1;
    if (offset < 0 || report.length < offset + 2) return [];
    const length = report[offset + 1];
    if (length > MAX_PAYLOAD || length + offset + 2 > report.length) return [];
    this.#buffer = Buffer.concat([
      this.#buffer,
      report.subarray(offset + 2, offset + 2 + length),
    ]);
    const messages = [];
    let newline;
    while ((newline = this.#buffer.indexOf(0x0a)) >= 0) {
      messages.push(this.#buffer.subarray(0, newline).toString("utf8").trim());
      this.#buffer = this.#buffer.subarray(newline + 1);
    }
    if (this.#buffer.length > 64 * 1024) this.#buffer = Buffer.alloc(0);
    return messages;
  }
}
