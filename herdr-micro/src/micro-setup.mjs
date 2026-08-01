import { execFile } from "node:child_process";
import fs from "node:fs/promises";
import path from "node:path";
import { promisify } from "node:util";
import { ensureControlConfig } from "./control-config.mjs";
import { ensureEffortConfig } from "./effort-config.mjs";
import { ensureLightingConfig } from "./lighting-config.mjs";
import { configureMicro } from "./layer-routing.mjs";
import { MicroDevice } from "./micro-device.mjs";
import {
  ensureStateDir,
  LOG_FILE,
  requestDaemon,
} from "./micro-control.mjs";

const run = promisify(execFile);
const { stdout: processes } = await run("/bin/ps", ["-axo", "comm="]);
const owner = processes
  .split("\n")
  .map((command) => command.trim())
  .find(
    (command) =>
      command.endsWith("/input") || command.endsWith("/ChatGPT"),
  );
if (owner) throw new Error(`quit ${owner} first`);
let bridgeRunning = false;
try {
  await requestDaemon("status", 250);
  bridgeRunning = true;
} catch {
  // No live bridge owns the socket.
}
if (bridgeRunning) throw new Error("stop the Micro bridge first");

async function readKeymap(device) {
  let offset = 0;
  let body = "";
  while (true) {
    const chunk = await device.request("fs.readbin", {
      file: "keymap.json",
      offset,
      len: 512,
    });
    const bytes = Buffer.from(chunk.data, "base64");
    if (bytes.length === 0) throw new Error("empty keymap chunk");
    body += bytes.toString("utf8");
    offset += bytes.length;
    if (offset >= chunk.total_size) return body;
  }
}

const device = await MicroDevice.open(() => {}, (error) => {
  console.error(`device error: ${error.message}`);
});
try {
  const status = await device.request("device.status");
  if (typeof status.version !== "string" || !status.version) {
    throw new Error("device did not report a firmware version");
  }
  const before = await readKeymap(device);
  const keymap = JSON.parse(before);
  const canonicalBefore = JSON.stringify(keymap);
  const after = JSON.stringify(configureMicro(keymap));
  if (after === canonicalBefore) {
    console.log("Layer 2 is already configured");
    process.exitCode = 0;
  } else {
    ensureStateDir();
    const backupPath = path.join(
      path.dirname(LOG_FILE),
      `keymap-before-setup-${Date.now()}.json`,
    );
    await fs.writeFile(backupPath, before, { flag: "wx" });
    console.log(`Backup: ${backupPath}`);

    const bytes = Buffer.from(after);
    for (let offset = 0; offset < bytes.length; offset += 384) {
      const data = bytes.subarray(offset, offset + 384);
      await device.request("fs.writebin", {
        file: "keymap.json",
        offset,
        data: data.toString("base64"),
        append: true,
        completed: offset + data.length >= bytes.length,
      });
    }
    if (await readKeymap(device) !== after) throw new Error("keymap read-back failed");
    console.log(
      `Cloned the OAI layout and bound AppSense Layer 2; backup: ${backupPath}`,
    );
  }

  const controls = ensureControlConfig();
  const effort = ensureEffortConfig();
  const lighting = ensureLightingConfig();
  console.log(`Firmware ${status.version}: compatible OAI keymap detected`);
  console.log(`Controls: ${controls}`);
  console.log(`Effort: ${effort}`);
  console.log(`Lighting: ${lighting}`);
} finally {
  await device.close();
}
