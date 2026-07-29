import { execFile } from "node:child_process";
import fs from "node:fs/promises";
import path from "node:path";
import { promisify } from "node:util";
import { configureAppSense } from "./layer-claims.mjs";
import { MicroDevice } from "./micro-device.mjs";
import {
  ensureStateDir,
  LOG_FILE,
  requestDaemon,
} from "./micro-control.mjs";

const run = promisify(execFile);
const { stdout: processes } = await run("/bin/ps", ["-axo", "command="]);
const owner = [
  "/Applications/input.app/Contents/MacOS/input",
  "/Applications/Codex.app/Contents/MacOS/ChatGPT",
  "/Applications/ChatGPT.app/Contents/MacOS/ChatGPT",
].find((candidate) => processes.includes(candidate));
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
  const before = await readKeymap(device);
  const after = JSON.stringify(configureAppSense(JSON.parse(before)));
  if (after === before) {
    console.log("AppSense Layers 1 and 2 already configured");
    process.exitCode = 0;
  } else {
    ensureStateDir();
    const backupPath = path.join(
      path.dirname(LOG_FILE),
      `keymap-before-appsense-${Date.now()}.json`,
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
    console.log(`Bound AppSense Layers 1 and 2; backup: ${backupPath}`);
  }
} finally {
  await device.close();
}
