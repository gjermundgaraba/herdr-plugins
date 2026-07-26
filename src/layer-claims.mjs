import fs from "node:fs";
import os from "node:os";
import path from "node:path";

const PLUGIN_ID = "gjermundgaraba.herdr-micro";

export function loadLayerClaims() {
  const configDir =
    process.env.HERDR_PLUGIN_CONFIG_DIR ??
    path.join(os.homedir(), ".config", "herdr", "plugins", "config", PLUGIN_ID);
  const file = path.join(configDir, "claims.json");
  let claims;
  try {
    claims = JSON.parse(fs.readFileSync(file, "utf8"));
  } catch (error) {
    if (error.code === "ENOENT") return [];
    throw new Error(`invalid ${file}: ${error.message}`);
  }
  if (
    !Array.isArray(claims) ||
    claims.some(
      ({ id, layer, process, titleIncludes }) =>
        typeof id !== "string" ||
        !Number.isInteger(layer) ||
        layer < 1 ||
        layer > 3 ||
        typeof process !== "string" ||
        (titleIncludes !== undefined && typeof titleIncludes !== "string"),
    )
  ) {
    throw new Error(`${file} must contain valid layer claims`);
  }
  return claims;
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

export function configureAppSense(keymap) {
  const profiles = (keymap.profiles ?? []).filter((profile) =>
    JSON.stringify(profile.layers?.[0]?.layout ?? {}).includes("KV_OAI_AG05"),
  );
  if (profiles.length !== 1) {
    throw new Error(`expected one OAI profile, found ${profiles.length}`);
  }
  const profile = profiles[0];
  if (profile.layers.length < 2) throw new Error("Layer 2 does not exist");
  keymap.linkedApps ??= [];
  if (!Array.isArray(keymap.linkedApps)) {
    throw new Error("linkedApps must be an array");
  }
  const ids = keymap.linkedApps.map(({ id }) => Number(id));
  if (ids.some((id) => !Number.isInteger(id) || id < 0)) {
    throw new Error("linked app IDs must be non-negative integers");
  }

  for (const layerNumber of [1, 2]) {
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
