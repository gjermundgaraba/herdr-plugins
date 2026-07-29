import fs from "node:fs";
import os from "node:os";
import path from "node:path";

const PLUGIN_ID = "gjermundgaraba.herdr-micro";
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

export function layerClaimsPath() {
  const configDir =
    process.env.HERDR_PLUGIN_CONFIG_DIR ??
    path.join(os.homedir(), ".config", "herdr", "plugins", "config", PLUGIN_ID);
  return path.join(configDir, "claims.json");
}

export function validateLayerClaims(claims) {
  if (!Array.isArray(claims)) {
    throw new Error("layer claims must be a valid array");
  }
  for (const claim of claims) {
    const { id, layer, process, titleIncludes } = claim ?? {};
    if (
      typeof id !== "string" ||
      !id ||
      !Number.isInteger(layer) ||
      layer < 1 ||
      layer > 6 ||
      typeof process !== "string" ||
      !process ||
      (titleIncludes !== undefined && typeof titleIncludes !== "string")
    ) {
      throw new Error("layer claims must be a valid array");
    }
  }
  return claims;
}

export function ensureLayerClaims(claims, file = layerClaimsPath()) {
  fs.mkdirSync(path.dirname(file), { recursive: true });
  if (!fs.existsSync(file)) {
    fs.writeFileSync(
      file,
      `${JSON.stringify(validateLayerClaims(claims), null, 2)}\n`,
      { mode: 0o600 },
    );
  }
  return file;
}

export function loadLayerClaims(file = layerClaimsPath()) {
  let claims;
  try {
    claims = JSON.parse(fs.readFileSync(file, "utf8"));
  } catch (error) {
    if (error.code === "ENOENT") return [];
    throw new Error(`invalid ${file}: ${error.message}`);
  }
  try {
    return validateLayerClaims(claims);
  } catch (error) {
    throw new Error(`invalid ${file}: ${error.message}`);
  }
}

export function resolveLayerClaim(claims, frontmost) {
  return (
    claims.findLast(
      (claim) =>
        claim.process === frontmost.process &&
        (!claim.titleIncludes ||
          frontmost.title.toLowerCase().includes(claim.titleIncludes.toLowerCase())),
    ) ?? null
  );
}

export function claimIdentity(claim) {
  return {
    appName: `Herdr Micro Layer ${claim.layer}`,
    process: `${PLUGIN_ID}.layer-${claim.layer}`,
  };
}

export function focusedAppForClaim(claim) {
  return claim ? claimIdentity(claim) : null;
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

export function cloneOaiLayout(keymap, targetLayer = 2) {
  if (!Number.isInteger(targetLayer) || targetLayer < 2 || targetLayer > 6) {
    throw new Error("target layer must be between 2 and 6");
  }
  const profile = oaiProfile(keymap);
  const source = profile.layers[0];
  const target = profile.layers[targetLayer - 1];
  if (!target) throw new Error(`Layer ${targetLayer} does not exist`);

  const sourceText = JSON.stringify(source.layout ?? {});
  if (!REQUIRED_OAI_KEYS.every((key) => sourceText.includes(key))) {
    throw new Error("Layer 1 is not a compatible Codex Micro OAI layout");
  }
  if (JSON.stringify(target.layout ?? {}) === sourceText) return keymap;

  const targetCodes = [
    ...(target.layout?.keymap ?? []).flat(),
    ...(target.layout?.encoders ?? []).flat(),
    ...(target.layout?.joystick?.sectors ?? []).map(({ k }) => k),
  ];
  if (targetCodes.some((code) => !["KC_NONE", "KI_X"].includes(code))) {
    throw new Error(`Layer ${targetLayer} is not blank; refusing to overwrite it`);
  }
  target.layout = structuredClone(source.layout);
  return keymap;
}

export function configureAppSense(keymap, targetLayer = 2) {
  const profile = oaiProfile(keymap);
  if (!profile.layers[targetLayer - 1]) {
    throw new Error(`Layer ${targetLayer} does not exist`);
  }
  keymap.linkedApps ??= [];
  if (!Array.isArray(keymap.linkedApps)) {
    throw new Error("linkedApps must be an array");
  }
  const ids = keymap.linkedApps.map(({ id }) => Number(id));
  if (ids.some((id) => !Number.isInteger(id) || id < 0)) {
    throw new Error("linked app IDs must be non-negative integers");
  }

  for (const layerNumber of [1, targetLayer]) {
    const identity = claimIdentity({ layer: layerNumber });
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

export function configureMicro(keymap, targetLayer = 2) {
  return configureAppSense(cloneOaiLayout(keymap, targetLayer), targetLayer);
}
