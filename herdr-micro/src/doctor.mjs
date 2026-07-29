import { execFile } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { promisify } from "node:util";
import {
  buttonConfigPath,
  loadButtons,
} from "./button-config.mjs";
import {
  effortConfigPath,
  loadEffortConfig,
} from "./effort-config.mjs";
import {
  layerClaimsPath,
  loadLayerClaims,
} from "./layer-claims.mjs";
import {
  lightingConfigPath,
  loadLightingConfig,
} from "./lighting-config.mjs";
import { requestDaemon } from "./micro-control.mjs";

const run = promisify(execFile);
const root = fileURLToPath(new URL("..", import.meta.url));
let failed = false;

function report(level, message) {
  console.log(`[${level}] ${message}`);
  if (level === "fail") failed = true;
}

function check(label, fn) {
  try {
    const detail = fn();
    report("ok", `${label}${detail ? `: ${detail}` : ""}`);
  } catch (error) {
    report("fail", `${label}: ${error.message}`);
  }
}

check("Node.js", () => {
  const major = Number(process.versions.node.split(".")[0]);
  if (major < 20) throw new Error("version 20 or newer is required");
  return process.version;
});
for (const binary of ["frontmost", "micro-hid"]) {
  check(binary, () => {
    fs.accessSync(path.join(root, "bin", binary), fs.constants.X_OK);
    return "executable";
  });
}
check("button configuration", () => {
  loadButtons();
  return buttonConfigPath();
});
let effort;
check("effort configuration", () => {
  effort = loadEffortConfig();
  return effortConfigPath();
});
check("lighting configuration", () => {
  loadLightingConfig();
  return lightingConfigPath();
});
check("layer claims", () => {
  const claims = loadLayerClaims();
  if (claims.length === 0) {
    report("warn", `no automatic layer claims in ${layerClaimsPath()}`);
  }
  return `${claims.length} configured`;
});

try {
  const status = await requestDaemon("status", 750);
  if (status.device === "connected") {
    report("ok", "Micro bridge: connected");
  } else {
    report("warn", `Micro bridge: ${status.device}`);
  }
} catch (error) {
  report("warn", `Micro bridge is not running: ${error.message}`);
}

if (!effort?.codex?.raise || !effort?.codex?.lower) {
  report("warn", `Codex effort shortcuts are unset in ${effortConfigPath()}`);
}

const piExtension = path.join(
  os.homedir(),
  ".pi",
  "agent",
  "extensions",
  "herdr-micro-effort.ts",
);
report(
  fs.existsSync(piExtension) ? "ok" : "warn",
  fs.existsSync(piExtension)
    ? `Pi effort extension: ${piExtension}`
    : "Pi effort extension is not installed (optional)",
);

try {
  await run("hunk", ["--version"]);
  report("ok", "Hunk diff integration");
} catch {
  report("warn", "Hunk is not installed; the diff button is unavailable");
}

try {
  const { stdout } = await run("/usr/sbin/ioreg", ["-l", "-d", "1", "-w", "0"]);
  const pid = /"kCGSSessionSecureInputPID"\s*=\s*(\d+)/.exec(stdout)?.[1];
  if (pid && pid !== "0") {
    const { stdout: command } = await run("/bin/ps", [
      "-p",
      pid,
      "-o",
      "command=",
    ]);
    report("warn", `Secure Input is held by ${command.trim() || `PID ${pid}`}`);
  } else {
    report("ok", "Secure Input is off");
  }
} catch {
  report("warn", "Secure Input status could not be read");
}

process.exitCode = failed ? 1 : 0;
