import assert from "node:assert/strict";
import test from "node:test";
import {
  discoveryEnvironment,
  sessionEnvironment,
} from "../src/session-routing.mjs";
import {
  focusedSession,
  parseGhosttyState,
  probeSessionTerminals,
} from "../src/ghostty-routing.mjs";

const sessions = ["default", "werk"];

test("sanitizes inherited routing context and selects a named session", () => {
  const base = {
    PATH: "/bin",
    HERDR_CONFIG_PATH: "/tmp/herdr.toml",
    HERDR_SOCKET_PATH: "/tmp/old.sock",
    HERDR_SESSION: "default",
    HERDR_WORKSPACE_ID: "w1",
    HERDR_TAB_ID: "w1:t1",
    HERDR_PANE_ID: "w1:p1",
    HERDR_ACTIVE_PANE_ID: "w1:p2",
    HERDR_ACTIVE_PANE_CWD: "/tmp",
    HERDR_PLUGIN_CONTEXT_JSON: "{}",
    HERDR_PLUGIN_ACTION_ID: "start",
    HERDR_PLUGIN_EVENT_JSON: "{}",
    HERDR_PLUGIN_ENTRYPOINT_ID: "pane",
    HERDR_PLUGIN_CONFIG_DIR: "/tmp/config",
  };
  assert.deepEqual(discoveryEnvironment(base), {
    PATH: "/bin",
    HERDR_CONFIG_PATH: "/tmp/herdr.toml",
    HERDR_PLUGIN_CONFIG_DIR: "/tmp/config",
  });
  assert.deepEqual(sessionEnvironment(sessions[1], base), {
    PATH: "/bin",
    HERDR_CONFIG_PATH: "/tmp/herdr.toml",
    HERDR_PLUGIN_CONFIG_DIR: "/tmp/config",
    HERDR_SESSION: "werk",
  });
});

test("probes Ghostty terminals and restores their titles", async () => {
  const terminals = [
    { id: "personal-terminal", name: "pers @ herdr" },
    { id: "work-terminal", name: "werk @ herdr" },
  ];
  const sessionTerminal = new Map([
    ["default", "personal-terminal"],
    ["werk", "work-terminal"],
  ]);
  const inspect = async () => ({
    frontmost: true,
    focusedTerminalId: "personal-terminal",
    terminals: structuredClone(terminals),
  });
  const setTitle = async (session, title) => {
    terminals.find(
      (terminal) => terminal.id === sessionTerminal.get(session),
    ).name = title;
  };

  const mappings = await probeSessionTerminals(sessions, {
    inspect,
    setTitle,
    createToken: (session) => `probe-${session}`,
  });

  assert.deepEqual(mappings, [
    {
      sessionName: "default",
      terminalId: "personal-terminal",
    },
    {
      sessionName: "werk",
      terminalId: "work-terminal",
    },
  ]);
  assert.deepEqual(terminals, [
    { id: "personal-terminal", name: "pers @ herdr" },
    { id: "work-terminal", name: "werk @ herdr" },
  ]);
  assert.equal(
    focusedSession(mappings, await inspect()),
    "default",
  );
  assert.equal(
    focusedSession(mappings, {
      ...(await inspect()),
      frontmost: false,
    }),
    null,
  );

  assert.deepEqual(
    parseGhosttyState(
      '{"frontmost":true,"focusedTerminalId":"personal-terminal",' +
        '"terminals":[{"id":"personal-terminal","name":"pers @ herdr"}]}',
    ),
    {
      frontmost: true,
      focusedTerminalId: "personal-terminal",
      terminals: [
        { id: "personal-terminal", name: "pers @ herdr" },
      ],
    },
  );
  assert.throws(
    () => parseGhosttyState('{"frontmost":true,"terminals":null}'),
    /invalid terminal state/,
  );
});
