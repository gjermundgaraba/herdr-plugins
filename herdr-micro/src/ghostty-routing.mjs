import { execFile } from "node:child_process";
import { randomUUID } from "node:crypto";
import path from "node:path";
import { setTimeout as sleep } from "node:timers/promises";
import { fileURLToPath } from "node:url";
import { promisify } from "node:util";
import {
  discoverSessions,
  sessionEnvironment,
} from "./session-routing.mjs";

const runFile = promisify(execFile);
const herdrBin = process.env.HERDR_BIN_PATH ?? "herdr";
const GHOSTTY_STATE_SCRIPT = String.raw`
const app = Application("Ghostty");
const frontmost = app.frontmost();
const terminals = app.terminals().map((terminal) => ({
  id: terminal.id(),
  name: terminal.name(),
}));
let focusedTerminalId = null;
if (frontmost && app.windows().length > 0) {
  const tab = app.windows()[0].selectedTab();
  focusedTerminalId = tab.focusedTerminal().id();
}
JSON.stringify({ frontmost, focusedTerminalId, terminals });
`;

export function parseGhosttyState(stdout) {
  const state = JSON.parse(stdout);
  if (
    typeof state?.frontmost !== "boolean" ||
    !Array.isArray(state.terminals) ||
    !state.terminals.every(
      (terminal) =>
        typeof terminal?.id === "string" &&
        terminal.id.length > 0 &&
        typeof terminal.name === "string",
    ) ||
    (state.focusedTerminalId !== null &&
      typeof state.focusedTerminalId !== "string")
  ) {
    throw new Error("Ghostty returned invalid terminal state");
  }
  return state;
}

export async function inspectGhostty() {
  const { stdout } = await runFile("/usr/bin/osascript", [
    "-l",
    "JavaScript",
    "-e",
    GHOSTTY_STATE_SCRIPT,
  ]);
  return parseGhosttyState(stdout);
}

async function setSessionTitle(sessionName, title) {
  const { stdout } = await runFile(
    herdrBin,
    ["terminal", "title", "set", title],
    { env: sessionEnvironment(sessionName) },
  );
  const result = JSON.parse(stdout).result;
  if (result?.changed !== true) {
    throw new Error(
      `Herdr session ${sessionName} did not change a terminal title`,
    );
  }
}

async function findToken(inspect, token) {
  for (let attempt = 0; attempt < 10; attempt += 1) {
    const state = await inspect();
    const matches = state.terminals.filter(
      (terminal) => terminal.name === token,
    );
    if (matches.length === 1) return matches[0];
    if (matches.length > 1) {
      throw new Error(`Ghostty exposed duplicate probe token ${token}`);
    }
    await sleep(50);
  }
  return null;
}

export async function probeSessionTerminals(
  sessions,
  {
    inspect = inspectGhostty,
    setTitle = setSessionTitle,
    createToken = (sessionName) =>
      `__herdr_micro_${sessionName}_${randomUUID()}__`,
  } = {},
) {
  const mappings = [];
  for (const sessionName of sessions) {
    const before = await inspect();
    const originals = new Map(
      before.terminals.map((terminal) => [terminal.id, terminal.name]),
    );
    const token = createToken(sessionName);
    await setTitle(sessionName, token);

    let terminal = null;
    try {
      terminal = await findToken(inspect, token);
      if (!terminal) {
        throw new Error(
          `Herdr session ${sessionName} did not appear in Ghostty`,
        );
      }
      if (!originals.has(terminal.id)) {
        throw new Error(
          `Ghostty topology changed while probing ${sessionName}`,
        );
      }
      if (mappings.some((mapping) => mapping.terminalId === terminal.id)) {
        throw new Error(
          `multiple Herdr sessions targeted Ghostty terminal ${terminal.id}`,
        );
      }
      mappings.push({
        sessionName,
        terminalId: terminal.id,
      });
    } finally {
      if (terminal && originals.has(terminal.id)) {
        await setTitle(sessionName, originals.get(terminal.id));
      }
    }

    const restored = await inspect();
    if (
      restored.terminals.find(
        (candidate) => candidate.id === terminal.id,
      )?.name !== originals.get(terminal.id)
    ) {
      throw new Error(`failed to restore ${sessionName} terminal title`);
    }
  }
  return mappings;
}

export function focusedSession(mappings, state) {
  if (!state.frontmost || !state.focusedTerminalId) return null;
  return (
    mappings.find(
      (mapping) => mapping.terminalId === state.focusedTerminalId,
    )?.sessionName ?? null
  );
}

async function main() {
  const watch = process.argv.slice(2).includes("--watch");
  const sessions = await discoverSessions();
  const mappings = await probeSessionTerminals(sessions);
  const state = await inspectGhostty();
  console.log(
    JSON.stringify(
      {
        mappings,
        focusedSession: focusedSession(mappings, state),
        focusedTerminalId: state.focusedTerminalId,
      },
      null,
      2,
    ),
  );
  if (!watch) return;

  let previous = focusedSession(mappings, state);
  console.log("watching; press Ctrl-C to stop");
  while (true) {
    const nextState = await inspectGhostty();
    const next = focusedSession(mappings, nextState);
    if (next !== previous) {
      console.log(`${previous ?? "none"} -> ${next ?? "none"}`);
      previous = next;
    }
    await sleep(250);
  }
}

if (
  process.argv[1] &&
  path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)
) {
  await main().catch((error) => {
    console.error(error.message);
    process.exitCode = 1;
  });
}
