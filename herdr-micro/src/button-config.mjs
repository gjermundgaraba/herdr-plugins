import fs from "node:fs";
import os from "node:os";
import path from "node:path";

const PLUGIN_ID = "gjermundgaraba.herdr-micro";
const BUILT_INS = new Set(["diff", "fast", "copy", "submit"]);
const AGENT_NAME = /^(default|[a-z][a-z0-9_-]*)$/;

export const DEFAULT_BUTTONS = {
  1: null,
  2: null,
  3: null,
  4: "copy",
  5: null,
  6: null,
  7: "submit",
};

export function buttonConfigPath() {
  const configDir =
    process.env.HERDR_PLUGIN_CONFIG_DIR ??
    path.join(os.homedir(), ".config", "herdr", "plugins", "config", PLUGIN_ID);
  return path.join(configDir, "buttons.json");
}

export function validateButtons(buttons) {
  if (!buttons || Array.isArray(buttons) || typeof buttons !== "object") {
    throw new Error("button configuration must be an object");
  }
  for (const [button, action] of Object.entries(buttons)) {
    if (!/^[1-7]$/.test(button)) throw new Error(`invalid button: ${button}`);
    if (action === null || BUILT_INS.has(action)) continue;
    if (
      !action ||
      Array.isArray(action) ||
      typeof action !== "object" ||
      Object.entries(action).length === 0 ||
      Object.entries(action).some(
        ([agent, prompt]) =>
          !AGENT_NAME.test(agent) ||
          typeof prompt !== "string" ||
          !prompt.trim(),
      )
    ) {
      throw new Error(`invalid action for button ${button}`);
    }
  }
  return buttons;
}

export function ensureButtonConfig(file = buttonConfigPath()) {
  fs.mkdirSync(path.dirname(file), { recursive: true });
  if (!fs.existsSync(file)) {
    fs.writeFileSync(file, `${JSON.stringify(DEFAULT_BUTTONS, null, 2)}\n`, {
      mode: 0o600,
    });
  }
  return file;
}

export function loadButtons(file = buttonConfigPath()) {
  ensureButtonConfig(file);
  try {
    return validateButtons(JSON.parse(fs.readFileSync(file, "utf8")));
  } catch (error) {
    throw new Error(`invalid ${file}: ${error.message}`);
  }
}

export function buttonAction(buttons, eventKey) {
  const match = /^ACT(0[6-9]|1[0-2])$/.exec(eventKey);
  return match ? (buttons[String(Number(match[1]) - 5)] ?? null) : null;
}

export function configuredPrompt(prompts, agent) {
  const prompt = Object.hasOwn(prompts, agent) ? prompts[agent] : prompts.default;
  if (!prompt) throw new Error(`no prompt configured for ${agent || "none"}`);
  return prompt;
}
