import { spawn } from "node:child_process";
import fs from "node:fs";
import { fileURLToPath } from "node:url";
import {
  ensureStateDir,
  LOG_FILE,
  requestDaemon,
} from "./micro-control.mjs";

try {
  const current = await requestDaemon("status", 500);
  console.log(JSON.stringify(current));
  process.exit(0);
} catch {
  // No live bridge owns the control socket.
}

ensureStateDir();
const log = fs.openSync(LOG_FILE, "a");
const daemon = fileURLToPath(new URL("./micro-daemon.mjs", import.meta.url));
const child = spawn(process.execPath, [daemon], {
  detached: true,
  stdio: ["ignore", log, log],
});
child.unref();
fs.closeSync(log);

const deadline = Date.now() + 5_000;
while (Date.now() < deadline) {
  try {
    const current = await requestDaemon("status", 250);
    console.log(JSON.stringify(current));
    process.exit(0);
  } catch {
    await new Promise((resolve) => setTimeout(resolve, 100));
  }
}

console.error(`Micro bridge did not start; see ${LOG_FILE}`);
process.exit(1);
