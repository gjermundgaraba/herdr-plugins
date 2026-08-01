import { execFile } from "node:child_process";
import { promisify } from "node:util";

const runFile = promisify(execFile);
const herdrBin = process.env.HERDR_BIN_PATH ?? "herdr";
const ROUTING_CONTEXT =
  /^HERDR_(?:SOCKET_PATH|SESSION|(?:PANE|TAB|WORKSPACE)_ID|ACTIVE_|PLUGIN_(?:CONTEXT|ACTION|EVENT|ENTRYPOINT))/;

export function discoveryEnvironment(base = process.env) {
  const env = { ...base };
  for (const key of Object.keys(env)) {
    if (ROUTING_CONTEXT.test(key)) delete env[key];
  }
  return env;
}

export function sessionEnvironment(sessionName, base = process.env) {
  return {
    ...discoveryEnvironment(base),
    HERDR_SESSION: sessionName,
  };
}

export async function discoverSessions() {
  const { stdout } = await runFile(
    herdrBin,
    ["session", "list", "--json"],
    { env: discoveryEnvironment() },
  );
  return JSON.parse(stdout).sessions
    .filter(({ running }) => running)
    .map(({ name }) => name);
}
