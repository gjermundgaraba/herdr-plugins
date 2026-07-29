import fs from "node:fs";
import os from "node:os";
import path from "node:path";

const PLUGIN_ID = "gjermundgaraba.herdr-micro";
const EFFECTS = {
  off: 0,
  solid: 1,
  snake: 2,
  rainbow: 3,
  breath: 4,
  gradient: 5,
  "shallow-breath": 6,
};

export const DEFAULT_LIGHTING = {
  states: {
    blocked: { color: "#ffaa00", brightness: 1, effect: "solid", speed: 0 },
    done: { color: "#22cc55", brightness: 1, effect: "solid", speed: 0 },
    working: { color: "#2277ff", brightness: 1, effect: "breath", speed: 0.35 },
    idle: { color: "#ffffff", brightness: 0.25, effect: "solid", speed: 0 },
    unknown: { color: "#ffffff", brightness: 0.08, effect: "solid", speed: 0 },
  },
  focusedBrightness: 1,
  ambient: "status",
  keys: null,
};

export function lightingConfigPath() {
  const configDir =
    process.env.HERDR_PLUGIN_CONFIG_DIR ??
    path.join(os.homedir(), ".config", "herdr", "plugins", "config", PLUGIN_ID);
  return path.join(configDir, "lighting.json");
}

export function validateLightingConfig(config) {
  if (!config || Array.isArray(config) || typeof config !== "object") {
    throw new Error("lighting configuration must be an object");
  }
  for (const status of ["blocked", "done", "working", "idle", "unknown"]) {
    const light = config.states?.[status];
    if (
      !light ||
      !/^#[0-9a-f]{6}$/i.test(light.color) ||
      !Object.hasOwn(EFFECTS, light.effect) ||
      !unit(light.brightness) ||
      !unit(light.speed)
    ) {
      throw new Error(`invalid ${status} light`);
    }
  }
  if (!unit(config.focusedBrightness)) {
    throw new Error("focusedBrightness must be between 0 and 1");
  }
  for (const zone of ["ambient", "keys"]) {
    if (![null, "status"].includes(config[zone])) {
      throw new Error(`${zone} must be \"status\" or null`);
    }
  }
  return config;
}

function unit(value) {
  return typeof value === "number" && value >= 0 && value <= 1;
}

export function ensureLightingConfig(file = lightingConfigPath()) {
  fs.mkdirSync(path.dirname(file), { recursive: true });
  if (!fs.existsSync(file)) {
    fs.writeFileSync(file, `${JSON.stringify(DEFAULT_LIGHTING, null, 2)}\n`, {
      mode: 0o600,
    });
  }
  return file;
}

export function loadLightingConfig(file = lightingConfigPath()) {
  ensureLightingConfig(file);
  try {
    const config = validateLightingConfig(
      JSON.parse(fs.readFileSync(file, "utf8")),
    );
    return {
      ...config,
      states: Object.fromEntries(
        Object.entries(config.states).map(([status, light]) => [
          status,
          {
            c: Number.parseInt(light.color.slice(1), 16),
            b: light.brightness,
            e: EFFECTS[light.effect],
            s: light.speed,
          },
        ]),
      ),
    };
  } catch (error) {
    throw new Error(`invalid ${file}: ${error.message}`);
  }
}
