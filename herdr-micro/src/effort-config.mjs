import fs from "node:fs";
import os from "node:os";
import path from "node:path";

const PLUGIN_ID = "gjermundgaraba.herdr-micro";

export const DEFAULT_EFFORT = {
  codex: {
    raise: null,
    lower: null,
  },
};

export function effortConfigPath() {
  const configDir =
    process.env.HERDR_PLUGIN_CONFIG_DIR ??
    path.join(os.homedir(), ".config", "herdr", "plugins", "config", PLUGIN_ID);
  return path.join(configDir, "effort.json");
}

export function validateEffortConfig(config) {
  if (!config || Array.isArray(config) || typeof config !== "object") {
    throw new Error("effort configuration must be an object");
  }
  const codex = config.codex;
  if (!codex || Array.isArray(codex) || typeof codex !== "object") {
    throw new Error("codex effort configuration must be an object");
  }
  for (const direction of ["raise", "lower"]) {
    const key = codex[direction];
    if (key !== null && (typeof key !== "string" || !key.trim())) {
      throw new Error(`codex ${direction} must be a key or null`);
    }
  }
  return config;
}

export function ensureEffortConfig(file = effortConfigPath()) {
  fs.mkdirSync(path.dirname(file), { recursive: true });
  if (!fs.existsSync(file)) {
    fs.writeFileSync(file, `${JSON.stringify(DEFAULT_EFFORT, null, 2)}\n`, {
      mode: 0o600,
    });
  }
  return file;
}

export function loadEffortConfig(file = effortConfigPath()) {
  ensureEffortConfig(file);
  try {
    return validateEffortConfig(JSON.parse(fs.readFileSync(file, "utf8")));
  } catch (error) {
    throw new Error(`invalid ${file}: ${error.message}`);
  }
}
