const PLUGIN_ID = "gjermundgaraba.herdr-micro";
export const CODEX_PROCESS = "com.openai.codex";
export const GHOSTTY_PROCESS = "com.mitchellh.ghostty";
export const HERDR_LAYER = 2;
const REQUIRED_OAI_KEYS = [
  "KV_OAI_AG00",
  "KV_OAI_AG01",
  "KV_OAI_AG02",
  "KV_OAI_AG03",
  "KV_OAI_AG04",
  "KV_OAI_AG05",
  "KV_OAI_ACT06",
  "KV_OAI_ACT07",
  "KV_OAI_ACT08",
  "KV_OAI_ACT09",
  "KV_OAI_ACT10",
  "KV_OAI_ACT11",
  "KV_OAI_ACT12",
  "KV_OAI_ENC_CC",
  "KV_OAI_ENC_CW",
  "KV_OAI_ENC_CLK",
];

export function layerIdentity(layer) {
  return {
    appName: `Herdr Micro Layer ${layer}`,
    process: `${PLUGIN_ID}.layer-${layer}`,
  };
}

export function automaticLayer(frontmost, focusedHerdrSession) {
  if (frontmost?.process === CODEX_PROCESS) return 1;
  if (
    frontmost?.process === GHOSTTY_PROCESS &&
    focusedHerdrSession
  ) {
    return HERDR_LAYER;
  }
  return null;
}

export function focusedAppForLayer(layer) {
  return layer ? layerIdentity(layer) : null;
}

function oaiProfile(keymap) {
  const profiles = (keymap.profiles ?? []).filter((profile) =>
    JSON.stringify(profile.layers?.[0]?.layout ?? {}).includes("KV_OAI_AG05"),
  );
  if (profiles.length !== 1) {
    throw new Error(`expected one OAI profile, found ${profiles.length}`);
  }
  return profiles[0];
}

export function configureMicro(keymap) {
  const profile = oaiProfile(keymap);
  const source = profile.layers[0];
  const target = profile.layers[HERDR_LAYER - 1];
  if (!target) throw new Error(`Layer ${HERDR_LAYER} does not exist`);

  const sourceText = JSON.stringify(source.layout ?? {});
  if (!REQUIRED_OAI_KEYS.every((key) => sourceText.includes(key))) {
    throw new Error("Layer 1 is not a compatible Codex Micro OAI layout");
  }
  if (JSON.stringify(target.layout ?? {}) !== sourceText) {
    const targetCodes = [
      ...(target.layout?.keymap ?? []).flat(),
      ...(target.layout?.encoders ?? []).flat(),
      ...(target.layout?.joystick?.sectors ?? []).map(({ k }) => k),
    ];
    if (targetCodes.some((code) => !["KC_NONE", "KI_X"].includes(code))) {
      throw new Error(
        `Layer ${HERDR_LAYER} is not blank; refusing to overwrite it`,
      );
    }
    target.layout = structuredClone(source.layout);
  }

  keymap.linkedApps ??= [];
  if (!Array.isArray(keymap.linkedApps)) {
    throw new Error("linkedApps must be an array");
  }
  const ids = keymap.linkedApps.map(({ id }) => Number(id));
  if (ids.some((id) => !Number.isInteger(id) || id < 0)) {
    throw new Error("linked app IDs must be non-negative integers");
  }

  for (const layerNumber of [1, HERDR_LAYER]) {
    const identity = layerIdentity(layerNumber);
    const matches = keymap.linkedApps.filter(
      (app) => app.process === identity.process,
    );
    if (matches.length > 1) {
      throw new Error(`duplicate ${identity.process} bindings`);
    }
    let app = matches[0];
    if (!app) {
      app = {
        id: Math.max(-1, ...ids) + 1,
      };
      ids.push(app.id);
      keymap.linkedApps.push(app);
    }
    Object.assign(app, {
      name: identity.appName,
      process: identity.process,
      path: "",
    });
    profile.layers[layerNumber - 1].linkedAppId = app.id;
  }
  return keymap;
}
