import fs from "node:fs";
import os from "node:os";
import path from "node:path";

const PLUGIN_ID = "gjermundgaraba.herdr-micro";
const AGENT_NAME = /^(default|[a-z][a-z0-9_-]*)$/;
const DIRECTIONS = new Set(["up", "down", "left", "right"]);

export const DEFAULT_CONTROLS = {
  version: 1,
  buttons: {
    1: null,
    2: null,
    3: {
      byAgent: {
        codex: { action: "fast" },
        pi: { action: "fast" },
        default: null,
      },
    },
    4: { action: "prompt", prompt: "/copy", submit: true },
    5: null,
    6: null,
    7: { action: "submit" },
  },
  dial: {
    clockwise: { action: "effort", direction: "raise" },
    counterclockwise: { action: "effort", direction: "lower" },
    press: { action: "prompt", prompt: "/model", submit: true },
  },
  joystick: {
    engageDistance: 0.75,
    releaseDistance: 0.3,
    up: { action: "scroll", direction: "up", percent: 50 },
    down: { action: "scroll", direction: "down", percent: 50 },
    left: { action: "focus-pane", direction: "left" },
    right: { action: "focus-pane", direction: "right" },
  },
};

export function controlConfigPath() {
  const configDir =
    process.env.HERDR_PLUGIN_CONFIG_DIR ??
    path.join(os.homedir(), ".config", "herdr", "plugins", "config", PLUGIN_ID);
  return path.join(configDir, "controls.json");
}

function object(value, label) {
  if (!value || Array.isArray(value) || typeof value !== "object") {
    throw new Error(`${label} must be an object`);
  }
  return value;
}

function fields(value, allowed, label) {
  const extra = Object.keys(value).find((key) => !allowed.includes(key));
  if (extra) throw new Error(`unknown ${label} field: ${extra}`);
}

function validateAction(value, label) {
  if (value === null) return;
  object(value, label);
  if (Object.hasOwn(value, "byAgent")) {
    fields(value, ["byAgent"], label);
    const variants = object(value.byAgent, `${label}.byAgent`);
    if (Object.keys(variants).length === 0) {
      throw new Error(`${label}.byAgent must not be empty`);
    }
    for (const [agent, action] of Object.entries(variants)) {
      if (!AGENT_NAME.test(agent)) throw new Error(`invalid agent: ${agent}`);
      if (action && Object.hasOwn(action, "byAgent")) {
        throw new Error(`${label}.byAgent.${agent} cannot contain byAgent`);
      }
      validateAction(action, `${label}.byAgent.${agent}`);
    }
    return;
  }

  switch (value.action) {
    case "prompt":
      fields(value, ["action", "prompt", "submit"], label);
      if (typeof value.prompt !== "string" || !value.prompt.trim()) {
        throw new Error(`${label}.prompt must be a non-empty string`);
      }
      if (value.submit !== undefined && typeof value.submit !== "boolean") {
        throw new Error(`${label}.submit must be true or false`);
      }
      break;
    case "diff":
    case "fast":
    case "submit":
      fields(value, ["action"], label);
      break;
    case "effort":
      fields(value, ["action", "direction"], label);
      if (!["raise", "lower"].includes(value.direction)) {
        throw new Error(`${label}.direction must be raise or lower`);
      }
      break;
    case "focus-pane":
      fields(value, ["action", "direction"], label);
      if (!DIRECTIONS.has(value.direction)) {
        throw new Error(`${label}.direction must be up, down, left, or right`);
      }
      break;
    case "scroll":
      fields(value, ["action", "direction", "percent"], label);
      if (!["up", "down"].includes(value.direction)) {
        throw new Error(`${label}.direction must be up or down`);
      }
      if (
        typeof value.percent !== "number" ||
        !Number.isFinite(value.percent) ||
        value.percent <= 0 ||
        value.percent > 100
      ) {
        throw new Error(`${label}.percent must be greater than 0 and at most 100`);
      }
      break;
    default:
      throw new Error(`${label}.action is invalid`);
  }
}

export function validateControls(config) {
  object(config, "control configuration");
  fields(config, ["version", "buttons", "dial", "joystick"], "control");
  if (config.version !== 1) throw new Error("version must be 1");

  for (const [button, binding] of Object.entries(
    object(config.buttons, "buttons"),
  )) {
    if (!/^[1-7]$/.test(button)) throw new Error(`invalid button: ${button}`);
    validateAction(binding, `buttons.${button}`);
  }

  const dial = object(config.dial, "dial");
  fields(dial, ["clockwise", "counterclockwise", "press"], "dial");
  for (const [input, binding] of Object.entries(dial)) {
    validateAction(binding, `dial.${input}`);
  }

  const joystick = object(config.joystick, "joystick");
  fields(
    joystick,
    [
      "engageDistance",
      "releaseDistance",
      "up",
      "down",
      "left",
      "right",
    ],
    "joystick",
  );
  const { engageDistance, releaseDistance } = joystick;
  if (
    typeof engageDistance !== "number" ||
    typeof releaseDistance !== "number" ||
    releaseDistance < 0 ||
    engageDistance > 1 ||
    releaseDistance >= engageDistance
  ) {
    throw new Error(
      "joystick distances must satisfy 0 <= releaseDistance < engageDistance <= 1",
    );
  }
  for (const direction of DIRECTIONS) {
    validateAction(joystick[direction] ?? null, `joystick.${direction}`);
  }
  return config;
}

export function ensureControlConfig(file = controlConfigPath()) {
  fs.mkdirSync(path.dirname(file), { recursive: true });
  if (!fs.existsSync(file)) {
    fs.writeFileSync(file, `${JSON.stringify(DEFAULT_CONTROLS, null, 2)}\n`, {
      mode: 0o600,
    });
  }
  return file;
}

export function loadControls(file = controlConfigPath()) {
  ensureControlConfig(file);
  try {
    return validateControls(JSON.parse(fs.readFileSync(file, "utf8")));
  } catch (error) {
    throw new Error(`invalid ${file}: ${error.message}`);
  }
}

export function resolveBinding(binding, agent) {
  if (!binding || !Object.hasOwn(binding, "byAgent")) return binding ?? null;
  return Object.hasOwn(binding.byAgent, agent)
    ? binding.byAgent[agent]
    : (binding.byAgent.default ?? null);
}

export function keyBinding(config, key, action) {
  const button = /^ACT(0[6-9]|1[0-2])$/.exec(key);
  if (button && action === 1) {
    return config.buttons[String(Number(button[1]) - 5)] ?? null;
  }
  if (action === 1 && key === "ENC_CLK") return config.dial.press ?? null;
  if (action !== 2) return null;
  if (key === "ENC_CC") return config.dial.clockwise ?? null;
  if (key === "ENC_CW") return config.dial.counterclockwise ?? null;
  return null;
}
