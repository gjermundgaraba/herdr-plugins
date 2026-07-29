import { execFile } from "node:child_process";
import fs from "node:fs/promises";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { promisify } from "node:util";
import { ensureButtonConfig } from "./button-config.mjs";
import { ensureEffortConfig } from "./effort-config.mjs";
import {
  configureMicro,
  ensureLayerClaims,
} from "./layer-claims.mjs";
import { MicroDevice } from "./micro-device.mjs";
import {
  ensureStateDir,
  LOG_FILE,
  requestDaemon,
} from "./micro-control.mjs";

const run = promisify(execFile);
const targetLayer = Number(process.argv[2] ?? 2);
if (!Number.isInteger(targetLayer) || targetLayer < 2 || targetLayer > 6) {
  throw new Error("target layer must be between 2 and 6");
}
const frontmostBin = fileURLToPath(new URL("../bin/frontmost", import.meta.url));
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
  const after = JSON.stringify(configureMicro(keymap, targetLayer));
  if (after === canonicalBefore) {
    console.log(`Layer ${targetLayer} is already configured`);
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
      `Cloned the OAI layout and bound AppSense Layer ${targetLayer}; backup: ${backupPath}`,
    );
  }

  const { stdout } = await run(frontmostBin, []);
  const frontmost = JSON.parse(stdout);
  const buttons = ensureButtonConfig();
  const effort = ensureEffortConfig();
  const claims = ensureLayerClaims([
    {
      id: "herdr",
      layer: targetLayer,
      process: frontmost.process,
    },
    {
      id: "codex",
      layer: 1,
      process: "com.openai.codex",
    },
  ]);
  console.log(`Firmware ${status.version}: compatible OAI keymap detected`);
  console.log(`Buttons: ${buttons}`);
  console.log(`Effort: ${effort}`);
  console.log(`Layer claims: ${claims}`);
} finally {
  await device.close();
}
