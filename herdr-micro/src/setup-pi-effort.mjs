import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { installPiEffort } from "./pi-effort-install.mjs";

const source = fileURLToPath(
  new URL("../integrations/pi/herdr-effort.js", import.meta.url),
);
const target = path.join(
  os.homedir(),
  ".pi",
  "agent",
  "extensions",
  "herdr-micro-effort.ts",
);
const result = await installPiEffort(source, target);
console.log(
  result.unchanged
    ? `Pi effort extension is current: ${target}`
    : `Installed Pi effort extension: ${target}`,
);
if (result.backup) console.log(`Previous extension backup: ${result.backup}`);
console.log("Run /reload in existing Pi sessions.");
